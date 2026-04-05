use std::collections::HashMap;

use crate::{
    hir::{
        Block, Expression, ExpressionKind, Function, FunctionId, Local, LocalId, Parameter,
        Statement, SymbolId, Type,
        error::{HirError, HirErrorKind},
        symbols::SymbolTable,
    },
    lexer::token::Span,
    parser::{
        self,
        expression::{self, BinaryOperator, UnaryOperator},
        statement::{self, Else},
    },
};

#[derive(Debug)]
pub(in crate::hir) struct FunctionSignature {
    name: SymbolId,
    params: Vec<Type>,
    return_type: Type,
}

pub(in crate::hir) struct FunctionBuilder<'s, 'f> {
    signatures: &'s [FunctionSignature],
    functions: &'s Functions,
    locals: Vec<Local>,
    scopes: Vec<HashMap<SymbolId, LocalId>>,
    return_type: Type,
    function: Option<statement::Function<'f>>,
    next_local: u32,
    symbols: &'s mut SymbolTable,
}

pub(in crate::hir) type Functions = HashMap<SymbolId, FunctionId>;

impl<'s, 'f> FunctionBuilder<'s, 'f> {
    pub fn new(
        signatures: &'s [FunctionSignature],
        functions: &'s Functions,
        symbols: &'s mut SymbolTable,
        function: statement::Function<'f>,
    ) -> Self {
        let return_type = function
            .return_type
            .as_ref()
            .map(From::from)
            .unwrap_or(Type::Unit);

        Self {
            return_type,
            functions,
            symbols,
            signatures,
            function: Some(function),
            next_local: 0,
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
        }
    }

    #[inline(always)]
    pub fn lower(mut self) -> Result<Function, HirError<'f>> {
        let function = self.function.take().expect("function to be present");
        let symbol = self.symbols.insert(function.name);
        let id = *self
            .functions
            .get(&symbol)
            .expect("function id present for this name");
        let signatures = &self.signatures[id.0 as usize];

