//! Function-body lowering: the `FunctionBuilder` state, its constructors,
//! shared scope/type-checking helpers, and the top-level entry points

use crate::{
    hir::{
        AdtDef, AdtId, Constant, ExprId, Expression, ExpressionKind, Function, FunctionId, Local,
        LocalId, Parameter, SymbolId, SymbolTable, TyInterner, Type, TypeKind, TypeckResults,
        collect::{self, ArrayTable, GenericEnv, ItemTable},
        error::{HirError, hir_error},
        ids::IndexVec,
        infer::InferTable,
        symbols::Mangler,
        type_resolver::{self, TypeResolver},
    },
    lexer::{Spanned, token::Span},
    parser::{
        expression::{self},
        statement::{self},
    },
};
use std::{
    collections::{HashMap, HashSet},
    ops::Index,
};

mod call;
mod expr;
mod pat;
mod stmt;

pub(in crate::hir) struct FunctionBuilder<'s, 'f, 'hir, 'src> {
    pub(super) scope: &'s ItemTable<'hir>,
    locals: IndexVec<LocalId, Local<'hir>>,
    scopes: Vec<HashMap<SymbolId, LocalId>>,
    return_type: Type<'hir>,
    return_type_span: Option<Span>,
    function: Option<&'f statement::Function<'src>>,
    impl_ctx: ImplCtx<'hir, 'src>,
    pub(super) generics: &'f [statement::GenericBound<'src>],
    function_id: FunctionId,
    next_local: u32,
    next_expr_id: u32,
    pub(super) is_const: bool,
    unsafe_ctx: UnsafeCtx,
    pub(super) arena: &'hir bumpalo::Bump,
    pub(super) typeck: TypeckResults<'hir>,
    /// Whether an overloaded index reached while lowering the current
    /// expression must select `IndexMutable`.
    pub(super) mutable_place: bool,
    infer: InferTable<'hir>,
    generic_env: GenericEnv<'hir>,
    const_scope: ConstScope<'hir>,
    loop_depth: u32,
}

/// Nesting of enclosing `@unsafe` contexts
#[derive(Default)]
struct UnsafeCtx {
    /// an `@unsafe fn` body seeds this at 1, each `@unsafe { … }` adds one
    depth: u32,
    /// operations that needed an unsafe context, to spot a block that needed none
    ops: u32,
}

/// What the enclosing `impl` resolves to while lowering a body
#[derive(Default)]
struct ImplCtx<'hir, 'src> {
    /// the implementing type's name, kept apart from [FunctionBuilder::function]
    /// because lowering takes that out before the body it belongs to is walked
    type_name: Option<&'hir str>,
    /// the unmangled impl name, which is where associated constants stay filed:
    /// unlike methods they are collected once and never specialised per instance
    template: Option<&'src str>,
    self_type: Option<Type<'hir>>,
}

/// Compile-time constants declared in the body currently being lowered
#[derive(Default)]
struct ConstScope<'hir> {
    /// spliced at use sites like top-level constants, but scoped to and
    /// dropped with the body
    body: HashMap<SymbolId, &'hir Constant<'hir>>,
    /// when lowering a constant initialiser, the names of the enclosing
    /// function's locals, referencing one is an error, not a resolution
    outer_locals: HashSet<SymbolId>,
}

/// A freshly lowered expression
///
/// Its arena reference plus the type and span the
/// lowering pass needs immediately for bidirectional checking
#[derive(Clone, Copy)]
pub(in crate::hir) struct Lowered<'hir> {
    pub(super) expr: &'hir Expression<'hir>,
    pub(super) typ: Type<'hir>,
    pub(super) span: Span,
}

trait NamedField<'src> {
    fn name(&self) -> &'src str;
    fn field_span(&self) -> Span;
}

