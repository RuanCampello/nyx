//! AST-type -> HIR-type resolution
//!
//! The structural walk (references, arrays, slices, primitives) lives in
//! [resolve], the context-dependent points (name lookup, generic
//! instantiation, and `Self` policy) are supplied by a [TypeResolver]

use crate::{
    hir::{
        RefTarget, SymbolTable, Type, TypeKind,
        error::{HirError, hir_error},
        scope::{self, ArrayTable, Enums, GenericEnv, Structs},
    },
    lexer::token::Span,
    parser::statement,
};

/// The context-dependent points of type-annotation resolution
pub(in crate::hir) trait TypeResolver<'h> {
    /// resolve a plain named type, after any generic-parameter environment lookup
    fn named(&mut self, name: &'h str, span: Span) -> Result<Type, HirError<'h>>;
    /// resolve a generic type application with already-resolved arguments
    fn generic(&mut self, name: &'h str, args: &[Type], span: Span) -> Result<Type, HirError<'h>>;
    /// the meaning of `Self` in this context
    fn self_type(&mut self, span: Span) -> Result<Type, HirError<'h>>;
    fn arrays(&self) -> &ArrayTable;
}

/// Read-only resolution context for callers outside item collection and body
/// lowering, generic applications are unresolvable here
#[derive(Clone, Copy)]
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
    resolve(&mut { *ctx }, typ, span)
}

/// The shared structural walk over an AST type annotation
pub(in crate::hir) fn resolve<'h, R: TypeResolver<'h> + ?Sized>(
    resolver: &mut R,
    typ: &statement::Type<'h>,
    span: Span,
) -> Result<Type, HirError<'h>> {
    match typ {
        statement::Type::Named(name) => resolver.named(name, span),

        statement::Type::Ref(inner, mutable) => {
            let inner = resolve(resolver, inner, span)?;
            Ok(Type::refer(ref_target(inner, span)?, *mutable))
        },

        statement::Type::Array(element, len) => {
            let element = resolve(resolver, element, span)?;
            let id = resolver.arrays().intern(element, *len as u32);
            Ok(Type::array(id))
        },

        statement::Type::Slice(element, mutable) => {
            let element = resolve(resolver, element, span)?;
            Ok(Type::slice(ref_target(element, span)?, *mutable))
        },

        statement::Type::SelfType => resolver.self_type(span),
        statement::Type::RefSelf => {
            let self_typ = resolver.self_type(span)?;
            Ok(Type::refer(ref_target(self_typ, span)?, false))
        },

        statement::Type::Generic(name, args) => {
            let mut resolved = Vec::with_capacity(args.len());
            for arg in args {
                resolved.push(resolve(resolver, arg.value_ref(), arg.span())?);
            }
            resolver.generic(name, &resolved, span)
        },

        other => Type::from_primitive_ast(other)
            .ok_or_else(|| hir_error!(span, UnknownType { name: "<unsupported type>" })),
    }
}

impl<'a, 'h> TypeResolver<'h> for ResolveCtx<'a> {
    fn named(&mut self, name: &'h str, span: Span) -> Result<Type, HirError<'h>> {
        if let Some(env) = self.env
            && let Some(&t) = env.get(name)
        {
            return Ok(t);
        }
        self.symbols
            .get_id(name)
            .and_then(|symbol| scope::nominal_type(self.struct_map, self.enum_map, symbol))
            .ok_or_else(|| hir_error!(span, UnknownType { name }))
    }

    fn generic(&mut self, name: &'h str, _args: &[Type], span: Span) -> Result<Type, HirError<'h>> {
        Err(hir_error!(span, UnknownType { name }))
    }

    fn self_type(&mut self, _span: Span) -> Result<Type, HirError<'h>> {
        Ok(self.self_type.unwrap_or(TypeKind::SelfType.into()))
    }

    fn arrays(&self) -> &ArrayTable {
        self.arrays
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

#[inline(always)]
fn ref_target<'h>(typ: Type, span: Span) -> Result<RefTarget, HirError<'h>> {
    RefTarget::try_from(typ).map_err(|_| {
        hir_error!(span, TypeMismatch { expected: Type::structure(Default::default()), found: typ })
    })
}
