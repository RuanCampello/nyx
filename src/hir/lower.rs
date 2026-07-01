use crate::{
    hir::{
        Arm, Block, Constant, EnumId, ExprId, Expression, ExpressionKind, Function, FunctionId,
        Intrinsic, Literal, Local, LocalId, Parameter, Pattern, PatternKind, RefTarget, Res,
        Statement, Struct, StructId, SymbolId, SymbolTable, SyscallCode, Type, TypeKind,
        TypeckResults,
        error::{CmpInterface, ConstFnViolationKind, HirError, hir_error},
        index_vec::IndexVec,
        infer::InferTable,
        place_base_local,
        scope::{ArrayTable, GenericEnv, Scope},
        symbols::{Mangler, qualified},
        type_resolver::{self, resolve_annotation},
    },
    lexer::{Spanned, token::Span},
    parser::{
        expression::{self, BinaryOperator, UnaryOperator},
        statement::{self, Else, PatternLit},
    },
};
use std::{
    collections::{HashMap, HashSet},
    ops::Index,
    str::FromStr,
};

pub(in crate::hir) struct FunctionBuilder<'s, 'f, 'hir, 'src> {
    scope: &'s mut Scope<'hir>,
    locals: IndexVec<LocalId, Local>,
    scopes: Vec<HashMap<SymbolId, LocalId>>,
    return_type: Type,
    function: Option<&'f statement::Function<'src>>,
    function_id: FunctionId,
    next_local: u32,
    next_expr_id: u32,
    symbols: &'s mut SymbolTable,
    is_const: bool,
    in_std: bool,
    self_type: Option<Type>,
    arena: &'hir bumpalo::Bump,
    typeck: TypeckResults,
    infer: InferTable,
    generic_env: GenericEnv,
}

/// A freshly lowered expression
///
/// Its arena reference plus the type and span the
/// lowering pass needs immediately for bidirectional checking
#[derive(Clone, Copy)]
pub(in crate::hir) struct Lowered<'hir> {
    expr: &'hir Expression<'hir>,
    typ: Type,
    span: Span,
}

