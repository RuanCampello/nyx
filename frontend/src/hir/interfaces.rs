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
        TyInterner, Type, TypeKind, collect,
        collect::ItemTable,
        declarations::Declarations,
        error::{HirError, HirErrorKind, hir_error},
        type_resolver,
    },
    parser::statement,
};
use std::collections::HashMap;

/// Run all interface-related validation against a fully-extended `ItemTable`.
/// Composes the per-impl signature check with the inheritance check; either
/// can fail independently.
///
/// Every violation is collected, then either softened into the scope's
/// diagnostics while validation continues across independent requirements
pub(in crate::hir) fn validate<'hir, 'd, 'h>(
    scope: &ItemTable<'hir>,
    declarations: &Declarations<'d, 'h>,
) -> Result<(), HirError<'hir>>
where
    'h: 'hir,
{
    let mut errors = Vec::new();
    validate_impls(scope, declarations, &mut errors);
    validate_hierarchy(scope, declarations, &mut errors);

    for error in errors {
        scope.soft(error);
    }
    Ok(())
}

fn validate_hierarchy<'hir, 'd, 'h>(
    scope: &ItemTable<'hir>,
    declarations: &Declarations<'d, 'h>,
    errors: &mut Vec<HirError<'hir>>,
) where
    'h: 'hir,
{
    for interface in &declarations.interfaces {
        for superinterface in &interface.superinterfaces {
            let known = scope
                .symbols
                .get_id(superinterface)
                .is_some_and(|symbol| scope.interfaces.defs.contains_key(&symbol));

            if !known {
                errors.push(hir_error!(interface.span, UnknownInterface { name: superinterface }));
            }
        }
    }
}

