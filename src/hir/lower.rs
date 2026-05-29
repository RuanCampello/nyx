use crate::{
    hir::{
        Block, Constant, EnumId, EnumVariant, Expression, ExpressionKind, Function, FunctionId,
        Intrinsic, Local, LocalId, Parameter, Receiver, ReceiverKind, RefTarget, RefTargetKind,
        Statement, Struct, StructId, SymbolId, SymbolTable, SyscallCode, Type, TypeKind,
        error::{ConstFnViolationKind, HirError, HirErrorKind, hir_error},
        index_vec::IndexVec,
        scope::Scope,
        symbols::Mangler,
        type_resolver::{self, resolve_annotation},
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
    locals: IndexVec<LocalId, Local>,
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
        let self_type = function
            .impl_type
            .and_then(|impl_type| scope.lookup_named_type(impl_type, symbols));

        Self {
            scope,
            symbols,
            is_const: function.is_const,
            in_std,
            return_type: Type::new(TypeKind::Unit),
            function: Some(function),
            function_id,
            next_local: 0,
            locals: IndexVec::new(),
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
            return_type: Type::new(TypeKind::Unit),
            function: None,
            function_id: FunctionId(0),
            next_local: 0,
            locals: IndexVec::new(),
            scopes: vec![HashMap::new()],
            self_type: None,
        }
    }

    #[inline(always)]
    pub fn lower(mut self) -> Result<Function, HirError<'src>> {
        let function = self.function.take().expect("function to be present");
        let id = self.function_id;
        let signatures = &self.scope.signatures[id];
        let symbol = signatures.name;
        self.return_type = signatures.return_type;

        let mut params = Vec::with_capacity(signatures.params.len());

        if let Some(receiver) = function.receiver {
            let typ = signatures.receiver_type().expect("receiver in AST without one in signature");
            let symbol = self.symbols.insert("self");
            let id = self.declare_local(symbol, typ, receiver.mutable)?;
            params.push(Parameter { typ, id, name: symbol, mutable: receiver.mutable });
        }

        params.extend(
            function
                .params
                .iter()
                .zip(signatures.explicit_params().iter())
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
            kind: signatures.kind,
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
                        return Err(hir_error!(
                            statement.span,
                            MissingInitialiser { name: statement.name.into() }
                        ));
                    },
                };

                let symbol = self.symbols.insert(statement.name);
                let id = self.declare_local(symbol, typ, statement.mutable)?;

                let stmt = match statement.value {
                    Some(ref expr) => {
                        let expr = self.lower_expr(expr, Some(typ))?;
                        self.assert_type(typ, expr.typ, expr.span)?;

                        Statement::LetInit { id, init: expr }
                    },
                    _ => Statement::LetUninit { id },
                };

                Ok((stmt, false))
            },

            Stmt::Return(statement) => {
                let value = match statement.value {
                    Some(ref expr) => {
                        let expr = self.lower_expr(expr, Some(self.return_type))?;
                        self.assert_type(self.return_type, expr.typ, expr.span)?;
                        Some(expr)
                    },
                    _ => None,
                };

                Ok((Statement::Return(value), true))
            },

            Stmt::If(statement) => self.lower_if(statement, is_tail),

            Stmt::While(statement) => {
                let condition = self.lower_expr(&statement.condition, None)?;
                self.assert_type(Type::new(TypeKind::Bool), condition.typ, statement.span)?;

                // PERFORMANCE: remove loops with constant false conditions
                let (body, _) = self.lower_block(&statement.body, false)?;

                Ok((Statement::While { condition, body }, false))
            },
            Stmt::Expr(expr, _) => {
                let tail_ret = is_tail && self.return_type.kind() != TypeKind::Unit;
                let expr = self.lower_expr(expr, tail_ret.then_some(self.return_type))?;

                Ok(match tail_ret {
                    true => {
                        self.assert_type(self.return_type, expr.typ, expr.span)?;
                        (Statement::Return(Some(expr)), true)
                    },
                    _ => (Statement::Expr(expr), false),
                })
            },
            Stmt::Block(block) => {
                let (block, returns) = self.lower_block(block, is_tail)?;
                Ok((Statement::Block(block), returns))
            },

            Stmt::Match(statement) => self.lower_match(statement, is_tail),

            Stmt::Interface(_) => unimplemented!("interface lowering is not yet implemented"),
            Stmt::Fn(_) => unimplemented!("nested functions are not supported yet"),
            Stmt::Struct(_) => unimplemented!("nested structs are not supported yet"),
            Stmt::Enum(_) => unimplemented!("nested enums are not supported yet"),
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

        let symbol = self
            .symbols
            .get_id(name)
            .ok_or_else(|| hir_error!(span, UndeclaredIdentifier { name: name.to_string() }))?;
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
                let typ = hint
                    .and_then(|t| t.is_number().then_some(t))
                    .unwrap_or(Type::new(TypeKind::I32));

                Ok(Expression { kind: ExpressionKind::Integer(*value), typ, span: *span })
            },

            Expr::Float(value, span) => {
                let typ = hint
                    .and_then(|t| t.is_float().then_some(t))
                    .unwrap_or(Type::new(TypeKind::F64));

                Ok(Expression { kind: ExpressionKind::Float(*value), typ, span: *span })
            },

            Expr::String(value, span) => {
                let sym = self.symbols.insert(value);
                Ok(Expression {
                    kind: ExpressionKind::String(sym),
                    typ: Type::new(TypeKind::Str),
                    span: *span,
                })
            },

            Expr::Char(value, span) => Ok(Expression {
                kind: ExpressionKind::Char(*value),
                typ: Type::new(TypeKind::Char),
                span: *span,
            }),

            Expr::Bool(value, span) => Ok(Expression {
                kind: ExpressionKind::Bool(*value),
                typ: Type::new(TypeKind::Bool),
                span: *span,
            }),

            Expr::Cast { expr: inner, target_type, span } => {
                let target = self.resolve_type(&target_type.value(), target_type.span())?;
                let lowered_expr = self.lower_expr(inner, None)?;
                let src = lowered_expr.typ;

                let src_castable =
                    src.is_primitive_castable() || matches!(src.kind(), TypeKind::Enum(_));
                if !src_castable || !target.is_primitive_castable() {
                    return Err(hir_error!(*span, InvalidCast { src, target }));
                }

                let lowered_expr = match src.kind() {
                    TypeKind::Enum(id) => Expression {
                        kind: ExpressionKind::EnumTag { value: Box::new(lowered_expr) },
                        typ: id.repr().typ(),
                        span: *span,
                    },
                    _ => lowered_expr,
                };

                Ok(Expression {
                    kind: ExpressionKind::Cast { from: Box::new(lowered_expr), to: target },
                    typ: target,
                    span: *span,
                })
            },

            Expr::Identifier(name, span) => self.lower_identifier(name, *span),

            Expr::QualifiedName { qualifier, name, span } => {
                let enum_symbol = self.symbols.insert(qualifier);
                let variant_symbol = self.symbols.insert(name);
                if let Some((id, value)) =
                    self.scope.enum_variants.get(&(enum_symbol, variant_symbol)).copied()
                {
                    return Ok(Expression {
                        kind: ExpressionKind::Integer(value),
                        typ: Type::new(TypeKind::Enum(id)),
                        span: *span,
                    });
                }

                let mangled_name = self.mangler().scoped_item(qualifier, name);
                let symbol = match self.symbols.get_id(&mangled_name) {
                    Some(sym) => sym,
                    None => {
                        return Err(hir_error!(
                            *span,
                            UndeclaredIdentifier { name: format!("{qualifier}::{name}") }
                        ));
                    },
                };

                let Some(c) = self.scope.constants.get(&symbol) else {
                    return Err(hir_error!(
                        *span,
                        UndeclaredIdentifier { name: format!("{qualifier}::{name}") }
                    ));
                };

                let mut val = c.value.clone();
                val.span = *span;
                Ok(val)
            },

            Expr::Unary { operator, expr, span } => {
                // for negation the hint flows through to the operand
                let inner_hint = match operator {
                    UnaryOperator::Neg => hint,
                    UnaryOperator::Not => hint,
                    UnaryOperator::Deref => hint.map(|h| {
                        Type::new(TypeKind::Ref {
                            mutable: false,
                            to: match h.kind() {
                                TypeKind::Struct(id) => RefTarget::new(RefTargetKind::Struct(id)),
                                TypeKind::Char => RefTarget::new(RefTargetKind::Char),
                                TypeKind::Ref { to, .. } => to,
                                _ => RefTarget::new(RefTargetKind::Char),
                            },
                        })
                    }),
                    UnaryOperator::Ref => hint.map(|h| h.strip_reference()),
                };
                let expr = self.lower_expr(expr, inner_hint)?;

                // PERFORMANCE: fold unary operations when operand is a constant literal
                let expected = match operator {
                    UnaryOperator::Neg => match expr.typ.is_number() {
                        true => expr.typ,
                        _ => {
                            return Err(hir_error!(
                                expr.span,
                                TypeMismatch {
                                    expected: Type::new(TypeKind::I32),
                                    found: expr.typ
                                }
                            ));
                        },
                    },

                    UnaryOperator::Not => {
                        match expr.typ == Type::new(TypeKind::Bool) || expr.typ.is_integer() {
                            true => expr.typ,
                            _ => {
                                return Err(hir_error!(
                                    expr.span,
                                    TypeMismatch {
                                        expected: Type::new(TypeKind::Bool),
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
                                    expected: Type::new(TypeKind::Ref {
                                        mutable: false,
                                        to: RefTarget::new(RefTargetKind::Char)
                                    }),
                                    found: expr.typ
                                }
                            ));
                        },
                    },

                    UnaryOperator::Ref => {
                        let to = RefTarget::try_from(expr.typ).map_err(|_| {
                            hir_error!(
                                expr.span,
                                TypeMismatch {
                                    expected: Type::new(TypeKind::Struct(Default::default())),
                                    found: expr.typ
                                }
                            )
                        })?;
                        Type::new(TypeKind::Ref { mutable: false, to })
                    },
                };

                if *operator != UnaryOperator::Deref && *operator != UnaryOperator::Ref {
                    self.assert_type(expected, expr.typ, expr.span)?;
                }

                Ok(Expression {
                    typ: expected,
                    span: *span,
                    kind: ExpressionKind::Unary { operator: *operator, expr: Box::new(expr) },
                })
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
                    | BinaryOperator::BitXor => Some(left.typ),
                    BinaryOperator::And | BinaryOperator::Or => Some(Type::new(TypeKind::Bool)),
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
            },

            Expr::Assignment { target, value, span } => {
                let (local, fields, typ) =
                    self.resolve_access_chain(target, *span).map_err(|err| match err.kind {
                        HirErrorKind::InvalidFieldAccess => {
                            hir_error!(*span, InvalidAssignmentTarget)
                        },
                        _ => err,
                    })?;

                if !self[local].mutable {
                    let err_span = *fields.is_empty().then_some(span).unwrap_or(&target.span());

                    return Err(hir_error!(
                        err_span,
                        ImmutableBind { name: self.symbols.get(self[local].name).to_string() }
                    ));
                }

                let value = self.lower_expr(value, Some(typ))?;
                self.assert_type(typ, value.typ, *span)?;

                let kind = match fields.is_empty() {
                    true => ExpressionKind::Assign { target: local, value: Box::new(value) },
                    _ => ExpressionKind::FieldAssign { local, fields, value: Box::new(value) },
                };
                Ok(Expression { kind, typ, span: *span })
            },

            Expr::Struct { name, fields, span, type_args: _ } => {
                let symbol = self.symbols.insert(name);
                let id = self
                    .scope
                    .struct_map
                    .get(&symbol)
                    .copied()
                    .ok_or_else(|| hir_error!(*span, UnknownType { name: name.to_string() }))?;

                let definition = &self.scope[id];
                let struct_name = self.symbols.get(definition.name).to_string();

                let mut seen = HashSet::with_capacity(fields.len());
                let mut lowered = Vec::with_capacity(fields.len());

                for field in fields {
                    let field_symbol = self.symbols.insert(field.name);
                    if !seen.insert(field_symbol) {
                        return Err(hir_error!(
                            field.span,
                            DuplicateField { name: field.name.to_string() }
                        ));
                    }

                    let Some(expected) = definition.fields.iter().find(|f| f.name == field_symbol)
                    else {
                        return Err(hir_error!(
                            field.span,
                            UnknownField { struct_name, field: field.name.to_string() }
                        ));
                    };

                    let value = self.lower_expr(&field.value, Some(expected.typ))?;
                    self.assert_type(expected.typ, value.typ, value.span)?;
                    lowered.push((field_symbol, value));
                }

                for expected in &definition.fields {
                    if !seen.contains(&expected.name) {
                        return Err(hir_error!(
                            *span,
                            MissingField {
                                struct_name,
                                field: self.symbols.get(expected.name).to_string()
                            }
                        ));
                    }
                }

                Ok(Expression {
                    typ: Type::new(TypeKind::Struct(id)),
                    span: *span,
                    kind: ExpressionKind::Struct { id, fields: lowered },
                })
            },

            Expr::Field { span, .. } => {
                let (local, fields, typ) = self.resolve_access_chain(expr, *span)?;

                Ok(Expression {
                    kind: ExpressionKind::FieldAccess { local, fields },
                    typ,
                    span: *span,
                })
            },

            Expr::Call { callee, args, span, type_args: _ } => {
                if let Expr::Field { expr: receiver, field: method_name, .. } = callee.as_ref() {
                    let (kind, receiver_type) = match self.resolve_access_chain(receiver, *span) {
                        Ok((base, path, typ)) => {
                            let kind = match path.is_empty() {
                                true => ReceiverKind::Local(base),
                                _ => ReceiverKind::Field { base, path },
                            };
                            (kind, typ)
                        },
                        _ => {
                            let lowered = self.lower_expr(receiver, None)?;
                            let typ = lowered.typ;
                            (ReceiverKind::Computed(Box::new(lowered)), typ)
                        },
                    };
                    let base_local = kind.base_local();

                    let receiver_base_type = receiver_type.strip_reference();
                    let method_symbol = self.symbols.insert(method_name);
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
                        *self.scope.methods.get(&(receiver_base_type, method_symbol)).ok_or_else(
                            || {
                                hir_error!(
                                    *span,
                                    UnknownMethod { struct_name, name: method_name.to_string() }
                                )
                            },
                        )?;

                    let signature = &self.scope.signatures[function];
                    let receiver_typ =
                        signature.receiver_type().expect("method call resolved to a free function");

                    if signature.receiver_mutable()
                        && base_local.filter(|&id| self[id].mutable).is_none()
                    {
                        let name = match base_local {
                            Some(id) => self.symbols.get(self[id].name).to_string(),
                            None => "temporary".to_string(),
                        };

                        return Err(hir_error!(*span, ImmutableBind { name }));
                    }

                    let explicit_params = signature.explicit_params();
                    if explicit_params.len() != args.len() {
                        return Err(hir_error!(
                            *span,
                            ArityMismatch {
                                name: method_name.to_string(),
                                expected: explicit_params.len(),
                                found: args.len()
                            }
                        ));
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
                            receiver: Receiver { kind, typ: receiver_typ },
                            args: lowered_args,
                        },
                    });
                }

                let function_id = match callee.as_ref() {
                    Expr::Identifier(name, _) => {
                        self.scope.resolve_function_call(None, name, self.symbols).ok_or_else(
                            || hir_error!(*span, UnknownFunction { name: name.to_string() }),
                        )?
                    },

                    other => {
                        return Err(hir_error!(
                            *span,
                            UnknownFunction { name: format!("{other:?}") }
                        ));
                    },
                };

                self.lower_direct_call(function_id, args, *span)
            },

            Expr::QualifiedCall { qualifier, name, args, span, type_args: _ } => {
                let id = self
                    .scope
                    .resolve_function_call(Some(qualifier), name, self.symbols)
                    .ok_or_else(|| {
                        hir_error!(*span, UnknownFunction { name: format!("{qualifier}::{name}") })
                    })?;

                self.lower_direct_call(id, args, *span)
            },

            Expr::TypeIntrinsic { kind, qualifier, typ, span } => {
                let name: &str = kind.into();
                let exists =
                    self.scope.resolve_function_call(*qualifier, name, self.symbols).is_some();

                if !exists {
                    let name =
                        qualifier.map_or_else(|| name.to_string(), |q| format!("{q}::{name}"));
                    return Err(hir_error!(*span, UnknownFunction { name }));
                }

                let ctx = type_resolver::ResolveCtx::root(
                    self.symbols,
                    &self.scope.struct_map,
                    &self.scope.enum_map,
                );
                let typ = resolve_annotation(&ctx, &typ.value(), typ.span())?;

                let structs: Vec<Option<Struct>> =
                    self.scope.structs.iter().map(|s| Some(s.clone())).collect();
                let (size, align) = typ.layout(&structs);

                let value = match kind {
                    expression::TypeIntrinsicKind::SizeOf => size as i64,
                    expression::TypeIntrinsicKind::AlignOf => align as i64,
                };

                Ok(Expression {
                    kind: ExpressionKind::Integer(value),
                    typ: Type::new(TypeKind::Uptr),
                    span: *span,
                })
            },
        }
    }

    fn lower_match(
        &mut self,
        match_stmt: &statement::Match<'src>,
        is_tail: bool,
    ) -> Result<(Statement, bool), HirError<'src>> {
        let scrutinee = self.lower_expr(&match_stmt.scrutinee, None)?;
        let scrutinee_enum = self.enum_type(scrutinee.typ, match_stmt.scrutinee.span())?;
        let temp_name = format!("__match{}", self.next_local);
        let temp_symbol = self.symbols.insert(&temp_name);
        let temp = self.declare_local(temp_symbol, scrutinee.typ, false)?;
        let init = Statement::LetInit { id: temp, init: scrutinee };

        let tail_ret = is_tail && self.return_type.kind() != TypeKind::Unit;
        let hint = tail_ret.then_some(self.return_type);
        let mut else_block = None;
        let mut all_return = true;

        for arm in match_stmt.arms.iter().rev() {
            let condition = self.match_arm_condition(temp, scrutinee_enum, arm)?;
            let (then_block, returns) =
                self.lower_match_arm_body(temp, scrutinee_enum, arm, hint)?;
            all_return &= returns;

            let statement = Statement::If { condition, then_block, else_block };
            else_block = Some(Block { statements: vec![statement], span: arm.span });
        }

        let statements = match else_block {
            Some(block) => vec![init, Statement::Block(block)],
            None => vec![init],
        };

        Ok((Statement::Block(Block { statements, span: match_stmt.span }), all_return))
    }

    fn lower_match_arm_body(
        &mut self,
        local: LocalId,
        id: EnumId,
        arm: &statement::MatchArm<'src>,
        hint: Option<Type>,
    ) -> Result<(Block, bool), HirError<'src>> {
        self.push_scope();
        let mut statements = Vec::new();

        if let Some((name, payload_type)) = self.payload_binding(id, &arm.patterns, arm.span)? {
            let symbol = self.symbols.insert(name);
            let id = self.declare_local(symbol, payload_type, false)?;
            let value = Expression {
                kind: ExpressionKind::EnumPayload {
                    value: Box::new(self.local_expr(local, arm.span)),
                },
                typ: payload_type,
                span: arm.span,
            };
            statements.push(Statement::LetInit { id, init: value });
        }

        let body = self.lower_expr(&arm.body, hint)?;
        let returns = hint.is_some();
        match hint {
            Some(expected) => {
                self.assert_type(expected, body.typ, body.span)?;
                statements.push(Statement::Return(Some(body)));
            },
            None => statements.push(Statement::Expr(body)),
        }

        self.pop_scope();
        Ok((Block { statements, span: arm.span }, returns))
    }

    fn lower_if(
        &mut self,
        if_stmt: &statement::If<'src>,
        is_tail: bool,
    ) -> Result<(Statement, bool), HirError<'src>> {
        let condition = self.lower_expr(&if_stmt.condition, None)?;
        self.assert_type(Type::new(TypeKind::Bool), condition.typ, condition.span)?;

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
                                Statement::Return(Some(lowered))
                            },
                            _ => Statement::Expr(lowered),
                        };

                        let block = Block { statements: vec![stmt], span };
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
        span: Span,
    ) -> Result<Expression, HirError<'src>> {
        let signature = &self.scope.signatures[function_id];
        let intrinsic = signature.kind.intrinsic();

        if self.is_const && !signature.is_const && intrinsic.is_none() {
            let name = self.symbols.get(signature.name).to_string();
            return Err(hir_error!(
                span,
                ConstFnViolation(ConstFnViolationKind::NonConstCall { name })
            ));
        }

        if intrinsic == Some(Intrinsic::Syscall) {
            return self.lower_syscall(args, signature.return_type, span);
        }

        if intrinsic.is_none() && signature.params.len() != args.len() {
            let name = self.symbols.get(signature.name).to_string();
            return Err(hir_error!(
                span,
                ArityMismatch { name, expected: signature.params.len(), found: args.len() }
            ));
        }

        let mut lowered_args = Vec::with_capacity(args.len());
        match intrinsic {
            Some(_) => {
                for arg in args {
                    lowered_args.push(self.lower_expr(arg, None)?);
                }
            },
            _ => {
                for (expr, &param_type) in args.iter().zip(signature.params.iter()) {
                    let expr = self.lower_expr(expr, Some(param_type))?;
                    self.assert_type(param_type, expr.typ, expr.span)?;
                    lowered_args.push(expr);
                }
            },
        }

        let kind = match intrinsic {
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
            return Err(hir_error!(span, UnknownFunction { name: "syscall".into() }));
        }

        let Some((code_arg, value_args)) = args.split_first() else {
            return Err(hir_error!(
                span,
                ArityMismatch { name: "syscall".to_string(), expected: 1, found: 0 }
            ));
        };

        if value_args.len() > 6 {
            return Err(hir_error!(
                span,
                ArityMismatch { name: "syscall".to_string(), expected: 7, found: args.len() }
            ));
        }

        let expression::Expression::Identifier(name, code_span) = code_arg else {
            return Err(hir_error!(
                code_arg.span(),
                UndeclaredIdentifier { name: format!("{code_arg:?}") }
            ));
        };

        let code = SyscallCode::from_str(name)
            .map_err(|_| hir_error!(*code_span, UndeclaredIdentifier { name: name.to_string() }))?;

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

    fn match_arm_condition(
        &mut self,
        local: LocalId,
        id: EnumId,
        arm: &statement::MatchArm<'src>,
    ) -> Result<Expression, HirError<'src>> {
        let mut condition = None;

        for pattern in &arm.patterns {
            let next = self.pattern_condition(local, id, pattern, arm.span)?;
            condition = Some(match condition {
                Some(left) => Expression {
                    typ: Type::new(TypeKind::Bool),
                    span: arm.span,
                    kind: ExpressionKind::Binary {
                        operator: BinaryOperator::Or,
                        left: Box::new(left),
                        right: Box::new(next),
                    },
                },
                None => next,
            });
        }

        Ok(condition.unwrap_or(Expression {
            kind: ExpressionKind::Bool(true),
            typ: Type::new(TypeKind::Bool),
            span: arm.span,
        }))
    }

    #[inline]
    fn pattern_condition(
        &mut self,
        local: LocalId,
        id: EnumId,
        pattern: &statement::Pattern<'src>,
        span: Span,
    ) -> Result<Expression, HirError<'src>> {
        match pattern {
            statement::Pattern::Wildcard => Ok(Expression {
                kind: ExpressionKind::Bool(true),
                typ: Type::new(TypeKind::Bool),
                span,
            }),
            statement::Pattern::Ident(name) => match self.variant(id, None, name) {
                Some(variant) => self.tag_comparison(local, id, variant.value, span),
                None => Ok(Expression {
                    kind: ExpressionKind::Bool(true),
                    typ: Type::new(TypeKind::Bool),
                    span,
                }),
            },
            statement::Pattern::Variant { qualifier, name, .. } => {
                let variant = self.variant(id, *qualifier, name).ok_or_else(|| {
                    hir_error!(span, UndeclaredIdentifier { name: name.to_string() })
                })?;
                self.tag_comparison(local, id, variant.value, span)
            },
        }
    }

    fn payload_binding(
        &mut self,
        id: EnumId,
        patterns: &[statement::Pattern<'src>],
        span: Span,
    ) -> Result<Option<(&'src str, Type)>, HirError<'src>> {
        for pattern in patterns {
            if let Some(binding) = self.pattern_payload_binding(id, pattern, span)? {
                return Ok(Some(binding));
            }
        }

        Ok(None)
    }

    #[inline]
    fn pattern_payload_binding(
        &mut self,
        id: EnumId,
        pattern: &statement::Pattern<'src>,
        span: Span,
    ) -> Result<Option<(&'src str, Type)>, HirError<'src>> {
        let statement::Pattern::Variant { qualifier, name, sub } = pattern else {
            return Ok(None);
        };
        let Some(sub) = sub.as_deref() else {
            return Ok(None);
        };

        let variant = self
            .variant(id, *qualifier, name)
            .ok_or_else(|| hir_error!(span, UndeclaredIdentifier { name: name.to_string() }))?;
        let Some(payload) = variant.payload else {
            return Ok(None);
        };

        Ok(match sub {
            statement::Pattern::Wildcard => None,
            statement::Pattern::Ident(name) => Some((*name, payload)),
            _ => None,
        })
    }

    #[inline]
    fn tag_comparison(
        &self,
        local: LocalId,
        id: EnumId,
        value: i64,
        span: Span,
    ) -> Result<Expression, HirError<'src>> {
        let tag_type = id.repr().typ();

        let left = Expression {
            kind: ExpressionKind::EnumTag { value: Box::new(self.local_expr(local, span)) },
            typ: tag_type,
            span,
        };
        let right = Expression { kind: ExpressionKind::Integer(value), typ: tag_type, span };

        Ok(Expression {
            kind: ExpressionKind::Binary {
                operator: BinaryOperator::Eq,
                left: Box::new(left),
                right: Box::new(right),
            },
            typ: Type::new(TypeKind::Bool),
            span,
        })
    }

    #[inline]
    fn enum_type(&self, typ: Type, span: Span) -> Result<EnumId, HirError<'src>> {
        let default = TypeKind::Enum(Default::default());

        match typ.kind() {
            TypeKind::Enum(id) => Ok(id),
            TypeKind::Ref { to, .. } => match to.kind() {
                RefTargetKind::Enum(id) => Ok(id),
                _ => {
                    Err(hir_error!(span, TypeMismatch { expected: Type::new(default), found: typ }))
                },
            },
            _ => Err(hir_error!(span, TypeMismatch { expected: Type::new(default), found: typ })),
        }
    }

    fn variant(&mut self, id: EnumId, qualifier: Option<&str>, name: &str) -> Option<EnumVariant> {
        if let Some(qualifier) = qualifier {
            let enum_symbol = self.symbols.insert(qualifier);
            if self.scope.enum_map.get(&enum_symbol).copied() != Some(id) {
                return None;
            }
        }

        let symbol = self.symbols.insert(name);
        self.scope.enums[id]
            .variants
            .iter()
            .find(|variant| variant.name == symbol)
            .copied()
    }

    fn type_for_binary(
        &self,
        operator: &BinaryOperator,
        left: Type,
        right: Type,
        span: Span,
    ) -> Result<Type, HirError<'src>> {
        let type_mismatch =
            |found| hir_error!(span, TypeMismatch { expected: Type::new(TypeKind::I32), found });

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
            },

            BinaryOperator::Eq | BinaryOperator::Ne => {
                self.assert_type(left, right, span)?;
                Ok(Type::new(TypeKind::Bool))
            },

            BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq => {
                self.assert_type(left, right, span)?;
                match left.is_number() || left == Type::new(TypeKind::Char) {
                    true => Ok(Type::new(TypeKind::Bool)),
                    _ => Err(type_mismatch(left)),
                }
            },

            BinaryOperator::And | BinaryOperator::Or => {
                self.assert_type(Type::new(TypeKind::Bool), left, span)?;
                self.assert_type(Type::new(TypeKind::Bool), right, span)?;

                Ok(Type::new(TypeKind::Bool))
            },

            BinaryOperator::BitAnd | BinaryOperator::BitOr | BinaryOperator::BitXor => {
                self.assert_type(left, right, span)?;

                match left == Type::new(TypeKind::Bool) || left.is_integer() {
                    true => Ok(left),
                    _ => Err(type_mismatch(left)),
                }
            },

            BinaryOperator::Shl | BinaryOperator::Shr => {
                if !left.is_integer() {
                    return Err(type_mismatch(left));
                }

                if !right.is_integer() {
                    return Err(type_mismatch(right));
                }

                Ok(left)
            },
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
                self.scope
                    .nominal_type(symbol)
                    .ok_or_else(|| hir_error!(span, UnknownType { name: name.to_string() }))
            },

            statement::Type::SelfType => self
                .self_type
                .ok_or_else(|| hir_error!(span, UnknownType { name: "Self".into() })),

            statement::Type::RefSelf => {
                let self_ty = self
                    .self_type
                    .ok_or_else(|| hir_error!(span, UnknownType { name: "Self".into() }))?;
                let to = RefTarget::try_from(self_ty).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch {
                            expected: Type::new(TypeKind::Struct(Default::default())),
                            found: self_ty
                        }
                    )
                })?;

                Ok(Type::new(TypeKind::Ref { mutable: false, to }))
            },

            statement::Type::Ref(inner) => {
                let inner_type = self.resolve_type(inner, span)?;
                let to = RefTarget::try_from(inner_type).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch {
                            expected: Type::new(TypeKind::Struct(Default::default())),
                            found: inner_type
                        }
                    )
                })?;
                Ok(Type::new(TypeKind::Ref { mutable: false, to }))
            },

            // Named / Ref / Self / RefSelf are handled above; the remainder are primitives.
            // `Generic` would only land here if monomorphisation skipped this site — that's
            // a compiler bug, not user-reachable, so error visibly rather than panicking.
            other => Type::from_primitive_ast(other)
                .ok_or_else(|| hir_error!(span, UnknownType { name: format!("{other:?}") })),
        }
    }

    #[inline(always)]
    #[must_use]
    fn assert_type(&self, expected: Type, found: Type, span: Span) -> Result<(), HirError<'src>> {
        match expected == found {
            true => Ok(()),
            false => Err(hir_error!(span, TypeMismatch { expected, found })),
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
            return Err(hir_error!(
                Span::default(),
                DuplicateBind { name: self.symbols.get(name).to_string() }
            ));
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
            .ok_or_else(|| {
                hir_error!(span, UndeclaredIdentifier { name: self.symbols.get(name).to_string() })
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
        let sid = match current.kind() {
            TypeKind::Struct(id) => id,
            TypeKind::Ref { to, .. } => match to.kind() {
                RefTargetKind::Struct(id) => id,
                _ => return Err(hir_error!(span, TypeMismatch {
                    expected: Type::new(TypeKind::Struct(Default::default())),
                    found: current
                })),
            },
            _ => return Err(hir_error!(span, TypeMismatch {
                expected: Type::new(TypeKind::Struct(Default::default())),
                found: current
            })),
        };

        let sym = self.symbols.insert(name);
        let def = &self.scope[sid];
        let struct_name = self.symbols.get(def.name).to_string();

        let field = def.fields.iter().find(|field| field.name == sym).ok_or_else(|| {
            hir_error!(span, UnknownField { struct_name, field: name.to_string() })
        })?;

        Ok((sym, field.typ))
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
                },

                Expr::Field { expr: next, field, .. } => {
                    fields.push(*field);
                    curr = next;
                },

                _ => return Err(hir_error!(span, InvalidFieldAccess)),
            }
        };

        fields.reverse();

        let mut current_type = self[id].typ;
        let mut field_symbols = Vec::with_capacity(fields.len());

        for (idx, &field_name) in fields.iter().enumerate() {
            let (sym, typ) = self.lookup_field(current_type, field_name, span)?;
            current_type = typ;
            field_symbols.push(sym);

            let is_last = idx == fields.len() - 1;
            let is_struct = match current_type.kind() {
                TypeKind::Struct(_) => true,
                TypeKind::Ref { to, .. } => matches!(to.kind(), RefTargetKind::Struct(_)),
                _ => false,
            };

            if !is_last && !is_struct {
                let expected = Type::new(TypeKind::Struct(Default::default()));
                return Err(hir_error!(span, TypeMismatch { expected, found: current_type }));
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

impl<'s, 'f, 'src> Index<LocalId> for FunctionBuilder<'s, 'f, 'src> {
    type Output = Local;
    fn index(&self, index: LocalId) -> &Self::Output {
        &self.locals[index]
    }
}

impl<'s, 'f, 'src> Index<StructId> for FunctionBuilder<'s, 'f, 'src> {
    type Output = Struct;
    fn index(&self, index: StructId) -> &Self::Output {
        &self.scope[index]
    }
}
