use crate::{
    hir::{
        Block, Expression, ExpressionKind, Function, FunctionId, Intrinsic, Local, LocalId, Method,
        Parameter, Receiver, Statement, Struct, StructField, StructId, SymbolId, Type,
        declarations::Declarations,
        error::{HirError, HirErrorKind},
        symbols::SymbolTable,
    },
    lexer::token::Span,
    parser::{
        expression::{self, BinaryOperator, UnaryOperator},
        statement::{self, Else},
    },
};
use std::{
    collections::{HashMap, HashSet},
    ops::Index,
    str::FromStr,
};

#[derive(Debug)]
pub(in crate::hir) struct FunctionSignature {
    pub name: SymbolId,
    pub params: Vec<Type>,
    pub return_type: Type,
    pub intrinsic: Option<Intrinsic>,
    pub method: Option<Method>,
}

#[derive(Debug)]
pub(in crate::hir) struct InterfaceSignature {
    pub name: SymbolId,
    pub methods: Vec<InterfaceMethodSignature>,
}

#[derive(Debug)]
pub(in crate::hir) struct InterfaceMethodSignature {
    pub name: SymbolId,
    pub params: Vec<Type>,
    pub return_type: Type,
    has_receiver: bool,
    receiver_mut: bool,
}

pub(in crate::hir) struct FunctionBuilder<'s, 'f> {
    signatures: &'s [FunctionSignature],
    functions: &'s Functions,
    methods: &'s Methods,
    structs: &'s [Struct],
    struct_map: &'s Structs,
    locals: Vec<Local>,
    scopes: Vec<HashMap<SymbolId, LocalId>>,
    return_type: Type,
    function: Option<&'f statement::Function<'f>>,
    function_id: FunctionId,
    next_local: u32,
    symbols: &'s mut SymbolTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visit {
    Unvisited,
    Visiting,
    Visited,
}

pub(in crate::hir) type Functions = HashMap<SymbolId, FunctionId>;
pub(in crate::hir) type Structs = HashMap<SymbolId, StructId>;
pub(in crate::hir) type Methods = HashMap<(StructId, SymbolId), FunctionId>;
pub(in crate::hir) type Interfaces = HashMap<SymbolId, InterfaceSignature>;

impl Default for FunctionSignature {
    fn default() -> Self {
        use lasso::{Key, Spur};

        Self {
            name: SymbolId(Spur::try_from_usize(0).expect("spur shouldn't fail")),
            params: Vec::new(),
            return_type: Type::Unit,
            intrinsic: None,
            method: None,
        }
    }
}

impl<'s, 'f> FunctionBuilder<'s, 'f> {
    pub fn new(
        signatures: &'s [FunctionSignature],
        functions: &'s Functions,
        methods: &'s Methods,
        structs: &'s [Struct],
        struct_map: &'s Structs,
        symbols: &'s mut SymbolTable,
        function_id: FunctionId,
        function: &'f statement::Function<'f>,
    ) -> Self {
        Self {
            functions,
            methods,
            structs,
            struct_map,
            symbols,
            signatures,
            return_type: Type::Unit,
            function: Some(function),
            function_id,
            next_local: 0,
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
        }
    }

    #[inline(always)]
    pub fn lower(mut self) -> Result<Function, HirError<'f>> {
        let function = self.function.take().expect("function to be present");
        let id = self.function_id;
        let signatures = &self.signatures[id.0 as usize];
        let symbol = signatures.name;
        self.return_type = signatures.return_type;

        let mut params = Vec::with_capacity(signatures.params.len());

        if let Some(receiver) = function.receiver {
            let typ = signatures.params[0];
            let symbol = self.symbols.insert("self");
            let id = self.declare_local(symbol, typ, receiver.mutable)?;
            params.push(Parameter {
                typ,
                id,
                name: symbol,
                mutable: receiver.mutable,
            });
        }