impl<'s, 'f, 'hir, 'src> FunctionBuilder<'s, 'f, 'hir, 'src>
where
    'src: 'hir,
{
    pub fn new(
        scope: &'s ItemTable<'hir>,
        function_id: FunctionId,
        function: &'f statement::Function<'src>,
        arena: &'hir bumpalo::Bump,
    ) -> Self {
        let self_type = function.impl_type.and_then(|impl_type| scope.lookup_named_type(impl_type));

        let mut builder = Self::raw(scope, arena);
        builder.is_const = function.is_const;
        builder.function = Some(function);
        builder.impl_ctx.type_name = function.impl_type;
        builder.impl_ctx.template = function.impl_type;
        builder.impl_ctx.self_type = self_type;
        builder.generics = &function.generics;
        builder.function_id = function_id;
        builder
    }

    pub fn new_instance(
        scope: &'s ItemTable<'hir>,
        function_id: FunctionId,
        function: &'f statement::Function<'src>,
        arena: &'hir bumpalo::Bump,
        generic_env: GenericEnv<'hir>,
        impl_type: Option<&'hir str>,
    ) -> Self {
        let mut builder = Self::new(scope, function_id, function, arena);
        builder.generic_env = generic_env;
        builder.impl_ctx.type_name = impl_type.or(builder.impl_ctx.type_name);
        builder
    }

    pub fn new_for_const(scope: &'s ItemTable<'hir>, arena: &'hir bumpalo::Bump) -> Self {
        Self::raw(scope, arena)
    }

    fn raw(scope: &'s ItemTable<'hir>, arena: &'hir bumpalo::Bump) -> Self {
        let unit = scope.types.common.unit;
        Self {
            scope,
            is_const: true,
            unsafe_ctx: UnsafeCtx::default(),
            return_type: unit,
            return_type_span: None,
            function: None,
            impl_ctx: ImplCtx::default(),
            generics: &[],
            function_id: FunctionId(0),
            next_local: 0,
            next_expr_id: 0,
            locals: IndexVec::new(),
            typeck: TypeckResults::default(),
            mutable_place: false,
            infer: InferTable::default(),
            scopes: vec![HashMap::new()],
            arena,
            generic_env: HashMap::new(),
            const_scope: ConstScope::default(),
            loop_depth: 0,
        }
    }

    pub(super) fn alloc(
        &mut self,
        kind: ExpressionKind<'hir>,
        typ: Type<'hir>,
        span: Span,
    ) -> Lowered<'hir> {
        let id = ExprId(self.next_expr_id);
        self.next_expr_id += 1;
        let expr = self.arena.alloc(Expression { id, kind, span });
        self.typeck.node_types.push(typ);

        Lowered { expr, typ, span }
    }

    fn lower_mutable_place(
        &mut self,
        expression: &expression::Expression<'src>,
        hint: Option<Type<'hir>>,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let previous = std::mem::replace(&mut self.mutable_place, true);
        let lowered = self.lower_expr(expression, hint);
        self.mutable_place = previous;
        lowered
    }

    pub fn lower(mut self) -> Result<Function<'hir>, HirError<'hir>> {
        let function = self.function.take().expect("function to be present");
        let id = self.function_id;
        let signature = self.scope.functions.defs[id].clone();
        let symbol = signature.name;
        self.return_type = signature.return_type;
        self.return_type_span = function.return_type.as_ref().map(Spanned::span);
        self.unsafe_ctx.depth = u32::from(signature.is_unsafe);

        let mut params = Vec::with_capacity(signature.params.len());

        if let Some(receiver) = function.receiver {
            let typ = signature.receiver_type().expect("receiver in AST without one in signature");
            let symbol = self.scope.symbols.insert("self");
            let id = self.declare_local(symbol, typ, receiver.mutable, receiver.span)?;
            params.push(Parameter { typ, id, name: symbol, mutable: receiver.mutable });
        }

        params.extend(
            function
                .params
                .iter()
                .zip(signature.explicit_params().iter())
                .map(|(parameter, &typ)| -> Result<_, HirError> {
                    let symbol = self.scope.symbols.insert(parameter.name);
                    let id = self.declare_local(symbol, typ, parameter.mutable, parameter.span)?;

                    Ok(Parameter { typ, id, name: symbol, mutable: parameter.mutable })
                })
                .collect::<Result<Vec<_>, _>>()?,
        );

        let (body, returns) = self.lower_block(&function.body, true)?;

        // an @intrinsic body is empty on purpose
        // the value comes from the compiler, not from anything written here
        if !returns && !function.is_intrinsic() && signature.return_type.kind() != TypeKind::Unit {
            let name = self.arena.alloc_str(self.scope.symbols.get(symbol));
            let span = function.return_type.as_ref().map_or(function.span, Spanned::span);
            self.soft(hir_error!(span, MissingReturn { name, expected: signature.return_type }));
        }

        self.resolve_inference();
        let generics = declared_fn_names(&self.generic_env, &self.scope.symbols);

        Ok(Function {
            id,
            name: symbol,
            decl_span: function.span,
            name_span: function.name_span,
            params,
            locals: self.locals,
            return_type: signature.return_type,
            is_const: function.is_const,
            is_pub: function.is_pub,
            inline: function.inline,
            is_unsafe: signature.is_unsafe,
            kind: signature.kind,
            owner: signature.owner,
            typeck: self.typeck,
            body,
            generics,
        })
    }

    #[inline]
    fn resolve_inference(&mut self) {
        let infer = &mut self.infer;
        let arrays = &self.scope.arrays;
        let types = &self.scope.types;

        for typ in self.typeck.node_types.iter_mut() {
            *typ = Self::resolve_deep(infer, arrays, types, *typ);
        }
        for local in self.locals.iter_mut() {
            local.typ = Self::resolve_deep(infer, arrays, types, local.typ);
        }
        for args in self.typeck.node_args.values_mut() {
            for typ in args.iter_mut() {
                *typ = Self::resolve_deep(infer, arrays, types, *typ);
            }
        }
    }

    fn resolve_deep(
        infer: &mut InferTable<'hir>,
        arrays: &ArrayTable<'hir>,
        types: &TyInterner<'hir>,
        typ: Type<'hir>,
    ) -> Type<'hir> {
        match typ.kind() {
            TypeKind::Array(id) => {
                let array = arrays.get(id);
                let element = Self::resolve_deep(infer, arrays, types, array.element);
                if element != array.element {
                    arrays.resolve(id, element);
                }

                typ
            },
            TypeKind::Slice { mutable, element } => {
                types.slice(Self::resolve_deep(infer, arrays, types, element), mutable)
            },
            TypeKind::Ref { mutable, to } => {
                types.refer(Self::resolve_deep(infer, arrays, types, to), mutable)
            },
            TypeKind::Raw { mutable, to } => {
                types.raw(Self::resolve_deep(infer, arrays, types, to), mutable)
            },
            _ => infer.resolve_or_default(typ),
        }
    }

    #[inline]
    fn mangler(&self) -> &Mangler<'_> {
        &self.scope.mangler
    }

    fn check_arity(
        &self,
        name: &'hir str,
        expected: usize,
        found: usize,
        decl: Option<Span>,
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        if expected == found {
            return Ok(());
        }
        Err(hir_error!(span, ArityMismatch { name, expected, found, decl }))
    }

    fn enum_type(
        &self,
        typ: Type<'hir>,
        span: Span,
    ) -> Result<(AdtId, &'hir [Type<'hir>]), HirError<'hir>> {
        let default = self.scope.types.adt(Default::default(), &[]);

        match typ.kind() {
            TypeKind::Adt(id, args) if self.scope[id].is_enum() => Ok((id, args)),
            TypeKind::Ref { to, .. } => match to.kind() {
                TypeKind::Adt(id, args) if self.scope[id].is_enum() => Ok((id, args)),
                _ => Err(hir_error!(span, TypeMismatch { expected: default, found: typ })),
            },
            _ => Err(hir_error!(span, TypeMismatch { expected: default, found: typ })),
        }
    }

    #[inline(always)]
    fn resolve_type(
        &mut self,
        typ: &statement::Type<'src>,
        span: Span,
    ) -> Result<Type<'hir>, HirError<'hir>> {
        type_resolver::resolve(self, typ, span)
    }

    #[inline(always)]
    pub(in crate::hir) fn assert_type(
        &mut self,
        expected: impl Into<Type<'hir>>,
        found: impl Into<Type<'hir>>,
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        self.assert_type_at(expected, found, span, None)
    }

    fn assert_type_at(
        &mut self,
        expected: impl Into<Type<'hir>>,
        found: impl Into<Type<'hir>>,
        span: Span,
        annotation: Option<Span>,
    ) -> Result<(), HirError<'hir>> {
        use TypeKind::*;
        let (expected, found) = (expected.into(), found.into());
        if expected.is_error() || found.is_error() || expected == found {
            return Ok(());
        }

        if expected.is_infer() || found.is_infer() {
            return match self.infer.unify(expected, found) {
                Ok(()) => Ok(()),
                _ => {
                    let expected = self.infer.resolve_or_default(expected);
                    let found = self.infer.resolve_or_default(found);
                    self.soft(Self::mismatch(expected, found, span, annotation));
                    Ok(())
                },
            };
        }

        if let (Raw { mutable: want_mut, to: want }, Ref { mutable, to }) =
            (expected.kind(), found.kind())
            && want == to
            && (mutable || !want_mut)
        {
            return Ok(());
        }

        if let (Array(expected), Array(found)) = (expected.kind(), found.kind()) {
            let (lhs, rhs) = (self.scope.arrays.get(expected), self.scope.arrays.get(found));
            if lhs.len == rhs.len {
                return self.assert_type_at(lhs.element, rhs.element, span, annotation);
            }
        }

        self.soft(Self::mismatch(expected, found, span, annotation));
        Ok(())
    }

    fn mismatch(
        expected: Type<'hir>,
        found: Type<'hir>,
        span: Span,
        annotation: Option<Span>,
    ) -> HirError<'hir> {
        match annotation {
            Some(annotation) => {
                hir_error!(span, TypeAnnotationMismatch { expected, found, annotation })
            },
            _ => hir_error!(span, TypeMismatch { expected, found }),
        }
    }

    #[inline(always)]
    fn poison(&mut self, error: HirError<'hir>) -> Type<'hir> {
        self.scope.poison(error)
    }

    #[inline(always)]
    fn soft(&mut self, error: HirError<'hir>) {
        self.scope.soft(error)
    }

    pub(in crate::hir) fn check_call_safety(&mut self, callee: FunctionId, span: Span) {
        if !self.scope.functions.defs[callee].is_unsafe {
            return;
        }

        self.require_unsafe(|this| {
            let signature = &this.scope.functions.defs[callee];
            let decl = collect::source_span(signature.decl_span);
            let name = this.arena.alloc_str(this.scope.symbols.get(signature.name));
            hir_error!(span, UnsafeCall { name, decl })
        });
    }

    fn require_unsafe(&mut self, error: impl FnOnce(&mut Self) -> HirError<'hir>) {
        match self.unsafe_ctx.depth > 0 {
            true => self.unsafe_ctx.ops += 1,
            _ => {
                let error = error(self);
                self.soft(error);
            },
        }
    }

    fn declare_local(
        &mut self,
        name: SymbolId,
        typ: Type<'hir>,
        mutable: bool,
        decl_span: Span,
    ) -> Result<LocalId, HirError<'hir>> {
        let scope = self.scopes.last_mut().expect("at least one scope is always present");

        if let Some(&existing) = scope.get(&name) {
            let previous = crate::hir::collect::source_span(self.locals[existing].decl_span);
            let name = self.arena.alloc_str(self.scope.symbols.get(name));
            return Err(hir_error!(decl_span, DuplicateBind { name, previous }));
        }

        let id = LocalId(self.next_local);
        self.next_local += 1;

        scope.insert(name, id);
        self.locals.push(Local { id, name, typ, mutable, decl_span });

        Ok(id)
    }

    fn resolve_local(&mut self, name: SymbolId, span: Span) -> Result<LocalId, HirError<'hir>> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name).copied())
            .ok_or_else(|| {
                let name = self.arena.alloc_str(self.scope.symbols.get(name));
                hir_error!(span, UndeclaredIdentifier { name })
            })
    }

    pub(super) fn bound_interface(
        &self,
        bound: &Spanned<statement::Type<'src>>,
    ) -> Option<SymbolId> {
        let name = match bound.value_ref() {
            statement::Type::Named(name) | statement::Type::Generic(name, _) => name,
            _ => return None,
        };
        self.scope.symbols.get_id(name)
    }

    fn lower_struct_fields<F, T>(
        &mut self,
        id: AdtId,
        generic_args: &'hir [Type<'hir>],
        fields: &[F],
        span: Span,
        allow_missing: bool,
        mut lower_field: impl FnMut(&mut Self, SymbolId, Type<'hir>, &F) -> Result<T, HirError<'hir>>,
    ) -> Result<Vec<(SymbolId, T)>, HirError<'hir>>
    where
        F: NamedField<'src>,
    {
        let definition_name = self.scope[id].name;
        let struct_name = self.arena.alloc_str(self.scope.symbols.get(definition_name));

        let mut seen = HashSet::with_capacity(fields.len());
        let mut lowered = Vec::with_capacity(fields.len());

        for field in fields {
            let field_symbol = self.scope.symbols.insert(field.name());
            if !seen.insert(field_symbol) {
                return Err(hir_error!(field.field_span(), DuplicateField { name: field.name() }));
            }

            let expected = self.scope[id]
                .field(field_symbol)
                .map(|f| f.typ.subst(&self.scope.types, &self.scope.arrays, generic_args))
                .ok_or_else(|| {
                    let (field, span) = (field.name(), field.field_span());
                    hir_error!(span, UnknownField { struct_name, field })
                })?;

            lowered.push((field_symbol, lower_field(self, field_symbol, expected, field)?));
        }

        if !allow_missing
            && let Some(name) =
                self.scope[id].fields().iter().find(|f| !seen.contains(&f.name)).map(|f| f.name)
        {
            let field = self.arena.alloc_str(self.scope.symbols.get(name));
            return Err(hir_error!(span, MissingField { struct_name, field }));
        }

        Ok(lowered)
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new())
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
}

