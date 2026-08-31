//! Interface-impl validation.
//!
//! After fn_defs are extended into `ItemTable`, this pass checks that every
//! `impl T with Interface` actually satisfies the interface: all required
//! methods are present, fn_defs match (after `Self` and generic-param
//! substitution), and every superinterface is implemented
//!
//! The pass is read-only against `ItemTable`, it never mutates the namespace

use crate::{
    hir::{
        self, FunctionId, InterfaceConstSignature, InterfaceMethodSignature, SymbolId, Type,
        collect::{self, ItemTable},
        declarations::Declarations,
        error::{HirError, HirErrorKind, hir_error},
        type_resolver::{self, ResolveCtx},
    },
    lexer::Spanned,
    parser::statement,
};

struct ImplCheck<'a, 'hir, 'src> {
    scope: &'a ItemTable<'hir>,
    implementation: &'a statement::Impl<'src>,
    interface: &'a hir::InterfaceSignature<'hir>,
    interface_name: &'src str,
    receiver: Type<'hir>,
    subst: Vec<Type<'hir>>,
}

/// Run all interface-related validation against a fully-extended `ItemTable`
/// Composes the per-impl signature check with the inheritance check, either
/// can fail independently
pub(in crate::hir) fn validate<'hir, 'd, 'h: 'hir>(
    scope: &ItemTable<'hir>,
    decls: &Declarations<'d, 'h>,
) {
    validate_hierarchy(scope, decls);
    validate_impls(scope, decls);
}

fn validate_impls<'hir, 'd, 'h: 'hir>(scope: &ItemTable<'hir>, decls: &Declarations<'d, 'h>) {
    for implementation in &decls.impls {
        let Some(interface_name) = implementation.interface else {
            continue;
        };
        let Some(check) = ImplCheck::new(scope, implementation, interface_name) else {
            continue;
        };

        check.superinterfaces();
        for required in &check.interface.methods {
            check.check_method(required);
        }
        for required in &check.interface.constants {
            check.check_constant(required);
        }
    }
}

fn validate_hierarchy<'hir, 'd, 'h: 'hir>(scope: &ItemTable<'hir>, decls: &Declarations<'d, 'h>) {
    for interface in &decls.interfaces {
        for &name in &interface.superinterfaces {
            let known = scope
                .symbols
                .get_id(name)
                .is_some_and(|symbol| scope.interfaces.defs.contains_key(&symbol));

            if !known {
                scope.soft(hir_error!(interface.span, UnknownInterface { name }));
            }
        }
    }
}

macro_rules! impl_error {
    ($check:expr, $span:expr, $kind:ident { $($field:ident $(: $value:expr)?),* $(,)? }) => {
        hir_error!($span, $kind {
            struct_name: $check.implementation.name,
            interface_name: $check.interface_name,
            $($field $(: $value)?),*
        })
    };
}