        let receiver_offset = usize::from(function.receiver.is_some());
        params.extend(
            function
                .params
                .iter()
                .zip(signatures.params.iter().skip(receiver_offset))
                .map(|(parameter, &typ)| -> Result<_, HirError> {
                    let symbol = self.symbols.insert(parameter.name);
                    let id = self.declare_local(symbol, typ, parameter.mutable)?;

                    Ok(Parameter {
                        typ,
                        id,
                        name: symbol,
                        mutable: parameter.mutable,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        );

        let (body, _) = self.lower_block(&function.body, true)?;

        Ok(Function {
            id,
            name: symbol,
            params,
            locals: self.locals,
            return_type: signatures.return_type,
            is_const: function.is_const,
            is_pub: function.is_pub,
            intrinsic: signatures.intrinsic,
            method: signatures.method,
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
                let typ = match (statement.typ.as_ref(), statement.value.as_ref()) {
                    (Some(typ), _) => self.resolve_type(&typ.value(), typ.span())?,
                    (_, Some(expr)) => self.infer(expr)?,
                    (None, None) => {
                        return Err(HirError {
                            kind: HirErrorKind::MissingInitialiser {
                                name: statement.name.to_string(),
                            },
                            span: statement.span,
                        });
                    }
                };

                let symbol = self.symbols.insert(statement.name);
                let id = self.declare_local(symbol, typ, statement.mutable)?;

                let init = match statement.value {
                    Some(ref expr) => {
                        let expr = self.lower_expr(expr, Some(typ))?;
                        self.assert_type(typ, expr.typ, expr.span)?;

                        Some(expr)
                    }
                    _ => None,
                };

                Ok((Statement::Let { id, init }, false))
            }

            Stmt::Return(statement) => {
                let value = match statement.value {
                    Some(ref expr) => {
                        let expr = self.lower_expr(expr, Some(self.return_type))?;
                        self.assert_type(self.return_type, expr.typ, expr.span)?;
                        Some(expr)
                    }
                    _ => None,
                };

                Ok((Statement::Return(value), true))
            }

            Stmt::If(statement) => self.lower_if(statement, is_tail),

            Stmt::While(statement) => {
                let condition = self.lower_expr(&statement.condition, None)?;
                self.assert_type(Type::Bool, condition.typ, statement.span)?;

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
                            self.assert_type(self.return_type, expr.typ, expr.span)?;
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

            Stmt::Interface(_) => unimplemented!("interface lowering is not yet implemented"),

            Stmt::Fn(_) => unimplemented!("nested functions are not supported yet"),
            Stmt::Struct(_) => unimplemented!("nested structs are not supported yet"),
            Stmt::Use(_) => unimplemented!("use declarations are not supported yet"),
            Stmt::Impl(_) => unimplemented!("nested impl blocks are not supported yet"),
        }
    }

    /// Lowers an expression with an optional type hint flowing downward (biderectional checking)
    ///
    /// The hint is used to resolve the concrete type of integer and float literals when the
    /// expected type is known from context
    ///
    /// When the hint is `None`, literals default to `i32` and `f64` respectively
    fn lower_expr(
        &mut self,
        expr: &expression::Expression<'f>,
        hint: Option<Type>,
    ) -> Result<Expression, HirError<'f>> {
        use expression::Expression as Expr;

        match expr {
            // literal coercion: use the hint to widen to the expected numeric type.
            Expr::Integer(value, span) => {
                let typ = match hint {
                    Some(t) if t.is_number() => t,
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
                    Some(t) if t.is_float() => t,
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
                let id = self.resolve_local(symbol, *span)?;

                Ok(Expression {
                    kind: ExpressionKind::Local(id),
                    typ: self[id].typ,
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
                                span: expr.span,
                            });
                        }
                    },
                    UnaryOperator::Not => Type::Bool,
                };

                self.assert_type(expected, expr.typ, expr.span)?;

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
                let result = self.type_for_binary(operator, left.typ, right.typ, *span)?;

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
            } => match target.as_ref() {
                Expr::Identifier(name, _) => {
                    let symbol = self.symbols.insert(name);
                    let id = self.resolve_local(symbol, *span)?;

                    if !self[id].mutable {
                        return Err(HirError {
                            kind: HirErrorKind::ImmutableBind {
                                name: name.to_string(),
                            },
                            span: *span,
                        });
                    }

                    let typ = self[id].typ;
                    let value = self.lower_expr(value, Some(typ))?;
                    self.assert_type(typ, value.typ, *span)?;

                    Ok(Expression {
                        kind: ExpressionKind::Assign {
                            target: id,
                            value: Box::new(value),
                        },
                        typ,
                        span: *span,
                    })
                }

                Expr::Field { expr, field, span } => {
                    let (local, fields, typ) = self.resolve_field_chain(expr, field, *span)?;

                    if !self[local].mutable {
                        return Err(HirError {
                            kind: HirErrorKind::ImmutableBind {
                                name: self.symbols.get(self[local].name).to_string(),
                            },
                            span: *span,
                        });
                    }

                    let value = self.lower_expr(value, Some(typ))?;
                    self.assert_type(typ, value.typ, *span)?;

                    Ok(Expression {
                        kind: ExpressionKind::FieldAssign {
                            local,
                            fields,
                            value: Box::new(value),
                        },
                        typ,
                        span: *span,
                    })
                }

                _ => Err(HirError {
                    kind: HirErrorKind::InvalidAssignmentTarget,
                    span: *span,
                }),
            },

            Expr::Struct { name, fields, span } => {
                let symbol = self.symbols.insert(name);
                let id = self.struct_map.get(&symbol).copied().ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownType {
                        name: (*name).to_string(),
                    },
                    span: *span,
                })?;
                let definition = &self.structs[id.0 as usize];
                let struct_name = self.symbols.get(definition.name).to_string();

                let mut seen = HashSet::with_capacity(fields.len());
                let mut lowered = Vec::with_capacity(fields.len());

                for field in fields {
                    let field_symbol = self.symbols.insert(field.name);
                    if !seen.insert(field_symbol) {
                        return Err(HirError {
                            kind: HirErrorKind::DuplicateField {
                                name: field.name.to_string(),
                            },
                            span: field.span,
                        });
                    }

                    let Some(expected) = definition.fields.iter().find(|f| f.name == field_symbol)
                    else {
                        return Err(HirError {
                            kind: HirErrorKind::UnknownField {
                                struct_name: struct_name,
                                field: field.name.to_string(),
                            },
                            span: field.span,
                        });
                    };

                    let value = self.lower_expr(&field.value, Some(expected.typ))?;
                    self.assert_type(expected.typ, value.typ, value.span)?;
                    lowered.push((field_symbol, value));
                }

                for expected in &definition.fields {
                    if !seen.contains(&expected.name) {
                        return Err(HirError {
                            kind: HirErrorKind::MissingField {
                                struct_name,
                                field: self.symbols.get(expected.name).to_string(),
                            },
                            span: *span,
                        });
                    }
                }

                Ok(Expression {
                    typ: Type::Struct(id),
                    span: *span,
                    kind: ExpressionKind::Struct {
                        id,
                        fields: lowered,
                    },
                })
            }

            Expr::Field { expr, field, span } => {
                let (local, fields, typ) = self.resolve_field_chain(expr, field, *span)?;

                Ok(Expression {
                    kind: ExpressionKind::FieldAccess { local, fields },
                    typ,
                    span: *span,
                })
            }

            Expr::Call { callee, args, span } => {
                if let Expr::Identifier(name, _) = callee.as_ref() {
                    if let Ok(intrinsic) = Intrinsic::from_str(name) {
                        return self.lower_intrinsic(intrinsic, args, *span);
                    }
                }

                if let Expr::Field {
                    expr: receiver,
                    field: method_name,
                    ..
                } = callee.as_ref()
                {
                    let (local, fields, receiver_type) =
                        self.resolve_receiver_chain(receiver, *span)?;
                    let receiver_struct = match receiver_type {
                        Type::Struct(id) => id,
                        Type::Ref { to, .. } => to,
                        found => {
                            return Err(HirError {
                                kind: HirErrorKind::TypeMismatch {
                                    expected: Type::Struct(StructId::default()),
                                    found,
                                },
                                span: *span,
                            });
                        }
                    };

                    let method_symbol = self.symbols.insert(method_name);
                    let function = *self
                        .methods
                        .get(&(receiver_struct, method_symbol))
                        .ok_or_else(|| HirError {
                            kind: HirErrorKind::UnknownMethod {
                                struct_name: self
                                    .symbols
                                    .get(self[receiver_struct].name)
                                    .to_string(),
                                name: method_name.to_string(),
                            },
                            span: *span,
                        })?;
                    let signature = &self.signatures[function.0 as usize];
                    let Type::Ref { mutable, .. } = signature.params[0] else {
                        unreachable!("method signature must start with receiver reference");
                    };

                    if mutable && !self[local].mutable {
                        return Err(HirError {
                            kind: HirErrorKind::ImmutableBind {
                                name: self.symbols.get(self[local].name).to_string(),
                            },
                            span: *span,
                        });
                    }

                    let explicit_params = &signature.params[1..];
                    if explicit_params.len() != args.len() {
                        return Err(HirError {
                            kind: HirErrorKind::ArityMismatch {
                                name: method_name.to_string(),
                                expected: explicit_params.len(),
                                found: args.len(),
                            },
                            span: *span,
                        });
                    }

                    let lowered_args = args
                        .iter()
                        .zip(explicit_params.iter())
                        .map(|(expr, &param_type)| -> Result<_, HirError> {
                            let expr = self.lower_expr(expr, Some(param_type))?;
                            self.assert_type(param_type, expr.typ, expr.span)?;
                            Ok(expr)
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    return Ok(Expression {
                        typ: signature.return_type,
                        span: *span,
                        kind: ExpressionKind::MethodCall {
                            function,
                            receiver: Receiver {
                                local,
                                fields,
                                typ: signature.params[0],
                            },
                            args: lowered_args,
                        },
                    });
                }

                let (function, signature) = match callee.as_ref() {
                    Expr::Identifier(name, _) => {
                        let symbol = self.symbols.insert(name);
                        let id = *self.functions.get(&symbol).ok_or_else(|| HirError {
                            kind: HirErrorKind::UnknownFunction {
                                name: name.to_string(),
                            },
                            span: *span,
                        })?;

                        (id, &self.signatures[id.0 as usize])
                    }

                    other => {
                        return Err(HirError {
                            // FIXME: improve this error messag
                            kind: HirErrorKind::UnknownFunction {
                                name: format!("{other:?}"),
                            },
                            span: *span,
                        });
                    }
                };

                if signature.intrinsic.is_none() && signature.params.len() != args.len() {
                    return Err(HirError {
                        kind: HirErrorKind::ArityMismatch {
                            name: self.symbols.get(signature.name).to_string(),
                            expected: signature.params.len(),
                            found: args.len(),
                        },
                        span: *span,
                    });
                }

                let mut lowered_args = Vec::with_capacity(args.len());

                match signature.intrinsic {
                    Some(_) => {
                        for arg in args.iter() {
                            let lowered = self.lower_expr(arg, None)?;
                            lowered_args.push(lowered);
                        }
                    }

                    _ => {
                        for (expr, &param_type) in args.iter().zip(signature.params.iter()) {
                            let expr = self.lower_expr(expr, Some(param_type))?;
                            self.assert_type(param_type, expr.typ, expr.span)?;
                            lowered_args.push(expr);
                        }
                    }
                }

                let return_type = signature.return_type;
                let kind = match signature.intrinsic {
                    Some(intrinsic) => ExpressionKind::IntrinsicCall {
                        intrinsic,
                        args: lowered_args,
                    },
                    _ => ExpressionKind::Call {
                        function,
                        args: lowered_args,
                    },
                };

                Ok(Expression {
                    typ: return_type,
                    span: *span,
                    kind,
                })
            }

            Expr::QualifiedCall {
                qualifier,
                name,
                args,
                span,
            } => {
                let mangled_name = format!("{qualifier}__{name}");
                let mangled_symbol = self.symbols.insert(&mangled_name);

                if let Some(&id) = self.functions.get(&mangled_symbol) {
                    let signature = &self.signatures[id.0 as usize];

                    if signature.params.len() != args.len() {
                        return Err(HirError {
                            kind: HirErrorKind::ArityMismatch {
                                name: format!("{qualifier}::{name}"),
                                expected: signature.params.len(),
                                found: args.len(),
                            },
                            span: *span,
                        });
                    }

                    let mut lowered_args = Vec::with_capacity(args.len());
                    for (expr, &param_type) in args.iter().zip(signature.params.iter()) {
                        let expr = self.lower_expr(expr, Some(param_type))?;
                        self.assert_type(param_type, expr.typ, expr.span)?;
                        lowered_args.push(expr);
                    }

                    return Ok(Expression {
                        typ: signature.return_type,
                        span: *span,
                        kind: ExpressionKind::Call {
                            function: id,
                            args: lowered_args,
                        },
                    });
                }

                if let Ok(intrinsic) = Intrinsic::from_str(name) {
                    return self.lower_intrinsic(intrinsic, args, *span);
                }

                let symbol = self.symbols.insert(name);
                let id = *self.functions.get(&symbol).ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownFunction {
                        name: format!("{qualifier}::{name}"),
                    },
                    span: *span,
                })?;
                let signature = &self.signatures[id.0 as usize];

                if signature.params.len() != args.len() {
                    return Err(HirError {
                        kind: HirErrorKind::ArityMismatch {
                            name: name.to_string(),
                            expected: signature.params.len(),
                            found: args.len(),
                        },
                        span: *span,
                    });
                }

                let mut lowered_args = Vec::with_capacity(args.len());
                for (expr, &param_type) in args.iter().zip(signature.params.iter()) {
                    let expr = self.lower_expr(expr, Some(param_type))?;
                    self.assert_type(param_type, expr.typ, expr.span)?;
                    lowered_args.push(expr);
                }

                let return_type = signature.return_type;
                let kind = match signature.intrinsic {
                    Some(intrinsic) => ExpressionKind::IntrinsicCall {
                        intrinsic,
                        args: lowered_args,
                    },
                    _ => ExpressionKind::Call {
                        function: id,
                        args: lowered_args,
                    },
                };

                Ok(Expression {
                    typ: return_type,
                    span: *span,
                    kind,
                })
            }
        }
    }

    fn lower_if(
        &mut self,
        if_stmt: &statement::If<'f>,
        is_tail: bool,
    ) -> Result<(Statement, bool), HirError<'f>> {
        let condition = self.lower_expr(&if_stmt.condition, None)?;
        self.assert_type(Type::Bool, condition.typ, condition.span)?;

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

                    Else::Expr(expr) => {
                        let hint = match is_tail && self.return_type != Type::Unit {
                            true => Some(self.return_type),
                            _ => None,
                        };
                        let lowered = self.lower_expr(expr, hint)?;
                        let span = lowered.span;

                        let stmt = match is_tail && self.return_type != Type::Unit {
                            true => {
                                self.assert_type(self.return_type, lowered.typ, lowered.span)?;
                                Statement::Return(Some(lowered))
                            }
                            _ => Statement::Expr(lowered),
                        };

                        let returns = is_tail && self.return_type != Type::Unit;
                        let block = Block {
                            statements: vec![stmt],
                            span,
                        };
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

    fn lower_intrinsic(
        &mut self,
        intrinsic: Intrinsic,
        args: &[expression::Expression<'f>],
        span: Span,
    ) -> Result<Expression, HirError<'f>> {
        let args = args
            .iter()
            .map(|arg| self.lower_expr(arg, None))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Expression {
            typ: Type::Unit,
            span,
            kind: ExpressionKind::IntrinsicCall { intrinsic, args },
        })
    }

    fn type_for_binary(
        &self,
        operator: &BinaryOperator,
        left: Type,
        right: Type,
        span: Span,
    ) -> Result<Type, HirError<'f>> {
        match operator {
            BinaryOperator::Add
            | BinaryOperator::Sub
            | BinaryOperator::Mul
            | BinaryOperator::Div => {
                self.assert_type(left, right, span)?;
                match left.is_number() {
                    true => Ok(left),
                    _ => Err(HirError {
                        kind: HirErrorKind::TypeMismatch {
                            expected: Type::I32,
                            found: left,
                        },
                        span,
                    }),
                }
            }

            BinaryOperator::Eq | BinaryOperator::Ne => {
                self.assert_type(left, right, span)?;
                Ok(Type::Bool)
            }

            BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq => {
                self.assert_type(left, right, span)?;
                match left.is_number() {
                    true => Ok(Type::Bool),
                    _ => Err(HirError {
                        kind: HirErrorKind::TypeMismatch {
                            expected: Type::I32,
                            found: left,
                        },
                        span,
                    }),
                }
            }

            BinaryOperator::And | BinaryOperator::Or => {
                self.assert_type(Type::Bool, left, span)?;
                self.assert_type(Type::Bool, right, span)?;

                Ok(Type::Bool)
            }
        }
    }

    #[inline(always)]
    fn infer(&mut self, expr: &expression::Expression<'f>) -> Result<Type, HirError<'f>> {
        let expr = self.lower_expr(expr, None)?;
        Ok(expr.typ)
    }

    #[inline(always)]
    fn resolve_type(
        &mut self,
        typ: &statement::Type<'f>,
        span: Span,
    ) -> Result<Type, HirError<'f>> {
        match typ {
            statement::Type::Named(name) => {
                let symbol = self.symbols.insert(name);
                let id = self.struct_map.get(&symbol).copied().ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownType {
                        name: name.to_string(),
                    },
                    span,
                })?;

                Ok(Type::Struct(id))
            }

            typ => Ok(typ.into()),
        }
    }

    #[inline(always)]
    #[must_use]
    fn assert_type(&self, expected: Type, found: Type, span: Span) -> Result<(), HirError<'f>> {
        match expected == found {
            true => Ok(()),
            false => Err(HirError {
                kind: HirErrorKind::TypeMismatch { expected, found },
                span,
            }),
        }
    }

    fn declare_local(
        &mut self,
        name: SymbolId,
        typ: Type,
        mutable: bool,
    ) -> Result<LocalId, HirError<'f>> {
        let scope = self.scopes.last_mut().expect("at least one scope is always present");

        if scope.contains_key(&name) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateBind {
                    name: self.symbols.get(name).to_string(),
                },
                span: Span::default(),
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
    fn resolve_local(&mut self, name: SymbolId, span: Span) -> Result<LocalId, HirError<'f>> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name).copied())
            .ok_or_else(|| HirError {
                kind: HirErrorKind::UndeclaredIdentifier {
                    name: self.symbols.get(name).to_string(),
                },
                span,
            })
    }

    /// this is used to resolve chain like `r.field.other_field` into
    /// `(LocalId(r), [sym("field"), sym("other_field")], Type::I32)`
    ///
    /// returns the origin `LocalId`, the full path as symbols and the final field's `Type`
    fn resolve_field_chain(
        &mut self,
        mut expr: &expression::Expression,
        last: &'f str,
        span: Span,
    ) -> Result<(LocalId, Vec<SymbolId>, Type), HirError<'f>> {
        use expression::Expression as Expr;

        let mut fields = vec![last];

        let id = loop {
            match expr {
                Expr::Identifier(name, ident_span) => {
                    let symbol = self.symbols.insert(name);
                    break self.resolve_local(symbol, *ident_span)?;
                }

                Expr::Field {
                    expr: next, field, ..
                } => {
                    fields.push(field);
                    expr = next;
                }

                _ => {
                    return Err(HirError {
                        kind: HirErrorKind::InvalidFieldAccess,
                        span,
                    });
                }
            }
        };

        fields.reverse();

        let mut current_type = self[id].typ;
        let mut field_symbols = Vec::with_capacity(fields.len());

        for (idx, &field_name) in fields.iter().enumerate() {
            let (Type::Struct(id) | Type::Ref { to: id, .. }) = current_type else {
                return Err(HirError {
                    kind: HirErrorKind::TypeMismatch {
                        expected: Type::Struct(StructId::default()),
                        found: current_type,
                    },
                    span,
                });
            };

            let field = self.symbols.insert(field_name);
            let struct_def = &self[id];
            let name = self.symbols.get(struct_def.name);

            let field_def = {
                struct_def.fields.iter().find(|f| f.name == field).ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownField {
                        struct_name: name.to_string(),
                        field: field_name.to_string(),
                    },
                    span,
                })
            }?;

            current_type = field_def.typ;
            field_symbols.push(field);

            let is_last = idx == fields.len() - 1;
            if !is_last {
                if !matches!(current_type, Type::Struct(_) | Type::Ref { .. }) {
                    return Err(HirError {
                        kind: HirErrorKind::TypeMismatch {
                            expected: Type::Struct(StructId::default()),
                            found: current_type,
                        },
                        span,
                    });
                }
            }
        }

        Ok((id, field_symbols, current_type))
    }

    #[inline(always)]
    fn resolve_receiver_chain(
        &mut self,
        expr: &expression::Expression<'f>,
        span: Span,
    ) -> Result<(LocalId, Vec<SymbolId>, Type), HirError<'f>> {
        use expression::Expression as Expr;

        match expr {
            Expr::Identifier(name, ident_span) => {
                let symbol = self.symbols.insert(name);
                let local = self.resolve_local(symbol, *ident_span)?;
                Ok((local, Vec::new(), self[local].typ))
            }

            Expr::Field { expr, field, .. } => self.resolve_field_chain(expr, field, span),

            _ => Err(HirError {
                kind: HirErrorKind::InvalidFieldAccess,
                span,
            }),
        }
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

