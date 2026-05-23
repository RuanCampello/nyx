use crate::{
    hir::{
        Block, Constant, Expression, ExpressionKind, Function, FunctionId, Intrinsic, Local,
        LocalId, Parameter, Receiver, RefTarget, Statement, Struct, StructField, StructId,
        SymbolId, SymbolTable, SyscallCode, Type,
        error::{ConstFnViolationKind, HirError, HirErrorKind, hir_error},
        mangle::Mangler,
        scope::{self, Scope, Structs},
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

pub(in crate::hir) struct FunctionBuilder<'s, 'f, 'src> {
    scope: &'s Scope<'s>,
    locals: Vec<Local>,
    scopes: Vec<HashMap<SymbolId, LocalId>>,
    return_type: Type,
    function: Option<&'f statement::Function<'src>>,
    function_id: FunctionId,
    next_local: u32,
    symbols: &'s mut SymbolTable,
    is_const: bool,
    in_std: bool,
    self_type: Option<Type>,
}

impl<'s, 'f, 'src> FunctionBuilder<'s, 'f, 'src> {
    pub fn new(
        scope: &'s Scope,
        symbols: &'s mut SymbolTable,
        function_id: FunctionId,
        function: &'f statement::Function<'src>,
        in_std: bool,
    ) -> Self {
        let self_type = function.impl_type.and_then(|impl_type| {
            scope::resolve_primitive_type(impl_type).or_else(|| {
                let struct_symbol = symbols.get_id(impl_type)?;
                scope.struct_map.get(&struct_symbol).copied().map(Type::Struct)
            })
        });

        Self {
            scope,
            symbols,
            is_const: function.is_const,
            in_std,
            return_type: Type::Unit,
            function: Some(function),
            function_id,
            next_local: 0,
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            self_type,
        }
    }

    pub fn new_for_const(scope: &'s Scope, symbols: &'s mut SymbolTable, in_std: bool) -> Self {
        Self {
            scope,
            symbols,
            is_const: true,
            in_std,
            return_type: Type::Unit,
            function: None,
            function_id: FunctionId(0),
            next_local: 0,
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            self_type: None,
        }
    }

    #[inline(always)]
    pub fn lower(mut self) -> Result<Function, HirError<'src>> {
        let function = self.function.take().expect("function to be present");
        let id = self.function_id;
        let signatures = &self.scope.signatures[id.0 as usize];
        let symbol = signatures.name;
        self.return_type = signatures.return_type;

        let mut params = Vec::with_capacity(signatures.params.len());

        if let Some(receiver) = function.receiver {
            let typ = signatures.params[0];
            let symbol = self.symbols.insert("self");
            let id = self.declare_local(symbol, typ, receiver.mutable)?;
            params.push(Parameter { typ, id, name: symbol, mutable: receiver.mutable });
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

                    Ok(Parameter { typ, id, name: symbol, mutable: parameter.mutable })
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
            inline: function.inline,
            intrinsic: signatures.intrinsic,
            method: signatures.method,
            body,
        })
    }

    fn lower_block(
        &mut self,
        block: &statement::Block<'src>,
        is_tail: bool,
    ) -> Result<(Block, bool), HirError<'src>> {
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
        Ok((Block { statements, span: block.span }, returns))
    }

    fn lower_statement(
        &mut self,
        statement: &statement::Statement<'src>,
        is_tail: bool,
    ) -> Result<(Statement, bool), HirError<'src>> {
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
            Stmt::Const(_) => unimplemented!("local constants are not supported yet"),
        }
    }

    fn lower_identifier(&mut self, name: &str, span: Span) -> Result<Expression, HirError<'src>> {
        if let Some(id) = self.local_id(name) {
            return Ok(self.local_expr(id, span));
        }

        if let Some(constant) = self.constant(name) {
            let mut value = constant.value.clone();
            value.span = span;

            return Ok(value);
        }

        let symbol = self.symbols.get_id(name).ok_or_else(|| HirError {
            kind: HirErrorKind::UndeclaredIdentifier { name: name.to_string() },
            span,
        })?;
        let id = self.resolve_local(symbol, span)?;

        Ok(self.local_expr(id, span))
    }

    fn local_id(&self, name: &str) -> Option<LocalId> {
        let symbol = self.symbols.get_id(name)?;

        self.scopes.iter().rev().find_map(|scope| scope.get(&symbol).copied())
    }

    fn local_expr(&self, id: LocalId, span: Span) -> Expression {
        Expression { kind: ExpressionKind::Local(id), typ: self[id].typ, span }
    }

    fn constant(&self, name: &str) -> Option<&Constant> {
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

    fn constant_by_symbol_name(&self, name: &str) -> Option<&Constant> {
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
    ) -> Result<Expression, HirError<'src>> {
        use expression::Expression as Expr;

        match expr {
            // literal coercion: use the hint to widen to the expected numeric type.
            Expr::Integer(value, span) => {
                let typ = match hint {
                    Some(t) if t.is_number() => t,
                    _ => Type::I32,
                };

                Ok(Expression { kind: ExpressionKind::Integer(*value), typ, span: *span })
            }

            Expr::Float(value, span) => {
                let typ = match hint {
                    Some(t) if t.is_float() => t,
                    _ => Type::F64,
                };

                Ok(Expression { kind: ExpressionKind::Float(*value), typ, span: *span })
            }

            Expr::String(value, span) => Ok(Expression {
                kind: ExpressionKind::String((*value).to_string()),
                typ: Type::String,
                span: *span,
            }),

            Expr::Char(value, span) => Ok(Expression {
                kind: ExpressionKind::Char(*value),
                typ: Type::Char,
                span: *span,
            }),

            Expr::Bool(value, span) => Ok(Expression {
                kind: ExpressionKind::Bool(*value),
                typ: Type::Bool,
                span: *span,
            }),

            Expr::Cast { expr: inner, target_type, span } => {
                let target = self.resolve_type(&target_type.value(), target_type.span())?;
                let lowered_expr = self.lower_expr(inner, None)?;
                let src = lowered_expr.typ;

                if !src.is_primitive_castable() || !target.is_primitive_castable() {
                    return Err(HirError {
                        kind: HirErrorKind::InvalidCast { src, target },
                        span: *span,
                    });
                }

                Ok(Expression {
                    kind: ExpressionKind::Cast { from: Box::new(lowered_expr), to: target },
                    typ: target,
                    span: *span,
                })
            }

            Expr::Identifier(name, span) => self.lower_identifier(name, *span),

            Expr::QualifiedName { qualifier, name, span } => {
                let mangled_name = self.mangler().scoped_item(qualifier, name);
                let symbol = match self.symbols.get_id(&mangled_name) {
                    Some(sym) => sym,
                    None => {
                        return Err(HirError {
                            kind: HirErrorKind::UndeclaredIdentifier {
                                name: format!("{qualifier}::{name}"),
                            },
                            span: *span,
                        });
                    }
                };

                let Some(c) = self.scope.constants.get(&symbol) else {
                    return Err(HirError {
                        kind: HirErrorKind::UndeclaredIdentifier {
                            name: format!("{qualifier}::{name}"),
                        },
                        span: *span,
                    });
                };

                let mut val = c.value.clone();
                val.span = *span;
                Ok(val)
            }

            Expr::Unary { operator, expr, span } => {
                // for negation the hint flows through to the operand
                let inner_hint = match operator {
                    UnaryOperator::Neg => hint,
                    UnaryOperator::Not => hint,
                    UnaryOperator::Deref => hint.map(|h| Type::Ref {
                        mutable: false,
                        to: match h {
                            Type::Struct(id) => RefTarget::Struct(id),
                            Type::Char => RefTarget::Char,
                            Type::Ref { to, .. } => to,
                            _ => RefTarget::Char,
                        },
                    }),
                    UnaryOperator::Ref => hint.map(|h| h.strip_reference()),
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
                    UnaryOperator::Not => match expr.typ == Type::Bool || expr.typ.is_integer() {
                        true => expr.typ,
                        _ => {
                            return Err(HirError {
                                kind: HirErrorKind::TypeMismatch {
                                    expected: Type::Bool,
                                    found: expr.typ,
                                },
                                span: expr.span,
                            });
                        }
                    },
                    UnaryOperator::Deref => match expr.typ {
                        Type::Ref { to, .. } => to.into(),
                        _ => {
                            return Err(HirError {
                                kind: HirErrorKind::TypeMismatch {
                                    expected: Type::Ref { mutable: false, to: RefTarget::Char },
                                    found: expr.typ,
                                },
                                span: expr.span,
                            });
                        }
                    },
                    UnaryOperator::Ref => {
                        let to = RefTarget::try_from(expr.typ).map_err(|_| HirError {
                            kind: HirErrorKind::TypeMismatch {
                                expected: Type::Struct(StructId::default()),
                                found: expr.typ,
                            },
                            span: expr.span,
                        })?;
                        Type::Ref { mutable: false, to }
                    }
                };

                if *operator != UnaryOperator::Deref && *operator != UnaryOperator::Ref {
                    self.assert_type(expected, expr.typ, expr.span)?;
                }

                Ok(Expression {
                    typ: expected,
                    span: *span,
                    kind: ExpressionKind::Unary { operator: *operator, expr: Box::new(expr) },
                })
            }

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
                    | BinaryOperator::BitXor => Some(left.typ),
                    BinaryOperator::And | BinaryOperator::Or => Some(Type::Bool),
                    BinaryOperator::Shl | BinaryOperator::Shr => None,
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

            Expr::Assignment { target, value, span } => {
                let (local, fields, typ) =
                    self.resolve_access_chain(target, *span).map_err(|err| {
                        if matches!(err.kind, HirErrorKind::InvalidFieldAccess) {
                            HirError { kind: HirErrorKind::InvalidAssignmentTarget, span: *span }
                        } else {
                            err
                        }
                    })?;

                if !self[local].mutable {
                    let err_span = if fields.is_empty() {
                        *span
                    } else {
                        target.span()
                    };
                    return Err(HirError {
                        kind: HirErrorKind::ImmutableBind {
                            name: self.symbols.get(self[local].name).to_string(),
                        },
                        span: err_span,
                    });
                }

                let value = self.lower_expr(value, Some(typ))?;
                self.assert_type(typ, value.typ, *span)?;

                if fields.is_empty() {
                    Ok(Expression {
                        kind: ExpressionKind::Assign { target: local, value: Box::new(value) },
                        typ,
                        span: *span,
                    })
                } else {
                    Ok(Expression {
                        kind: ExpressionKind::FieldAssign { local, fields, value: Box::new(value) },
                        typ,
                        span: *span,
                    })
                }
            }

            Expr::Struct { name, fields, span } => {
                let symbol = self.symbols.insert(name);
                let id = self.scope.struct_map.get(&symbol).copied().ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownType { name: (*name).to_string() },
                    span: *span,
                })?;
                let definition = &self.scope.structs[id.0 as usize];
                let struct_name = self.symbols.get(definition.name).to_string();

                let mut seen = HashSet::with_capacity(fields.len());
                let mut lowered = Vec::with_capacity(fields.len());

                for field in fields {
                    let field_symbol = self.symbols.insert(field.name);
                    if !seen.insert(field_symbol) {
                        return Err(HirError {
                            kind: HirErrorKind::DuplicateField { name: field.name.to_string() },
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
                    kind: ExpressionKind::Struct { id, fields: lowered },
                })
            }

            Expr::Field { span, .. } => {
                let (local, fields, typ) = self.resolve_access_chain(expr, *span)?;

                Ok(Expression {
                    kind: ExpressionKind::FieldAccess { local, fields },
                    typ,
                    span: *span,
                })
            }

            Expr::Call { callee, args, span } => {
                if let Expr::Field { expr: receiver, field: method_name, .. } = callee.as_ref() {
                    let (local, fields, receiver_expr, receiver_type) =
                        match self.resolve_access_chain(receiver, *span) {
                            Ok((loc, flds, typ)) => (Some(loc), flds, None, typ),
                            _ => {
                                let lowered = self.lower_expr(receiver, None)?;
                                let typ = lowered.typ;
                                (None, Vec::new(), Some(Box::new(lowered)), typ)
                            }
                        };

                    let receiver_base_type = receiver_type.strip_reference();
                    let method_symbol = self.symbols.insert(method_name);
                    let struct_name = match receiver_base_type {
                        Type::Struct(sid) => self.symbols.get(self[sid].name).to_string(),
                        other => other.to_string(),
                    };
                    let function =
                        *self.scope.methods.get(&(receiver_base_type, method_symbol)).ok_or_else(
                            || HirError {
                                kind: HirErrorKind::UnknownMethod {
                                    struct_name,
                                    name: method_name.to_string(),
                                },
                                span: *span,
                            },
                        )?;

                    let signature = &self.scope.signatures[function.0 as usize];
                    let Type::Ref { mutable, .. } = signature.params[0] else {
                        unreachable!("method signature must start with receiver reference");
                    };

                    if mutable {
                        if local.filter(|&id| self[id].mutable).is_none() {
                            let name = match local {
                                Some(id) => self.symbols.get(self[id].name).to_string(),
                                None => "temporary".to_string(),
                            };

                            return Err(HirError {
                                kind: HirErrorKind::ImmutableBind { name },
                                span: *span,
                            });
                        }
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
                                value: receiver_expr,
                                typ: signature.params[0],
                            },
                            args: lowered_args,
                        },
                    });
                }

                let function_id = match callee.as_ref() {
                    Expr::Identifier(name, _) => {
                        let symbol = self.symbols.insert(&self.mangler().item(name));
                        *self.scope.functions.get(&symbol).ok_or_else(|| HirError {
                            kind: HirErrorKind::UnknownFunction { name: name.to_string() },
                            span: *span,
                        })?
                    }

                    other => {
                        return Err(HirError {
                            kind: HirErrorKind::UnknownFunction { name: format!("{other:?}") },
                            span: *span,
                        });
                    }
                };

                self.lower_direct_call(function_id, args, *span)
            }

            Expr::QualifiedCall { qualifier, name, args, span } => {
                let mangled_name = self.mangler().scoped_item(qualifier, name);
                let mangled_symbol = self.symbols.insert(&mangled_name);

                let id = self.scope.functions.get(&mangled_symbol).copied().or_else(|| {
                    let receiver_type = match scope::resolve_primitive_type(qualifier) {
                        Some(primitive) => primitive,
                        _ => {
                            let struct_symbol = self.symbols.insert(qualifier);
                            let struct_id = *self.scope.struct_map.get(&struct_symbol)?;
                            Type::Struct(struct_id)
                        }
                    };

                    self.scope
                        .interface_impls
                        .iter()
                        .filter(|&&(t, _)| t == receiver_type)
                        .find_map(|&(_, interface_sym)| {
                            let interface_name = self.symbols.get(interface_sym);
                            let interface_mangled = self.symbols.insert(
                                &self.mangler().interface_item(qualifier, interface_name, name),
                            );
                            self.scope.functions.get(&interface_mangled).copied()
                        })
                });

                if let Some(id) = id {
                    return self.lower_direct_call(id, args, *span);
                }

                let symbol = self.symbols.insert(&self.mangler().item(name));
                let id = *self.scope.functions.get(&symbol).ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownFunction { name: format!("{qualifier}::{name}") },
                    span: *span,
                })?;

                self.lower_direct_call(id, args, *span)
            }

            Expr::TypeIntrinsic { kind, qualifier, typ, span } => {
                let name = kind.into();
                let lookup_symbol = match qualifier {
                    Some(q) => {
                        let mangled = self.symbols.insert(&self.mangler().scoped_item(q, name));
                        match self.scope.functions.contains_key(&mangled) {
                            true => mangled,
                            _ => self.symbols.insert(&self.mangler().item(name)),
                        }
                    }
                    None => self.symbols.insert(&self.mangler().item(name)),
                };

                if !self.scope.functions.contains_key(&lookup_symbol) {
                    return Err(HirError {
                        kind: HirErrorKind::UnknownFunction {
                            name: match qualifier {
                                Some(q) => format!("{q}::{name}"),
                                None => name.to_string(),
                            },
                        },
                        span: *span,
                    });
                }

                let typ = resolve_annotation(
                    self.symbols,
                    &self.scope.struct_map,
                    &typ.value(),
                    typ.span(),
                    None,
                )?;

                let structs: Vec<Option<Struct>> =
                    self.scope.structs.iter().map(|s| Some(s.clone())).collect();
                let (size, align) = typ.layout(&structs);

                let value = match kind {
                    expression::TypeIntrinsicKind::SizeOf => size as i64,
                    expression::TypeIntrinsicKind::AlignOf => align as i64,
                };

                Ok(Expression {
                    kind: ExpressionKind::Integer(value),
                    typ: Type::Uptr,
                    span: *span,
                })
            }
        }
    }

    fn lower_if(
        &mut self,
        if_stmt: &statement::If<'src>,
        is_tail: bool,
    ) -> Result<(Statement, bool), HirError<'src>> {
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
                        let block = Block { span: block.span, statements: vec![statement] };

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
                        let block = Block { statements: vec![stmt], span };
                        Ok((Some(block), returns))
                    }
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
        span: Span,
    ) -> Result<Expression, HirError<'src>> {
        let signature = &self.scope.signatures[function_id.0 as usize];

        if self.is_const && !signature.is_const && signature.intrinsic.is_none() {
            return Err(HirError {
                kind: HirErrorKind::ConstFnViolation(ConstFnViolationKind::NonConstCall {
                    name: self.symbols.get(signature.name).to_string(),
                }),
                span,
            });
        }

        if signature.intrinsic == Some(Intrinsic::Syscall) {
            return self.lower_syscall(args, signature.return_type, span);
        }

        if signature.intrinsic.is_none() && signature.params.len() != args.len() {
            return Err(HirError {
                kind: HirErrorKind::ArityMismatch {
                    name: self.symbols.get(signature.name).to_string(),
                    expected: signature.params.len(),
                    found: args.len(),
                },
                span,
            });
        }

        let mut lowered_args = Vec::with_capacity(args.len());
        match signature.intrinsic {
            Some(_) => {
                for arg in args {
                    lowered_args.push(self.lower_expr(arg, None)?);
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

        let kind = match signature.intrinsic {
            Some(intrinsic) => ExpressionKind::IntrinsicCall { intrinsic, args: lowered_args },
            _ => ExpressionKind::Call { function: function_id, args: lowered_args },
        };

        Ok(Expression { typ: signature.return_type, span, kind })
    }

    fn lower_syscall(
        &mut self,
        args: &[expression::Expression<'src>],
        return_type: Type,
        span: Span,
    ) -> Result<Expression, HirError<'src>> {
        if !self.in_std {
            return Err(HirError {
                kind: HirErrorKind::UnknownFunction { name: "syscall".to_string() },
                span,
            });
        }

        let Some((code_arg, value_args)) = args.split_first() else {
            return Err(HirError {
                kind: HirErrorKind::ArityMismatch {
                    name: "syscall".to_string(),
                    expected: 1,
                    found: 0,
                },
                span,
            });
        };

        if value_args.len() > 6 {
            return Err(HirError {
                kind: HirErrorKind::ArityMismatch {
                    name: "syscall".to_string(),
                    expected: 7,
                    found: args.len(),
                },
                span,
            });
        }

        let expression::Expression::Identifier(name, code_span) = code_arg else {
            return Err(HirError {
                kind: HirErrorKind::UndeclaredIdentifier { name: format!("{code_arg:?}") },
                span: code_arg.span(),
            });
        };

        let code = SyscallCode::from_str(name).map_err(|_| HirError {
            kind: HirErrorKind::UndeclaredIdentifier { name: name.to_string() },
            span: *code_span,
        })?;

        let args = value_args
            .iter()
            .map(|arg| self.lower_expr(arg, None))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Expression {
            typ: return_type,
            span,
            kind: ExpressionKind::Syscall { code, args },
        })
    }

    fn type_for_binary(
        &self,
        operator: &BinaryOperator,
        left: Type,
        right: Type,
        span: Span,
    ) -> Result<Type, HirError<'src>> {
        let type_mismatch = |found| HirError {
            kind: HirErrorKind::TypeMismatch { expected: Type::I32, found },
            span,
        };

        match operator {
            BinaryOperator::Add
            | BinaryOperator::Sub
            | BinaryOperator::Mul
            | BinaryOperator::Div => {
                self.assert_type(left, right, span)?;
                match left.is_number() {
                    true => Ok(left),
                    _ => Err(type_mismatch(left)),
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
                match left.is_number() || left == Type::Char {
                    true => Ok(Type::Bool),
                    _ => Err(type_mismatch(left)),
                }
            }

            BinaryOperator::And | BinaryOperator::Or => {
                self.assert_type(Type::Bool, left, span)?;
                self.assert_type(Type::Bool, right, span)?;

                Ok(Type::Bool)
            }

            BinaryOperator::BitAnd | BinaryOperator::BitOr | BinaryOperator::BitXor => {
                self.assert_type(left, right, span)?;

                match left == Type::Bool || left.is_integer() {
                    true => Ok(left),
                    _ => Err(type_mismatch(left)),
                }
            }

            BinaryOperator::Shl | BinaryOperator::Shr => {
                if !left.is_integer() {
                    return Err(type_mismatch(left));
                }

                if !right.is_integer() {
                    return Err(type_mismatch(right));
                }

                Ok(left)
            }
        }
    }

    #[inline(always)]
    fn infer(&mut self, expr: &expression::Expression<'src>) -> Result<Type, HirError<'src>> {
        let expr = self.lower_expr(expr, None)?;
        Ok(expr.typ)
    }

    #[inline(always)]
    fn resolve_type(
        &mut self,
        typ: &statement::Type<'src>,
        span: Span,
    ) -> Result<Type, HirError<'src>> {
        match typ {
            statement::Type::Named(name) => {
                let symbol = self.symbols.insert(name);
                let id = self.scope.struct_map.get(&symbol).copied().ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownType { name: name.to_string() },
                    span,
                })?;

                Ok(Type::Struct(id))
            }

            statement::Type::SelfType => self.self_type.ok_or_else(|| HirError {
                kind: HirErrorKind::UnknownType { name: "Self".to_string() },
                span,
            }),

            statement::Type::RefSelf => {
                let self_ty = self.self_type.ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownType { name: "Self".to_string() },
                    span,
                })?;
                let to = RefTarget::try_from(self_ty).map_err(|_| HirError {
                    kind: HirErrorKind::TypeMismatch {
                        expected: Type::Struct(StructId::default()),
                        found: self_ty,
                    },
                    span,
                })?;

                Ok(Type::Ref { mutable: false, to })
            }

            typ => Ok(typ.into()),
        }
    }

    #[inline(always)]
    #[must_use]
    fn assert_type(&self, expected: Type, found: Type, span: Span) -> Result<(), HirError<'src>> {
        match expected == found {
            true => Ok(()),
            false => Err(HirError { kind: HirErrorKind::TypeMismatch { expected, found }, span }),
        }
    }

    fn declare_local(
        &mut self,
        name: SymbolId,
        typ: Type,
        mutable: bool,
    ) -> Result<LocalId, HirError<'src>> {
        let scope = self.scopes.last_mut().expect("at least one scope is always present");

        if scope.contains_key(&name) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateBind { name: self.symbols.get(name).to_string() },
                span: Span::default(),
            });
        }

        let id = LocalId(self.next_local);
        self.next_local += 1;

        scope.insert(name, id);
        self.locals.push(Local { id, name, typ, mutable });

        Ok(id)
    }

    #[inline(always)]
    fn resolve_local(&mut self, name: SymbolId, span: Span) -> Result<LocalId, HirError<'src>> {
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

    /// Resolve a single field step on `current`, returning `(field_symbol, field_type)`
    fn lookup_field(
        &mut self,
        current: Type,
        name: &str,
        span: Span,
    ) -> Result<(SymbolId, Type), HirError<'src>> {
        #[rustfmt::skip]
        let sid = match current {
            Type::Struct(id) => id,
            Type::Ref { to: RefTarget::Struct(id), .. } => id,
            found => return Err(hir_error!(span, TypeMismatch {
                expected: Type::Struct(Default::default()),
                found
            })),
        };

        let sym = self.symbols.insert(name);
        let def = &self.scope.structs[sid.0 as usize];
        let struct_name = self.symbols.get(def.name).to_string();

        let field =
            def.fields.iter().find(|field| field.name == sym).ok_or_else(|| {
                hir_error!(span, UnknownField { struct_name, field: name.to_string() })
            });
        todo!()
    }

    /// This is used to resolve access chains like `x.y.z` or just `x` into
    /// `(LocalId(x), [sym("y"), sym("z")], Type)`
    ///
    /// Returns the origin `LocalId`, the full path as symbols, and the final field's `Type`.
    fn resolve_access_chain(
        &mut self,
        expr: &expression::Expression<'src>,
        span: Span,
    ) -> Result<(LocalId, Vec<SymbolId>, Type), HirError<'src>> {
        use expression::Expression as Expr;

        let mut fields = Vec::new();
        let mut curr = expr;

        let id = loop {
            match curr {
                Expr::Identifier(name, ident_span) => {
                    let symbol = self.symbols.insert(name);
                    break self.resolve_local(symbol, *ident_span)?;
                }

                Expr::Field { expr: next, field, .. } => {
                    fields.push(*field);
                    curr = next;
                }

                _ => {
                    return Err(HirError { kind: HirErrorKind::InvalidFieldAccess, span });
                }
            }
        };

        fields.reverse();

        let mut current_type = self[id].typ;
        let mut field_symbols = Vec::with_capacity(fields.len());

        for (idx, &field_name) in fields.iter().enumerate() {
            let struct_id = match current_type {
                Type::Struct(id) => id,
                Type::Ref { to: RefTarget::Struct(id), .. } => id,
                found => {
                    return Err(HirError {
                        kind: HirErrorKind::TypeMismatch {
                            expected: Type::Struct(StructId::default()),
                            found,
                        },
                        span,
                    });
                }
            };

            let field = self.symbols.insert(field_name);
            let struct_def = &self[struct_id];
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
            #[rustfmt::skip]
            let is_struct_to_struct = matches!(current_type, Type::Struct(_) | Type::Ref { to: RefTarget::Struct(_), .. });
            if !is_last && !is_struct_to_struct {
                return Err(HirError {
                    kind: HirErrorKind::TypeMismatch {
                        expected: Type::Struct(StructId::default()),
                        found: current_type,
                    },
                    span,
                });
            }
        }

        Ok((id, field_symbols, current_type))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hir) enum Visit {
    Unvisited,
    Visiting,
    Visited,
}

