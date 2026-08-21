use crate::{
    hir::{
        Block, Constant, Expression, LoopKind, Owner, Statement, Type, TypeKind,
        error::{HirError, hir_error},
        lower::FunctionBuilder,
    },
    lexer::Spanned,
    parser::{
        expression,
        statement::{self, Else, ItemKind},
    },
};

impl<'s, 'f, 'hir, 'src> FunctionBuilder<'s, 'f, 'hir, 'src>
where
    'src: 'hir,
{
    pub(super) fn lower_block(
        &mut self,
        block: &statement::Block<'src>,
        is_tail: bool,
    ) -> Result<(Block<'hir>, bool), HirError<'hir>> {
        self.push_scope();
        let last_idx = block.statements.len().saturating_sub(1);

        let mut statements_vec = Vec::with_capacity(block.statements.len());
        let mut returns = false;
        for (idx, statement) in block.statements.iter().enumerate() {
            if let statement::Statement::Item(statement::Item {
                kind: ItemKind::Const(constant),
                ..
            }) = statement
            {
                if let Err(error) = self.declare_body_const(constant) {
                    self.soft(error);
                }

                continue;
            }

            match self.lower_statement(statement, is_tail && idx == last_idx) {
                Ok((statement, did_return)) => {
                    statements_vec.push(statement);
                    returns |= did_return;
                },
                Err(error) => self.soft(error),
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
                    (Some(typ), _) => self
                        .resolve_type(&typ.value(), typ.span())
                        .unwrap_or_else(|e| self.poison(e)),
                    (_, Some(expr)) => self.infer(expr).unwrap_or_else(|e| self.poison(e)),
                    (None, None) => self.poison(hir_error!(
                        statement.span,
                        MissingInitialiser { name: statement.name }
                    )),
                };

                let symbol = self.scope.symbols.insert(statement.name);
                let id = self.declare_local(symbol, typ, statement.mutable, statement.name_span)?;

                let mut diverges = false;
                let annotation = statement.typ.as_ref().map(Spanned::span);
                let stmt = match statement.value {
                    Some(ref expr) => match self.lower_expr(expr, Some(typ)) {
                        Ok(expr) => {
                            self.check_type_at(typ, expr.typ, expr.span, annotation)?;
                            diverges = expr.typ.diverges();

                            Statement::LetInit { id, init: expr.expr }
                        },
                        Err(error) => {
                            self.soft(error);
                            Statement::LetUninit { id }
                        },
                    },
                    _ => Statement::LetUninit { id },
                };

                Ok((stmt, diverges))
            },

            Stmt::Return(statement) => {
                let value = self.lower_return_value(statement)?;
                Ok((Statement::Return(value), true))
            },
            Stmt::If(statement) => self.lower_if(statement, is_tail),
            Stmt::Loop(statement) => self.lower_loop(statement),
            Stmt::Break(span) => {
                if self.loop_depth == 0 {
                    let err = hir_error!(*span, LoopControlOutsideLoop { kind: "break" });
                    return Err(err);
                }
                Ok((Statement::Break, false))
            },
            Stmt::Continue(span) => {
                if self.loop_depth == 0 {
                    let err = hir_error!(*span, LoopControlOutsideLoop { kind: "continue" });
                    return Err(err);
                }
                Ok((Statement::Continue, false))
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

            Stmt::Unsafe { block, marker } => {
                // a block nested in a context that already allows the operations
                // grants nothing, so it is redundant however much it contains
                let redundant = self.unsafe_ctx.depth > 0;
                let before = self.unsafe_ctx.ops;

                self.unsafe_ctx.depth += 1;
                let lowered = self.lower_block(block, is_tail);
                self.unsafe_ctx.depth -= 1;

                let (block, returns) = lowered?;
                if redundant || self.unsafe_ctx.ops == before {
                    let diagnostic = hir_error!(*marker, UnusedUnsafe).into();
                    self.scope.diagnostics.borrow_mut().warn(diagnostic);
                }

                Ok((Statement::Block(block), returns))
            },

            Stmt::Match(statement) => {
                let tail_ret = is_tail && self.return_type.kind() != TypeKind::Unit;
                let expr = self.lower_match(statement, tail_ret.then_some(self.return_type))?;

                self.handle_tail_expr(expr, tail_ret)
            },

            Stmt::Item(statement::Item { kind, .. }) => {
                Err(hir_error!(kind.span(), NestedItem { kind: kind.keyword() }))
            },
        }
    }

    fn lower_loop(
        &mut self,
        statement: &statement::Loop<'src>,
    ) -> Result<(Statement<'hir>, bool), HirError<'hir>> {
        use statement::LoopHeader as Loop;

        let (kind, body) = match &statement.header {
            Loop::Infinite => {
                let body = self.lower_loop_body(&statement.body)?;
                (LoopKind::Infinite, body)
            },
            Loop::Range { binding, start, end, inclusive } => {
                let start = self.lower_expr(start, None)?;
                let end = self.lower_expr(end, Some(start.typ))?;
                self.assert_type(start.typ, end.typ, end.span)?;
                let typ = self.infer.resolve_shallow(start.typ);
                if !typ.is_integer() && !typ.is_infer() {
                    return Err(hir_error!(start.span, InvalidRangeType { typ }));
                }

                self.push_scope();
                let binding = binding
                    .map(|binding| {
                        let symbol = self.scope.symbols.insert(binding.name);
                        self.declare_local(symbol, typ, false, binding.span)
                    })
                    .transpose()?;
                let body = self.lower_loop_body(&statement.body);
                self.pop_scope();

                (
                    LoopKind::Range {
                        binding,
                        start: start.expr,
                        end: end.expr,
                        inclusive: *inclusive,
                    },
                    body?,
                )
            },

            Loop::Iterable { binding, iterable } => {
                let iterable = self.lower_expr(iterable, None)?;
                let element = match iterable.typ.kind() {
                    TypeKind::Array(id) => self.scope.arrays.get(id).element,
                    TypeKind::Slice { element, .. } => element.into(),
                    _ => return Err(hir_error!(iterable.span, NotIterable { typ: iterable.typ })),
                };

                let resolved = self.infer.resolve_shallow(element);
                if !resolved.is_infer() && !resolved.is_copy(self.scope) {
                    return Err(hir_error!(binding.span, NonCopyLoopItem { typ: resolved }));
                }

                self.push_scope();
                let symbol = self.scope.symbols.insert(binding.name);
                let binding = self.declare_local(symbol, element, false, binding.span)?;
                let body = self.lower_loop_body(&statement.body);
                self.pop_scope();

                (LoopKind::Iterable { binding, iterable: iterable.expr }, body?)
            },
        };

        Ok((Statement::Loop { kind, body }, false))
    }

    fn lower_loop_body(
        &mut self,
        body: &statement::Block<'src>,
    ) -> Result<Block<'hir>, HirError<'hir>> {
        self.loop_depth += 1;
        let body = self.lower_block(body, false).map(|(body, _)| body);
        self.loop_depth -= 1;
        body
    }

    fn declare_body_const(
        &mut self,
        constant: &statement::Const<'src>,
    ) -> Result<(), HirError<'hir>> {
        let typ = self
            .resolve_type(&constant.typ.value(), constant.typ.span())
            .unwrap_or_else(|e| self.poison(e));

        let name = self.scope.symbols.insert(constant.name);
        if let Some(existing) = self.const_scope.body.get(&name) {
            let previous = crate::hir::collect::source_span(existing.decl_span);
            return Err(hir_error!(
                constant.span,
                DuplicateConstant { name: constant.name, previous }
            ));
        }

        let outer_locals = self.scopes.iter().flat_map(|scope| scope.keys().copied()).collect();
        let siblings = self.const_scope.body.clone();

        let (value, typeck) = {
            let mut builder = FunctionBuilder::new_for_const(self.scope, self.arena);
            builder.const_scope.outer_locals = outer_locals;
            builder.const_scope.body = siblings;

            let lowered = builder.lower_expr(&constant.value, Some(typ))?;
            builder.assert_type(typ, lowered.typ, lowered.span)?;
            builder.resolve_inference();
            let value = lowered.expr;

            (value, builder.typeck)
        };

        let decl_span = constant.span;
        let constant = self.arena.alloc(Constant {
            name,
            typ,
            owner: Owner::Free,
            value,
            typeck,
            is_pub: false,
            decl_span,
            name_span: constant.name_span,
        });
        self.const_scope.body.insert(name, constant);

        Ok(())
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

    fn infer(&mut self, expr: &expression::Expression<'src>) -> Result<Type<'hir>, HirError<'hir>> {
        let expr = self.lower_expr(expr, None)?;
        Ok(expr.typ)
    }

    /// the value a `return` carries, checked against the enclosing signature
    pub(super) fn lower_return_value(
        &mut self,
        returned: &statement::Return<'src>,
    ) -> Result<Option<&'hir Expression<'hir>>, HirError<'hir>> {
        Ok(match returned.value.as_ref() {
            Some(expr) => {
                let expr = self.lower_expr(expr, Some(self.return_type))?;
                self.check_type_at(self.return_type, expr.typ, expr.span, self.return_type_span)?;
                Some(expr.expr)
            },
            _ => {
                let (typ, span) = (self.return_type, self.return_type_span);
                self.check_type_at(typ, TypeKind::Unit, returned.span, span)?;
                None
            },
        })
    }
}