fn lower_struct<'h>(
    id: usize,
    declarations: &[(SymbolId, &statement::Struct<'h>)],
    map: &Structs,
    symbols: &mut SymbolTable,
    lowered: &mut [Option<Struct>],
    states: &mut [Visit],
) -> Result<(), HirError<'h>> {
    match states[id] {
        Visit::Visited => return Ok(()),
        Visit::Visiting => {
            let (_, declaration) = declarations[id];
            return Err(HirError {
                kind: HirErrorKind::CircularStruct {
                    name: declaration.name.to_string(),
                },
                span: declaration.span,
            });
        }
        Visit::Unvisited => {}
    }

    states[id] = Visit::Visiting;
    let (name, declaration) = declarations[id];
    let mut seen = HashSet::new();
    let mut fields = Vec::with_capacity(declaration.fields.len());

    for (idx, field) in declaration.fields.iter().enumerate() {
        let field_symbol = symbols.insert(field.name);
        if !seen.insert(field_symbol) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateField {
                    name: field.name.to_string(),
                },
                span: field.span,
            });
        }

        let typ = resolve_annotation(symbols, map, &field.typ.value(), field.typ.span())?;
        if let Type::Struct(dep) = typ {
            lower_struct(dep.0 as usize, declarations, map, symbols, lowered, states)?;
        }

        fields.push(StructField {
            name: field_symbol,
            typ,
            offset: 0,
            declared_index: idx as u32,
        });
    }

    let (fields, size, align) = layout_fields(fields, lowered);
    lowered[id] = Some(Struct {
        id: StructId(id as u32),
        name,
        fields,
        size,
        align,
    });
    states[id] = Visit::Visited;

    Ok(())
}