        let params = function
            .params
            .iter()
            .zip(signatures.params.iter())
            .map(|(parameter, &typ)| -> Result<_, HirError> {
                let symbol = self.symbols.insert(parameter.name);
                let id = self.declare_local(symbol, typ, true)?;

                Ok(Parameter {
                    typ,
                    id,
                    name: symbol,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let (body, _) = self.lower_block(&function.body, true)?;

        Ok(Function {
            id,
            name: symbol,
            params,
            locals: self.locals,
            return_type: self.return_type,
            body,
        })
    }

    fn lower_block(
        &mut self,
        block: &statement::Block<'f>,
        is_tail: bool,
    ) -> Result<(Block, bool), HirError<'f>> {
        self.push_scope();
        let last_idx = block.statements.len().saturating_sub(1);

        let (statements, returns) = block.statements.iter().enumerate().try_fold(
            (Vec::new(), false),
            |(mut statements, mut returns), (idx, statement)| -> Result<_, HirError> {
                let (statement, did_return) =
                    self.lower_statement(statement, is_tail && idx == last_idx)?;
                statements.push(statement);

                returns |= did_return;
                Ok((statements, returns))
            },
        )?;

        self.pop_scope();
        Ok((
            Block {
                statements,
                span: block.span,
            },
            returns,
        ))
    }

    fn lower_statement(
        &mut self,
        statement: &statement::Statement<'f>,
        is_tail: bool,
    ) -> Result<(Statement, bool), HirError<'f>> {
        use statement::Statement as Stmt;

        match statement {
            Stmt::Let(statement) => {
                let typ = match (statement.typ, statement.value.as_ref()) {
                    (Some(typ), _) => Type::from(typ),
                    (_, Some(expr)) => self.infer(expr)?,
                    (None, None) => {
                        return Err(HirError {
                            kind: HirErrorKind::MissingInitialiser {
                                name: statement.name.to_string(),
                            },
                        });
                    }
                };

                let symbol = self.symbols.insert(statement.name);
                let id = self.declare_local(symbol, typ, statement.mutable)?;

                if let Some(ref expr) = statement.value {
                    // we pass the declared type so int/float literals are widen
                    let expr = self.lower_expr(expr, Some(typ))?;
                    self.assert_type(typ, expr.typ)?;
                }

                Ok((Statement::Let { id }, false))
            }

            Stmt::Return(statement) => {
                let value = match statement.value {
                    Some(ref expr) => {
                        let expr = self.lower_expr(expr, Some(self.return_type))?;
                        self.assert_type(self.return_type, expr.typ)?;
                        Some(expr)
                    }
                    _ => None,
                };

                Ok((Statement::Return(value), true))
            }

            Stmt::If(statement) => self.lower_if(statement, is_tail),

            Stmt::While(statement) => {
                let condition = self.lower_expr(&statement.condition, None)?;
                self.assert_type(Type::Bool, condition.typ)?;

                // PERFORMANCE: remove loops with constant false conditions
                let (body, _) = self.lower_block(&statement.body, false)?;

                Ok((Statement::While { condition, body }, false))
            }

            Stmt::Expr(expr, _) => {
                let hint = match is_tail && self.return_type != Type::Unit {
                    true => Some(self.return_type),
                    _ => None,
                };
                let expr = self.lower_expr(expr, hint)?;

                match is_tail {
                    true => match self.return_type == Type::Unit {
                        true => Ok((Statement::Expr(expr), false)),
                        _ => {
                            self.assert_type(self.return_type, expr.typ)?;
                            Ok((Statement::Return(Some(expr)), true))
                        }
                    },
                    _ => Ok((Statement::Expr(expr), false)),
                }
            }

            Stmt::Block(block) => {
                let (block, returns) = self.lower_block(block, is_tail)?;
                Ok((Statement::Block(block), returns))
            }

            Stmt::Fn(_) => unimplemented!("nested functions are not supported yet"),
        }
    }

    /// Lowers an expression with an optional type hint flowing downward (biderectional checking).
    ///
    /// The hint is used to resolve the concrete type of integer and float literals when the
    /// expected type is known from context (call arguments, let bindings, assignments, etc.).
    ///
    /// When the hint is `None`, literals default to `i32` and `f64` respectively.
    fn lower_expr(
        &mut self,
        expr: &expression::Expression,
        hint: Option<Type>,
    ) -> Result<Expression, HirError<'f>> {
        use expression::Expression as Expr;

        match expr {
            // literal coercion: use the hint to widen to the expected numeric type.
            Expr::Integer(value, span) => {
                let typ = match hint {
                    Some(t @ (Type::I32 | Type::I64)) => t,
                    Some(t @ (Type::F32 | Type::F64)) => t,
                    _ => Type::I32,
                };
                Ok(Expression {
                    kind: ExpressionKind::Integer(*value),
                    typ,
                    span: *span,
                })
            }

            Expr::Float(value, span) => {
                let typ = match hint {
                    Some(t @ (Type::F32 | Type::F64)) => t,
                    _ => Type::F64,
                };
                Ok(Expression {
                    kind: ExpressionKind::Float(*value),
                    typ,
                    span: *span,
                })
            }

            Expr::String(value, span) => Ok(Expression {
                kind: ExpressionKind::String((*value).to_string()),
                typ: Type::String,
                span: *span,
            }),

            Expr::Bool(value, span) => Ok(Expression {
                kind: ExpressionKind::Bool(*value),
                typ: Type::Bool,
                span: *span,
            }),

            Expr::Identifier(name, span) => {
                let symbol = self.symbols.insert(name);
                let id = self.resolve_local(symbol, span)?;

                Ok(Expression {
                    kind: ExpressionKind::Local(id),
                    typ: self.locals[id.0 as usize].typ,
                    span: *span,
                })
            }

            Expr::Unary {
                operator,
                expr,
                span,
            } => {
                // for negation the hint flows through to the operand
                let inner_hint = match operator {
                    UnaryOperator::Neg => hint,
                    UnaryOperator::Not => Some(Type::Bool),
                };
                let expr = self.lower_expr(expr, inner_hint)?;

                // PERFORMANCE: fold unary operations when operand is a constant literal
                let expected = match operator {
                    UnaryOperator::Neg => match expr.typ.is_number() {
                        true => expr.typ,
                        _ => {
                            return Err(HirError {
                                kind: HirErrorKind::TypeMismatch {
                                    expected: Type::I32,
                                    found: expr.typ,
                                },
                            });
                        }
                    },
                    UnaryOperator::Not => Type::Bool,
                };

                self.assert_type(expected, expr.typ)?;

                Ok(Expression {
                    typ: expr.typ,
                    span: *span,
                    kind: ExpressionKind::Unary {
                        operator: *operator,
                        expr: Box::new(expr),
                    },
                })
            }

            Expr::Binary {
                left,
                operator,
                right,
                span,
            } => {
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
                    | BinaryOperator::Ne => Some(left.typ),
                    BinaryOperator::And | BinaryOperator::Or => Some(Type::Bool),
                };
                let right = self.lower_expr(right, right_hint)?;

                // PERFORMANCE: constant fold binary operator on literals
                let result = self.type_for_binary(operator, left.typ, right.typ, span)?;

                Ok(Expression {
                    kind: ExpressionKind::Binary {
                        operator: *operator,
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    typ: result,
                    span: *span,
                })
            }

            Expr::Assignment {
                target,
                value,
                span,
            } => {
                let symbol = self.symbols.insert(target);
                let id = self.resolve_local(symbol, span)?;

                if !self.locals[id.0 as usize].mutable {
                    return Err(HirError {
                        kind: HirErrorKind::ImmutableBind {
                            name: target.to_string(),
                        },
                    });
                }

                let target_typ = self.locals[id.0 as usize].typ;
                // we need to pass the targets type as a hint :D
                let value = self.lower_expr(value, Some(target_typ))?;

                self.assert_type(target_typ, value.typ)?;

                Ok(Expression {
                    kind: ExpressionKind::Assign {
                        target: id,
                        value: Box::new(value),
                    },
                    typ: target_typ,
                    span: *span,
                })
            }

            Expr::Call { callee, args, span } => {
                let (function, signature) = match callee.as_ref() {
                    Expr::Identifier(name, _) => {
                        let symbol = self.symbols.insert(name);
                        let id = *self.functions.get(&symbol).ok_or_else(|| HirError {
                            kind: HirErrorKind::UnknownFunction {
                                name: name.to_string(),
                            },
                        })?;

                        (id, &self.signatures[id.0 as usize])
                    }

                    other => {
                        return Err(HirError {
                            kind: HirErrorKind::UnknownFunction {
                                name: format!("{other:?}"),
                            },
                        });
                    }
                };

                if signature.params.len() != args.len() {
                    return Err(HirError {
                        kind: HirErrorKind::ArityMismatch {
                            name: self.symbols.get(signature.name).to_string(),
                            expected: signature.params.len(),
                            found: args.len(),
                        },
                    });
                }

                let mut lowered_args = Vec::with_capacity(args.len());

                for (expr, &param_typ) in args.iter().zip(signature.params.iter()) {
                    // each parameters declared type as the hint
                    // that allows `foo(1)` to work when `foo` expects an `i64`.
                    let expr = self.lower_expr(expr, Some(param_typ))?;
                    self.assert_type(param_typ, expr.typ)?;
                    lowered_args.push(expr);
                }

                let return_type = signature.return_type;
                Ok(Expression {
                    typ: return_type,
                    span: *span,
                    kind: ExpressionKind::Call {
                        function,
                        args: lowered_args,
                    },
                })
            }
        }
    }