fn declared_fn_names<'hir>(env: &GenericEnv<'hir>, symbols: &SymbolTable) -> Vec<SymbolId> {
    let mut named: Vec<(u8, &str)> = Vec::with_capacity(env.len());
    for (name, typ) in env {
        match typ.kind() {
            TypeKind::GenericParam(i) => named.push((i, name)),
            _ => return Vec::new(),
        }
    }

    named.sort_unstable_by_key(|&(index, _)| index);
    match named.iter().enumerate().all(|(at, &(index, _))| at == index as usize) {
        true => named.into_iter().map(|(_, name)| symbols.insert(name)).collect(),
        _ => Vec::new(),
    }
}

pub(in crate::hir) fn lower_const<'hir, 'src>(
    scope: &ItemTable<'hir>,
    expr: &expression::Expression<'src>,
    expected_type: Type<'hir>,
    arena: &'hir bumpalo::Bump,
) -> Result<(&'hir Expression<'hir>, TypeckResults<'hir>), HirError<'hir>>
where
    'src: 'hir,
{
    let mut builder = FunctionBuilder::new_for_const(scope, arena);
    let lowered = builder.lower_expr(expr, Some(expected_type))?;

    builder.assert_type(expected_type, lowered.typ, lowered.span)?;
    builder.resolve_inference();
    let value = lowered.expr;

    Ok((value, builder.typeck))
}

impl<'s, 'f, 'hir, 'src> TypeResolver<'hir, 'hir> for FunctionBuilder<'s, 'f, 'hir, 'src>
where
    'src: 'hir,
{
    fn named(&mut self, name: &'hir str, span: Span) -> Result<Type<'hir>, HirError<'hir>> {
        type_resolver::resolve_named(
            Some(&self.generic_env),
            name,
            span,
            |name| Some(self.scope.symbols.insert(name)),
            |symbol| self.scope.nominal_type(symbol),
        )
    }

    fn generic(
        &mut self,
        name: &'hir str,
        args: &[Type<'hir>],
        span: Span,
    ) -> Result<Type<'hir>, HirError<'hir>> {
        self.scope.generic_adt(name, args, span)
    }

    fn self_type(&mut self, span: Span) -> Result<Type<'hir>, HirError<'hir>> {
        self.impl_ctx
            .self_type
            .ok_or_else(|| hir_error!(span, UnknownType { name: "Self" }))
    }

    fn arrays(&self) -> &ArrayTable<'hir> {
        &self.scope.arrays
    }

    fn types(&self) -> &TyInterner<'hir> {
        &self.scope.types
    }
}

impl<'src> NamedField<'src> for expression::StructField<'src> {
    fn name(&self) -> &'src str {
        self.name
    }

    fn field_span(&self) -> Span {
        self.span
    }
}

impl<'src> NamedField<'src> for statement::PatternField<'src> {
    fn name(&self) -> &'src str {
        self.name
    }

    fn field_span(&self) -> Span {
        self.span
    }
}

impl<'s, 'f, 'hir, 'src> Index<LocalId> for FunctionBuilder<'s, 'f, 'hir, 'src> {
    type Output = Local<'hir>;
    fn index(&self, index: LocalId) -> &Self::Output {
        &self.locals[index]
    }
}

impl<'s, 'f, 'hir, 'src> Index<AdtId> for FunctionBuilder<'s, 'f, 'hir, 'src> {
    type Output = AdtDef<'hir>;
    fn index(&self, index: AdtId) -> &Self::Output {
        &self.scope[index]
    }
}