pub fn collect_function_signatures<'h>(
    decls: &Declarations<'h>,
    symbols: &mut SymbolTable,
    struct_map: &Structs,
) -> Result<(Vec<FunctionSignature>, Functions, Methods), HirError<'h>> {
    let mut signatures = Vec::new();
    let mut functions = Functions::new();
    let mut methods = Methods::new();

    for function in &decls.functions {
        if function.receiver.is_some() {
            return Err(HirError {
                kind: HirErrorKind::ReceiverOutsideImpl,
                span: function.span,
            });
        }

        let symbol = symbols.insert(function.name);
        if functions.contains_key(&symbol) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateFunction {
                    name: function.name.to_string(),
                },
                span: function.span,
            });
        }

        let function_id = FunctionId(signatures.len() as u32);
        functions.insert(symbol, function_id);

        let params = function
            .params
            .iter()
            .map(|p| resolve_annotation(symbols, struct_map, &p.typ.value(), p.typ.span()))
            .collect::<Result<Vec<_>, _>>()?;
        let return_type = function
            .return_type
            .as_ref()
            .map(|s| resolve_annotation(symbols, struct_map, &s.value(), s.span()))
            .transpose()?
            .unwrap_or(Type::Unit);

        let name_str = symbols.get(symbol);
        let intrinsic = Intrinsic::from_str(name_str).ok();

        signatures.push(FunctionSignature {
            return_type,
            params,
            name: symbol,
            intrinsic,
            method: None,
        })
    }

    for implementation in &decls.impls {
        let struct_symbol = symbols.insert(implementation.name);
        let struct_id = struct_map.get(&struct_symbol).copied().ok_or_else(|| HirError {
            kind: HirErrorKind::UnknownType {
                name: implementation.name.to_string(),
            },
            span: implementation.span,
        })?;

        for method in &implementation.methods {
            let method_symbol = symbols.insert(method.name);
            let mangled = symbols.insert(&format!("{}__{}", implementation.name, method.name));

            match method.receiver {
                Some(receiver) => {
                    if methods.contains_key(&(struct_id, method_symbol)) {
                        return Err(HirError {
                            kind: HirErrorKind::DuplicateMethod {
                                struct_name: implementation.name.to_string(),
                                name: method.name.to_string(),
                            },
                            span: method.span,
                        });
                    }

                    let function_id = FunctionId(signatures.len() as u32);
                    methods.insert((struct_id, method_symbol), function_id);

                    let mut params = Vec::with_capacity(method.params.len() + 1);
                    params.push(Type::Ref {
                        mutable: receiver.mutable,
                        to: struct_id,
                    });
                    params.extend(
                        method
                            .params
                            .iter()
                            .map(|p| {
                                resolve_annotation(
                                    symbols,
                                    struct_map,
                                    &p.typ.value(),
                                    p.typ.span(),
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    let return_type = method
                        .return_type
                        .as_ref()
                        .map(|s| resolve_annotation(symbols, struct_map, &s.value(), s.span()))
                        .transpose()?
                        .unwrap_or(Type::Unit);

                    signatures.push(FunctionSignature {
                        return_type,
                        params,
                        name: mangled,
                        intrinsic: None,
                        method: Some(Method {
                            receiver: struct_id,
                            name: method_symbol,
                            mutable: receiver.mutable,
                        }),
                    });
                }

                None => {
                    if functions.contains_key(&mangled) {
                        return Err(HirError {
                            kind: HirErrorKind::DuplicateFunction {
                                name: format!("{}::{}", implementation.name, method.name),
                            },
                            span: method.span,
                        });
                    }

                    let function_id = FunctionId(signatures.len() as u32);
                    functions.insert(mangled, function_id);

                    let params = method
                        .params
                        .iter()
                        .map(|p| {
                            resolve_annotation(symbols, struct_map, &p.typ.value(), p.typ.span())
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let return_type = method
                        .return_type
                        .as_ref()
                        .map(|s| resolve_annotation(symbols, struct_map, &s.value(), s.span()))
                        .transpose()?
                        .unwrap_or(Type::Unit);

                    signatures.push(FunctionSignature {
                        return_type,
                        params,
                        name: mangled,
                        intrinsic: None,
                        method: None,
                    });
                }
            }
        }
    }

    Ok((signatures, functions, methods))
}

pub(in crate::hir) fn collect_interfaces<'h>(
    decls: &Declarations<'h>,
    symbols: &mut SymbolTable,
) -> Result<Interfaces, HirError<'h>> {
    let mut interfaces = Interfaces::new();

    for declaration in &decls.interfaces {
        let name = symbols.insert(declaration.name);
        if interfaces.contains_key(&name) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateInterface {
                    name: declaration.name.to_string(),
                },
                span: declaration.span,
            });
        }

        let methods = declaration
            .methods
            .iter()
            .map(|method| {
                let name = symbols.insert(method.name);
                let has_receiver = method.receiver.is_some();
                let receiver_mut =
                    method.receiver.map(|receiver| receiver.mutable).unwrap_or(false);

                let mut params = Vec::with_capacity(method.params.len());

                if has_receiver {
                    params.push(Type::Unit);
                }

                for param in &method.params {
                    let typ = param.typ.value();
                    params.push(Type::from(&typ));
                }

                let return_type = match &method.return_type {
                    Some(typ) => Type::from(&typ.value()),
                    _ => Type::Unit,
                };

                InterfaceMethodSignature {
                    name,
                    params,
                    return_type,
                    has_receiver,
                    receiver_mut,
                }
            })
            .collect();

        interfaces.insert(name, InterfaceSignature { name, methods });
    }

    Ok(interfaces)
}

