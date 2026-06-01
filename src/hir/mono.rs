//! Monomorphisation of generic functions
//!
//! A generic free function is lowered once to an *open* template: its signature is
//! registered with `GenericParam(i)` types (so call sites resolve and type-check),
//! and its AST body is parked in [`Scope::generic_fns`]. This pass takes the set of
//! already-lowered concrete functions, discovers every concrete `(function,
//! type_args)` reachable from them, and emits one specialised body per instance by
//! **re-lowering the template** with the generic parameters bound to concrete types
//!
//! The call graph is read straight out of the [`super::TypeckResults`] side-tables that
//! `A0` introduced: `type_dependent_defs` gives the (template) callee of every call,
//! and `type_dependent_substs` gives the concrete args — exactly the input a
//! collector needs, so no HIR tree-walk is required here.
//!
//! # How this differs from rustc, and what to refactor for advanced generic
//!
//! rustc never multiplies the *definition* of a generic item. A type is
//! `TyKind::Adt(AdtDef, GenericArgs)` and a parameter is `TyKind::Param`; there is
//! exactly one `AdtDef` for `Optional`, and `Optional<i32>` vs `Optional<T>` differ
//! only by the interned args. Monomorphisation is an [`Instance { def, args }`] that
//! couples one *shared, generic* MIR body with some args; the body is folded with
//! those args lazily at codegen (`instantiate_mir_and_normalize_erasing_regions`),
//! *"no monomorphized MIR is ever created"*
//!
//! Nyx instead uses the name-mangling model (`Optional<i32>` becomes a fresh
//! concrete `Enum` named `Optional$i32`), so this pass **clones a body per instance**
//! rather than folding one shared body. That keeps the whole backend on concrete
//! types but costs us three things, all of which want the `Adt(id, GenericArgsId)`
//! type model from `original_plan.txt` to fix properly:
//!
//! 1. **Inference can't see through generic aggregates.** `fn f<T>(x: &Box<T>)`
//!    erases `T` into `Box$i32`, so [`crate::hir::lower`] can only infer `T` from
//!    bare/`&T` params and needs a turbofish otherwise. With `Adt(Box,[Param0])` the
//!    arg type literally contains `Param0` and unifies structurally.
//! 2. **Methods on generic types aren't specialised here.** `impl Box<T>` /
//!    `impl Optional<T>` methods are parked in [`Scope::generic_impls`] but this pass
//!    only handles free functions. Doing methods needs the receiver instance
//!    (`Box$i32`) to carry its args so `.get()` resolves to the right specialisation —
//!    i.e. method/trait selection against a generic impl, which is what rustc's
//!    `Instance::resolve` + trait solving does. The clean refactor is to instantiate
//!    an ADT's impl methods *together with* the ADT in `Scope::instantiate_*`.
//! 3. **One body is cloned per instance** instead of shared + substituted. With a
//!    `Param`-carrying generic MIR body and `Type::subst` over it (the original plan's
//!    Components D/F), we'd lower once and specialise the MIR, matching rustc.
//!
//! Until then, this handles the self-contained, common case: generic *free*
//! functions whose parameters are scalars/references

use crate::hir::{
    Function, FunctionId, FunctionKind, RefTarget, Res, Type, TypeKind,
    error::HirError,
    index_vec::IndexVec,
    lower::FunctionBuilder,
    scope::{FunctionSignature, Scope},
    symbols::SymbolTable,
};
use std::collections::HashMap;

/// A concrete instantiation request
type InstanceKey = (FunctionId, Vec<Type>);

#[derive(Default)]
struct Collector {
    instances: HashMap<InstanceKey, FunctionId>,
    worklist: Vec<InstanceKey>,
}