    #[inline(always)]
    fn lower_type(&mut self, expr: &expression::Expression) -> Result<Expression, HirError<'f>> {
        use expression::Expression as Expr;

        match expr {
            Expr::Integer(value, span) => Ok(Expression {
                kind: ExpressionKind::Integer(*value),
                typ: Type::I32,
                span: *span,
            }),

            Expr::Float(value, span) => Ok(Expression {
                kind: ExpressionKind::Float(*value),
                typ: Type::F64,
                span: *span,
            }),

            Expr::String(value, span) => Ok(Expression {
                kind: ExpressionKind::String((*value).to_string()),
                typ: Type::String,
                span: *span,
            }),

            Expr::Bool(value, span) => Ok(Expression {
                kind: ExpressionKind::Bool(*value),
                typ: Type::Bool,
                span: *span,
            }),
            _ => unsafe { std::hint::unreachable_unchecked() },
        }
    }

    fn lower_if(
        &mut self,
        if_stmt: &statement::If<'f>,
        is_tail: bool,
    ) -> Result<(Statement, bool), HirError<'f>> {
        let condition = self.lower_expr(&if_stmt.condition, None)?;
        self.assert_type(Type::Bool, condition.typ)?;

        let (then_block, then_returns) = self.lower_block(&if_stmt.then_branch, is_tail)?;
        let (else_block, else_returns) = if_stmt
            .else_branch
            .as_ref()
            .map(|else_branch| -> Result<_, HirError> {
                match else_branch.as_ref() {
                    Else::If(block) => {
                        let (statement, returns) = self.lower_if(block, is_tail)?;
                        let block = Block {
                            span: block.span,
                            statements: vec![statement],
                        };

                        Ok((Some(block), returns))
                    }
                    Else::Block(block) => {
                        let (block, returns) = self.lower_block(block, is_tail)?;
                        Ok((Some(block), returns))
                    }
                }
            })
            .transpose()?
            .unwrap_or((None, false));

        Ok((
            Statement::If {
                condition,
                then_block,
                else_block,
            },
            then_returns && else_returns,
        ))
    }

    fn type_for_binary(
        &self,
        operator: &BinaryOperator,
        left: Type,
        right: Type,
        span: &Span,
    ) -> Result<Type, HirError<'f>> {
        match operator {
            BinaryOperator::Add
            | BinaryOperator::Sub
            | BinaryOperator::Mul
            | BinaryOperator::Div => {
                self.assert_type(left, right)?;
                match left.is_number() {
                    true => Ok(left),
                    other => Err(HirError {
                        kind: HirErrorKind::TypeMismatch {
                            expected: Type::I32,
                            found: left,
                        },
                    }),
                }
            }

            BinaryOperator::Eq | BinaryOperator::Ne => {
                self.assert_type(left, right)?;
                Ok(Type::Bool)
            }

            BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq => {
                self.assert_type(left, right)?;
                match left.is_number() {
                    true => Ok(Type::Bool),
                    _ => Err(HirError {
                        kind: HirErrorKind::TypeMismatch {
                            expected: Type::I32,
                            found: left,
                        },
                    }),
                }
            }

            BinaryOperator::And | BinaryOperator::Or => {
                self.assert_type(Type::Bool, left)?;
                self.assert_type(Type::Bool, right)?;

                Ok(Type::Bool)
            }
        }
    }

    #[inline(always)]
    fn infer(&mut self, expr: &expression::Expression) -> Result<Type, HirError<'f>> {
        let expr = self.lower_expr(expr, None)?;
        Ok(expr.typ)
    }

    #[inline(always)]
    #[must_use]
    fn assert_type(&self, expected: Type, found: Type) -> Result<(), HirError<'f>> {
        match expected == found {
            true => Ok(()),
            false => Err(HirError {
                kind: HirErrorKind::TypeMismatch { expected, found },
            }),
        }
    }

    fn declare_local(
        &mut self,
        name: SymbolId,
        typ: Type,
        mutable: bool,
    ) -> Result<LocalId, HirError<'f>> {
        let scope = self
            .scopes
            .last_mut()
            .expect("at least one scope is always present");

        if scope.contains_key(&name) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateBind {
                    name: self.symbols.get(name).to_string(),
                },
            });
        }

        let id = LocalId(self.next_local);
        self.next_local += 1;

        scope.insert(name, id);
        self.locals.push(Local {
            id,
            name,
            typ,
            mutable,
        });

        Ok(id)
    }

    #[inline(always)]
    fn resolve_local(&mut self, name: SymbolId, span: &Span) -> Result<LocalId, HirError<'f>> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name).copied())
            .ok_or_else(|| HirError {
                kind: HirErrorKind::UndeclaredIdentifier {
                    name: self.symbols.get(name).to_string(),
                },
            })
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

pub fn collect_function_signatures<'h>(
    statements: &[statement::Statement<'h>],
    symbols: &mut SymbolTable,
) -> Result<(Vec<FunctionSignature>, Functions), HirError<'h>> {
    let mut signatures = Vec::new();
    let mut functions = Functions::new();

    for statement in statements {
        let function = match statement {
            statement::Statement::Fn(func) => func,
            other => {
                return Err(HirError {
                    kind: HirErrorKind::TopLevelNonFunction,
                });
            }
        };

        let symbol = symbols.insert(function.name);
        if functions.contains_key(&symbol) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateFunction {
                    name: function.name.to_string(),
                },
            });
        }

        let function_id = FunctionId(signatures.len() as u32);
        functions.insert(symbol, function_id);

        let params = function.params.iter().map(|p| Type::from(p.typ)).collect();
        let return_type = function.return_type.map(From::from).unwrap_or(Type::Unit);
        signatures.push(FunctionSignature {
            return_type,
            params,
            name: symbol,
        })
    }

    Ok((signatures, functions))
}