pub(in crate::hir) fn validate_interface_impls<'h>(
    statements: &[statement::Statement<'h>],
    interfaces: &Interfaces,
    methods: &Methods,
    struct_map: &Structs,
    symbols: &mut SymbolTable,
) -> Result<(), HirError<'h>> {
    for statement in statements {
        let statement::Statement::Impl(implementation) = statement else {
            continue;
        };

        let Some(interface_name) = implementation.interface else {
            continue;
        };

        let interface = symbols.insert(interface_name);
        let interface = interfaces.get(&interface).ok_or_else(|| HirError {
            kind: HirErrorKind::UnknownInterface {
                name: interface_name.to_string(),
            },
            span: implementation.span,
        })?;

        let struct_symbol = symbols.insert(implementation.name);
        let struct_id = *struct_map.get(&struct_symbol).expect("impl struct must exists");

        for required_method in &interface.methods {
            if !methods.contains_key(&(struct_id, required_method.name)) {
                let method_name = symbols.get(required_method.name).to_string();

                return Err(HirError {
                    kind: HirErrorKind::MissingInterfaceMethod {
                        struct_name: implementation.name.to_string(),
                        interface_name: interface_name.to_string(),
                        method_name,
                    },
                    span: implementation.span,
                });
            }
        }
    }

    Ok(())
}

