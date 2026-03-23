use std::collections::HashMap;

use crate::{
    hir::{
        Block, Expression, Function, FunctionId, Local, LocalId, Parameter, Statement, SymbolId,
        Type,
        error::{HirError, HirErrorKind},
        symbols::SymbolTable,
    },
    lexer::token::Span,
    parser::{
        self, expression,
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
    fn new(
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

        let mut params = Vec::new();
        for (parameter, &typ) in function.params.iter().zip(signatures.params.iter()) {
            let symbol = self.symbols.insert(parameter.name);
            let id = self.declare_local(symbol, typ, true)?;

            params.push(Parameter {
                typ: typ,
                id,
                name: symbol,
            })
        }

        todo!()
    }

    fn lower_block(
        &mut self,
        block: &statement::Block<'f>,
        is_tail: bool,
    ) -> Result<(Block, bool), HirError<'f>> {
        self.push_scope();
        let mut statements = Vec::new();
        let mut returns = false;

        for (idx, statement) in block.statements.iter().enumerate() {
            let is_tail = is_tail && idx + 1 == block.statements.len();
            let (statement, did_return) = self.lower_statement(statement, is_tail)?;
            statements.push(statement);
            returns |= did_return;
        }

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
                let value = match statement.value {
                    Some(ref expr) => {
                        let expr = self.lower_expr(expr)?;
                        self.assert_type(typ, expr.typ)?;
                        Some(expr)
                    }
                    _ => None,
                };

                Ok((Statement::Let { id }, false))
            }

            Stmt::Return(statement) => {
                let value = match statement.value {
                    Some(ref expr) => {
                        let expr = self.lower_expr(expr)?;
                        self.assert_type(self.return_type, expr.typ)?;
                        Some(expr)
                    }
                    _ => None,
                };

                Ok((Statement::Return(value), true))
            }

            Stmt::If(statement) => {
                let condition = self.lower_expr(&statement.condition)?;
                self.assert_type(Type::Bool, condition.typ)?;

                // PERFORMANCE: evaluate constant conditions and prune dead branches
                let (then_block, returns) = self.lower_block(&statement.then_branch, is_tail)?;
                let (else_block, else_returns) = match statement.else_branch {
                    Some(ref statement) => match &**statement {
                        Else::If(inner) => {
                            todo!()
                        }
                        Else::Block(block) => {
                            let (block, returns) = self.lower_block(block, is_tail)?;
                            (Some(block), returns)
                        }
                    },
                    _ => (None, false),
                };

                Ok((
                    Statement::If {
                        condition,
                        then_block,
                        else_block,
                    },
                    returns && else_returns,
                ))
            }

            Stmt::While(statement) => {
                let condition = self.lower_expr(&statement.condition)?;
                self.assert_type(Type::Bool, condition.typ)?;

                // PERFORMANCE: remove loops with constant false conditions
                let (body, _) = self.lower_block(&statement.body, false)?;

                Ok((Statement::While { condition, body }, false))
            }

            Stmt::Expr(expr, _) => {
                let expr = self.lower_expr(expr)?;

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

    fn lower_expr(&mut self, expr: &expression::Expression) -> Result<Expression, HirError<'f>> {
        todo!()
    }

    fn infer(&mut self, expr: &expression::Expression) -> Result<Type, HirError<'f>> {
        todo!()
    }

    #[inline(always)]
    #[must_use]
    const fn assert_type(&self, expected: Type, found: Type) -> Result<(), HirError<'f>> {
        return Err(HirError {
            kind: HirErrorKind::TypeMismatch { expected, found },
        });
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

        let params = function
            .params
            .iter()
            .map(|param| Type::from(param.typ))
            .collect::<Vec<_>>();
        let return_type = function.return_type.map(From::from).unwrap_or(Type::Unit);

        signatures.push(FunctionSignature {
            return_type,
            params,
            name: symbol,
        })
    }

    Ok((signatures, functions))
}