/// Specialise every generic instance reachable from `functions` and append the
/// emitted bodies, repointing each generic call to its specialisation
pub(in crate::hir) fn monomorphise<'hir>(
    mut functions: IndexVec<FunctionId, Function<'hir>>,
    scope: &mut Scope<'hir>,
    symbols: &mut SymbolTable,
    arena: &'hir bumpalo::Bump,
) -> Result<IndexVec<FunctionId, Function<'hir>>, HirError<'hir>> {
    if scope.generic_fns.is_empty() {
        return Ok(functions);
    }

    let mut collector = Collector::default();

    for function in functions.iter() {
        collector.collect(function, scope);
    }

    while let Some(key) = collector.worklist.pop() {
        if collector.instances.contains_key(&key) {
            continue;
        }
        let (id, function) = specialise(&key, scope, symbols, arena)?;
        collector.instances.insert(key, id);
        collector.collect(&function, scope);
        functions.push(function);
    }

    for function in functions.iter_mut() {
        collector.rewrite(function, scope);
    }

    Ok(functions)
}

impl Collector {
    fn collect(&mut self, function: &Function<'_>, scope: &Scope<'_>) {
        for (&expr, &callee) in &function.typeck.type_dependent_defs {
            let Res::Function(callee) = callee else {
                continue;
            };
            if !scope.generic_fns.contains_key(&callee) {
                continue;
            }

            let args = function.typeck.node_args.get(&expr).cloned().unwrap_or_default();
            self.worklist.push((callee, args));
        }
    }

    fn rewrite(&self, function: &mut Function<'_>, scope: &Scope<'_>) {
        let updates: Vec<_> = function
            .typeck
            .type_dependent_defs
            .iter()
            .filter_map(|(&expr, &callee)| {
                let Res::Function(callee) = callee else {
                    return None;
                };
                if !scope.generic_fns.contains_key(&callee) {
                    return None;
                }
                let args = function.typeck.node_args.get(&expr).cloned().unwrap_or_default();
                self.instances.get(&(callee, args)).map(|&id| (expr, id))
            })
            .collect();

        for (expr, id) in updates {
            function.typeck.type_dependent_defs.insert(expr, Res::Function(id));
        }
    }
}

fn specialise<'hir>(
    key: &InstanceKey,
    scope: &mut Scope<'hir>,
    symbols: &mut SymbolTable,
    arena: &'hir bumpalo::Bump,
) -> Result<(FunctionId, Function<'hir>), HirError<'hir>> {
    let (template_id, args) = key;

    let template = scope.generic_fns[template_id].clone();
    let open = &scope.signatures[*template_id];

    let base = symbols.get(open.name).to_string();
    let kind = open.kind;
    let is_const = open.is_const;
    let inline = open.inline;

    let mangled = scope.mangle_generic(&base, args, symbols);
    let name = symbols.insert(&mangled);

    let mut env = scope.generic_fn_envs.get(template_id).cloned().unwrap_or_default();
    env.extend(
        template
            .generics
            .iter()
            .zip(args)
            .map(|(generic, &typ)| (generic.name.to_string(), typ)),
    );

    let receiver_type = match kind {
        FunctionKind::Method(method) => Some(method.receiver),
        FunctionKind::Free | FunctionKind::Intrinsic(_) => None,
    };
    let mut params =
        Vec::with_capacity(template.params.len() + usize::from(receiver_type.is_some()));
    if let (Some(receiver), Some(receiver_type)) = (template.receiver, receiver_type) {
        params.push(Type::new(TypeKind::Ref {
            mutable: receiver.mutable,
            to: RefTarget::try_from(receiver_type).expect("receiver must be a reference target"),
        }));
    }
    params.extend(scope.resolve_params(&template.params, symbols, receiver_type, Some(&env))?);
    let return_type = scope.resolve_return_type(
        template.return_type.as_ref(),
        symbols,
        receiver_type,
        Some(&env),
    )?;

    let id = scope.push_signature(FunctionSignature {
        name,
        params,
        return_type,
        kind,
        is_const,
        inline,
    });
    if matches!(kind, FunctionKind::Free) {
        scope.functions.insert(name, id);
    }

    // TODO(advanced-generics): `in_std` is hard-coded `false` because templates do
    // not currently record their origin module, a generic std helper that lowers a
    // `syscall` would mis-resolve. Track the origin on the template when needed
    let function =
        FunctionBuilder::new_instance(scope, symbols, id, &template, false, arena, env).lower()?;

    Ok((id, function))
}