pub(in crate::hir) fn function_id_for<'h>(
    function: &statement::Function<'h>,
    functions: &Functions,
    methods: &Methods,
    struct_map: &Structs,
    symbols: &mut SymbolTable,
) -> Result<FunctionId, HirError<'h>> {
    match (function.receiver, function.impl_type) {
        (Some(_), _) => {
            let impl_type = function.impl_type.expect("method must know its impl owner");
            let struct_symbol = symbols.insert(impl_type);
            let struct_id = *struct_map.get(&struct_symbol).ok_or_else(|| HirError {
                kind: HirErrorKind::UnknownType {
                    name: impl_type.to_string(),
                },
                span: function.span,
            })?;
            let method_symbol = symbols.insert(function.name);

            methods.get(&(struct_id, method_symbol)).copied().ok_or_else(|| HirError {
                kind: HirErrorKind::UnknownFunction {
                    name: function.name.to_string(),
                },
                span: function.span,
            })
        }
        (None, Some(impl_type)) => {
            let mangled = symbols.insert(&format!("{impl_type}__{}", function.name));
            functions.get(&mangled).copied().ok_or_else(|| HirError {
                kind: HirErrorKind::UnknownFunction {
                    name: format!("{}::{}", impl_type, function.name),
                },
                span: function.span,
            })
        }
        (None, None) => {
            let symbol = symbols.insert(function.name);
            functions.get(&symbol).copied().ok_or_else(|| HirError {
                kind: HirErrorKind::UnknownFunction {
                    name: function.name.to_string(),
                },
                span: function.span,
            })
        }
    }
}

