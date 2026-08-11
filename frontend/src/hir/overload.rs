//! Resolution and canonicalisation of source-level operator overloads

use crate::{
    hir::{
        Arm, Block, Expression, ExpressionKind, FunctionId, LoopKind, Res, Statement, SymbolId,
        Type, TypeKind,
        error::{CmpInterface, ConstFnViolationKind, HirError, hir_error},
        lower::{FunctionBuilder, Lowered},
    },
    lexer::token::Span,
    parser::expression::{self, BinaryOperator, UnaryOperator},
};

#[derive(Clone, Copy)]
struct IndexMethod {
    function: FunctionId,
    index: Type,
    output: Type,
}

impl<'s, 'f, 'hir, 'src> FunctionBuilder<'s, 'f, 'hir, 'src>
where
    'src: 'hir,
{
    pub(super) fn lower_overloaded_comparison(
        &mut self,
        operator: BinaryOperator,
        left: Lowered<'hir>,
        right: Lowered<'hir>,
        span: Span,
    ) -> Result<Option<Lowered<'hir>>, HirError<'hir>> {
        let Some(method) = operator.overload_method() else {
            return Ok(None);
        };
        let receiver = left.typ.strip_reference();
        if !matches!(receiver.kind(), TypeKind::Struct(_) | TypeKind::Enum(_)) {
            return Ok(None);
        }

        let method = self.scope.symbols.insert(method);
        let Some(&function) = self.scope.methods.get(&(receiver, method)) else {
            let type_name = match receiver.kind() {
                TypeKind::Struct(id) => self.scope.symbols.get(self.scope[id].name).to_string(),
                TypeKind::Enum(id) => self
                    .scope
                    .enums
                    .get(id)
                    .map(|definition| self.scope.symbols.get(definition.name).to_string())
                    .unwrap_or_else(|| receiver.to_string()),
                _ => unreachable!("comparison receiver must be nominal"),
            };
            let type_name = self.arena.alloc_str(&type_name);
            return Err(hir_error!(
                span,
                OperatorRequiresInterface {
                    op: operator.symbol(),
                    type_name,
                    interface_name: operator.required_interface(),
                }
            ));
        };

        self.check_overload_call(function, span)?;
        let lowered = self.alloc(
            ExpressionKind::Binary { operator, left: left.expr, right: right.expr },
            TypeKind::Bool.into(),
            span,
        );
        self.typeck.type_dependent_defs.insert(lowered.expr.id, Res::Function(function));

        Ok(Some(lowered))
    }

    pub(super) fn lower_index_overload(
        &mut self,
        base: Lowered<'hir>,
        index: &expression::Expression<'src>,
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let method = self.resolve_index_method(base.typ.strip_reference(), false, span)?;
        let index = self.lower_expr(index, Some(method.index))?;
        self.assert_type(method.index, index.typ, index.span)?;

        let lowered = self.alloc(
            ExpressionKind::Index { base: base.expr, index: index.expr },
            method.output,
            span,
        );
        self.typeck
            .type_dependent_defs
            .insert(lowered.expr.id, Res::Function(method.function));
        self.has_index_overloads = true;

        Ok(lowered)
    }

    pub(super) fn make_place_mutable(
        &mut self,
        expr: &'hir Expression<'hir>,
    ) -> Result<(), HirError<'hir>> {
        match expr.kind {
            ExpressionKind::Field { base, .. } => self.make_place_mutable(base),

            ExpressionKind::Index { base, .. } => {
                if self.typeck.type_dependent_def(expr.id).and_then(Res::function).is_some() {
                    let base_type = self.typeck.type_of(base.id);
                    if matches!(
                        base_type.kind(),
                        TypeKind::Ref { mutable: false, .. } | TypeKind::Raw { mutable: false, .. }
                    ) {
                        return Err(hir_error!(expr.span, AssignBehindSharedRef));
                    }

                    let method =
                        self.resolve_index_method(base_type.strip_reference(), true, expr.span)?;
                    assert_eq!(
                        method.output,
                        self.typeck.type_of(expr.id),
                        "Index and IndexMutable must use the same Output type"
                    );
                    self.typeck.type_dependent_defs.insert(expr.id, Res::Function(method.function));
                }

                self.make_place_mutable(base)
            },

            // An explicit pointer dereference starts a new place. Mutating its pointee does not
            // require mutable access to projections used to compute the pointer value.
            ExpressionKind::Unary { operator: UnaryOperator::Deref, .. } => Ok(()),

            _ => Ok(()),
        }
    }

    pub(super) fn normalise_overloads(&mut self, block: Block<'hir>) -> Block<'hir> {
        if !self.has_index_overloads {
            return block;
        }

        let block = self.normalise_block(block);
        self.has_index_overloads = false;
        block
    }

    pub(super) fn normalise_overload(
        &mut self,
        expression: &'hir Expression<'hir>,
    ) -> &'hir Expression<'hir> {
        if !self.has_index_overloads {
            return expression;
        }

        let expression = self.normalise_expression(expression);
        self.has_index_overloads = false;
        expression
    }

    fn resolve_index_method(
        &mut self,
        receiver: Type,
        mutable: bool,
        span: Span,
    ) -> Result<IndexMethod, HirError<'hir>> {
        let (interface_name, method_name) = match mutable {
            true => ("IndexMutable", "index_mut"),
            false => ("Index", "index"),
        };
        let interface = self.scope.symbols.get_id(interface_name);
        let name = self.scope.symbols.insert(method_name);
        let implemented = interface
            .is_some_and(|interface| self.scope.interface_impls.contains(&(receiver, interface)));
        let function = implemented
            .then(|| self.scope.methods.get(&(receiver, name)).copied())
            .flatten();
        let function = match (mutable, function) {
            (_, Some(function)) => function,
            (true, None) => {
                return Err(hir_error!(span, NotMutablyIndexable { typ: receiver }));
            },
            (false, None) => return Err(hir_error!(span, NotIndexable { typ: receiver })),
        };

        let signature = &self.scope.signatures[function];
        let [index] = signature.explicit_params() else {
            panic!("index method must have exactly one explicit parameter")
        };
        let index = *index;
        let output = match signature.return_type.kind() {
            TypeKind::Ref { mutable: found, to } => {
                assert_eq!(found, mutable, "index method returned a reference of wrong mutability");
                to.into()
            },
            _ => panic!("index method must return a reference"),
        };

        self.check_overload_call(function, span)?;
        Ok(IndexMethod { function, index, output })
    }

    fn check_overload_call(
        &mut self,
        function: FunctionId,
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        let signature = &self.scope.signatures[function];
        if self.is_const && !signature.is_const {
            let name = self.arena.alloc_str(self.scope.symbols.get(signature.name));
            return Err(hir_error!(
                span,
                ConstFnViolation(ConstFnViolationKind::NonConstCall { name })
            ));
        }

        self.check_call_safety(function, span)
    }

    fn normalise_block(&mut self, block: Block<'hir>) -> Block<'hir> {
        let statements = block
            .statements
            .iter()
            .map(|&statement| self.normalise_statement(statement))
            .collect::<Vec<_>>();

        Block {
            statements: self.arena.alloc_slice_copy(&statements),
            span: block.span,
        }
    }

    fn normalise_statement(&mut self, statement: Statement<'hir>) -> Statement<'hir> {
        match statement {
            Statement::LetInit { id, init } => {
                Statement::LetInit { id, init: self.normalise_expression(init) }
            },
            Statement::Expr(expression) => Statement::Expr(self.normalise_expression(expression)),
            Statement::Return(Some(expression)) => {
                Statement::Return(Some(self.normalise_expression(expression)))
            },
            Statement::If { condition, then_block, else_block } => Statement::If {
                condition: self.normalise_expression(condition),
                then_block: self.normalise_block(then_block),
                else_block: else_block.map(|block| self.normalise_block(block)),
            },
            Statement::Loop { kind, body } => Statement::Loop {
                kind: self.normalise_loop(kind),
                body: self.normalise_block(body),
            },
            Statement::Block(block) => Statement::Block(self.normalise_block(block)),
            Statement::LetUninit { .. }
            | Statement::Return(None)
            | Statement::Break
            | Statement::Continue => statement,
        }
    }

    fn normalise_loop(&mut self, kind: LoopKind<'hir>) -> LoopKind<'hir> {
        match kind {
            LoopKind::Range { binding, start, end, inclusive } => LoopKind::Range {
                binding,
                start: self.normalise_expression(start),
                end: self.normalise_expression(end),
                inclusive,
            },
            LoopKind::Iterable { binding, iterable } => {
                LoopKind::Iterable { binding, iterable: self.normalise_expression(iterable) }
            },
            LoopKind::Infinite => kind,
        }
    }

    fn normalise_expression(
        &mut self,
        expression: &'hir Expression<'hir>,
    ) -> &'hir Expression<'hir> {
        use ExpressionKind as Kind;

        let kind = match expression.kind {
            Kind::Unary { operator, expr } => {
                Kind::Unary { operator, expr: self.normalise_expression(expr) }
            },
            Kind::Binary { operator, left, right } => Kind::Binary {
                operator,
                left: self.normalise_expression(left),
                right: self.normalise_expression(right),
            },
            Kind::Field { base, field } => {
                Kind::Field { base: self.normalise_expression(base), field }
            },
            Kind::Assign { target, value } => Kind::Assign {
                target: self.normalise_expression(target),
                value: self.normalise_expression(value),
            },
            Kind::Struct { id, fields } => {
                Kind::Struct { id, fields: self.normalise_fields(fields) }
            },
            Kind::Array { elements } => {
                Kind::Array { elements: self.normalise_expressions(elements) }
            },
            Kind::ArrayRepeat { value, count } => {
                Kind::ArrayRepeat { value: self.normalise_expression(value), count }
            },
            Kind::Index { base, index } => {
                let base = self.normalise_expression(base);
                let index = self.normalise_expression(index);
                match self.typeck.type_dependent_def(expression.id).and_then(Res::function) {
                    Some(_) => return self.normalise_index(expression, base, index),
                    None => Kind::Index { base, index },
                }
            },
            Kind::Call { callee, args } => Kind::Call {
                callee: self.normalise_expression(callee),
                args: self.normalise_expressions(args),
            },
            Kind::MethodCall { name, receiver, args } => Kind::MethodCall {
                name,
                receiver: self.normalise_expression(receiver),
                args: self.normalise_expressions(args),
            },
            Kind::Syscall { code, args } => {
                Kind::Syscall { code, args: self.normalise_expressions(args) }
            },
            Kind::IntrinsicCall { intrinsic, args } => {
                Kind::IntrinsicCall { intrinsic, args: self.normalise_expressions(args) }
            },
            Kind::Cast { from, to } => Kind::Cast { from: self.normalise_expression(from), to },
            Kind::Match { scrutinee, arms } => Kind::Match {
                scrutinee: self.normalise_expression(scrutinee),
                arms: self.normalise_arms(arms),
            },
            Kind::Literal(_)
            | Kind::Local(_)
            | Kind::Path(_)
            | Kind::Const(_)
            | Kind::Static(_)
            | Kind::TypeIntrinsic { .. } => return expression,
        };

        self.arena.alloc(Expression { id: expression.id, kind, span: expression.span })
    }

    fn normalise_index(
        &mut self,
        expression: &'hir Expression<'hir>,
        receiver: &'hir Expression<'hir>,
        index: &'hir Expression<'hir>,
    ) -> &'hir Expression<'hir> {
        let resolution = self
            .typeck
            .type_dependent_defs
            .remove(&expression.id)
            .and_then(Res::function)
            .expect("overloaded index must have a resolved method");
        let return_type = self.scope.signatures[resolution].return_type;
        let TypeKind::Ref { mutable, to } = return_type.kind() else {
            panic!("index method must return a reference")
        };
        assert_eq!(
            Type::from(to),
            self.typeck.type_of(expression.id),
            "index method output must match the index expression"
        );

        let name = self.scope.symbols.insert(match mutable {
            true => "index_mut",
            false => "index",
        });
        let args = self.arena.alloc_slice_copy(&[index]);
        let call = self.alloc(
            ExpressionKind::MethodCall { name, receiver, args },
            return_type,
            expression.span,
        );
        self.typeck.type_dependent_defs.insert(call.expr.id, Res::Function(resolution));

        self.arena.alloc(Expression {
            id: expression.id,
            kind: ExpressionKind::Unary { operator: UnaryOperator::Deref, expr: call.expr },
            span: expression.span,
        })
    }

    fn normalise_expressions(
        &mut self,
        expressions: &'hir [&'hir Expression<'hir>],
    ) -> &'hir [&'hir Expression<'hir>] {
        let values = expressions
            .iter()
            .map(|&expression| self.normalise_expression(expression))
            .collect::<Vec<_>>();

        self.arena.alloc_slice_copy(&values)
    }

    fn normalise_fields(
        &mut self,
        fields: &'hir [(SymbolId, &'hir Expression<'hir>)],
    ) -> &'hir [(SymbolId, &'hir Expression<'hir>)] {
        let values = fields
            .iter()
            .map(|&(name, expression)| (name, self.normalise_expression(expression)))
            .collect::<Vec<_>>();

        self.arena.alloc_slice_copy(&values)
    }

    fn normalise_arms(&mut self, arms: &'hir [Arm<'hir>]) -> &'hir [Arm<'hir>] {
        let values = arms
            .iter()
            .map(|arm| Arm {
                pattern: arm.pattern,
                guard: arm.guard.map(|guard| self.normalise_expression(guard)),
                body: self.normalise_expression(arm.body),
                span: arm.span,
            })
            .collect::<Vec<_>>();

        self.arena.alloc_slice_copy(&values)
    }
}

impl BinaryOperator {
    #[inline(always)]
    const fn overload_method(self) -> Option<&'static str> {
        Some(match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::LtEq => "le",
            Self::Gt => "gt",
            Self::GtEq => "ge",
            _ => return None,
        })
    }

    #[inline(always)]
    const fn required_interface(self) -> CmpInterface {
        match self {
            Self::Eq | Self::Ne => CmpInterface::Equality,
            Self::Lt | Self::LtEq | Self::Gt | Self::GtEq => CmpInterface::Ordering,
            _ => unreachable!(),
        }
    }

    #[inline(always)]
    const fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::LtEq => "<=",
            Self::Gt => ">",
            Self::GtEq => ">=",
            _ => unreachable!(),
        }
    }
}