impl<'s, 'f, 'hir, 'src> FunctionBuilder<'s, 'f, 'hir, 'src>
where
    'src: 'hir,
{
    pub fn new(
        scope: &'s mut Scope<'hir>,
        symbols: &'s mut SymbolTable,
        function_id: FunctionId,
        function: &'f statement::Function<'src>,
        in_std: bool,
        arena: &'hir bumpalo::Bump,
    ) -> Self {
        let self_type = function
            .impl_type
            .and_then(|impl_type| scope.lookup_named_type(impl_type, symbols));

        Self {
            scope,
            symbols,
            is_const: function.is_const,
            in_std,
            return_type: TypeKind::Unit.into(),
            function: Some(function),
            function_id,
            next_local: 0,
            next_expr_id: 0,
            locals: IndexVec::new(),
            typeck: TypeckResults::default(),
            infer: InferTable::default(),
            scopes: vec![HashMap::new()],
            self_type,
            arena,
            generic_env: HashMap::new(),
        }
    }

    pub fn new_instance(
        scope: &'s mut Scope<'hir>,
        symbols: &'s mut SymbolTable,
        function_id: FunctionId,
        function: &'f statement::Function<'src>,
        in_std: bool,
        arena: &'hir bumpalo::Bump,
        generic_env: GenericEnv,
    ) -> Self {
        let mut builder = Self::new(scope, symbols, function_id, function, in_std, arena);
        builder.generic_env = generic_env;
        builder
    }

    pub fn new_for_const(
        scope: &'s mut Scope<'hir>,
        symbols: &'s mut SymbolTable,
        in_std: bool,
        arena: &'hir bumpalo::Bump,
    ) -> Self {
        Self {
            scope,
            symbols,
            is_const: true,
            in_std,
            return_type: TypeKind::Unit.into(),
            function: None,
            function_id: FunctionId(0),
            next_local: 0,
            next_expr_id: 0,
            locals: IndexVec::new(),
            typeck: TypeckResults::default(),
            infer: InferTable::default(),
            scopes: vec![HashMap::new()],
            self_type: None,
            arena,
            generic_env: HashMap::new(),
        }
    }

    /// Push an expression node into the arena, record its type in the parallel
    /// side-table, and return a [`Lowered`] handle
    #[inline]
    fn alloc(&mut self, kind: ExpressionKind<'hir>, typ: Type, span: Span) -> Lowered<'hir> {
        let id = ExprId(self.next_expr_id);
        self.next_expr_id += 1;
        let expr = self.arena.alloc(Expression { id, kind, span });
        self.typeck.node_types.push(typ);

        Lowered { expr, typ, span }
    }

    #[inline(always)]
    pub fn lower(mut self) -> Result<Function<'hir>, HirError<'hir>> {
        let function = self.function.take().expect("function to be present");
        let id = self.function_id;
        let signature = self.scope.signatures[id].clone();
        let symbol = signature.name;
        self.return_type = signature.return_type;

        let mut params = Vec::with_capacity(signature.params.len());

        if let Some(receiver) = function.receiver {
            let typ = signature.receiver_type().expect("receiver in AST without one in signature");
            let symbol = self.symbols.insert("self");
            let id = self.declare_local(symbol, typ, receiver.mutable, receiver.span)?;
            params.push(Parameter { typ, id, name: symbol, mutable: receiver.mutable });
        }

        params.extend(
            function
                .params
                .iter()
                .zip(signature.explicit_params().iter())
                .map(|(parameter, &typ)| -> Result<_, HirError> {
                    let symbol = self.symbols.insert(parameter.name);
                    let id = self.declare_local(symbol, typ, parameter.mutable, parameter.span)?;

                    Ok(Parameter { typ, id, name: symbol, mutable: parameter.mutable })
                })
                .collect::<Result<Vec<_>, _>>()?,
        );

        let (body, _) = self.lower_block(&function.body, true)?;
        self.resolve_inference();
        let generics = declared_fn_names(&self.generic_env, self.symbols);

        Ok(Function {
            id,
            name: symbol,
            decl_span: function.span,
            params,
            locals: self.locals,
            return_type: signature.return_type,
            is_const: function.is_const,
            is_pub: function.is_pub,
            inline: function.inline,
            kind: signature.kind,
            typeck: self.typeck,
            body,
            generics,
        })
    }

    fn lower_block(
        &mut self,
        block: &statement::Block<'src>,
        is_tail: bool,
    ) -> Result<(Block<'hir>, bool), HirError<'hir>> {
        self.push_scope();
        let last_idx = block.statements.len().saturating_sub(1);

        let mut statements_vec = Vec::with_capacity(block.statements.len());
        let mut returns = false;
        for (idx, statement) in block.statements.iter().enumerate() {
            match self.lower_statement(statement, is_tail && idx == last_idx) {
                Ok((statement, did_return)) => {
                    statements_vec.push(statement);
                    returns |= did_return;
                },
                Err(error) => self.soft(error)?,
            }
        }

        self.pop_scope();
        let statements = self.arena.alloc_slice_copy(&statements_vec);
        Ok((Block { statements, span: block.span }, returns))
    }

    fn lower_statement(
        &mut self,
        statement: &statement::Statement<'src>,
        is_tail: bool,
    ) -> Result<(Statement<'hir>, bool), HirError<'hir>> {
        use statement::Statement as Stmt;

        match statement {
            Stmt::Let(statement) => {
                let typ = match (statement.typ.as_ref(), statement.value.as_ref()) {
                    (Some(typ), _) => {
                        self.resolve_type(&typ.value(), typ.span()).or_else(|e| self.poison(e))?
                    },
                    (_, Some(expr)) => self.infer(expr).or_else(|e| self.poison(e))?,
                    (None, None) => self.poison(hir_error!(
                        statement.span,
                        MissingInitialiser { name: statement.name }
                    ))?,
                };

                let symbol = self.symbols.insert(statement.name);
                let id = self.declare_local(symbol, typ, statement.mutable, statement.name_span)?;

                let mut diverges = false;
                let stmt = match statement.value {
                    Some(ref expr) => match self.lower_expr(expr, Some(typ)) {
                        Ok(expr) => {
                            self.assert_type(typ, expr.typ, expr.span)?;
                            diverges = expr.typ.diverges();

                            Statement::LetInit { id, init: expr.expr }
                        },
                        // a broken initialiser must not take the binding with it:
                        // keep it declared so its uses do not cascade
                        Err(error) => {
                            self.soft(error)?;
                            Statement::LetUninit { id }
                        },
                    },
                    _ => Statement::LetUninit { id },
                };

                Ok((stmt, diverges))
            },

            Stmt::Return(statement) => {
                let value = statement
                    .value
                    .as_ref()
                    .map(|expr| {
                        let expr = self.lower_expr(expr, Some(self.return_type))?;
                        self.assert_type(self.return_type, expr.typ, expr.span)?;
                        Ok(expr.expr)
                    })
                    .transpose()?;

                Ok((Statement::Return(value), true))
            },

            Stmt::If(statement) => self.lower_if(statement, is_tail),

            Stmt::While(statement) => {
                let condition = self.lower_expr(&statement.condition, None)?;
                self.assert_type(TypeKind::Bool, condition.typ, statement.span)?;

                // PERFORMANCE: remove loops with constant false conditions
                let (body, _) = self.lower_block(&statement.body, false)?;

                Ok((Statement::While { condition: condition.expr, body }, false))
            },
            Stmt::Expr(expr, _) => {
                let tail_ret = is_tail && self.return_type.kind() != TypeKind::Unit;
                let expr = self.lower_expr(expr, tail_ret.then_some(self.return_type))?;

                self.handle_tail_expr(expr, tail_ret)
            },
            Stmt::Block(block) => {
                let (block, returns) = self.lower_block(block, is_tail)?;
                Ok((Statement::Block(block), returns))
            },

            Stmt::Match(statement) => {
                let tail_ret = is_tail && self.return_type.kind() != TypeKind::Unit;
                let expr = self.lower_match(statement, tail_ret.then_some(self.return_type))?;

                self.handle_tail_expr(expr, tail_ret)
            },

            Stmt::Item(statement::Item { kind: statement::ItemKind::Const(constant), .. }) => {
                let typ = self
                    .resolve_type(&constant.typ.value(), constant.typ.span())
                    .or_else(|e| self.poison(e))?;

                let symbol = self.symbols.insert(constant.name);
                let id = self.declare_local(symbol, typ, false, constant.span)?;

                let stmt = match self.lower_expr(&constant.value, Some(typ)) {
                    Ok(expr) => {
                        self.assert_type(typ, expr.typ, expr.span)?;
                        Statement::LetInit { id, init: expr.expr }
                    },
                    Err(error) => {
                        self.soft(error)?;
                        Statement::LetUninit { id }
                    },
                };

                Ok((stmt, false))
            },

            Stmt::Item(statement::Item { kind, .. }) => {
                Err(hir_error!(kind.span(), NestedItem { kind: kind.keyword() }))
            },
        }
    }

    fn lower_identifier(
        &mut self,
        name: &'src str,
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        if let Some(id) = self.local_id(name) {
            return Ok(self.local_expr(id, span));
        }

        if let Some(c) = self.constant(name) {
            let (val, symbol) = (c.value, c.name);
            let typeck = c.typeck.clone();
            let lowered = self.splice_const(val, &typeck, span);
            self.typeck.const_uses.insert(lowered.expr.id, symbol);
            return Ok(lowered);
        }

        let symbol = self
            .symbols
            .get_id(name)
            .ok_or_else(|| hir_error!(span, UndeclaredIdentifier { name }))?;
        let id = self.resolve_local(symbol, span)?;

        Ok(self.local_expr(id, span))
    }

    #[inline(always)]
    fn handle_tail_expr(
        &mut self,
        expr: Lowered<'hir>,
        tail_ret: bool,
    ) -> Result<(Statement<'hir>, bool), HirError<'hir>> {
        match tail_ret && !expr.typ.diverges() {
            true => {
                self.assert_type(self.return_type, expr.typ, expr.span)?;
                Ok((Statement::Return(Some(expr.expr)), true))
            },
            _ => Ok((Statement::Expr(expr.expr), tail_ret || expr.typ.diverges())),
        }
    }

    /// Copy a constant's expression subtree (nodes and their types) into this
    /// body's arena, re-spanning the inlined root to the use site
    fn splice_const(
        &mut self,
        expr: &Expression<'hir>,
        const_typeck: &TypeckResults,
        span: Span,
    ) -> Lowered<'hir> {
        use ExpressionKind as Kind;

        let kind = match &expr.kind {
            Kind::Literal(lit) => Kind::Literal(*lit),
            Kind::Local(id) => Kind::Local(*id),
            Kind::Unary { operator, expr } => {
                let expr = self.splice_const(expr, const_typeck, span);
                Kind::Unary { operator: *operator, expr: expr.expr }
            },
            Kind::Binary { operator, left, right } => {
                let left = self.splice_const(left, const_typeck, span);
                let right = self.splice_const(right, const_typeck, span);
                Kind::Binary { operator: *operator, left: left.expr, right: right.expr }
            },
            Kind::Field { base, field } => {
                let base = self.splice_const(base, const_typeck, span);
                Kind::Field { base: base.expr, field: *field }
            },
            Kind::Assign { target, value } => {
                let target = self.splice_const(target, const_typeck, span);
                let value = self.splice_const(value, const_typeck, span);
                Kind::Assign { target: target.expr, value: value.expr }
            },
            Kind::Path(symbol) => Kind::Path(*symbol),
            Kind::Array { elements } => {
                let elements = elements
                    .iter()
                    .map(|element| self.splice_const(element, const_typeck, span).expr)
                    .collect::<Vec<_>>();
                Kind::Array { elements: self.arena.alloc_slice_copy(&elements) }
            },
            Kind::ArrayRepeat { value, count } => {
                let value = self.splice_const(value, const_typeck, span);
                Kind::ArrayRepeat { value: value.expr, count: *count }
            },
            Kind::Index { base, index } => {
                let base = self.splice_const(base, const_typeck, span);
                let index = self.splice_const(index, const_typeck, span);
                Kind::Index { base: base.expr, index: index.expr }
            },
            Kind::MethodCall { name, receiver, args } => {
                let receiver = self.splice_const(receiver, const_typeck, span);
                let args = args
                    .iter()
                    .map(|arg| self.splice_const(arg, const_typeck, span).expr)
                    .collect::<Vec<_>>();
                let args = self.arena.alloc_slice_copy(&args);
                Kind::MethodCall { name: *name, receiver: receiver.expr, args }
            },
            Kind::Struct { id, fields } => {
                let fields = fields
                    .iter()
                    .map(|(sym, val)| (*sym, self.splice_const(val, const_typeck, span).expr))
                    .collect::<Vec<_>>();
                let fields = self.arena.alloc_slice_copy(&fields);
                Kind::Struct { id: *id, fields }
            },
            Kind::Call { callee, args } => {
                let callee = self.splice_const(callee, const_typeck, span);
                let args = args
                    .iter()
                    .map(|arg| self.splice_const(arg, const_typeck, span).expr)
                    .collect::<Vec<_>>();
                let args = self.arena.alloc_slice_copy(&args);
                Kind::Call { callee: callee.expr, args }
            },
            Kind::Syscall { code, args } => {
                let args = args
                    .iter()
                    .map(|arg| self.splice_const(arg, const_typeck, span).expr)
                    .collect::<Vec<_>>();
                let args = self.arena.alloc_slice_copy(&args);
                Kind::Syscall { code: *code, args }
            },
            Kind::IntrinsicCall { intrinsic, args } => {
                let args = args
                    .iter()
                    .map(|arg| self.splice_const(arg, const_typeck, span).expr)
                    .collect::<Vec<_>>();
                let args = self.arena.alloc_slice_copy(&args);
                Kind::IntrinsicCall { intrinsic: *intrinsic, args }
            },
            Kind::TypeIntrinsic { kind, typ } => Kind::TypeIntrinsic { kind: *kind, typ: *typ },
            Kind::Cast { from, to } => {
                let from = self.splice_const(from, const_typeck, span);
                Kind::Cast { from: from.expr, to: *to }
            },
            Kind::Match { scrutinee, arms } => {
                let scrutinee = self.splice_const(scrutinee, const_typeck, span);
                let mut arms_copied = Vec::with_capacity(arms.len());

                for arm in *arms {
                    let pattern = self.splice_pattern(arm.pattern);
                    let guard = arm.guard.map(|g| self.splice_const(g, const_typeck, span).expr);
                    let body = self.splice_const(arm.body, const_typeck, span);
                    arms_copied.push(Arm {
                        pattern: self.arena.alloc(pattern),
                        guard,
                        body: body.expr,
                        span: arm.span,
                    });
                }
                Kind::Match {
                    scrutinee: scrutinee.expr,
                    arms: self.arena.alloc_slice_copy(&arms_copied),
                }
            },
        };

        let typ = const_typeck.type_of(expr.id);
        let lowered = self.alloc(kind, typ, span);

        // carry over the resolved call target / generic args for spliced calls
        const_typeck
            .type_dependent_def(expr.id)
            .and_then(|def| self.typeck.type_dependent_defs.insert(lowered.expr.id, def));
        const_typeck
            .node_args(expr.id)
            .and_then(|args| self.typeck.node_args.insert(lowered.expr.id, args.to_vec()));

        lowered
    }

    fn splice_pattern(&self, pat: &Pattern<'hir>) -> Pattern<'hir> {
        let kind = match &pat.kind {
            PatternKind::Wildcard => PatternKind::Wildcard,
            PatternKind::Literal(lit) => PatternKind::Literal(*lit),
            PatternKind::Binding(id) => PatternKind::Binding(*id),
            PatternKind::Or(pats) => {
                let spliced: Vec<_> = pats.iter().map(|p| self.splice_pattern(p)).collect();
                PatternKind::Or(self.arena.alloc_slice_copy(&spliced))
            },
            PatternKind::Variant { id: enum_id, variant_idx, sub } => {
                let sub = sub.map(|s| {
                    let spliced = self.splice_pattern(s);
                    &*self.arena.alloc(spliced)
                });
                PatternKind::Variant { id: *enum_id, variant_idx: *variant_idx, sub }
            },
        };
        Pattern { kind, span: pat.span }
    }

    /// resolves every remaining integer inference variable in the body, pinning
    /// unconstrained ones to `i32`, so no [TypeKind::Infer] reaches MIR
    fn resolve_inference(&mut self) {
        let infer = &mut self.infer;
        let arrays = &self.scope.arrays;

        for typ in self.typeck.node_types.iter_mut() {
            *typ = Self::resolve_deep(infer, arrays, *typ);
        }
        for local in self.locals.iter_mut() {
            local.typ = Self::resolve_deep(infer, arrays, local.typ);
        }
        for args in self.typeck.node_args.values_mut() {
            for typ in args.iter_mut() {
                *typ = Self::resolve_deep(infer, arrays, *typ);
            }
        }
    }

    /// resolves inference variables nested inside compound types, pinning the
    /// elements of arrays left inferred so the [Hir] array table only ever exposes
    /// concrete element types to later stages
    fn resolve_deep(infer: &mut InferTable, arrays: &ArrayTable, typ: Type) -> Type {
        match typ.kind() {
            TypeKind::Array(id) => {
                let array = arrays.get(id);
                let element = Self::resolve_deep(infer, arrays, array.element);
                // mutate in place rather than re-intern: an inferred array is unique
                // to this body, so no other type id observes the change, and the
                // global table never retains an unresolved element for MIR to lay out
                if element != array.element {
                    arrays.resolve(id, element);
                }
                typ
            },
            TypeKind::Slice { mutable, element } => {
                match RefTarget::try_from(Self::resolve_deep(infer, arrays, element.into())) {
                    Ok(target) => Type::slice(target, mutable),
                    Err(()) => typ,
                }
            },
            TypeKind::Ref { mutable, to } => {
                match RefTarget::try_from(Self::resolve_deep(infer, arrays, to.into())) {
                    Ok(target) => Type::refer(target, mutable),
                    Err(()) => typ,
                }
            },
            _ => infer.resolve_or_default(typ),
        }
    }

    fn local_id(&self, name: &str) -> Option<LocalId> {
        let symbol = self.symbols.get_id(name)?;

        self.scopes.iter().rev().find_map(|scope| scope.get(&symbol).copied())
    }

    fn local_expr(&mut self, id: LocalId, span: Span) -> Lowered<'hir> {
        let typ = self[id].typ;
        self.alloc(ExpressionKind::Local(id), typ, span)
    }

    fn constant(&self, name: &str) -> Option<&Constant<'hir>> {
        if let Some(impl_type) = self.function.and_then(|function| function.impl_type) {
            let scoped = self.mangler().scoped_item(impl_type, name);
            if let Some(constant) = self.constant_by_symbol_name(&scoped) {
                return Some(constant);
            }
        }

        let top_level = self.mangler().item(name);
        self.constant_by_symbol_name(&top_level)
            .or_else(|| self.constant_by_symbol_name(name))
    }

    #[inline]
    fn constant_by_symbol_name(&self, name: &str) -> Option<&Constant<'hir>> {
        let symbol = self.symbols.get_id(name)?;

        self.scope.constants.get(&symbol)
    }

    #[inline]
    fn mangler(&self) -> &Mangler<'_> {
        &self.scope.mangler
    }

    /// Lowers an expression with an optional type hint flowing downward (biderectional checking)
    ///
    /// The hint is used to resolve the concrete type of integer and float literals when the
    /// expected type is known from context
    ///
    /// When the hint is `None`, literals default to `i32` and `f64` respectively
    fn lower_expr(
        &mut self,
        expr: &expression::Expression<'src>,
        hint: Option<Type>,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        use expression::Expression as Expr;

        match expr {
            // a concrete numeric hint pins the literal; an inference hint joins
            // its class; otherwise a fresh integer variable defers the choice to
            // the literal's first constrained use
            Expr::Integer(value, span) => {
                let typ = match hint {
                    Some(t) if t.is_number() || t.is_infer() => t,
                    _ => self.infer.fresh(),
                };

                Ok(self.alloc((*value).into(), typ, *span))
            },

            Expr::Float(value, span) => {
                let typ =
                    hint.and_then(|t| t.is_float().then_some(t)).unwrap_or(TypeKind::F64.into());

                Ok(self.alloc((*value).into(), typ, *span))
            },

            Expr::String(value, span) => {
                let sym = self.symbols.insert(value);
                Ok(self.alloc(
                    ExpressionKind::Literal(Literal::Str(sym)),
                    TypeKind::Str.into(),
                    *span,
                ))
            },

            Expr::Char(value, span) => {
                Ok(self.alloc((*value).into(), TypeKind::Char.into(), *span))
            },
            Expr::Bool(value, span) => {
                Ok(self.alloc((*value).into(), TypeKind::Bool.into(), *span))
            },

            Expr::Cast { expr: inner, target_type, span } => {
                let target = self.resolve_type(&target_type.value(), target_type.span())?;
                let lowered_expr = self.lower_expr(inner, None)?;
                // an unresolved integer variable is always cast-compatible: it
                // resolves to an integral type, which is primitive-castable
                let src = self.infer.resolve_shallow(lowered_expr.typ);

                let src_castable = src.is_primitive_castable()
                    || src.is_infer()
                    || matches!(src.kind(), TypeKind::Enum(_));
                if !src_castable || !target.is_primitive_castable() {
                    return Err(hir_error!(*span, InvalidCast { src, target }));
                }

                Ok(self.alloc(
                    ExpressionKind::Cast { from: lowered_expr.expr, to: target },
                    target,
                    *span,
                ))
            },

            Expr::Identifier(name, span) => self.lower_identifier(name, *span),

            Expr::QualifiedName { qualifier, name, span } => {
                let enum_symbol = self.symbols.insert(qualifier);
                let variant_symbol = self.symbols.insert(name);
                if let Some((id, value)) =
                    self.scope.enum_variants.get(&(enum_symbol, variant_symbol)).copied()
                {
                    return Ok(self.alloc(
                        ExpressionKind::Literal(Literal::Int(value)),
                        Type::enumerable(id),
                        *span,
                    ));
                }

                let mangled_name = self.mangler().scoped_item(qualifier, name);
                let qualified = qualified(self.arena, qualifier, name);
                let symbol = match self.symbols.get_id(&mangled_name) {
                    Some(sym) => sym,
                    None => {
                        return Err(hir_error!(*span, UndeclaredIdentifier { name: qualified }));
                    },
                };

                let c =
                    self.scope.constants.get(&symbol).cloned().ok_or_else(|| {
                        hir_error!(*span, UndeclaredIdentifier { name: qualified })
                    })?;

                let lowered = self.splice_const(c.value, &c.typeck, *span);
                self.typeck.const_uses.insert(lowered.expr.id, c.name);
                Ok(lowered)
            },

            Expr::Unary { operator, expr, span } => {
                let inner_hint = match operator {
                    UnaryOperator::Neg => hint,
                    UnaryOperator::Not => hint,
                    UnaryOperator::Deref => hint.map(|h| {
                        let to = match h.kind() {
                            TypeKind::Ref { to, .. } => to,
                            TypeKind::Struct(id) => RefTarget::new(TypeKind::Struct(id)),
                            _ => RefTarget::new(TypeKind::Char),
                        };
                        Type::refer(to, false)
                    }),
                    UnaryOperator::Ref | UnaryOperator::RefMut => hint.map(|h| h.strip_reference()),
                };
                let expr = self.lower_expr(expr, inner_hint)?;

                // PERFORMANCE: fold unary operations when operand is a constant literal
                let expected = match operator {
                    UnaryOperator::Neg => match expr.typ.is_number() {
                        true => expr.typ,
                        _ => {
                            return Err(hir_error!(
                                expr.span,
                                TypeMismatch { expected: TypeKind::I32.into(), found: expr.typ }
                            ));
                        },
                    },

                    UnaryOperator::Not => {
                        match expr.typ == TypeKind::Bool.into() || expr.typ.is_integer() {
                            true => expr.typ,
                            _ => {
                                return Err(hir_error!(
                                    expr.span,
                                    TypeMismatch {
                                        expected: TypeKind::Bool.into(),
                                        found: expr.typ
                                    }
                                ));
                            },
                        }
                    },

                    UnaryOperator::Deref => match expr.typ.kind() {
                        TypeKind::Ref { to, .. } => to.into(),
                        _ => {
                            return Err(hir_error!(
                                expr.span,
                                TypeMismatch {
                                    expected: Type::refer(RefTarget::new(TypeKind::Char), false),
                                    found: expr.typ
                                }
                            ));
                        },
                    },

                    UnaryOperator::Ref | UnaryOperator::RefMut => {
                        match self.coerce_array_to_slice(expr.typ, hint) {
                            // `&array` unsizes to a `&[T]`/`&mut [T]` slice in slice context
                            Some(slice) => slice,
                            None => {
                                let err = hir_error!(
                                    expr.span,
                                    TypeMismatch {
                                        expected: Type::structure(Default::default()),
                                        found: expr.typ
                                    }
                                );
                                let to = RefTarget::try_from(expr.typ).map_err(|_| err)?;
                                Type::refer(to, *operator == UnaryOperator::RefMut)
                            },
                        }
                    },
                };

                if !matches!(
                    operator,
                    UnaryOperator::Deref | UnaryOperator::Ref | UnaryOperator::RefMut
                ) {
                    self.assert_type(expected, expr.typ, expr.span)?;
                }

                Ok(self.alloc(
                    ExpressionKind::Unary { operator: *operator, expr: expr.expr },
                    expected,
                    *span,
                ))
            },

            Expr::Binary { left, operator, right, span } => {
                // for arithmetic or comparison, we propagate the hint to the left side first to
                // resolve the concrete numeric type, then use that resolved type as the hint
                // for the right side.
                //
                // this ensures `x + 1` where `x: i64` correctly widens the literal `1` to `i64`
                let left = self.lower_expr(left, hint)?;
                let right_hint = match operator {
                    BinaryOperator::Add
                    | BinaryOperator::Sub
                    | BinaryOperator::Mul
                    | BinaryOperator::Div
                    | BinaryOperator::Lt
                    | BinaryOperator::LtEq
                    | BinaryOperator::Gt
                    | BinaryOperator::GtEq
                    | BinaryOperator::Eq
                    | BinaryOperator::Ne
                    | BinaryOperator::BitAnd
                    | BinaryOperator::BitOr
                    | BinaryOperator::BitXor => Some(self.infer.resolve_shallow(left.typ)),
                    BinaryOperator::And | BinaryOperator::Or => Some(TypeKind::Bool.into()),
                    BinaryOperator::Shl | BinaryOperator::Shr => None,
                };
                let right = self.lower_expr(right, right_hint)?;

                if let Some(method) = operator.overload_method() {
                    let receiver = left.typ.strip_reference();
                    if matches!(receiver.kind(), TypeKind::Struct(_) | TypeKind::Enum(_)) {
                        let method_symbol = self.symbols.insert(method);
                        if let Some(&function) = self.scope.methods.get(&(receiver, method_symbol))
                        {
                            let lowered = self.alloc(
                                ExpressionKind::Binary {
                                    operator: *operator,
                                    left: left.expr,
                                    right: right.expr,
                                },
                                TypeKind::Bool.into(),
                                *span,
                            );
                            self.typeck
                                .type_dependent_defs
                                .insert(lowered.expr.id, Res::Function(function));
                            return Ok(lowered);
                        }

                        let type_name = match receiver.kind() {
                            TypeKind::Struct(sid) => self.symbols.get(self[sid].name).to_string(),
                            TypeKind::Enum(id) => self
                                .scope
                                .enums
                                .get(id)
                                .map(|e| self.symbols.get(e.name).to_string())
                                .unwrap_or_else(|| receiver.to_string()),
                            _ => receiver.to_string(),
                        };
                        let type_name = self.arena.alloc_str(&type_name);
                        return Err(hir_error!(
                            *span,
                            OperatorRequiresInterface {
                                op: operator.symbol(),
                                type_name,
                                interface_name: operator.required_interface(),
                            }
                        ));
                    }
                }

                // PERFORMANCE: constant fold binary operator on literals
                let result = self.type_for_binary(operator, left.typ, right.typ, *span)?;

                Ok(self.alloc(
                    ExpressionKind::Binary {
                        operator: *operator,
                        left: left.expr,
                        right: right.expr,
                    },
                    result,
                    *span,
                ))
            },

            Expr::Assignment { target, value, span } => {
                let target_lowered = self.lower_expr(target, None)?;

                let through_slice = match &target_lowered.expr.kind {
                    ExpressionKind::Index { base, .. } => {
                        match self.typeck.type_of(base.id).kind() {
                            TypeKind::Slice { mutable, .. } => Some(mutable),
                            _ => None,
                        }
                    },
                    _ => None,
                };

                match through_slice {
                    Some(true) => {},
                    Some(false) => return Err(hir_error!(target.span(), AssignBehindSharedRef)),
                    None => {
                        let local = place_base_local(target_lowered.expr)
                            .ok_or_else(|| hir_error!(*span, InvalidAssignmentTarget))?;

                        if !self[local].mutable {
                            let err_span = match &target_lowered.expr.kind {
                                ExpressionKind::Local(_) => span,
                                ExpressionKind::Field { .. } | ExpressionKind::Index { .. } => {
                                    &target.span()
                                },
                                _ => span,
                            };

                            let name = self.arena.alloc_str(self.symbols.get(self[local].name));
                            return Err(hir_error!(*err_span, ImmutableBind { name }));
                        }
                    },
                }

                let value = self.lower_expr(value, Some(target_lowered.typ))?;
                self.assert_type(target_lowered.typ, value.typ, *span)?;

                let typ = target_lowered.typ;
                Ok(self.alloc(
                    ExpressionKind::Assign { target: target_lowered.expr, value: value.expr },
                    typ,
                    *span,
                ))
            },

            Expr::Struct { name, fields, span, type_args } => {
                let id = if !type_args.is_empty() {
                    let mut resolved = Vec::with_capacity(type_args.len());
                    for arg in type_args {
                        resolved.push(self.resolve_type(arg.value_ref(), arg.span())?);
                    }
                    let struct_sym = self.symbols.insert(name);
                    if let Some(template) = self.scope.generic_structs.get(&struct_sym) {
                        self.check_bounds(&template.generics, &resolved, *span)?;
                    }
                    let typ =
                        self.scope.instantiate_generic(name, &resolved, *span, self.symbols)?;
                    match typ.kind() {
                        TypeKind::Struct(id) => id,
                        _ => return Err(hir_error!(*span, UnknownType { name })),
                    }
                } else {
                    let symbol = self.symbols.insert(name);
                    self.scope
                        .struct_map
                        .get(&symbol)
                        .copied()
                        .ok_or_else(|| hir_error!(*span, UnknownType { name }))?
                };

                let definition_name = self.scope[id].name;
                let struct_name = self.arena.alloc_str(self.symbols.get(definition_name));
                let definition_fields = self.scope[id].fields.clone();

                let mut seen = HashSet::with_capacity(fields.len());
                let mut lowered = Vec::with_capacity(fields.len());

                for field in fields {
                    let field_symbol = self.symbols.insert(field.name);
                    if !seen.insert(field_symbol) {
                        return Err(hir_error!(field.span, DuplicateField { name: field.name }));
                    }

                    let Some(expected) = definition_fields.iter().find(|f| f.name == field_symbol)
                    else {
                        return Err(hir_error!(
                            field.span,
                            UnknownField { struct_name, field: field.name }
                        ));
                    };

                    let value = self.lower_expr(&field.value, Some(expected.typ))?;
                    self.assert_type(expected.typ, value.typ, value.span)?;
                    lowered.push((field_symbol, value.expr));
                }

                for expected in &definition_fields {
                    if !seen.contains(&expected.name) {
                        return Err(hir_error!(
                            *span,
                            MissingField {
                                struct_name,
                                field: self.arena.alloc_str(self.symbols.get(expected.name)),
                            }
                        ));
                    }
                }

                let fields = self.arena.alloc_slice_copy(&lowered);
                Ok(self.alloc(ExpressionKind::Struct { id, fields }, Type::structure(id), *span))
            },

            Expr::Field { expr: base, field, span } => {
                let base_lowered = self.lower_expr(base, None)?;
                if !matches!(
                    &base_lowered.expr.kind,
                    ExpressionKind::Local(_) | ExpressionKind::Field { .. }
                ) {
                    return Err(hir_error!(*span, InvalidFieldAccess));
                }

                let (field_symbol, typ) = self.lookup_field(base_lowered.typ, field, *span)?;
                Ok(self.alloc(
                    ExpressionKind::Field { base: base_lowered.expr, field: field_symbol },
                    typ,
                    *span,
                ))
            },

            Expr::Array { elements, span } => {
                let element_hint = hint.and_then(|h| self.element_type(h));
                let mut lowered = Vec::with_capacity(elements.len());
                let mut element_type = None;

                for element in elements {
                    let value = self.lower_expr(element, element_type.or(element_hint))?;

                    let expected = element_type.get_or_insert(value.typ);
                    self.assert_type(*expected, value.typ, value.span)?;

                    lowered.push(value.expr);
                }

                let element_type = element_type
                    .or(element_hint)
                    .ok_or_else(|| hir_error!(*span, EmptyArrayType))?;
                // an un-annotated element stays an inference variable so later uses
                // (e.g. assigning a `uptr` into a slot) can pin it; `resolve_inference`
                // settles the concrete element before the body leaves HIR
                let element_type = self.infer.resolve_shallow(element_type);
                let id = self.scope.arrays.intern(element_type, elements.len() as u32);
                let elements = self.arena.alloc_slice_copy(&lowered);

                Ok(self.alloc(ExpressionKind::Array { elements }, Type::array(id), *span))
            },

            Expr::ArrayRepeat { value, count, span } => {
                let element_hint = hint.and_then(|h| self.element_type(h));
                let value = self.lower_expr(value, element_hint)?;
                let count = *count as u32;
                // the element stays an inference variable until `resolve_inference`
                // settles its concrete type in place (see `Expr::Array`)
                let element_type = self.infer.resolve_shallow(value.typ);
                let id = self.scope.arrays.intern(element_type, count);

                Ok(self.alloc(
                    ExpressionKind::ArrayRepeat { value: value.expr, count },
                    Type::array(id),
                    *span,
                ))
            },

            Expr::Index { base, index, span } => {
                let base_lowered = self.lower_expr(base, None)?;
                let element = self
                    .element_type(base_lowered.typ)
                    .ok_or_else(|| hir_error!(*span, NotIndexable { typ: base_lowered.typ }))?;

                let index_lowered = self.lower_expr(index, Some(TypeKind::Uptr.into()))?;
                // an un-annotated counter used as an index defaults to `uptr`; a
                // counter already pinned to another integer keeps it (indices are
                // accepted at any integer width)
                let index_type = self.infer.resolve_shallow(index_lowered.typ);
                if index_type.is_infer() {
                    self.infer.unify(TypeKind::Uptr.into(), index_type).ok();
                }
                let index_type = self.infer.resolve_shallow(index_type);
                if !index_type.is_integer() {
                    return Err(hir_error!(
                        index_lowered.span,
                        TypeMismatch { expected: TypeKind::Uptr.into(), found: index_type }
                    ));
                }

                // a statically-known array index is bounds-checked at compile time,
                // dynamic indices and all slices fall back to the runtime check in MIR
                if let TypeKind::Array(id) = base_lowered.typ.kind()
                    && let ExpressionKind::Literal(Literal::Int(k)) = index_lowered.expr.kind
                {
                    let len = self.scope.arrays.get(id).len;
                    if k < 0 || k as u64 >= len as u64 {
                        return Err(hir_error!(*span, IndexOutOfBounds { index: k as u64, len }));
                    }
                }

                Ok(self.alloc(
                    ExpressionKind::Index { base: base_lowered.expr, index: index_lowered.expr },
                    element,
                    *span,
                ))
            },

            Expr::Call { callee, args, span, type_args } => {
                if let Expr::Field { expr: receiver, field: method_name, .. } = callee.as_ref() {
                    let receiver_lowered = self.lower_expr(receiver, None)?;
                    let base_local = place_base_local(receiver_lowered.expr);
                    let receiver_type = receiver_lowered.typ;

                    let receiver_base_type = receiver_type.strip_reference();

                    let method_symbol = self.symbols.insert(method_name);

                    let lookup_type = match receiver_base_type.kind() {
                        TypeKind::Slice { element, .. } => {
                            self.scope.specialize_slice_impls(receiver_base_type, self.symbols)?;
                            Type::slice(element, false)
                        },
                        TypeKind::Array(id) => {
                            let resolved =
                                self.infer.resolve_or_default(self.scope.arrays.get(id).element);
                            let element = RefTarget::try_from(resolved).map_err(|_| {
                                hir_error!(
                                    receiver_lowered.span,
                                    TypeMismatch {
                                        expected: Type::structure(Default::default()),
                                        found: resolved
                                    }
                                )
                            })?;
                            let slice = Type::slice(element, false);
                            self.scope.specialize_slice_impls(slice, self.symbols)?;
                            slice
                        },
                        _ => receiver_base_type,
                    };

                    let struct_name = match receiver_base_type.kind() {
                        TypeKind::Struct(sid) => self.symbols.get(self[sid].name).to_string(),
                        TypeKind::Enum(id) => self
                            .scope
                            .enums
                            .get(id)
                            .map(|e| self.symbols.get(e.name).to_string())
                            .unwrap_or_else(|| receiver_base_type.to_string()),
                        _ => receiver_base_type.to_string(),
                    };
                    let function =
                        *self.scope.methods.get(&(lookup_type, method_symbol)).ok_or_else(
                            || {
                                let struct_name = self.arena.alloc_str(&struct_name);
                                hir_error!(*span, UnknownMethod { struct_name, name: method_name })
                            },
                        )?;

                    if let Some(intrinsic) = self.scope.signatures[function].kind.intrinsic() {
                        let return_type = self.scope.signatures[function].return_type;
                        let receiver = self.coerce_method_receiver(receiver_lowered, lookup_type);
                        let args = self.arena.alloc_slice_copy(&[receiver]);
                        return Ok(self.alloc(
                            ExpressionKind::IntrinsicCall { intrinsic, args },
                            return_type,
                            *span,
                        ));
                    }

                    let signature = self.scope.signatures[function].clone();
                    assert!(
                        signature.receiver_type().is_some(),
                        "method call resolved to a free function"
                    );

                    if signature.receiver_mutable()
                        && base_local.filter(|&id| self[id].mutable).is_none()
                    {
                        let name = base_local.map_or("temporary", |id| {
                            self.arena.alloc_str(self.symbols.get(self[id].name))
                        });

                        return Err(hir_error!(*span, ImmutableBind { name }));
                    }

                    let explicit_params = signature.explicit_params();
                    if explicit_params.len() != args.len() {
                        return Err(hir_error!(
                            *span,
                            ArityMismatch {
                                name: method_name,
                                expected: explicit_params.len(),
                                found: args.len()
                            }
                        ));
                    }

                    let lowered_args = args
                        .iter()
                        .zip(explicit_params.iter())
                        .map(|(expr, &param_type)| -> Result<&'hir Expression<'hir>, HirError> {
                            let expr = self.lower_expr(expr, Some(param_type))?;
                            self.assert_type(param_type, expr.typ, expr.span)?;
                            Ok(expr.expr)
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    let lowered_args = self.arena.alloc_slice_copy(&lowered_args);
                    let return_type = signature.return_type;
                    let substs = self.resolve_turbofish(type_args)?;

                    let lowered = self.alloc(
                        ExpressionKind::MethodCall {
                            name: method_symbol,
                            receiver: receiver_lowered.expr,
                            args: lowered_args,
                        },
                        return_type,
                        *span,
                    );
                    self.typeck
                        .type_dependent_defs
                        .insert(lowered.expr.id, Res::Function(function));
                    if !substs.is_empty() {
                        self.typeck.node_args.insert(lowered.expr.id, substs);
                    }

                    return Ok(lowered);
                }

                let function_id = match callee.as_ref() {
                    Expr::Identifier(name, _) => self
                        .scope
                        .resolve_function_call(None, name, self.symbols)
                        .ok_or_else(|| hir_error!(*span, UnknownFunction { name }))?,

                    other => {
                        let name = self.arena.alloc_str(&format!("{other:?}"));
                        return Err(hir_error!(*span, UnknownFunction { name }));
                    },
                };

                match self.scope.generic_fns.contains_key(&function_id) {
                    true => self.lower_generic_call(function_id, args, type_args, *span),
                    false => self.lower_direct_call(function_id, args, type_args, *span),
                }
            },

            Expr::QualifiedCall { qualifier, name, args, span, type_args } => {
                // `Enum::Variant(payload)` constructs a tagged-union value
                if let Some(lowered) =
                    self.lower_variant(qualifier, name, args, type_args, hint, *span)?
                {
                    return Ok(lowered);
                }

                let id = self
                    .scope
                    .resolve_function_call(Some(qualifier), name, self.symbols)
                    .ok_or_else(|| {
                        let name = qualified(self.arena, qualifier, name);
                        hir_error!(*span, UnknownFunction { name })
                    })?;

                match self.scope.generic_fns.contains_key(&id) {
                    true => self.lower_generic_call(id, args, type_args, *span),
                    false => self.lower_direct_call(id, args, type_args, *span),
                }
            },

            Expr::TypeIntrinsic { kind, qualifier, typ, span } => {
                let name: &str = kind.into();
                let exists =
                    self.scope.resolve_function_call(*qualifier, name, self.symbols).is_some();

                if !exists {
                    let name = qualifier.map_or(name, |q| qualified(self.arena, q, name));
                    return Err(hir_error!(*span, UnknownFunction { name }));
                }

                let ctx = type_resolver::ResolveCtx::root(
                    self.symbols,
                    &self.scope.struct_map,
                    &self.scope.enum_map,
                    &self.scope.arrays,
                );
                let typ = resolve_annotation(&ctx, &typ.value(), typ.span())?;

                Ok(self.alloc(
                    ExpressionKind::TypeIntrinsic { kind: *kind, typ },
                    TypeKind::Uptr.into(),
                    *span,
                ))
            },
        }
    }

    fn lower_match(
        &mut self,
        match_stmt: &statement::Match<'src>,
        hint: Option<Type>,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let scrutinee = self.lower_expr(&match_stmt.scrutinee, None)?;
        let mut arms = Vec::with_capacity(match_stmt.arms.len());

        let mut unified_type = hint;

        for arm in &match_stmt.arms {
            self.push_scope();

            let pattern =
                self.lower_pattern(scrutinee.typ, arm.pattern.value_ref(), arm.pattern.span())?;
            let pattern = self.arena.alloc(pattern);

            let guard = arm.guard.as_ref().map(|g| self.lower_expr(g, None)).transpose()?;
            if let Some(ref g) = guard {
                self.assert_type(TypeKind::Bool, g.typ, g.span)?;
            }

            let body = self.lower_expr(&arm.body, unified_type)?;
            match unified_type {
                Some(expected) if !body.typ.diverges() => {
                    self.assert_type(expected, body.typ, body.span)?
                },
                _ => unified_type = Some(body.typ),
            }

            self.pop_scope();

            arms.push(Arm {
                pattern,
                guard: guard.map(|g| g.expr),
                body: body.expr,
                span: arm.span,
            });
        }

        let return_type = unified_type.unwrap_or(TypeKind::Unit.into());
        let arms = self.arena.alloc_slice_copy(&arms);

        Ok(self.alloc(
            ExpressionKind::Match { scrutinee: scrutinee.expr, arms },
            return_type,
            match_stmt.span,
        ))
    }

    fn lower_pattern(
        &mut self,
        scrutinee_type: Type,
        pattern: &statement::Pattern<'src>,
        span: Span,
    ) -> Result<Pattern<'hir>, HirError<'hir>> {
        match pattern {
            statement::Pattern::Wildcard => Ok(Pattern { kind: PatternKind::Wildcard, span }),

            statement::Pattern::Literal(lit) => {
                let kind = match lit {
                    PatternLit::Int(n) => PatternKind::Literal(Literal::Int(*n)),
                    PatternLit::Float(f) => PatternKind::Literal(Literal::Float(*f)),
                    PatternLit::Bool(b) => PatternKind::Literal(Literal::Bool(*b)),
                    PatternLit::Char(c) => PatternKind::Literal(Literal::Char(*c)),
                };
                Ok(Pattern { kind, span })
            },

            statement::Pattern::Or(alts) => {
                let lowered: Vec<Pattern<'hir>> = alts
                    .iter()
                    .map(|alt| self.lower_pattern(scrutinee_type, alt.value_ref(), alt.span()))
                    .collect::<Result<_, _>>()?;
                let slice = self.arena.alloc_slice_copy(&lowered);
                Ok(Pattern { kind: PatternKind::Or(slice), span })
            },

            statement::Pattern::Ident(name) => {
                if let TypeKind::Enum(_) = scrutinee_type.kind() {
                    let id = self.enum_type(scrutinee_type, span)?;
                    let enum_def = &self.scope[id];

                    if let Some(idx) =
                        enum_def.variants.iter().position(|v| self.symbols.get(v.name) == *name)
                    {
                        let variant = enum_def.variants[idx];
                        if variant.payload.is_some() {
                            return Err(hir_error!(
                                span,
                                TypeMismatch {
                                    expected: scrutinee_type,
                                    found: TypeKind::Unit.into(),
                                }
                            ));
                        }

                        return Ok(Pattern {
                            kind: PatternKind::Variant { id, variant_idx: idx, sub: None },
                            span,
                        });
                    }
                }

                let symbol = self.symbols.insert(name);
                let local_id = self.declare_local(symbol, scrutinee_type, false, span)?;
                Ok(Pattern { kind: PatternKind::Binding(local_id), span })
            },

            statement::Pattern::Variant { qualifier, name, sub } => {
                let id = self.enum_type(scrutinee_type, span)?;
                let enum_def = &self.scope[id];

                if let Some(qualifier) = qualifier {
                    // the scrutinee type pins the concrete enum `id`; the qualifier
                    // must name either that concrete enum or — for a generic enum —
                    // its template base (`Optional` for `Optional$Ordering`)
                    let enum_symbol = self.symbols.insert(qualifier);
                    let matches = self.scope.enum_map.get(&enum_symbol).copied() == Some(id)
                        || self.scope.generic_enums.contains_key(&enum_symbol);
                    if !matches {
                        let name = qualified(self.arena, qualifier, name);
                        return Err(hir_error!(span, UnknownType { name }));
                    }
                }

                let variant_idx = enum_def
                    .variants
                    .iter()
                    .position(|v| self.symbols.get(v.name) == *name)
                    .ok_or_else(|| hir_error!(span, UnknownType { name }))?;

                let variant = enum_def.variants[variant_idx];

                let sub = match (sub, variant.payload) {
                    (Some(pat), Some(payload)) => {
                        let lowered = self.lower_pattern(payload, pat.value_ref(), pat.span())?;
                        Some(&*self.arena.alloc(lowered))
                    },
                    (None, None) => None,
                    _ => {
                        return Err(hir_error!(
                            span,
                            TypeMismatch { expected: scrutinee_type, found: TypeKind::Unit.into() }
                        ));
                    },
                };

                Ok(Pattern { kind: PatternKind::Variant { id, variant_idx, sub }, span })
            },
        }
    }

    fn lower_if(
        &mut self,
        if_stmt: &statement::If<'src>,
        is_tail: bool,
    ) -> Result<(Statement<'hir>, bool), HirError<'hir>> {
        let condition = self.lower_expr(&if_stmt.condition, None)?;
        self.assert_type(TypeKind::Bool, condition.typ, condition.span)?;
        let condition = condition.expr;

        let (then_block, then_returns) = self.lower_block(&if_stmt.then_branch, is_tail)?;
        let (else_block, else_returns) = if_stmt
            .else_branch
            .as_ref()
            .map(|else_branch| -> Result<_, HirError> {
                match else_branch.as_ref() {
                    Else::If(block) => {
                        let (statement, returns) = self.lower_if(block, is_tail)?;
                        let statements = self.arena.alloc_slice_copy(&[statement]);
                        let block = Block { span: block.span, statements };

                        Ok((Some(block), returns))
                    },

                    Else::Block(block) => {
                        let (block, returns) = self.lower_block(block, is_tail)?;
                        Ok((Some(block), returns))
                    },

                    Else::Expr(expr) => {
                        let tail_ret = is_tail && self.return_type.kind() != TypeKind::Unit;
                        let hint = tail_ret.then_some(self.return_type);
                        let lowered = self.lower_expr(expr, hint)?;
                        let span = lowered.span;

                        let stmt = match tail_ret {
                            true => {
                                self.assert_type(self.return_type, lowered.typ, lowered.span)?;
                                Statement::Return(Some(lowered.expr))
                            },
                            _ => Statement::Expr(lowered.expr),
                        };

                        let statements = self.arena.alloc_slice_copy(&[stmt]);
                        let block = Block { statements, span };
                        Ok((Some(block), tail_ret))
                    },
                }
            })
            .transpose()?
            .unwrap_or((None, false));

        Ok((
            Statement::If { condition, then_block, else_block },
            then_returns && else_returns,
        ))
    }

    fn lower_direct_call(
        &mut self,
        function_id: FunctionId,
        args: &[expression::Expression<'src>],
        type_args: &[Spanned<statement::Type<'src>>],
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let signature = self.scope.signatures[function_id].clone();
        let intrinsic = signature.kind.intrinsic();

        if self.is_const && !signature.is_const && intrinsic.is_none() {
            let name = self.arena.alloc_str(self.symbols.get(signature.name));
            return Err(hir_error!(
                span,
                ConstFnViolation(ConstFnViolationKind::NonConstCall { name })
            ));
        }

        if intrinsic == Some(Intrinsic::Syscall) {
            return self.lower_syscall(args, signature.return_type, span);
        }

        if intrinsic.is_none() && signature.params.len() != args.len() {
            let name = self.arena.alloc_str(self.symbols.get(signature.name));
            return Err(hir_error!(
                span,
                ArityMismatch { name, expected: signature.params.len(), found: args.len() }
            ));
        }

        let mut lowered_args = Vec::with_capacity(args.len());
        match intrinsic {
            Some(_) => {
                for arg in args {
                    let arg = self.lower_expr(arg, None)?;
                    lowered_args.push(arg.expr);
                }
            },
            _ => {
                for (expr, &param_type) in args.iter().zip(signature.params.iter()) {
                    let expr = self.lower_expr(expr, Some(param_type))?;
                    self.assert_type(param_type, expr.typ, expr.span)?;
                    lowered_args.push(expr.expr);
                }
            },
        }

        let lowered_args = self.arena.alloc_slice_copy(&lowered_args);
        let (callee_name, return_type) = (signature.name, signature.return_type);

        if let Some(intrinsic) = intrinsic {
            let kind = ExpressionKind::IntrinsicCall { intrinsic, args: lowered_args };
            return Ok(self.alloc(kind, return_type, span));
        }

        let callee = self.alloc(ExpressionKind::Path(callee_name), Type::default(), span).expr;
        let substs = self.resolve_turbofish(type_args)?;
        let lowered =
            self.alloc(ExpressionKind::Call { callee, args: lowered_args }, return_type, span);

        self.typeck
            .type_dependent_defs
            .insert(lowered.expr.id, Res::Function(function_id));
        if !substs.is_empty() {
            self.typeck.node_args.insert(lowered.expr.id, substs);
        }

        Ok(lowered)
    }

    /// Resolve explicit turbofish `type_args` to concrete HIR types
    fn resolve_turbofish(
        &self,
        type_args: &[Spanned<statement::Type<'src>>],
    ) -> Result<Vec<Type>, HirError<'hir>> {
        if type_args.is_empty() {
            return Ok(Vec::new());
        }

        let mut ctx = type_resolver::ResolveCtx::root(
            self.symbols,
            &self.scope.struct_map,
            &self.scope.enum_map,
            &self.scope.arrays,
        );
        if let Some(typ) = self.self_type {
            ctx = ctx.with_self(typ);
        }

        type_args
            .iter()
            .map(|t| resolve_annotation(&ctx, &t.value(), t.span()))
            .collect()
    }

    /// Try to lower `Qualifier::Name(args)` as an enum variant constructor. Returns
    /// `None` if `Qualifier` is not an enum (so the caller falls back to a function
    /// call). For a generic enum, the concrete instantiation comes from `hint`.
    fn lower_variant(
        &mut self,
        qualifier: &str,
        name: &'src str,
        args: &[expression::Expression<'src>],
        type_args: &[Spanned<statement::Type<'src>>],
        hint: Option<Type>,
        span: Span,
    ) -> Result<Option<Lowered<'hir>>, HirError<'hir>> {
        let qualifier_symbol = self.symbols.insert(qualifier);

        let Some(enum_id) =
            self.resolve_enum_id(qualifier, qualifier_symbol, type_args, hint, span)?
        else {
            return Ok(None);
        };

        let variant_symbol = self.symbols.insert(name);
        let enum_def = &self.scope.enums[enum_id];
        let Some(index) = enum_def.variants.iter().position(|v| v.name == variant_symbol) else {
            return Ok(None);
        };

        let payload_typ = enum_def.variants[index].payload;
        let payload = self.lower_variant_payload(name, payload_typ, args, span)?;

        let callee = self.alloc(ExpressionKind::Path(variant_symbol), Type::default(), span).expr;
        let arguments: &[&Expression] = payload.map_or(&[], |p| self.arena.alloc_slice_copy(&[p]));

        let typ = Type::enumerable(enum_id);
        let lowered = self.alloc(ExpressionKind::Call { callee, args: arguments }, typ, span);
        self.typeck
            .type_dependent_defs
            .insert(lowered.expr.id, Res::Variant { id: enum_id, index });

        Ok(Some(lowered))
    }

    fn lower_generic_call(
        &mut self,
        function_id: FunctionId,
        args: &[expression::Expression<'src>],
        type_args: &[Spanned<statement::Type<'src>>],
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let (callee_name, arity, open_return) = {
            let signature = &self.scope.signatures[function_id];
            (signature.name, signature.params.len(), signature.return_type)
        };
        let generic_count = self.scope.generic_fns[&function_id].generics.len();

        if arity != args.len() {
            let name = self.arena.alloc_str(self.symbols.get(callee_name));
            return Err(hir_error!(
                span,
                ArityMismatch { name, expected: arity, found: args.len() }
            ));
        }

        let mut lowered_args = Vec::with_capacity(args.len());
        let mut arg_types = Vec::with_capacity(args.len());
        for arg in args {
            let lowered = self.lower_expr(arg, None)?;
            arg_types.push(lowered.typ);
            lowered_args.push(lowered.expr);
        }
        let lowered_args = self.arena.alloc_slice_copy(&lowered_args);

        let substs = match type_args.is_empty() {
            false => self.resolve_turbofish(type_args)?,
            true => {
                let open_params = &self.scope.signatures[function_id].params;
                infer_type_args(open_params, &arg_types, generic_count)
            },
        };

        let template = &self.scope.generic_fns[&function_id];
        self.check_bounds(&template.generics, &substs, span)?;

        let return_type = open_return.subst(&substs);
        let callee = self.alloc(ExpressionKind::Path(callee_name), Type::default(), span).expr;
        let lowered =
            self.alloc(ExpressionKind::Call { callee, args: lowered_args }, return_type, span);

        self.typeck
            .type_dependent_defs
            .insert(lowered.expr.id, Res::Function(function_id));
        self.typeck.node_args.insert(lowered.expr.id, substs);

        Ok(lowered)
    }

    fn lower_syscall(
        &mut self,
        args: &[expression::Expression<'src>],
        return_type: Type,
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        if !self.in_std {
            return Err(hir_error!(span, UnknownFunction { name: "syscall" }));
        }

        let Some((code_arg, value_args)) = args.split_first() else {
            return Err(hir_error!(span, ArityMismatch { name: "syscall", expected: 1, found: 0 }));
        };

        if value_args.len() > 6 {
            return Err(hir_error!(
                span,
                ArityMismatch { name: "syscall", expected: 7, found: args.len() }
            ));
        }

        let expression::Expression::Identifier(name, code_span) = code_arg else {
            let name = self.arena.alloc_str(&format!("{code_arg:?}"));
            return Err(hir_error!(code_arg.span(), UndeclaredIdentifier { name }));
        };

        let code = SyscallCode::from_str(name)
            .map_err(|_| hir_error!(*code_span, UndeclaredIdentifier { name }))?;

        let args = value_args
            .iter()
            .map(|arg| self.lower_expr(arg, None).map(|lowered| lowered.expr))
            .collect::<Result<Vec<_>, _>>()?;

        let return_type = match code {
            SyscallCode::Exit => TypeKind::Never.into(),
            _ => return_type,
        };

        let args = self.arena.alloc_slice_copy(&args);
        Ok(self.alloc(ExpressionKind::Syscall { code, args }, return_type, span))
    }

    fn lower_variant_payload(
        &mut self,
        name: &'src str,
        typ: Option<Type>,
        args: &[expression::Expression<'src>],
        span: Span,
    ) -> Result<Option<&'hir Expression<'hir>>, HirError<'hir>> {
        match (typ, args.first()) {
            (Some(expected), Some(arg)) => {
                let lowered = self.lower_expr(arg, Some(expected))?;
                self.assert_type(expected, lowered.typ, lowered.span)?;
                Ok(Some(lowered.expr))
            },
            (None, None) => Ok(None),
            (Some(_), None) => Err(hir_error!(span, ArityMismatch { name, expected: 1, found: 0 })),
            (None, Some(_)) => {
                Err(hir_error!(span, ArityMismatch { name, expected: 0, found: args.len() }))
            },
        }
    }

    #[inline]
    fn enum_type(&self, typ: Type, span: Span) -> Result<EnumId, HirError<'hir>> {
        let default = TypeKind::Enum(Default::default());

        match typ.kind() {
            TypeKind::Enum(id) => Ok(id),
            TypeKind::Ref { to, .. } => match to.kind() {
                TypeKind::Enum(id) => Ok(id),
                _ => {
                    Err(hir_error!(span, TypeMismatch { expected: Type::new(default), found: typ }))
                },
            },
            _ => Err(hir_error!(span, TypeMismatch { expected: Type::new(default), found: typ })),
        }
    }

    fn type_for_binary(
        &mut self,
        operator: &BinaryOperator,
        left: Type,
        right: Type,
        span: Span,
    ) -> Result<Type, HirError<'hir>> {
        let type_mismatch =
            |found| hir_error!(span, TypeMismatch { expected: TypeKind::I32.into(), found });

        match operator {
            BinaryOperator::Add
            | BinaryOperator::Sub
            | BinaryOperator::Mul
            | BinaryOperator::Div => {
                self.assert_type(left, right, span)?;
                let left = self.infer.resolve_shallow(left);
                match left.is_number() || left.is_infer() {
                    true => Ok(left),
                    _ => Err(type_mismatch(left)),
                }
            },

            BinaryOperator::Eq | BinaryOperator::Ne => {
                self.assert_type(left, right, span)?;
                Ok(TypeKind::Bool.into())
            },

            BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq => {
                self.assert_type(left, right, span)?;
                let left = self.infer.resolve_shallow(left);
                match left.is_number() || left.is_infer() || left == TypeKind::Char.into() {
                    true => Ok(TypeKind::Bool.into()),
                    _ => Err(type_mismatch(left)),
                }
            },

            BinaryOperator::And | BinaryOperator::Or => {
                self.assert_type(TypeKind::Bool, left, span)?;
                self.assert_type(TypeKind::Bool, right, span)?;

                Ok(TypeKind::Bool.into())
            },

            BinaryOperator::BitAnd | BinaryOperator::BitOr | BinaryOperator::BitXor => {
                self.assert_type(left, right, span)?;
                let left = self.infer.resolve_shallow(left);
                match left == TypeKind::Bool.into() || left.is_integer() || left.is_infer() {
                    true => Ok(left),
                    _ => Err(type_mismatch(left)),
                }
            },

            BinaryOperator::Shl | BinaryOperator::Shr => {
                let left = self.infer.resolve_shallow(left);
                let right = self.infer.resolve_shallow(right);
                if !left.is_integer() && !left.is_infer() {
                    return Err(type_mismatch(left));
                }

                if !right.is_integer() && !right.is_infer() {
                    return Err(type_mismatch(right));
                }

                Ok(left)
            },
        }
    }

    #[inline(always)]
    fn infer(&mut self, expr: &expression::Expression<'src>) -> Result<Type, HirError<'hir>> {
        let expr = self.lower_expr(expr, None)?;
        Ok(expr.typ)
    }

    #[inline(always)]
    fn resolve_type(
        &mut self,
        typ: &statement::Type<'src>,
        span: Span,
    ) -> Result<Type, HirError<'hir>> {
        match typ {
            statement::Type::Named(name) => {
                if let Some(&typ) = self.generic_env.get(*name) {
                    return Ok(typ);
                }

                let symbol = self.symbols.insert(name);
                self.scope
                    .nominal_type(symbol)
                    .ok_or_else(|| hir_error!(span, UnknownType { name }))
            },

            statement::Type::SelfType => {
                self.self_type.ok_or_else(|| hir_error!(span, UnknownType { name: "Self" }))
            },

            statement::Type::RefSelf => {
                let self_ty =
                    self.self_type.ok_or_else(|| hir_error!(span, UnknownType { name: "Self" }))?;
                let to = RefTarget::try_from(self_ty).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch {
                            expected: Type::structure(Default::default()),
                            found: self_ty
                        }
                    )
                })?;

                Ok(Type::refer(to, false))
            },

            statement::Type::Ref(inner, mutable) => {
                let inner_type = self.resolve_type(inner, span)?;
                let to = RefTarget::try_from(inner_type).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch {
                            expected: Type::structure(Default::default()),
                            found: inner_type
                        }
                    )
                })?;
                Ok(Type::refer(to, *mutable))
            },

            statement::Type::Generic(name, args) => {
                let mut resolved = Vec::with_capacity(args.len());
                for arg in args {
                    resolved.push(self.resolve_type(arg.value_ref(), arg.span())?);
                }
                let typ = self.scope.instantiate_generic(name, &resolved, span, self.symbols)?;
                if let Some(struct_sym) = self.symbols.get_id(name) {
                    if let Some(template) = self.scope.generic_structs.get(&struct_sym) {
                        self.check_bounds(&template.generics, &resolved, span)?;
                    } else if let Some(template) = self.scope.generic_enums.get(&struct_sym) {
                        self.check_bounds(&template.generics, &resolved, span)?;
                    }
                }
                Ok(typ)
            },

            statement::Type::Array(element, len) => {
                let element = self.resolve_type(element, span)?;
                let id = self.scope.arrays.intern(element, *len as u32);
                Ok(Type::array(id))
            },

            statement::Type::Slice(element, mutable) => {
                let element = self.resolve_type(element, span)?;
                let element = RefTarget::try_from(element).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch {
                            expected: Type::structure(Default::default()),
                            found: element
                        }
                    )
                })?;
                Ok(Type::slice(element, *mutable))
            },

            other => Type::from_primitive_ast(other)
                .ok_or_else(|| hir_error!(span, UnknownType { name: "<unsupported type>" })),
        }
    }

    #[inline(always)]
    fn assert_type(
        &mut self,
        expected: impl Into<Type>,
        found: impl Into<Type>,
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        let (expected, found) = (expected.into(), found.into());
        // a poison type is compatible with everything, so it never cascades
        if expected.is_error() || found.is_error() || expected == found {
            return Ok(());
        }

        if expected.is_infer() || found.is_infer() {
            return match self.infer.unify(expected, found) {
                Ok(()) => Ok(()),
                Err(()) => {
                    let expected = self.infer.resolve_or_default(expected);
                    let found = self.infer.resolve_or_default(found);
                    self.soft(hir_error!(span, TypeMismatch { expected, found }))
                },
            };
        }

        // two arrays of the same length agree when their elements do; this lets a
        // let-init's twice-lowered literal unify the inference variables behind its
        // distinct interned ids (see `Stmt::Let` lowering and `Expr::Array`)
        if let (TypeKind::Array(expected), TypeKind::Array(found)) = (expected.kind(), found.kind())
        {
            let (lhs, rhs) = (self.scope.arrays.get(expected), self.scope.arrays.get(found));
            if lhs.len == rhs.len {
                return self.assert_type(lhs.element, rhs.element, span);
            }
        }

        self.soft(hir_error!(span, TypeMismatch { expected, found }))
    }

    #[inline(always)]
    fn poison(&mut self, error: HirError<'hir>) -> Result<Type, HirError<'hir>> {
        self.scope.poison(error)
    }

    #[inline(always)]
    fn soft(&mut self, error: HirError<'hir>) -> Result<(), HirError<'hir>> {
        self.scope.soft(error)
    }

    fn declare_local(
        &mut self,
        name: SymbolId,
        typ: Type,
        mutable: bool,
        decl_span: Span,
    ) -> Result<LocalId, HirError<'hir>> {
        let scope = self.scopes.last_mut().expect("at least one scope is always present");

        if scope.contains_key(&name) {
            let name = self.arena.alloc_str(self.symbols.get(name));
            return Err(hir_error!(decl_span, DuplicateBind { name }));
        }

        let id = LocalId(self.next_local);
        self.next_local += 1;

        scope.insert(name, id);
        self.locals.push(Local { id, name, typ, mutable, decl_span });

        Ok(id)
    }

    #[inline(always)]
    fn resolve_local(&mut self, name: SymbolId, span: Span) -> Result<LocalId, HirError<'hir>> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name).copied())
            .ok_or_else(|| {
                let name = self.arena.alloc_str(self.symbols.get(name));
                hir_error!(span, UndeclaredIdentifier { name })
            })
    }

    fn resolve_enum_id(
        &mut self,
        qualifier: &str,
        symbol: SymbolId,
        type_args: &[Spanned<statement::Type<'src>>],
        hint: Option<Type>,
        span: Span,
    ) -> Result<Option<EnumId>, HirError<'hir>> {
        // standard non-generic enum
        if let Some(&id) = self.scope.enum_map.get(&symbol) {
            return Ok(Some(id));
        }

        // generic enum template
        if self.scope.generic_enums.contains_key(&symbol) {
            // explicit generic arguments passed Enum::<Int>::Variant
            if !type_args.is_empty() {
                let mut resolved = Vec::with_capacity(type_args.len());
                for arg in type_args {
                    resolved.push(self.resolve_type(arg.value_ref(), arg.span())?);
                }

                if let Some(template) = self.scope.generic_enums.get(&symbol) {
                    self.check_bounds(&template.generics, &resolved, span)?;
                }

                let typ =
                    self.scope.instantiate_generic(qualifier, &resolved, span, self.symbols)?;

                if let TypeKind::Enum(id) = typ.kind() {
                    return Ok(Some(id));
                }
            }
        }
        // type infered via type hint: let x: Enum<Int> = Variant;
        if let Some(TypeKind::Enum(id)) = hint.map(|t| t.kind()) {
            return Ok(Some(id));
        }

        Ok(None)
    }

    // TODO: this in the future will be replaced by some kind of
    // Deref equivalent we find more convienient
    // that would be much more important when we introduce more ds and iterables
    fn coerce_array_to_slice(&mut self, operand: Type, hint: Option<Type>) -> Option<Type> {
        let slice = hint?;
        let TypeKind::Slice { element, .. } = slice.kind() else {
            return None;
        };
        let TypeKind::Array(id) = operand.kind() else {
            return None;
        };

        let array_element = self.scope.arrays.get(id).element;
        // unifying pins an inferred element to the slice's element, so `&[0; 3]`
        // in `&[uptr]` context yields a `[uptr; 3]` array
        self.infer.unify(array_element, element.into()).ok().map(|()| slice)
    }

    fn coerce_method_receiver(
        &mut self,
        receiver: Lowered<'hir>,
        lookup_type: Type,
    ) -> &'hir Expression<'hir> {
        let TypeKind::Array(id) = receiver.typ.kind() else {
            return receiver.expr;
        };
        let TypeKind::Slice { element, .. } = lookup_type.kind() else {
            return receiver.expr;
        };

        let array_element = self.infer.resolve_shallow(self.scope.arrays.get(id).element);
        if !array_element.is_infer() && array_element != Type::from(element) {
            return receiver.expr;
        }

        self.alloc(
            ExpressionKind::Unary { operator: UnaryOperator::Ref, expr: receiver.expr },
            lookup_type,
            receiver.span,
        )
        .expr
    }

    /// The element type of an indexable type: an array, a slice, or a reference to either.
    fn element_type(&self, typ: Type) -> Option<Type> {
        match typ.kind() {
            TypeKind::Array(id) => Some(self.scope.arrays.get(id).element),
            TypeKind::Slice { element, .. } => Some(element.into()),
            TypeKind::Ref { to, .. } => self.element_type(to.into()),
            _ => None,
        }
    }

    fn lookup_field(
        &mut self,
        current: Type,
        name: &str,
        span: Span,
    ) -> Result<(SymbolId, Type), HirError<'hir>> {
        #[rustfmt::skip]
        let sid = match current.kind() {
            TypeKind::Struct(id) => id,
            TypeKind::Ref { to, .. } => match to.kind() {
                TypeKind::Struct(id) => id,
                _ => return Err(hir_error!(span, TypeMismatch {
                    expected: Type::structure(Default::default()),
                    found: current
                })),
            },
            _ => return Err(hir_error!(span, TypeMismatch {
                expected: Type::structure(Default::default()),
                found: current
            })),
        };

        let sym = self.symbols.insert(name);
        let def = &self.scope[sid];
        let struct_name = self.arena.alloc_str(self.symbols.get(def.name));

        let field = def.fields.iter().find(|field| field.name == sym).ok_or_else(|| {
            let field = self.arena.alloc_str(name);
            hir_error!(span, UnknownField { struct_name, field })
        })?;

        Ok((sym, field.typ))
    }

    fn check_bounds(
        &self,
        generics: &[statement::GenericBound<'src>],
        args: &[Type],
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        let current_generics = self.function.map(|f| f.generics.as_slice()).unwrap_or(&[]);
        for (i, param) in generics.iter().enumerate() {
            let concrete_type = args[i];
            for bound in &param.bounds {
                let interface_name = match bound.value_ref() {
                    statement::Type::Named(name) => name,
                    statement::Type::Generic(name, _) => name,
                    _ => continue,
                };

                let satisfied = match concrete_type.kind() {
                    TypeKind::GenericParam(idx) => {
                        current_generics.get(idx as usize).is_some_and(|param_bound| {
                            param_bound.bounds.iter().any(|b| {
                                let name = match b.value_ref() {
                                    statement::Type::Named(n) => n,
                                    statement::Type::Generic(n, _) => n,
                                    _ => "",
                                };
                                name == *interface_name
                            })
                        })
                    },
                    _ => self.symbols.get_id(interface_name).is_some_and(|interface_sym| {
                        self.scope.interface_impls.contains(&(concrete_type, interface_sym))
                    }),
                };

                if !satisfied {
                    return Err(hir_error!(
                        span,
                        UnsatisfiedBound { type_name: concrete_type, bound_name: interface_name }
                    ));
                }
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new())
    }

    #[inline(always)]
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
}

impl BinaryOperator {
    #[inline(always)]
    const fn overload_method<'op>(&self) -> Option<&'op str> {
        Some(match self {
            BinaryOperator::Eq => "eq",
            BinaryOperator::Ne => "ne",
            BinaryOperator::Lt => "lt",
            BinaryOperator::LtEq => "le",
            BinaryOperator::Gt => "gt",
            BinaryOperator::GtEq => "ge",
            _ => return None,
        })
    }

    #[inline(always)]
    const fn required_interface(&self) -> CmpInterface {
        match self {
            BinaryOperator::Eq | BinaryOperator::Ne => CmpInterface::Equality,
            BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq => CmpInterface::Ordering,
            _ => unsafe { std::hint::unreachable_unchecked() },
        }
    }

    #[inline(always)]
    const fn symbol<'op>(&self) -> &'op str {
        match self {
            BinaryOperator::Eq => "==",
            BinaryOperator::Ne => "!=",
            BinaryOperator::Lt => "<",
            BinaryOperator::LtEq => "<=",
            BinaryOperator::Gt => ">",
            BinaryOperator::GtEq => ">=",
            _ => unsafe { std::hint::unreachable_unchecked() },
        }
    }
}

fn declared_fn_names(env: &GenericEnv, symbols: &mut SymbolTable) -> Vec<SymbolId> {
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
        false => Vec::new(),
    }
}