pub fn collect_structs<'h>(
    decls: &Declarations<'h>,
    symbols: &mut SymbolTable,
) -> Result<(Vec<Struct>, Structs), HirError<'h>> {
    let mut map = Structs::new();
    let mut declarations = Vec::new();

    for declaration in &decls.structs {
        let symbol = symbols.insert(declaration.name);
        if map.contains_key(&symbol) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateStruct {
                    name: declaration.name.to_string(),
                },
                span: declaration.span,
            });
        }

        let id = StructId(declarations.len() as u32);
        map.insert(symbol, id);
        declarations.push((symbol, *declaration));
    }

    let mut lowered = vec![None; declarations.len()];
    let mut states = vec![Visit::Unvisited; declarations.len()];

    for id in 0..declarations.len() {
        lower_struct(id, &declarations, &map, symbols, &mut lowered, &mut states)?;
    }

    let structs = lowered
        .into_iter()
        .map(|definition| definition.expect("all struct definitions are lowered"))
        .collect();

    Ok((structs, map))
}

fn resolve_annotation<'h>(
    symbols: &mut SymbolTable,
    struct_map: &Structs,
    typ: &statement::Type<'h>,
    span: Span,
) -> Result<Type, HirError<'h>> {
    match typ {
        statement::Type::Named(name) => {
            let symbol = symbols.insert(name);
            struct_map.get(&symbol).copied().map(Type::Struct).ok_or_else(|| HirError {
                kind: HirErrorKind::UnknownType {
                    name: (*name).to_string(),
                },
                span,
            })
        }
        typ => Ok(typ.into()),
    }
}

