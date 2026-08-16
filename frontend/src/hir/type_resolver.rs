//! AST-type -> HIR-type resolution
//!
//! The structural walk (references, arrays, slices, primitives) lives in
//! [resolve], the context-dependent points (name lookup, generic
//! instantiation, and `Self` policy) are supplied by a [TypeResolver]

use crate::{
    hir::{
        AdtDef, AdtId, SymbolId, SymbolTable, TyInterner, Type,
        collect::{self, ArrayTable, Enums, GenericEnv, Structs},
        error::{HirError, hir_error},
        ids::IndexVec,
    },
    lexer::token::Span,
    parser::statement,
};

/// The context-dependent points of type-annotation resolution
pub(in crate::hir) trait TypeResolver<'h, 'hir> {
    /// resolve a plain named type, after any generic-parameter environment lookup
    fn named(&mut self, name: &'h str, span: Span) -> Result<Type<'hir>, HirError<'h>>;
    /// resolve a generic type application with already-resolved arguments
    fn generic(
        &mut self,
        name: &'h str,
        args: &[Type<'hir>],
        span: Span,
    ) -> Result<Type<'hir>, HirError<'h>>;
    /// the meaning of `Self` in this context
    fn self_type(&mut self, span: Span) -> Result<Type<'hir>, HirError<'h>>;

    fn associated(
        &mut self,
        qualifier: Option<Type<'hir>>,
        name: &'h str,
        span: Span,
    ) -> Result<Type<'hir>, HirError<'h>> {
        let _ = qualifier;
        Err(hir_error!(span, UnknownType { name }))
    }

    fn arrays(&self) -> &ArrayTable<'hir>;
    fn types(&self) -> &TyInterner<'hir>;
}

/// Read-only resolution context for callers outside item collection and body
/// lowering, generic applications are unresolvable here
#[derive(Clone, Copy)]
pub(in crate::hir) struct ResolveCtx<'a, 'hir> {
    pub symbols: &'a SymbolTable,
    pub struct_map: &'a Structs,
    pub enum_map: &'a Enums,
    pub adts: &'a IndexVec<AdtId, AdtDef<'hir>>,
    pub arrays: &'a ArrayTable<'hir>,
    pub types: &'a TyInterner<'hir>,
    pub self_type: Option<Type<'hir>>,
    pub env: Option<&'a GenericEnv<'hir>>,
}

/// Resolve an AST type annotation against the current type namespace
pub(in crate::hir) fn resolve_annotation<'h, 'hir>(
    ctx: &ResolveCtx<'_, 'hir>,
    typ: &statement::Type<'h>,
    span: Span,
) -> Result<Type<'hir>, HirError<'h>> {
    resolve(&mut { *ctx }, typ, span)
}

/// The shared structural walk over an AST type annotation
pub(in crate::hir) fn resolve<'h, 'hir, R: TypeResolver<'h, 'hir> + ?Sized>(
    resolver: &mut R,
    typ: &statement::Type<'h>,
    span: Span,
) -> Result<Type<'hir>, HirError<'h>> {
    match typ {
        statement::Type::Named(name) => resolver.named(name, span),
        statement::Type::Ref(inner, mutable) => {
            let inner = resolve(resolver, inner, span)?;
            Ok(resolver.types().refer(inner, *mutable))
        },
        statement::Type::Raw(inner, mutable) => {
            let inner = resolve(resolver, inner, span)?;
            Ok(resolver.types().raw(inner, *mutable))
        },
        statement::Type::Array(element, len) => {
            let element = resolve(resolver, element, span)?;
            let id = resolver.arrays().intern(element, *len as u32);
            Ok(resolver.types().array(id))
        },
        statement::Type::Slice(element, mutable) => {
            let element = resolve(resolver, element, span)?;
            Ok(resolver.types().slice(element, *mutable))
        },
        statement::Type::SelfType => resolver.self_type(span),
        statement::Type::RefSelf => {
            let self_typ = resolver.self_type(span)?;
            Ok(resolver.types().refer(self_typ, false))
        },
        statement::Type::Associated(qualifier, name) => {
            let qualifier = match **qualifier {
                statement::Type::SelfType => None,
                ref other => Some(resolve(resolver, other, span)?),
            };
            resolver.associated(qualifier, name, span)
        },
        statement::Type::Generic(name, args) => {
            let mut resolved = Vec::with_capacity(args.len());
            for arg in args {
                resolved.push(resolve(resolver, arg.value_ref(), arg.span())?);
            }
            resolver.generic(name, &resolved, span)
        },

        other => resolver
            .types()
            .from_primitive_ast(other)
            .ok_or_else(|| hir_error!(span, UnknownType { name: "<unsupported type>" })),
    }
}

pub(in crate::hir) fn resolve_named<'h, 'hir>(
    env: Option<&GenericEnv<'hir>>,
    name: &'h str,
    span: Span,
    resolve_symbol: impl FnOnce(&str) -> Option<SymbolId>,
    nominal_type: impl FnOnce(SymbolId) -> Option<Type<'hir>>,
) -> Result<Type<'hir>, HirError<'h>> {
    if let Some(&typ) = env.and_then(|env| env.get(name)) {
        return Ok(typ);
    }
    resolve_symbol(name)
        .and_then(nominal_type)
        .ok_or_else(|| hir_error!(span, UnknownType { name }))
}

impl<'a, 'h, 'hir> TypeResolver<'h, 'hir> for ResolveCtx<'a, 'hir> {
    fn named(&mut self, name: &'h str, span: Span) -> Result<Type<'hir>, HirError<'h>> {
        resolve_named(
            self.env,
            name,
            span,
            |name| self.symbols.get_id(name),
            |symbol| collect::nominal_type(self.types, self.struct_map, self.enum_map, symbol),
        )
    }

    fn generic(
        &mut self,
        name: &'h str,
        args: &[Type<'hir>],
        span: Span,
    ) -> Result<Type<'hir>, HirError<'h>> {
        let id = self
            .symbols
            .get_id(name)
            .and_then(|symbol| {
                self.struct_map.get(&symbol).or_else(|| self.enum_map.get(&symbol)).copied()
            })
            .ok_or_else(|| hir_error!(span, UnknownType { name }))?;

        let expected = self.adts[id].generics.len();
        if expected != args.len() {
            return Err(hir_error!(
                span,
                ArityMismatch { name, expected, found: args.len(), decl: None }
            ));
        }

        Ok(self.types.adt(id, args))
    }

    fn self_type(&mut self, _span: Span) -> Result<Type<'hir>, HirError<'h>> {
        Ok(self.self_type.unwrap_or(self.types.common.self_type))
    }

    fn arrays(&self) -> &ArrayTable<'hir> {
        self.arrays
    }

    fn types(&self) -> &TyInterner<'hir> {
        self.types
    }
}

impl<'a, 'hir> ResolveCtx<'a, 'hir> {
    #[rustfmt::skip]
    pub fn root(
        symbols: &'a SymbolTable,
        struct_map: &'a Structs,
        enum_map: &'a Enums,
        adts: &'a IndexVec<AdtId, AdtDef<'hir>>,
        arrays: &'a ArrayTable<'hir>,
        types: &'a TyInterner<'hir>,
    ) -> Self {
        Self { symbols, struct_map, enum_map, adts, arrays, types, self_type: None, env: None }
    }

    pub fn with_self(mut self, t: Type<'hir>) -> Self {
        self.self_type = Some(t);
        self
    }
}