impl<'a, 'hir, 'src: 'hir> ImplCheck<'a, 'hir, 'src> {
    fn new(
        scope: &'a ItemTable<'hir>,
        implementation: &'a statement::Impl<'src>,
        interface_name: &'src str,
    ) -> Option<Self> {
        let interface = scope
            .symbols
            .get_id(interface_name)
            .and_then(|id| scope.interfaces.defs.get(&id));

        let Some(interface) = interface else {
            scope.soft(hir_error!(implementation.span, UnknownInterface { name: interface_name }));
            return None;
        };

        let env = collect::open_impl_env(&scope.types, implementation);
        let ctx = scope.root_ctx().with_env(&env);

        let annotation = &implementation.receiver;
        let receiver =
            type_resolver::resolve_annotation(&ctx, annotation.value_ref(), annotation.span())
                .map_err(|err| scope.soft(err))
                .ok()?;

        let mut check = Self {
            receiver,
            scope,
            interface,
            interface_name,
            implementation,
            subst: Vec::new(),
        };
        check.subst = check.subst_table(ctx.with_self(receiver))?;

        Some(check)
    }

    fn subst_table(&self, ctx: ResolveCtx<'_, 'hir>) -> Option<Vec<Type<'hir>>> {
        let arity = self.interface.generic_params.len();
        let associated = &self.interface.associated_types;
        let declared: &[_] =
            match self.implementation.interface_type.as_ref().map(Spanned::value_ref) {
                Some(statement::Type::Generic(_, decl)) => decl,
                _ => &[],
            };

        let mut args = declared
            .iter()
            .map(|arg| {
                type_resolver::resolve_annotation(&ctx, arg.value_ref(), arg.span())
                    .map_err(|err| self.scope.soft(err))
                    .ok()
            })
            .collect::<Option<Vec<_>>>()?;

        args.resize(arity, self.scope.types.common.self_type);
        args.extend(associated.iter().map(|&associated| {
            let key = (self.receiver, associated);
            self.scope.interfaces.associated_types.get(&key).copied().unwrap_or_else(|| {
                let (span, name) = (self.implementation.span, self.name(associated));
                let err = hir_error!(
                    span,
                    UnboundAssociatedType { name, interface_name: self.interface_name }
                );
                self.scope.soft(err);
                self.scope.types.common.error
            })
        }));

        Some(args)
    }

    fn superinterfaces(&self) {
        let span = self.implementation.span;
        for &required in &self.interface.superinterfaces {
            let known = self.scope.interfaces.defs.contains_key(&required);
            if known && !self.scope.implements_interface(self.receiver, required) {
                let superinterface_name = self.name(required);
                let err =
                    impl_error!(self, span, MissingSuperinterfaceImpl { superinterface_name });
                self.scope.soft(err);
            }
        }
    }

    fn check_method(&self, required: &InterfaceMethodSignature<'hir>) {
        let name = self.scope.symbols.get(required.name);
        let Some(found) = self.implementation.methods.iter().find(|m| m.name == name) else {
            let span = self.implementation.span;
            return self.scope.soft(HirError { kind: self.missing_method(required), span });
        };
        let Some(id) = self.function_id(found, required) else {
            return;
        };

        let (signature, has_receiver) = (&self.scope.functions.defs[id], found.receiver.is_some());
        let (receiver_mut, params) = (signature.receiver_mutable(), signature.explicit_params());
        let method_name = self.name(required.name);
        let decl = collect::source_span(required.decl_span);
        if required.is_const && !signature.is_const {
            self.scope.soft(impl_error!(
                self,
                found.span,
                NonConstInterfaceMethod { method_name, decl }
            ));
        }

        let expected_params = required.params.iter().map(|&typ| self.substituted(typ));
        if has_receiver == required.has_receiver
            && (!has_receiver || receiver_mut == required.receiver_mut)
            && signature.return_type == self.substituted(required.return_type)
            && expected_params.eq(params.iter().copied())
        {
            return;
        }

        let typ = signature.return_type;
        let actual = format_signature(method_name, (has_receiver, receiver_mut), params, typ);
        let receiver = (required.has_receiver, required.receiver_mut);
        let expected =
            format_signature(method_name, receiver, &required.params, required.return_type);
        let (arena, span) = (self.scope.arena, found.span);
        let (expected, found) = (arena.alloc_str(&expected), arena.alloc_str(&actual));
        self.scope.soft(impl_error!(
            self,
            span,
            InterfaceSignatureMismatch { method_name, expected, found, decl }
        ));
    }

    fn check_constant(&self, required: &InterfaceConstSignature<'hir>) {
        let (constant_name, decl) =
            (self.name(required.name), collect::source_span(required.decl_span));
        let ItemTable { symbols, mangler, values, .. } = self.scope;
        let name = symbols.get(required.name);

        let Some(declared) = self.implementation.constants.iter().find(|c| c.name == name) else {
            let span = self.implementation.span;
            let err = impl_error!(self, span, MissingInterfaceConstant { constant_name, decl });
            return self.scope.soft(err);
        };

        let mangled = mangler.scoped_item(self.implementation.name, declared.name);
        let found =
            symbols.get_id(&mangled).and_then(|id| values.constants.get(&id)).map(|c| c.typ);

        let expected = self.substituted(required.typ);
        match found {
            Some(found) if found != expected => self.scope.soft(impl_error!(
                self,
                declared.span,
                InterfaceConstantTypeMismatch { constant_name, expected, found, decl }
            )),
            _ => {},
        }
    }

    fn function_id(
        &self,
        method: &statement::Function<'src>,
        required: &InterfaceMethodSignature,
    ) -> Option<FunctionId> {
        let resolved = match method.receiver {
            Some(_) => self
                .scope
                .symbols
                .get_id(method.name)
                .and_then(|symbol| self.scope.method(self.receiver, symbol))
                .ok_or_else(|| HirError { kind: self.missing_method(required), span: method.span }),
            _ => self.scope.function_id(method, Some(self.implementation.name), |_| {
                self.missing_method(required)
            }),
        };
        resolved.map_err(|error| self.scope.soft(error)).ok()
    }

    #[inline]
    fn missing_method(&self, required: &InterfaceMethodSignature) -> HirErrorKind<'hir> {
        HirErrorKind::MissingInterfaceMethod {
            struct_name: self.implementation.name,
            interface_name: self.interface_name,
            method_name: self.name(required.name),
            decl: collect::source_span(required.decl_span),
        }
    }

    #[inline]
    fn name(&self, symbol: SymbolId) -> &'hir str {
        self.scope.arena.alloc_str(self.scope.symbols.get(symbol))
    }

    #[inline]
    fn substituted(&self, typ: Type<'hir>) -> Type<'hir> {
        use hir::TypeKind as T;
        let (types, self_typ) = (&self.scope.types, self.receiver);
        let bound = (!self.subst.is_empty())
            .then(|| typ.subst(types, &self.scope.arrays, &self.subst))
            .unwrap_or(typ);

        match bound.kind() {
            T::SelfType => self_typ,
            T::Ref { mutable, to } if to.kind() == T::SelfType => types.refer(self_typ, mutable),
            _ => bound,
        }
    }
}

fn format_signature(
    name: &str,
    receiver: (bool, bool),
    params: &[Type<'_>],
    return_type: Type<'_>,
) -> String {
    let (has_receiver, receiver_mut) = receiver;
    let parameters = has_receiver
        .then(|| match receiver_mut {
            true => "&mut self",
            _ => "&self",
        })
        .into_iter()
        .map(str::to_owned)
        .chain(params.iter().map(ToString::to_string))
        .collect::<Vec<_>>();
    format!("fn {name}({}): {return_type}", parameters.join(", "))
}