fn layout_fields(
    mut fields: Vec<StructField>,
    structs: &[Option<Struct>],
) -> (Vec<StructField>, u32, u32) {
    // PERFORMANCE: field reordering is a small stable sort by layout class
    fields.sort_by(|a, b| {
        let (a_size, a_align) = &a.typ.layout(structs);
        let (b_size, b_align) = &b.typ.layout(structs);

        b_align
            .cmp(&a_align)
            .then_with(|| b_size.cmp(&a_size))
            .then_with(|| a.declared_index.cmp(&b.declared_index))
    });

    let mut offset = 0;
    let mut struct_align = 1;

    for field in &mut fields {
        let (size, align) = field.typ.layout(structs);

        struct_align = struct_align.max(align);
        offset = align_to(offset, align);
        field.offset = offset;
        offset += size;
    }

    let size = align_to(offset, struct_align);

    (fields, size, struct_align)
}

#[inline(always)]
const fn align_to(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}

pub fn signatures_from_hir(functions: &[Function]) -> (Vec<FunctionSignature>, Functions, Methods) {
    let mut signatures = Vec::with_capacity(functions.len());
    let mut map = Functions::new();
    let mut methods = Methods::new();

    for (idx, function) in functions.iter().enumerate() {
        let id = FunctionId(idx as u32);
        match function.method {
            Some(method) => {
                methods.insert((method.receiver, method.name), id);
            }
            None => {
                map.insert(function.name, id);
            }
        }

        signatures.push(function.into());
    }

    (signatures, map, methods)
}

impl<'s, 'f> Index<LocalId> for FunctionBuilder<'s, 'f> {
    type Output = Local;

    fn index(&self, index: LocalId) -> &Self::Output {
        &self.locals[index.0 as usize]
    }
}

impl<'s, 'f> Index<StructId> for FunctionBuilder<'s, 'f> {
    type Output = Struct;

    fn index(&self, index: StructId) -> &Self::Output {
        &self.structs[index.0 as usize]
    }
}