pub(in crate::hir) fn lower_structs<'h>(
    declarations: &[(SymbolId, &statement::Struct<'h>)],
    map: &Structs,
    symbols: &mut SymbolTable,
    lowered: &mut [Option<Struct>],
) -> Result<(), HirError<'h>> {
    // pre-insert all field names into the symbol table to avoid mutable borrows of symbols
    // during the topological sort dfs
    for (_, declaration) in declarations {
        for field in &declaration.fields {
            symbols.insert(field.name);
        }
    }

    let mut states = vec![Visit::Unvisited; declarations.len()];
    let symbols = &*symbols; // shadow as shared reference
    for id in 0..declarations.len() {
        lower_struct(id, declarations, map, symbols, lowered, &mut states)?;
    }

    Ok(())
}

pub(in crate::hir) fn lower_struct<'h>(
    id: usize,
    declarations: &[(SymbolId, &statement::Struct<'h>)],
    map: &Structs,
    symbols: &SymbolTable,
    lowered: &mut [Option<Struct>],
    states: &mut [Visit],
) -> Result<(), HirError<'h>> {
    match states[id] {
        Visit::Visited => return Ok(()),
        Visit::Visiting => {
            let (_, declaration) = declarations[id];
            return Err(HirError {
                kind: HirErrorKind::CircularStruct { name: declaration.name.to_string() },
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
        let field_symbol = symbols.get_id(field.name).unwrap();
        if !seen.insert(field_symbol) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateField { name: field.name.to_string() },
                span: field.span,
            });
        }

        let typ = resolve_annotation(symbols, map, &field.typ.value(), field.typ.span(), None)?;
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
    lowered[id] = Some(Struct { id: StructId(id as u32), name, fields, size, align });
    states[id] = Visit::Visited;

    Ok(())
}

pub(in crate::hir) fn lower_const<'s, 'src>(
    scope: &'s Scope,
    symbols: &'s mut SymbolTable,
    expr: &expression::Expression<'src>,
    expected_type: Type,
    in_std: bool,
) -> Result<Expression, HirError<'src>> {
    let mut builder = FunctionBuilder::new_for_const(scope, symbols, in_std);
    let lowered = builder.lower_expr(expr, Some(expected_type))?;

    builder.assert_type(expected_type, lowered.typ, lowered.span)?;

    Ok(lowered)
}

