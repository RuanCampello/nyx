//! Structural monomorphisation of generic HIR bodies.
//!
//! Generic functions are lowered exactly once with `GenericParam` types. This
//! pass discovers concrete calls, clones the shared HIR body through
//! [Folder], and recursively substitutes the requested arguments

use crate::hir::{
    self, Expression, ExpressionKind, Function, FunctionId, FunctionKind, ItemTable, Res, Type,
    ids::IndexVec,
    visit::{self, Folder},
};
use std::collections::HashMap;

#[derive(Default)]
struct Collector<'hir> {
    instances: HashMap<InstanceKey<'hir>, FunctionId>,
    worklist: Vec<InstanceKey<'hir>>,
}

/// Folds a generic template into one concrete instance. Holds `scope`
/// wholesale rather than re-listing the handful of its fields this needs
/// (types/arrays/constants/symbols/methods/fn_defs) one by one.
struct SubstFolder<'a, 'hir> {
    scope: &'a ItemTable<'hir>,
    args: &'a [Type<'hir>],
}

type InstanceKey<'hir> = (FunctionId, Vec<Type<'hir>>);

impl<'hir> Folder<'hir> for SubstFolder<'_, 'hir> {
    fn arena(&self) -> &'hir bumpalo::Bump {
        self.scope.arena
    }

    fn fold_type(&mut self, typ: Type<'hir>) -> Type<'hir> {
        typ.subst(&self.scope.types, &self.scope.arrays, self.args)
    }

    fn fold_expression(&mut self, expression: &'hir Expression<'hir>) -> &'hir Expression<'hir> {
        if let ExpressionKind::ParamConst { param, interface, name } = expression.kind {
            let concrete = self.args[param as usize];
            let short_name = self.scope.symbols.get(name);
            let constant = self
                .scope
                .interface_constant(concrete, interface, short_name)
                .expect("generic bound guarantees an associated constant implementation");
            return self.scope.arena.alloc(Expression {
                id: expression.id,
                kind: ExpressionKind::Const(constant),
                span: expression.span,
            });
        }
        visit::fold_expression(self, expression)
    }

    fn fold_res(&mut self, resolution: Res) -> Res {
        match resolution {
            Res::ParamMethod { param, interface, name } => {
                let receiver = self.args[param as usize];
                let function = self
                    .scope
                    .functions
                    .methods
                    .get(&(receiver, name))
                    .copied()
                    .unwrap_or_else(|| {
                        panic!(
                            "interface {interface:?} guarantees method {name:?} for {receiver:?}"
                        )
                    });
                Res::Function(function)
            },
            Res::ParamFunction { param, interface, name } => {
                let receiver = self.args[param as usize];
                let function = self
                    .scope
                    .free_impl_function(receiver, interface, name)
                    .expect("generic bound guarantees an associated function implementation");
                Res::Function(function)
            },
            other => other,
        }
    }
}

pub(in crate::hir) fn monomorphise<'hir>(
    mut functions: IndexVec<FunctionId, Function<'hir>>,
    templates: &HashMap<FunctionId, Function<'hir>>,
    scope: &ItemTable<'hir>,
) -> IndexVec<FunctionId, Function<'hir>> {
    if templates.is_empty() {
        return functions;
    }

    let mut collector = Collector::default();
    for function in functions.iter() {
        collector.collect(function, templates);
    }

    let mut next_instance = scope.functions.defs.len() as u32;
    while let Some(key) = collector.worklist.pop() {
        if collector.instances.contains_key(&key) {
            continue;
        }
        let id = FunctionId(next_instance);
        let Some(function) = specialise(&key, id, templates, scope) else {
            collector.instances.insert(key.clone(), key.0);
            continue;
        };

        next_instance += 1;
        collector.instances.insert(key, id);
        collector.collect(&function, templates);
        functions.push(function);
    }

    for function in functions.iter_mut() {
        collector.rewrite(function, templates);
    }
    functions
}

impl<'hir> Collector<'hir> {
    fn collect(
        &mut self,
        function: &Function<'hir>,
        templates: &HashMap<FunctionId, Function<'hir>>,
    ) {
        for (&expr, &resolution) in &function.typeck.type_dependent_defs {
            let Res::Function(callee) = resolution else {
                continue;
            };
            if !templates.contains_key(&callee) {
                continue;
            }
            let args = function.typeck.node_args.get(&expr).cloned().unwrap_or_default();
            self.worklist.push((callee, args));
        }
    }

    fn rewrite(
        &self,
        function: &mut Function<'hir>,
        templates: &HashMap<FunctionId, Function<'hir>>,
    ) {
        let updates: Vec<_> = function
            .typeck
            .type_dependent_defs
            .iter()
            .filter_map(|(&expr, &resolution)| {
                let Res::Function(callee) = resolution else {
                    return None;
                };
                if !templates.contains_key(&callee) {
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
    key: &InstanceKey<'hir>,
    id: FunctionId,
    templates: &HashMap<FunctionId, Function<'hir>>,
    scope: &ItemTable<'hir>,
) -> Option<Function<'hir>> {
    let (template_id, args) = key;
    let template = templates.get(template_id)?;
    let open = scope.functions.defs[*template_id].clone();

    let base = scope.symbols.get(open.name).to_owned();
    let mangled = scope.instance_symbol(&base, args);
    let name = scope.symbols.insert(&mangled);

    let mut folder = SubstFolder { scope, args };
    let mut function = visit::fold_function(&mut folder, template);
    function.name = name;
    function.generics.clear();

    function.id = id;
    function.kind = match open.kind {
        FunctionKind::Method(method) => FunctionKind::Method(hir::Method {
            receiver: folder.fold_type(method.receiver),
            ..method
        }),
        other => other,
    };
    function.owner = open.owner.map_type(|on| folder.fold_type(on));
    Some(function)
}
