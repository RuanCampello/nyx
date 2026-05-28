//! AST-type -> HIR-type resolution.

use crate::{
    hir::{
        RefTarget, RefTargetKind, SymbolTable, Type, TypeKind,
        error::{HirError, hir_error},
        scope::{self, Enums, Structs},
    },
    lexer::token::Span,
    parser::statement,
};
use std::collections::HashMap;

/// Resolve an AST type annotation against the current type namespace.
pub(in crate::hir) fn resolve_annotation<'h>(
    symbols: &SymbolTable,
    struct_map: &Structs,
    enum_map: &Enums,
    typ: &statement::Type<'h>,
    span: Span,
    self_type: Option<Type>,
    env: Option<&HashMap<String, Type>>,
) -> Result<Type, HirError<'h>> {
    match typ {
        statement::Type::Named(name) => {
            if let Some(env) = env {
                if let Some(&t) = env.get(*name) {
                    return Ok(t);
                }
            }
            symbols
                .get_id(name)
                .and_then(|symbol| scope::nominal_type(struct_map, enum_map, symbol))
                .ok_or_else(|| hir_error!(span, UnknownType { name: name.to_string() }))
        },

        statement::Type::Ref(typ) => {
            let typ = resolve_annotation(symbols, struct_map, enum_map, typ, span, self_type, env)?;
            let to = RefTarget::try_from(typ).map_err(|_| {
                hir_error!(
                    span,
                    TypeMismatch {
                        expected: Type::new(TypeKind::Struct(Default::default())),
                        found: typ,
                    }
                )
            })?;

            Ok(Type::new(TypeKind::Ref { mutable: false, to }))
        },
        statement::Type::SelfType => Ok(self_type.unwrap_or(Type::new(TypeKind::SelfType))),
        statement::Type::RefSelf => self_type.map_or(
            Ok(Type::new(TypeKind::Ref {
                mutable: false,
                to: RefTarget::new(RefTargetKind::SelfType),
            })),
            |self_typ| {
                let to = RefTarget::try_from(self_typ).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch {
                            expected: Type::new(TypeKind::Struct(Default::default())),
                            found: self_typ,
                        }
                    )
                })?;

                Ok(Type::new(TypeKind::Ref { mutable: false, to }))
            },
        ),
        other => Type::from_primitive_ast(other)
            .ok_or_else(|| hir_error!(span, UnknownType { name: format!("{other:?}") })),
    }
}
