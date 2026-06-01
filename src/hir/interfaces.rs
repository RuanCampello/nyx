//! Interface-impl validation.
//!
//! After signatures are extended into `Scope`, this pass checks that every
//! `impl T with Interface` actually satisfies the interface: all required
//! methods are present, signatures match (after `Self` and generic-param
//! substitution), and every superinterface is implemented.
//!
//! The pass is read-only against `Scope`, it never mutates the namespace

use crate::{
    hir::{
        RefTarget, RefTargetKind, SymbolTable, Type, TypeKind,
        declarations::Declarations,
        error::{HirError, HirErrorKind, hir_error},
        scope::Scope,
        type_resolver,
    },
    parser::statement,
};
use std::collections::HashMap;

/// Run all interface-related validation against a fully-extended `Scope`.
/// Composes the per-impl signature check with the inheritance check; either
/// can fail independently.
pub(in crate::hir) fn validate<'hir, 'd, 'h>(
    scope: &Scope<'hir>,
    declarations: &Declarations<'d, 'h>,
    symbols: &mut SymbolTable,
) -> Result<(), HirError<'hir>>
where
    'h: 'hir,
{
    validate_impls(scope, declarations, symbols)?;
    validate_hierarchy(scope, declarations, symbols)?;

    Ok(())
}

fn validate_hierarchy<'hir, 'd, 'h>(
    scope: &Scope<'hir>,
    declarations: &Declarations<'d, 'h>,
    symbols: &mut SymbolTable,
) -> Result<(), HirError<'hir>>
where
    'h: 'hir,
{
    for interface in &declarations.interfaces {
        for superinterface in &interface.superinterfaces {
            let symbol = symbols.insert(superinterface);

            if !scope.interfaces.contains_key(&symbol) {
                return Err(hir_error!(interface.span, UnknownInterface { name: superinterface }));
            }
        }
    }

    Ok(())
}

fn validate_impls<'hir, 'd, 'h>(
    scope: &Scope<'hir>,
    declarations: &Declarations<'d, 'h>,
    symbols: &mut SymbolTable,
) -> Result<(), HirError<'hir>>
where
    'h: 'hir,
{
    for implementation in &declarations.impls {
        let Some(interface_name) = implementation.interface else {
            continue;
        };

        let interface_sym = symbols.insert(interface_name);
        let interface = scope.interfaces.get(&interface_sym).ok_or_else(|| {
            hir_error!(implementation.span, UnknownInterface { name: interface_name })
        })?;

        let receiver_type = scope
            .lookup_named_type(implementation.name, symbols)
            .expect("impl type must exist in scope after declaration extension");

        let concrete_args: Vec<_> = match (
            interface.generic_params.is_empty(),
            implementation.interface_type.as_ref().map(|s| s.value()),
        ) {
            (false, Some(statement::Type::Generic(_, args))) => {
                let ctx =
                    type_resolver::ResolveCtx::root(symbols, &scope.struct_map, &scope.enum_map)
                        .with_self(receiver_type);
                args.iter()
                    .map(|arg| type_resolver::resolve_annotation(&ctx, &arg.value(), arg.span()))
                    .collect::<Result<_, _>>()?
            },
            _ => Vec::new(),
        };

        let impl_methods: HashMap<_, _> =
            implementation.methods.iter().map(|m| (m.name, m)).collect();

        for &required in &interface.superinterfaces {
            if !scope.interfaces.contains_key(&required) {
                let name = scope.arena.alloc_str(symbols.get(required));
                return Err(hir_error!(implementation.span, UnknownInterface { name }));
            }

            if !scope.interface_impls.contains(&(receiver_type, required)) {
                return Err(hir_error!(
                    implementation.span,
                    MissingSuperinterfaceImpl {
                        struct_name: implementation.name,
                        interface_name,
                        superinterface_name: scope.arena.alloc_str(symbols.get(required)),
                    }
                ));
            }
        }

        for required in &interface.methods {
            let method_name = scope.arena.alloc_str(symbols.get(required.name));
            let Some(impl_method) = impl_methods.get(method_name) else {
                return Err(hir_error!(
                    implementation.span,
                    MissingInterfaceMethod {
                        struct_name: implementation.name,
                        interface_name,
                        method_name,
                    }
                ));
            };

            let impl_has_receiver = impl_method.receiver.is_some();
            let function_id =
                scope.function_id(impl_method, symbols, Some(implementation.name), |_| {
                    HirErrorKind::MissingInterfaceMethod {
                        struct_name: implementation.name,
                        interface_name,
                        method_name: impl_method.name,
                    }
                })?;

            let signature = &scope.signatures[function_id];
            let impl_receiver_mut = signature.receiver_mutable();
            let impl_explicit_params = signature.explicit_params();

            let subst_table = build_subst_table(&concrete_args, interface.generic_params.len());
            let required_params: Vec<_> = required
                .params
                .iter()
                .map(|&t| substitute_self(t.subst(&subst_table), receiver_type))
                .collect();
            let required_return_type =
                substitute_self(required.return_type.subst(&subst_table), receiver_type);

            let signature_ok = impl_has_receiver == required.has_receiver
                && (!required.has_receiver || required.receiver_mut == impl_receiver_mut)
                && required_params == impl_explicit_params
                && required_return_type == signature.return_type;

            if !signature_ok {
                let expected = format_signature(
                    method_name,
                    required.has_receiver,
                    required.receiver_mut,
                    &required.params,
                    required.return_type,
                );
                let found = format_signature(
                    method_name,
                    impl_has_receiver,
                    impl_receiver_mut,
                    impl_explicit_params,
                    signature.return_type,
                );

                return Err(hir_error!(
                    impl_method.span,
                    InterfaceSignatureMismatch {
                        struct_name: implementation.name,
                        interface_name,
                        method_name,
                        expected: scope.arena.alloc_str(&expected),
                        found: scope.arena.alloc_str(&found),
                        impl_span: implementation.span,
                    }
                ));
            }
        }
    }

    Ok(())
}

/// Replace `Self` (and `&Self`) with the concrete receiver type
#[inline]
fn substitute_self(typ: Type, self_type: Type) -> Type {
    match typ.kind() {
        TypeKind::SelfType => self_type,
        TypeKind::Ref { mutable, to } if to.kind() == RefTargetKind::SelfType => {
            RefTarget::try_from(self_type)
                .map(|to| Type::new(TypeKind::Ref { mutable, to }))
                .unwrap_or(typ)
        },
        _ => typ,
    }
}

/// Pad `concrete` with `SelfType` up to `arity` so any declared `GenericParam`
/// index missing a concrete type rewrites to `SelfType`, which the subsequent
/// [`substitute_self`] pass then resolves against the receiver type
#[inline]
fn build_subst_table(concrete: &[Type], arity: usize) -> Vec<Type> {
    let mut table: Vec<Type> = concrete.to_vec();
    if table.len() < arity {
        table.resize(arity, Type::new(TypeKind::SelfType));
    }
    table
}

fn format_signature(
    name: &str,
    has_receiver: bool,
    receiver_mut: bool,
    params: &[Type],
    return_type: Type,
) -> String {
    let mut parameters: Vec<_> = has_receiver
        .then_some(vec![
            match receiver_mut {
                true => "&mut self",
                _ => "&self",
            }
            .to_string(),
        ])
        .unwrap_or_default();

    parameters.extend(params.iter().map(|t| t.to_string()));
    format!("fn {name}({}): {return_type}", parameters.join(", "))
}
