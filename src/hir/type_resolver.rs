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

pub(in crate::hir) struct ResolveCtx<'a> {
    pub symbols: &'a SymbolTable,
    pub struct_map: &'a Structs,
    pub enum_map: &'a Enums,
    pub self_type: Option<Type>,
    pub env: Option<&'a HashMap<String, Type>>,
}

/// Resolve an AST type annotation against the current type namespace.
pub(in crate::hir) fn resolve_annotation<'h>(
    ctx: &ResolveCtx<'_>,
    typ: &statement::Type<'h>,
    span: Span,
) -> Result<Type, HirError<'h>> {
    match typ {
        statement::Type::Named(name) => {
            if let Some(env) = ctx.env {
                if let Some(&t) = env.get(*name) {
                    return Ok(t);
                }
            }
            ctx.symbols
                .get_id(name)
                .and_then(|symbol| scope::nominal_type(ctx.struct_map, ctx.enum_map, symbol))
                .ok_or_else(|| hir_error!(span, UnknownType { name: name.to_string() }))
        },

        statement::Type::Ref(typ) => {
            let typ = resolve_annotation(ctx, typ, span)?;
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
        statement::Type::SelfType => Ok(ctx.self_type.unwrap_or(Type::new(TypeKind::SelfType))),
        statement::Type::RefSelf => ctx.self_type.map_or(
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

impl<'a> ResolveCtx<'a> {
    pub fn root(symbols: &'a SymbolTable, struct_map: &'a Structs, enum_map: &'a Enums) -> Self {
        Self { symbols, struct_map, enum_map, self_type: None, env: None }
    }

    pub fn with_self(mut self, t: Type) -> Self {
        self.self_type = Some(t);
        self
    }
}