fn infer_type_args(open_params: &[Type], arg_types: &[Type], count: usize) -> Vec<Type> {
    let mut bindings = vec![None; count];
    for (&param, &actual) in open_params.iter().zip(arg_types) {
        unify_generic(param, actual, &mut bindings);
    }
    bindings.into_iter().map(Option::unwrap_or_default).collect()
}

fn unify_generic(param: Type, actual: Type, bindings: &mut [Option<Type>]) {
    let slot = match param.kind() {
        TypeKind::GenericParam(i) => bindings.get_mut(i as usize).map(|slot| (slot, actual)),
        TypeKind::Ref { to, .. } => match to.kind() {
            TypeKind::GenericParam(i) => {
                bindings.get_mut(i as usize).map(|slot| (slot, actual.strip_reference()))
            },
            _ => None,
        },
        _ => None,
    };

    if let Some((slot, value)) = slot {
        slot.get_or_insert(value);
    }
}

pub(in crate::hir) fn lower_const<'hir, 'src>(
    scope: &mut Scope<'hir>,
    symbols: &mut SymbolTable,
    expr: &expression::Expression<'src>,
    expected_type: Type,
    in_std: bool,
    arena: &'hir bumpalo::Bump,
) -> Result<(&'hir Expression<'hir>, TypeckResults), HirError<'hir>>
where
    'src: 'hir,
{
    let mut builder = FunctionBuilder::new_for_const(scope, symbols, in_std, arena);
    let lowered = builder.lower_expr(expr, Some(expected_type))?;

    builder.assert_type(expected_type, lowered.typ, lowered.span)?;
    builder.resolve_inference();

    Ok((lowered.expr, builder.typeck))
}

impl<'s, 'f, 'hir, 'src> Index<LocalId> for FunctionBuilder<'s, 'f, 'hir, 'src> {
    type Output = Local;
    fn index(&self, index: LocalId) -> &Self::Output {
        &self.locals[index]
    }
}

impl<'s, 'f, 'hir, 'src> Index<StructId> for FunctionBuilder<'s, 'f, 'hir, 'src> {
    type Output = Struct;
    fn index(&self, index: StructId) -> &Self::Output {
        &self.scope[index]
    }
}
