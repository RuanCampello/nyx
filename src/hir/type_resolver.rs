//! AST-type -> HIR-type resolution.

use crate::{
    hir::{
        RefTarget, SymbolTable, Type, TypeKind,
        error::{HirError, hir_error},
        scope::{self, ArrayTable, Enums, GenericEnv, Structs},
    },
    lexer::token::Span,
    parser::statement,
};

pub(in crate::hir) struct ResolveCtx<'a> {
    pub symbols: &'a SymbolTable,
    pub struct_map: &'a Structs,
    pub enum_map: &'a Enums,
    pub arrays: &'a ArrayTable,
    pub self_type: Option<Type>,
    pub env: Option<&'a GenericEnv>,
}

/// Resolve an AST type annotation against the current type namespace.
pub(in crate::hir) fn resolve_annotation<'h>(
    ctx: &ResolveCtx<'_>,
    typ: &statement::Type<'h>,
    span: Span,
) -> Result<Type, HirError<'h>> {
    match typ {
        statement::Type::Named(name) => {
            if let Some(env) = ctx.env
                && let Some(&t) = env.get(*name)
            {
                return Ok(t);
            }
            ctx.symbols
                .get_id(name)
                .and_then(|symbol| scope::nominal_type(ctx.struct_map, ctx.enum_map, symbol))
                .ok_or_else(|| hir_error!(span, UnknownType { name }))
        },

        statement::Type::Ref(typ) => {
            let typ = resolve_annotation(ctx, typ, span)?;
            let to = RefTarget::try_from(typ).map_err(|_| {
                hir_error!(
                    span,
                    TypeMismatch { expected: Type::structure(Default::default()), found: typ }
                )
            })?;

            Ok(Type::refer(to, false))
        },
        statement::Type::Array(element, len) => {
            let element = resolve_annotation(ctx, element, span)?;
            let id = ctx.arrays.intern(element, *len as u32);
            Ok(Type::array(id))
        },

        statement::Type::Slice(element) => {
            let element = resolve_annotation(ctx, element, span)?;

            let err = hir_error!(
                span,
                TypeMismatch {
                    expected: Type::structure(Default::default()),
                    found: element
                }
            );
            let element = RefTarget::try_from(element).map_err(|_| err)?;

            Ok(Type::slice(element, false))
        },

        statement::Type::SelfType => Ok(ctx.self_type.unwrap_or(TypeKind::SelfType.into())),
        statement::Type::RefSelf => ctx.self_type.map_or(
            Ok(Type::refer(RefTarget::new(TypeKind::SelfType), false)),
            |self_typ| {
                let to = RefTarget::try_from(self_typ).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch {
                            expected: Type::structure(Default::default()),
                            found: self_typ,
                        }
                    )
                })?;

                Ok(Type::refer(to, false))
            },
        ),
        // the only unhandled variant that fails `from_primitive_ast` is `Generic`,
        // which carries its own name; everything else resolves to a primitive
        other => Type::from_primitive_ast(other).ok_or_else(|| {
            let name = match other {
                statement::Type::Generic(name, _) => name,
                _ => "<unsupported type>",
            };
            hir_error!(span, UnknownType { name })
        }),
    }
}

impl<'a> ResolveCtx<'a> {
    #[rustfmt::skip]
    pub fn root(
        symbols: &'a SymbolTable,
        struct_map: &'a Structs,
        enum_map: &'a Enums,
        arrays: &'a ArrayTable,
    ) -> Self {
        Self { symbols, struct_map, enum_map, arrays, self_type: None, env: None }
    }

    pub fn with_self(mut self, t: Type) -> Self {
        self.self_type = Some(t);
        self
    }
}