pub(in crate::hir) fn resolve_annotation<'h>(
    symbols: &SymbolTable,
    struct_map: &Structs,
    typ: &statement::Type<'h>,
    span: Span,
    self_type: Option<Type>,
) -> Result<Type, HirError<'h>> {
    match typ {
        statement::Type::Named(name) => symbols
            .get_id(name)
            .and_then(|symbol| struct_map.get(&symbol).copied())
            .map(Type::Struct)
            .ok_or_else(|| HirError {
                kind: HirErrorKind::UnknownType { name: name.to_string() },
                span,
            }),
        statement::Type::SelfType => Ok(self_type.unwrap_or(Type::SelfType)),
        statement::Type::RefSelf => self_type.map_or(
            Ok(Type::Ref { mutable: false, to: RefTarget::SelfType }),
            |self_typ| {
                let to = RefTarget::try_from(self_typ).map_err(|_| HirError {
                    kind: HirErrorKind::TypeMismatch {
                        expected: Type::Struct(Default::default()),
                        found: self_typ,
                    },
                    span,
                })?;

                Ok(Type::Ref { mutable: false, to })
            },
        ),
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

impl<'s, 'f, 'src> Index<LocalId> for FunctionBuilder<'s, 'f, 'src> {
    type Output = Local;
    fn index(&self, index: LocalId) -> &Self::Output {
        &self.locals[index.0 as usize]
    }
}

impl<'s, 'f, 'src> Index<StructId> for FunctionBuilder<'s, 'f, 'src> {
    type Output = Struct;
    fn index(&self, index: StructId) -> &Self::Output {
        &self.scope.structs[index.0 as usize]
    }
}