fn validate_impls<'hir, 'd, 'h>(
    scope: &ItemTable<'hir>,
    declarations: &Declarations<'d, 'h>,
    errors: &mut Vec<HirError<'hir>>,
) where
    'h: 'hir,
{
    for implementation in &declarations.impls {
        let Some(interface_name) = implementation.interface else {
            continue;
        };

        let interface = scope
            .symbols
            .get_id(interface_name)
            .and_then(|symbol| scope.interfaces.defs.get(&symbol));
        let Some(interface) = interface else {
            errors.push(hir_error!(implementation.span, UnknownInterface { name: interface_name }));
            continue;
        };

        let receiver_type = scope
            .lookup_named_type(implementation.name)
            .expect("impl type must exist in scope after declaration extension");

        let mut concrete_args: Vec<_> = match (
            interface.generic_params.is_empty(),
            implementation.interface_type.as_ref().map(|s| s.value()),
        ) {
            (false, Some(statement::Type::Generic(_, args))) => {
                let (structs, enums, arrays) =
                    (&scope.adts.struct_map, &scope.adts.enum_map, &scope.arrays);
                let ctx = type_resolver::ResolveCtx::root(
                    &scope.symbols,
                    structs,
                    enums,
                    &scope.adts.defs,
                    arrays,
                    &scope.types,
                )
                .with_self(receiver_type);
                match args
                    .iter()
                    .map(|arg| type_resolver::resolve_annotation(&ctx, &arg.value(), arg.span()))
                    .collect::<Result<_, _>>()
                {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        errors.push(error);
                        continue;
                    },
                }
            },
            _ => Vec::new(),
        };

        concrete_args.resize(interface.generic_params.len(), scope.types.common.self_type);

        for &associated in &interface.associated_types {
            let bound =
                scope.interfaces.associated_types.get(&(receiver_type, associated)).copied();
            match bound {
                Some(typ) => concrete_args.push(typ),
                None => {
                    let name = scope.arena.alloc_str(scope.symbols.get(associated));
                    let span = implementation.span;
                    errors.push(hir_error!(span, UnboundAssociatedType { name, interface_name }));
                    concrete_args.push(scope.types.common.error);
                },
            }
        }

        let impl_methods: HashMap<_, _> =
            implementation.methods.iter().map(|m| (m.name, m)).collect();
        let impl_constants: HashMap<_, _> = implementation
            .constants
            .iter()
            .map(|constant| (constant.name, constant))
            .collect();

        for &required in &interface.superinterfaces {
            if !scope.interfaces.defs.contains_key(&required) {
                let name = scope.arena.alloc_str(scope.symbols.get(required));
                errors.push(hir_error!(implementation.span, UnknownInterface { name }));
                continue;
            }

            if !scope.implements_interface(receiver_type, required) {
                errors.push(hir_error!(
                    implementation.span,
                    MissingSuperinterfaceImpl {
                        struct_name: implementation.name,
                        interface_name,
                        superinterface_name: scope.arena.alloc_str(scope.symbols.get(required)),
                    }
                ));
            }
        }

        for required in &interface.methods {
            let method_name = scope.arena.alloc_str(scope.symbols.get(required.name));
            let Some(impl_method) = impl_methods.get(method_name) else {
                errors.push(hir_error!(
                    implementation.span,
                    MissingInterfaceMethod {
                        struct_name: implementation.name,
                        interface_name,
                        method_name,
                        decl: collect::source_span(required.decl_span),
                    }
                ));
                continue;
            };

            let impl_has_receiver = impl_method.receiver.is_some();
            let function_id =
                match scope.function_id(impl_method, Some(implementation.name), |_| {
                    HirErrorKind::MissingInterfaceMethod {
                        struct_name: implementation.name,
                        interface_name,
                        method_name: impl_method.name,
                        decl: collect::source_span(required.decl_span),
                    }
                }) {
                    Ok(id) => id,
                    Err(error) => {
                        errors.push(error);
                        continue;
                    },
                };

            let signature = &scope.functions.defs[function_id];
            let impl_receiver_mut = signature.receiver_mutable();
            let impl_explicit_params = signature.explicit_params();

            let subst_table = build_subst_table(
                &concrete_args,
                interface.generic_params.len(),
                scope.types.common.self_type,
            );
            let required_params: Vec<_> = required
                .params
                .iter()
                .map(|&t| {
                    substitute_self(
                        &scope.types,
                        t.subst(&scope.types, &scope.arrays, &subst_table),
                        receiver_type,
                    )
                })
                .collect();
            let required_return_type = substitute_self(
                &scope.types,
                required.return_type.subst(&scope.types, &scope.arrays, &subst_table),
                receiver_type,
            );

            let signature_ok = impl_has_receiver == required.has_receiver
                && (!required.has_receiver || required.receiver_mut == impl_receiver_mut)
                && required_params == impl_explicit_params
                && required_return_type == signature.return_type;

            if required.is_const && !signature.is_const {
                errors.push(hir_error!(
                    impl_method.span,
                    NonConstInterfaceMethod {
                        struct_name: implementation.name,
                        interface_name,
                        method_name,
                        decl: collect::source_span(required.decl_span),
                    }
                ));
            }

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

                errors.push(hir_error!(
                    impl_method.span,
                    InterfaceSignatureMismatch {
                        struct_name: implementation.name,
                        interface_name,
                        method_name,
                        expected: scope.arena.alloc_str(&expected),
                        found: scope.arena.alloc_str(&found),
                        decl: collect::source_span(required.decl_span),
                    }
                ));
            }
        }

        for required in &interface.constants {
            let constant_name = scope.arena.alloc_str(scope.symbols.get(required.name));
            let Some(impl_constant) = impl_constants.get(constant_name) else {
                errors.push(hir_error!(
                    implementation.span,
                    MissingInterfaceConstant {
                        struct_name: implementation.name,
                        interface_name,
                        constant_name,
                        decl: collect::source_span(required.decl_span),
                    }
                ));
                continue;
            };

            let symbol_name = scope.mangler.scoped_item(implementation.name, impl_constant.name);
            let Some(found) = scope
                .symbols
                .get_id(&symbol_name)
                .and_then(|symbol| scope.values.constants.get(&symbol).copied())
                .map(|constant| constant.typ)
            else {
                continue;
            };

            let subst_table = build_subst_table(
                &concrete_args,
                interface.generic_params.len(),
                scope.types.common.self_type,
            );
            let expected = substitute_self(
                &scope.types,
                required.typ.subst(&scope.types, &scope.arrays, &subst_table),
                receiver_type,
            );

            if found != expected {
                errors.push(hir_error!(
                    impl_constant.span,
                    InterfaceConstantTypeMismatch {
                        struct_name: implementation.name,
                        interface_name,
                        constant_name,
                        expected,
                        found,
                        decl: collect::source_span(required.decl_span),
                    }
                ));
            }
        }
    }
}

/// Replace `Self` (and `&Self`) with the concrete receiver type
#[inline]
fn substitute_self<'hir>(
    types: &TyInterner<'hir>,
    typ: Type<'hir>,
    self_type: Type<'hir>,
) -> Type<'hir> {
    match typ.kind() {
        TypeKind::SelfType => self_type,
        TypeKind::Ref { mutable, to } if to.kind() == TypeKind::SelfType => {
            types.refer(self_type, mutable)
        },
        _ => typ,
    }
}

/// Pad `concrete` with `SelfType` up to `arity` so any declared `GenericParam`
/// index missing a concrete type rewrites to `SelfType`, which the subsequent
/// [substitute_self] pass then resolves against the receiver type
#[inline]
fn build_subst_table<'hir>(
    concrete: &[Type<'hir>],
    arity: usize,
    self_type: Type<'hir>,
) -> Vec<Type<'hir>> {
    let mut table: Vec<Type<'hir>> = concrete.to_vec();
    if table.len() < arity {
        table.resize(arity, self_type);
    }
    table
}

fn format_signature(
    name: &str,
    has_receiver: bool,
    receiver_mut: bool,
    params: &[Type<'_>],
    return_type: Type<'_>,
) -> String {
    let mut parameters: Vec<_> = has_receiver
        .then_some(vec![
            match receiver_mut {
                true => "&mut self",
                _ => "&self",
            }
            .into(),
        ])
        .unwrap_or_default();

    parameters.extend(params.iter().map(|t| t.to_string()));
    format!("fn {name}({}): {return_type}", parameters.join(", "))
}
