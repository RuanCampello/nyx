use crate::{
    hir::{
        AdtId, Arm, ArmBody, Constant, Expression, ExpressionKind, Literal, LocalId, Pattern,
        PatternKind, Res, Statement, Static, StaticId, SymbolId, Type, TypeKind, collect,
        error::{HirError, hir_error},
        exhaustive, lang,
        lower::{FunctionBuilder, Lowered, call::GenericCall},
        place_base_local,
        symbols::qualified,
    },
    lexer::{Spanned, token::Span},
    parser::{
        expression::{self, BinaryOperator, UnaryOperator},
        statement::{self},
    },
};
use std::borrow::Cow;

/// Unifies the values several branches yield, whether they are match arms or
/// the two sides of an `if`
struct Branches<'hir> {
    unified: Option<Type<'hir>>,
    divergent: Option<Type<'hir>>,
}

/// The failure carriers `?` understands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TryFamily {
    Optional,
    Result,
}

impl<'s, 'f, 'hir, 'src> FunctionBuilder<'s, 'f, 'hir, 'src>
where
    'src: 'hir,
{
    fn lower_identifier(
        &mut self,
        name: &'src str,
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        if let Some(id) = self.local_id(name) {
            return Ok(self.local_expr(id, span));
        }

        if let Some(c) = self.constant(name) {
            let lowered = self.alloc(ExpressionKind::Const(c), c.typ, span);
            self.typeck.const_uses.insert(lowered.expr.id, c.name);
            return Ok(lowered);
        }

        if let Some((id, item)) = self.static_item(name) {
            self.check_static_safety(&item, span)?;

            return Ok(self.alloc(ExpressionKind::Static(id), item.typ, span));
        }

        let symbol = self
            .scope
            .symbols
            .get_id(name)
            .ok_or_else(|| hir_error!(span, UndeclaredIdentifier { name }))?;

        if self.const_scope.outer_locals.contains(&symbol) {
            return Err(hir_error!(span, NonConstValue { name }));
        }

        let id = self.resolve_local(symbol, span)?;

        Ok(self.local_expr(id, span))
    }

    pub(in crate::hir::lower) fn handle_tail_expr(
        &mut self,
        expr: Lowered<'hir>,
        tail_ret: bool,
    ) -> Result<(Statement<'hir>, bool), HirError<'hir>> {
        let carries_value = tail_ret && !expr.typ.diverges() && expr.typ.kind() != TypeKind::Unit;

        match carries_value {
            true => {
                self.check_type_at(self.return_type, expr.typ, expr.span, self.return_type_span)?;
                Ok((Statement::Return(Some(expr.expr)), true))
            },
            _ => Ok((Statement::Expr(expr.expr), expr.typ.diverges())),
        }
    }

    fn local_id(&self, name: &str) -> Option<LocalId> {
        let symbol = self.scope.symbols.get_id(name)?;

        self.scopes.iter().rev().find_map(|scope| scope.get(&symbol).copied())
    }

    fn local_expr(&mut self, id: LocalId, span: Span) -> Lowered<'hir> {
        let typ = self[id].typ;
        self.alloc(ExpressionKind::Local(id), typ, span)
    }

    fn place_indirection(&self, expr: &Expression<'hir>) -> Option<bool> {
        match &expr.kind {
            ExpressionKind::Unary { operator: UnaryOperator::Deref, expr } => {
                match self.typeck.type_of(expr.id).kind() {
                    TypeKind::Ref { mutable, .. } | TypeKind::Raw { mutable, .. } => Some(mutable),
                    _ => None,
                }
            },

            ExpressionKind::Index { base, .. } | ExpressionKind::Field { base, .. } => {
                match self.typeck.type_of(base.id).kind() {
                    TypeKind::Slice { mutable, .. } => Some(mutable),
                    TypeKind::Ref { mutable: true, .. } | TypeKind::Raw { mutable: true, .. } => {
                        Some(true)
                    },
                    TypeKind::Ref { .. } | TypeKind::Raw { .. } => None,
                    _ => self.place_indirection(base),
                }
            },

            _ => None,
        }
    }

    /// lowers the left-hand side of an assignment, rejecting a target that is
    /// not an assignable place and one bound immutably
    fn lower_assignment_target(
        &mut self,
        target: &expression::Expression<'src>,
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let lowered = self.lower_mutable_place(target, None)?;

        if let ExpressionKind::Static(id) = lowered.expr.kind {
            let item = self.scope.values.statics[id];
            if !item.is_mut {
                let name = self.arena.alloc_str(self.scope.symbols.get(item.name));
                let decl = collect::source_span(item.decl_span);
                return Err(hir_error!(span, ImmutableBind { name, decl }));
            }

            return Ok(lowered);
        }

        let is_place = self.place_indirection(lowered.expr).is_some()
            || place_base_local(lowered.expr).is_some();
        if !is_place {
            return Err(hir_error!(span, InvalidAssignmentTarget));
        }

        let blame = match lowered.expr.kind {
            ExpressionKind::Local(_) => span,
            _ => target.span(),
        };
        self.check_mutable_place(lowered.expr, blame)?;
        self.make_place_mutable(lowered.expr)?;

        Ok(lowered)
    }

    fn check_mutable_place(
        &self,
        expr: &Expression<'hir>,
        blame: Span,
    ) -> Result<(), HirError<'hir>> {
        match self.place_indirection(expr) {
            Some(true) => Ok(()),
            Some(false) => Err(hir_error!(expr.span, AssignBehindSharedRef)),
            _ => match place_base_local(expr) {
                None => Ok(()),
                Some(local) if self[local].mutable => Ok(()),
                Some(local) => {
                    let name = self.arena.alloc_str(self.scope.symbols.get(self[local].name));
                    let decl = collect::source_span(self[local].decl_span);
                    Err(hir_error!(blame, ImmutableBind { name, decl }))
                },
            },
        }
    }

    fn resolve_self_path<'p>(&self, path: &'p [&'hir str]) -> Cow<'p, [&'hir str]> {
        match (path.split_first(), self.impl_ctx.type_name) {
            (Some((&"Self", rest)), Some(impl_type)) => {
                let mut resolved = Vec::with_capacity(rest.len() + 1);
                resolved.push(impl_type);
                resolved.extend_from_slice(rest);

                Cow::Owned(resolved)
            },
            _ => Cow::Borrowed(path),
        }
    }

    fn constant(&self, name: &str) -> Option<&'hir Constant<'hir>> {
        // a body-level constant lexically shadows any module or impl constant
        if let Some(symbol) = self.scope.symbols.get_id(name)
            && let Some(&constant) = self.const_scope.body.get(&symbol)
        {
            return Some(constant);
        }

        for impl_type in [self.impl_ctx.type_name, self.impl_ctx.template.map(|t| &*t)]
            .into_iter()
            .flatten()
        {
            let scoped = self.mangler().scoped_item(impl_type, name);
            if let Some(constant) = self.constant_by_symbol_name(&scoped) {
                return Some(constant);
            }
        }

        let top_level = self.mangler().item(name);
        self.constant_by_symbol_name(&top_level)
            .or_else(|| self.constant_by_symbol_name(name))
    }

    fn static_item(&self, name: &str) -> Option<(StaticId, Static<'hir>)> {
        let top_level = self.mangler().item(name);

        self.static_by_symbol_name(&top_level)
            .or_else(|| self.static_by_symbol_name(name))
    }

    fn static_by_symbol_name(&self, name: &str) -> Option<(StaticId, Static<'hir>)> {
        let symbol = self.scope.symbols.get_id(name)?;

        self.scope
            .values
            .static_map
            .get(&symbol)
            .map(|&id| (id, self.scope.values.statics[id]))
    }

    fn check_static_safety(
        &mut self,
        item: &Static<'hir>,
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        if !item.is_mut {
            return Ok(());
        }

        self.require_unsafe(|this| {
            let name = this.arena.alloc_str(this.scope.symbols.get(item.name));
            hir_error!(span, UnsafeStatic { name })
        });
        Ok(())
    }

    fn constant_by_symbol_name(&self, name: &str) -> Option<&'hir Constant<'hir>> {
        let symbol = self.scope.symbols.get_id(name)?;

        self.scope.values.constants.get(&symbol).copied()
    }

    #[inline]
    fn template_associated_constant(
        &self,
        qualifier: &str,
        name: &str,
    ) -> Option<&'hir Constant<'hir>> {
        self.impl_ctx
            .template
            .filter(|_| self.impl_ctx.type_name == Some(qualifier))
            .and_then(|template| {
                self.constant_by_symbol_name(&self.mangler().scoped_item(template, name))
            })
    }

    fn generic_associated_constant(
        &self,
        qualifier: &str,
        name: &str,
    ) -> Option<&'hir Constant<'hir>> {
        let concrete = *self.generic_env.get(qualifier)?;
        let generic = self.resolve_generic(qualifier)?;

        let (required_interface, ()) = self.find_bound_item(generic, |interface| {
            interface
                .constants
                .iter()
                .any(|constant| {
                    self.scope.symbols.get(constant.name).rsplit("::").next() == Some(name)
                })
                .then_some(())
        })?;

        self.scope.interface_constant(concrete, required_interface, name)
    }

    fn generic_associated_constant_param(
        &self,
        qualifier: &str,
        name: &str,
    ) -> Option<(u8, SymbolId, SymbolId, Type<'hir>)> {
        let concrete = *self.generic_env.get(qualifier)?;
        let TypeKind::GenericParam(param) = concrete.kind() else {
            return None;
        };

        let generic = self.resolve_generic(qualifier)?;
        let (interface, constant) = self.find_bound_item(generic, |interface| {
            interface
                .constants
                .iter()
                .find(|constant| {
                    self.scope.symbols.get(constant.name).rsplit("::").next() == Some(name)
                })
                .cloned()
        })?;

        Some((param, interface, constant.name, constant.typ))
    }

    pub(in crate::hir) fn lower_expr(
        &mut self,
        expr: &expression::Expression<'src>,
        hint: Option<Type<'hir>>,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        use expression::Expression as Expr;

        match expr {
            Expr::Integer(value, span) => {
                let typ = match hint {
                    Some(t) if t.is_number() || t.is_infer() => t,
                    _ => self.infer.fresh(&self.scope.types),
                };

                Ok(self.alloc((*value as i64).into(), typ, *span))
            },

            Expr::Float(value, span) => {
                let typ = hint
                    .and_then(|t| t.is_float().then_some(t))
                    .unwrap_or(self.scope.types.common.f64);

                Ok(self.alloc((*value).into(), typ, *span))
            },

            Expr::String(value, span) => {
                let sym = self.scope.symbols.insert(value);
                Ok(self.alloc(
                    ExpressionKind::Literal(Literal::Str(sym)),
                    self.scope.types.common.str,
                    *span,
                ))
            },

            Expr::Char(value, span) => {
                Ok(self.alloc((*value).into(), self.scope.types.common.char, *span))
            },
            Expr::Bool(value, span) => {
                Ok(self.alloc((*value).into(), self.scope.types.common.bool, *span))
            },

            Expr::Cast { expr: inner, target_type, span } => {
                use TypeKind::*;

                let target = self.resolve_type(&target_type.value(), target_type.span())?;
                let lowered_expr = self.lower_expr(inner, None)?;
                let src = self.infer.resolve_shallow(lowered_expr.typ);

                let castable = match (src.kind(), target.kind()) {
                    (Raw { .. } | Ref { .. }, Raw { .. }) => true,
                    (Raw { .. }, _) => target.is_integer(),
                    (_, Raw { .. }) => src.is_integer() || src.is_infer(),
                    _ => {
                        let src_castable = src.is_primitive_castable()
                            || src.is_infer()
                            || matches!(src.kind(), Adt(_, _));
                        src_castable && target.is_primitive_castable()
                    },
                };
                if !castable {
                    return Err(hir_error!(*span, InvalidCast { src, target }));
                }

                Ok(self.alloc(
                    ExpressionKind::Cast { from: lowered_expr.expr, to: target },
                    target,
                    *span,
                ))
            },
            Expr::Identifier(name, span) => self.lower_identifier(name, *span),
            Expr::QualifiedName { path, name, span } => {
                let path = &*self.resolve_self_path(path);
                let qualifier = self.arena.alloc_str(&path.join("::"));
                let enum_symbol = self.scope.symbols.insert(qualifier);
                let variant_symbol = self.scope.symbols.insert(name);
                if let Some((id, value)) =
                    self.scope.adts.variants.get(&(enum_symbol, variant_symbol)).copied()
                {
                    let has_payload = self.scope[id].variants().iter().any(|v| v.payload.is_some());
                    if (has_payload || !self.scope[id].generics.is_empty())
                        && let Some(lowered) =
                            self.lower_variant(path, name, &[], &[], hint, *span)?
                    {
                        return Ok(lowered);
                    }

                    return Ok(self.alloc(
                        ExpressionKind::Literal(Literal::Int(value)),
                        self.scope.types.adt(id, &[]),
                        *span,
                    ));
                }

                let mangled_name = self.mangler().scoped_item(qualifier, name);
                let qualified = qualified(self.arena, qualifier, name);
                let constant = self
                    .scope
                    .symbols
                    .get_id(&mangled_name)
                    .or_else(|| self.scope.symbols.get_id(&self.mangler().item(name)))
                    .and_then(|symbol| self.scope.values.constants.get(&symbol).copied())
                    .or_else(|| self.template_associated_constant(qualifier, name))
                    .or_else(|| self.generic_associated_constant(qualifier, name));

                if let Some(constant) = constant {
                    let lowered = self.alloc(ExpressionKind::Const(constant), constant.typ, *span);
                    self.typeck.const_uses.insert(lowered.expr.id, constant.name);
                    return Ok(lowered);
                }

                if let Some((param, interface, name, typ)) =
                    self.generic_associated_constant_param(qualifier, name)
                {
                    return Ok(self.alloc(
                        ExpressionKind::ParamConst { param, interface, name },
                        typ,
                        *span,
                    ));
                }
                Err(hir_error!(*span, UndeclaredIdentifier { name: qualified }))
            },

            Expr::Unary { operator, expr, span } => {
                let inner_hint = match operator {
                    UnaryOperator::Neg => hint,
                    UnaryOperator::Not => hint,
                    UnaryOperator::Deref => hint.and_then(|h| {
                        let to = match h.kind() {
                            TypeKind::Ref { to, .. } | TypeKind::Raw { to, .. } => to,
                            TypeKind::Adt(id, args) => self.scope.types.adt(id, args),
                            _ => return None,
                        };
                        Some(self.scope.types.refer(to, false))
                    }),
                    UnaryOperator::Ref | UnaryOperator::RefMut => hint.map(|h| h.strip_reference()),
                };
                let expr = match operator {
                    UnaryOperator::RefMut => self.lower_mutable_place(expr, inner_hint)?,
                    _ => self.lower_expr(expr, inner_hint)?,
                };

                // PERFORMANCE: fold unary operations when operand is a constant literal
                let expected = match operator {
                    UnaryOperator::Neg => match expr.typ.is_number() {
                        true => expr.typ,
                        _ => {
                            let (expected, found) = (self.scope.types.common.i32, expr.typ);
                            return Err(hir_error!(expr.span, TypeMismatch { expected, found }));
                        },
                    },

                    UnaryOperator::Not => {
                        match expr.typ == self.scope.types.common.bool || expr.typ.is_integer() {
                            true => expr.typ,
                            _ => {
                                let (expected, found) = (self.scope.types.common.bool, expr.typ);
                                return Err(hir_error!(
                                    expr.span,
                                    TypeMismatch { expected, found }
                                ));
                            },
                        }
                    },

                    UnaryOperator::Deref => match expr.typ.kind() {
                        TypeKind::Ref { to, .. } => to,
                        TypeKind::Raw { to, .. } => {
                            let (found, span) = (expr.typ, expr.span);
                            self.require_unsafe(|_| hir_error!(span, UnsafeDeref { found }));
                            to
                        },
                        _ => return Err(hir_error!(expr.span, InvalidDeref { found: expr.typ })),
                    },

                    UnaryOperator::Ref | UnaryOperator::RefMut => {
                        match self.coerce_array_to_slice(expr.typ, hint) {
                            // `&array` unsizes to a `&[T]`/`&mut [T]` slice in slice context
                            Some(slice) => slice,
                            _ => {
                                self.scope.types.refer(expr.typ, *operator == UnaryOperator::RefMut)
                            },
                        }
                    },
                };

                if *operator == UnaryOperator::RefMut {
                    self.check_mutable_place(expr.expr, expr.expr.span)?;
                    self.make_place_mutable(expr.expr)?;
                }

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
                    BinaryOperator::And | BinaryOperator::Or => Some(self.scope.types.common.bool),
                    BinaryOperator::Shl | BinaryOperator::Shr => None,
                };
                let right = self.lower_expr(right, right_hint)?;

                if let Some(lowered) =
                    self.lower_overloaded_comparison(*operator, left, right, *span)?
                {
                    return Ok(lowered);
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
                let target_lowered = self.lower_assignment_target(target, *span)?;
                let value = self.lower_expr(value, Some(target_lowered.typ))?;
                self.assert_type(target_lowered.typ, value.typ, *span)?;

                let typ = target_lowered.typ;
                let instr =
                    ExpressionKind::Assign { target: target_lowered.expr, value: value.expr };
                Ok(self.alloc(instr, typ, *span))
            },

            Expr::CompoundAssignment { target, operator, value, span } => {
                let target_lowered = self.lower_assignment_target(target, *span)?;
                let (typ, expr, operator) = (target_lowered.typ, target_lowered.expr, *operator);

                // the operand rules are the ones 'target <op> value' would obey,
                // so 'x <<= 1' accepts a shift amount of another width just as 'x << 1' does
                let hint = match operator {
                    BinaryOperator::Shl | BinaryOperator::Shr => None,
                    _ => Some(self.infer.resolve_shallow(target_lowered.typ)),
                };
                let value = self.lower_expr(value, hint)?;
                let result = self.type_for_binary(&operator, typ, value.typ, *span)?;
                self.assert_type(typ, result, *span)?;

                let instr =
                    ExpressionKind::CompoundAssign { target: expr, operator, value: value.expr };
                Ok(self.alloc(instr, typ, *span))
            },

            Expr::Struct { name, fields, span, type_args } => {
                let typ = if !type_args.is_empty() {
                    let mut resolved = Vec::with_capacity(type_args.len());
                    for arg in type_args {
                        resolved.push(self.resolve_type(arg.value_ref(), arg.span())?);
                    }

                    let typ = self.scope.generic_adt(name, &resolved, *span)?;
                    match typ.kind() {
                        TypeKind::Adt(id, _) if self.scope[id].is_struct() => typ,
                        _ => return Err(hir_error!(*span, UnknownType { name })),
                    }
                } else {
                    let symbol = self.scope.symbols.insert(name);
                    let id = self
                        .scope
                        .adts
                        .struct_map
                        .get(&symbol)
                        .copied()
                        .ok_or_else(|| hir_error!(*span, UnknownType { name }))?;
                    self.scope.types.adt(id, &[])
                };

                let TypeKind::Adt(id, generic_args) = typ.kind() else {
                    unreachable!("struct literal type must be an ADT")
                };

                let lowered = self.lower_struct_fields(
                    id,
                    generic_args,
                    fields,
                    *span,
                    false,
                    |this, _, expected, field| {
                        let value = this.lower_expr(&field.value, Some(expected))?;
                        this.assert_type(expected, value.typ, value.span)?;
                        Ok(value.expr)
                    },
                )?;

                let fields = self.arena.alloc_slice_copy(&lowered);
                Ok(self.alloc(ExpressionKind::Struct { id, fields }, typ, *span))
            },

            Expr::Try { value, span } => self.lower_try(value, *span),
            Expr::Interpolated { span, .. } => Err(hir_error!(*span, InterpolationOutsidePrint)),
            Expr::Block { block, .. } => self.lower_block_expr(block, hint),
            Expr::If { inner, .. } => self.lower_if_expr(inner, hint),
            Expr::Match { inner, .. } => self.lower_match(inner, hint),

            Expr::Field { expr: base, field, span } => {
                let base_lowered = self.lower_expr(base, None)?;
                let is_place = matches!(
                    &base_lowered.expr.kind,
                    ExpressionKind::Local(_)
                        | ExpressionKind::Field { .. }
                        | ExpressionKind::Index { .. }
                        | ExpressionKind::Unary { operator: UnaryOperator::Deref, .. }
                );
                if !is_place {
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
                let array_type = self.array_type(element_type, elements.len() as u32);
                let elements = self.arena.alloc_slice_copy(&lowered);

                Ok(self.alloc(ExpressionKind::Array { elements }, array_type, *span))
            },

            Expr::ArrayRepeat { value, count, span } => {
                let element_hint = hint.and_then(|h| self.element_type(h));
                let value = self.lower_expr(value, element_hint)?;
                let count = *count as u32;
                let array_type = self.array_type(value.typ, count);

                Ok(self.alloc(
                    ExpressionKind::ArrayRepeat { value: value.expr, count },
                    array_type,
                    *span,
                ))
            },

            Expr::Index { base, index, span } => {
                let base_lowered = self.lower_expr(base, None)?;
                let Some(element) = self.element_type(base_lowered.typ) else {
                    return self.lower_index_overload(base_lowered, index, *span);
                };

                let index_lowered = self.lower_expr(index, Some(self.scope.types.common.uptr))?;
                let index_type = self.infer.resolve_shallow(index_lowered.typ);
                if index_type.is_infer() {
                    self.infer.unify(self.scope.types.common.uptr, index_type).ok();
                }

                let index_type = self.infer.resolve_shallow(index_type);
                if !index_type.is_integer() {
                    return Err(hir_error!(
                        index_lowered.span,
                        TypeMismatch { expected: self.scope.types.common.uptr, found: index_type }
                    ));
                }

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
                    return self.lower_method_call(receiver, method_name, args, type_args, *span);
                }

                let function_id = match callee.as_ref() {
                    Expr::Identifier(name, _) => self
                        .scope
                        .resolve_function_call(None, name)
                        .ok_or_else(|| hir_error!(*span, UnknownFunction { name }))?,

                    other => {
                        let name = self.arena.alloc_str(&format!("{other:?}"));
                        return Err(hir_error!(*span, UnknownFunction { name }));
                    },
                };

                match self.scope.functions.defs[function_id].body.is_some() {
                    true => self.lower_generic_call(
                        function_id,
                        GenericCall::Free,
                        args,
                        type_args,
                        *span,
                    ),
                    _ => self.lower_direct_call(function_id, args, type_args, *span),
                }
            },

            Expr::QualifiedCall { path, name, args, span, type_args } => {
                let path = &*self.resolve_self_path(path);

                if let Some(lowered) =
                    self.lower_variant(path, name, args, type_args, hint, *span)?
                {
                    return Ok(lowered);
                }

                if path.len() == 1 {
                    let name_symbol = self.scope.symbols.insert(name);
                    if let Some(function) =
                        self.resolve_concrete_bound_function(path[0], name_symbol)
                    {
                        return self.lower_direct_call(function, args, type_args, *span);
                    }

                    if let Some((param, interface, signature)) =
                        self.resolve_bound_function(path[0], name_symbol)
                    {
                        return self.lower_param_function_call(
                            param,
                            interface,
                            name_symbol,
                            signature,
                            args,
                            *span,
                        );
                    }
                }

                let id =
                    self.scope.resolve_qualified_function_call(path, name).ok_or_else(|| {
                        let qualifier = path.join("::");
                        let name = qualified(self.arena, &qualifier, name);
                        hir_error!(*span, UnknownFunction { name })
                    })?;

                match self.scope.functions.defs[id].body.is_some() {
                    true => self.lower_generic_call(id, GenericCall::Free, args, type_args, *span),
                    _ => self.lower_direct_call(id, args, type_args, *span),
                }
            },

            Expr::TypeIntrinsic { kind, path, typ, span } => {
                let name = kind.into();
                let exists = match path {
                    Some(path) => self.scope.resolve_qualified_function_call(path, name).is_some(),
                    _ => self.scope.resolve_function_call(None, name).is_some(),
                };

                if !exists {
                    let name = path.as_ref().map_or(name, |path| {
                        let qualifier = path.join("::");
                        qualified(self.arena, &qualifier, name)
                    });
                    return Err(hir_error!(*span, UnknownFunction { name }));
                }

                let typ = self.resolve_type(&typ.value(), typ.span())?;

                Ok(self.alloc(
                    ExpressionKind::TypeIntrinsic { kind: *kind, typ },
                    self.scope.types.common.uptr,
                    *span,
                ))
            },
        }
    }

    fn lower_method_call(
        &mut self,
        receiver: &expression::Expression<'src>,
        method_name: &'src str,
        args: &[expression::Expression<'src>],
        type_args: &[Spanned<statement::Type<'src>>],
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        use TypeKind::*;

        let receiver_lowered = self.lower_expr(receiver, None)?;
        let receiver_type = receiver_lowered.typ;
        let receiver_base_type = receiver_type.strip_reference();
        let method_symbol = self.scope.symbols.insert(method_name);

        let lookup_type = match receiver_base_type.kind() {
            Slice { element, .. } => self.scope.types.slice(element, false),
            Array(id) => {
                let resolved = self.infer.resolve_or_default(self.scope.arrays.get(id).element);
                let slice = self.scope.types.slice(resolved, false);
                slice
            },
            _ => receiver_base_type,
        };

        let struct_name = match receiver_base_type.kind() {
            Adt(id, _) => self.scope.symbols.get(self[id].name).to_string(),
            _ => receiver_base_type.to_string(),
        };

        if let GenericParam(param) = lookup_type.kind()
            && let Some((interface, signature)) = self.resolve_bound_method(param, method_symbol)
        {
            return self.lower_param_method_call(
                param,
                interface,
                method_symbol,
                signature,
                receiver,
                receiver_lowered,
                args,
                span,
            );
        }

        let function = self.scope.method(lookup_type, method_symbol).ok_or_else(|| {
            let struct_name = self.arena.alloc_str(&struct_name);
            hir_error!(span, UnknownMethod { struct_name, name: method_name })
        })?;

        if let Some(intrinsic) = self.scope.functions.defs[function].kind.intrinsic() {
            if intrinsic.is_wrapping() {
                return self.lower_wrapping_call(
                    intrinsic,
                    function,
                    receiver_lowered,
                    args,
                    method_name,
                    span,
                );
            }

            let return_type = self.scope.functions.defs[function].return_type;
            let receiver = self.coerce_method_receiver(receiver_lowered, lookup_type);
            let args = self.arena.alloc_slice_copy(&[receiver]);
            let callee = self
                .alloc(
                    ExpressionKind::Path(self.scope.functions.defs[function].name),
                    self.scope.types.common.unit,
                    span,
                )
                .expr;

            let lowered = self.alloc(ExpressionKind::Call { callee, args }, return_type, span);
            self.typeck
                .type_dependent_defs
                .insert(lowered.expr.id, Res::Intrinsic(intrinsic));

            return Ok(lowered);
        }

        let signature = self.scope.functions.defs[function].clone();
        assert!(signature.receiver_type().is_some(), "method call resolved to a free function");

        let receiver_lowered = match (signature.receiver_mutable(), receiver_type.kind()) {
            (true, Ref { mutable: true, .. } | Slice { mutable: true, .. }) => receiver_lowered,
            (true, _) => self.lower_mutable_place(receiver, None)?,
            _ => receiver_lowered,
        };
        let base_local = place_base_local(receiver_lowered.expr);
        let receiver_type = receiver_lowered.typ;

        let receiver_is_mut_ref =
            matches!(receiver_type.kind(), Ref { mutable: true, .. } | Slice { mutable: true, .. });

        if signature.receiver_mutable()
            && !receiver_is_mut_ref
            && base_local.filter(|&id| self[id].mutable).is_none()
        {
            let name = base_local.map_or("temporary", |id| {
                self.arena.alloc_str(self.scope.symbols.get(self[id].name))
            });
            let decl = base_local.and_then(|id| collect::source_span(self[id].decl_span));

            return Err(hir_error!(span, ImmutableBind { name, decl }));
        }

        if signature.receiver_mutable() && !receiver_is_mut_ref {
            self.make_place_mutable(receiver_lowered.expr)?;
        }

        let explicit_params = signature.explicit_params();
        self.check_arity(
            method_name,
            explicit_params.len(),
            args.len(),
            collect::source_span(signature.decl_span),
            span,
        )?;

        if self.scope.functions.defs[function].body.is_some() {
            return self.lower_generic_call(
                function,
                GenericCall::Method { name: method_symbol, receiver: receiver_lowered.expr },
                args,
                type_args,
                span,
            );
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
            span,
        );
        self.check_call_safety(function, span);
        self.typeck.type_dependent_defs.insert(lowered.expr.id, Res::Function(function));
        if !substs.is_empty() {
            self.typeck.node_args.insert(lowered.expr.id, substs);
        }

        Ok(lowered)
    }

    /// reports a match that leaves some value of the scrutinee unhandled
    fn check_exhaustive(
        &self,
        scrutinee: Type<'hir>,
        arms: &[Arm<'hir>],
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        let context =
            exhaustive::Context { adts: &self.scope.adts.defs, symbols: &self.scope.symbols };
        let rows: Vec<_> = arms
            .iter()
            .map(|arm| exhaustive::Row { pattern: arm.pattern, guarded: arm.guard.is_some() })
            .collect();

        let Some(witness) = context.missing_patterns(scrutinee, &rows, 1).into_iter().next() else {
            return Ok(());
        };
        let missing = self.arena.alloc_str(&witness.0);
        Err(hir_error!(span, NonExhaustiveMatch { typ: scrutinee, missing }))
    }

    pub(in crate::hir::lower) fn lower_block_expr(
        &mut self,
        block: &statement::Block<'src>,
        hint: Option<Type<'hir>>,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        use statement::Statement as Stmt;

        self.push_scope();

        let (leading, tail) = match block.statements.split_last() {
            Some((Stmt::Expr { expr, semi: false, .. }, leading)) => (leading, Some(expr)),
            _ => (block.statements.as_slice(), None),
        };

        let (mut statements, mut diverges) = (Vec::with_capacity(leading.len()), false);
        for statement in leading {
            match self.lower_statement(statement, false) {
                Ok((statement, returns)) => {
                    diverges |= returns;
                    statements.push(statement);
                },
                Err(error) => self.soft(error),
            }
        }

        let tail = tail.map(|tail| self.lower_expr(tail, hint)).transpose()?;
        self.pop_scope();

        let typ = match tail {
            Some(tail) => tail.typ,
            None if diverges => self.scope.types.common.never,
            None => self.scope.types.common.unit,
        };
        let statements = self.arena.alloc_slice_copy(&statements);
        let tail = tail.map(|tail| tail.expr);

        Ok(self.alloc(ExpressionKind::Block { statements, tail }, typ, block.span))
    }

    /// an interpolated literal flattens into the print call's own arguments
    pub(super) fn lower_print_args(
        &mut self,
        args: &[expression::Expression<'src>],
    ) -> Result<Vec<&'hir Expression<'hir>>, HirError<'hir>> {
        use expression::Segment;

        let mut lowered = Vec::with_capacity(args.len());
        for arg in args {
            let expression::Expression::Interpolated { segments, span } = arg else {
                let value = self.lower_expr(arg, None)?;
                lowered.push(self.printable(value)?);
                continue;
            };

            for segment in segments {
                match segment {
                    Segment::Text(text) => {
                        let symbol = self.scope.symbols.insert(text);
                        let kind = ExpressionKind::Literal(Literal::Str(symbol));

                        lowered.push(self.alloc(kind, self.scope.types.common.str, *span).expr);
                    },
                    Segment::Value(expr) => {
                        let value = self.lower_expr(expr, None)?;
                        lowered.push(self.printable(value)?);
                    },
                }
            }
        }

        Ok(lowered)
    }

    pub(super) fn lower_match(
        &mut self,
        match_stmt: &statement::Match<'src>,
        hint: Option<Type<'hir>>,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let scrutinee = self.lower_expr(&match_stmt.scrutinee, None)?;
        let mut arms = Vec::with_capacity(match_stmt.arms.len());

        let mut branches = Branches::new(hint);

        for arm in &match_stmt.arms {
            self.push_scope();

            let pattern =
                self.lower_pattern(scrutinee.typ, arm.pattern.value_ref(), arm.pattern.span())?;
            let pattern = self.arena.alloc(pattern);

            let guard = arm.guard.as_ref().map(|g| self.lower_expr(g, None)).transpose()?;
            if let Some(ref g) = guard {
                self.assert_type(TypeKind::Bool, g.typ, g.span)?;
            }

            let body = match &arm.body {
                statement::ArmBody::Expr(expr) => {
                    let body = self.lower_expr(expr, branches.hint())?;
                    self.record_branch(&mut branches, &body)?;

                    ArmBody::Expr(body.expr)
                },
                // an arm that leaves the match yields nothing to unify against,
                // so it only makes the match itself diverge when every arm does
                statement::ArmBody::Return(returned) => {
                    let value = self.lower_return_value(returned)?;
                    branches.diverge(self.scope.types.common.never);

                    ArmBody::Return(value)
                },
                statement::ArmBody::Break(span) => {
                    if self.loop_depth == 0 {
                        return Err(hir_error!(*span, LoopControlOutsideLoop { kind: "break" }));
                    }

                    branches.diverge(self.scope.types.common.never);
                    ArmBody::Break
                },
                statement::ArmBody::Continue(span) => {
                    if self.loop_depth == 0 {
                        return Err(hir_error!(*span, LoopControlOutsideLoop { kind: "continue" }));
                    }

                    branches.diverge(self.scope.types.common.never);
                    ArmBody::Continue
                },
            };

            self.pop_scope();
            arms.push(Arm { pattern, guard: guard.map(|g| g.expr), body, span: arm.span });
        }

        self.check_exhaustive(scrutinee.typ, &arms, match_stmt.span)?;

        // every arm diverging makes the match itself diverge,
        // whatever the context expected
        let return_type = branches.resolve(self.scope.types.common.unit);
        let arms = self.arena.alloc_slice_copy(&arms);

        Ok(self.alloc(
            ExpressionKind::Match { scrutinee: scrutinee.expr, arms },
            return_type,
            match_stmt.span,
        ))
    }

    /// an `if` in value position, folds one branch into the running unification, per [Branches]
    fn record_branch(
        &mut self,
        branches: &mut Branches<'hir>,
        branch: &Lowered<'hir>,
    ) -> Result<(), HirError<'hir>> {
        if branch.typ.diverges() {
            branches.diverge(branch.typ);
            return Ok(());
        }

        Ok(match branches.unified {
            Some(expected) => self.assert_type(expected, branch.typ, branch.span)?,
            _ => branches.unified = Some(branch.typ),
        })
    }

    fn lower_if_expr(
        &mut self,
        if_stmt: &statement::If<'src>,
        hint: Option<Type<'hir>>,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        use statement::Else;

        let condition = self.lower_expr(&if_stmt.condition, None)?;
        self.assert_type(TypeKind::Bool, condition.typ, condition.span)?;
        let then_block = self.lower_block_expr(&if_stmt.then_branch, hint)?;

        let mut branches = Branches::new(hint);
        self.record_branch(&mut branches, &then_block)?;
        let onward = branches.hint();

        let else_block = match if_stmt.else_branch.as_deref() {
            Some(Else::Block(block)) => Some(self.lower_block_expr(block, onward)?),
            Some(Else::If(inner)) => Some(self.lower_if_expr(inner, onward)?),
            Some(Else::Expr(expr)) => {
                let lowered = self.lower_expr(expr, None)?;
                let statements = self.arena.alloc_slice_copy(&[Statement::Expr(lowered.expr)]);
                let kind = ExpressionKind::Block { statements, tail: None };

                Some(self.alloc(kind, self.scope.types.common.unit, lowered.span))
            },
            _ => None,
        };

        let typ = match else_block {
            Some(ref other) => {
                self.record_branch(&mut branches, other)?;
                branches.resolve(self.scope.types.common.unit)
            },
            _ => {
                if !then_block.typ.diverges() {
                    self.assert_type(TypeKind::Unit, then_block.typ, then_block.span)?;
                }

                self.scope.types.common.unit
            },
        };

        let kind = ExpressionKind::If {
            condition: condition.expr,
            then_block: then_block.expr,
            else_block: else_block.map(|block| block.expr),
        };

        Ok(self.alloc(kind, typ, if_stmt.span))
    }

    fn printable(
        &mut self,
        value: Lowered<'hir>,
    ) -> Result<&'hir Expression<'hir>, HirError<'hir>> {
        let typ = self.infer.resolve_or_default(value.typ);

        match lang::print_kind(typ) {
            Some(_) => Ok(value.expr),
            _ => Err(hir_error!(value.span, NotPrintable { typ })),
        }
    }

    /// `?` expands here into the match a user would otherwise write by hand
    fn lower_try(
        &mut self,
        value: &expression::Expression<'src>,
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let scrutinee = self.lower_expr(value, None)?;
        let (id, generic_args, family) = self.try_family(scrutinee.typ, scrutinee.span)?;

        let (success, failure) = {
            let def = &self.scope[id];
            let index_of = |name: &str| {
                def.variants().iter().position(|v| self.scope.symbols.get(v.name) == name)
            };

            match (index_of(family.success()), index_of(family.failure())) {
                (Some(success), Some(failure)) => (success, failure),
                _ => {
                    return Err(hir_error!(scrutinee.span, TryOnNonTryable { typ: scrutinee.typ }));
                },
            }
        };

        let payload_of = |this: &Self, index: usize| {
            this.scope[id].variants()[index]
                .payload
                .map(|p| p.subst(&this.scope.types, &this.scope.arrays, generic_args))
        };

        let Some(unwrapped) = payload_of(self, success) else {
            return Err(hir_error!(scrutinee.span, TryOnNonTryable { typ: scrutinee.typ }));
        };
        self.check_try_return(id, generic_args, family, span)?;

        // a name no identifier can spell, so the binding can never be shadowed by, or collide with, anything the user wrote
        let binding = self.scope.symbols.insert("?");

        self.push_scope();
        let local = self.declare_local(binding, unwrapped, false, span)?;
        let success_arm = Arm {
            pattern: self.arena.alloc(Pattern {
                kind: PatternKind::Variant {
                    id,
                    variant_idx: success,
                    sub: Some(
                        self.arena.alloc(Pattern { kind: PatternKind::Binding(local), span }),
                    ),
                },
                span,
            }),
            guard: None,
            body: ArmBody::Expr(self.alloc(ExpressionKind::Local(local), unwrapped, span).expr),
            span,
        };
        self.pop_scope();
        self.push_scope();

        let failure_arm =
            self.lower_try_failure_arm(id, failure, payload_of(self, failure), span)?;
        self.pop_scope();

        let arms = self.arena.alloc_slice_copy(&[success_arm, failure_arm]);

        Ok(self.alloc(ExpressionKind::Match { scrutinee: scrutinee.expr, arms }, unwrapped, span))
    }

    /// the arm that leaves the function, rebuilding the failure in the return
    /// type: `None -> return None`, or `Failure(e) -> return Failure(e)`
    fn lower_try_failure_arm(
        &mut self,
        id: AdtId,
        index: usize,
        payload: Option<Type<'hir>>,
        span: Span,
    ) -> Result<Arm<'hir>, HirError<'hir>> {
        let (sub, args) = match payload {
            Some(typ) => {
                let binding = self.scope.symbols.insert("?");
                let local = self.declare_local(binding, typ, false, span)?;
                let carried = self.alloc(ExpressionKind::Local(local), typ, span).expr;

                (
                    Some(&*self.arena.alloc(Pattern { kind: PatternKind::Binding(local), span })),
                    &*self.arena.alloc_slice_copy(&[carried]),
                )
            },
            _ => (None, &*self.arena.alloc_slice_copy(&[])),
        };

        let name = self.scope[id].variants()[index].name;
        let callee =
            self.alloc(ExpressionKind::Path(name), self.scope.types.common.unit, span).expr;
        let rebuilt = self.alloc(ExpressionKind::Call { callee, args }, self.return_type, span);
        self.typeck
            .type_dependent_defs
            .insert(rebuilt.expr.id, Res::Variant { id, index });

        Ok(Arm {
            pattern: self.arena.alloc(Pattern {
                kind: PatternKind::Variant { id, variant_idx: index, sub },
                span,
            }),
            guard: None,
            body: ArmBody::Return(Some(rebuilt.expr)),
            span,
        })
    }

    fn try_family(
        &self,
        typ: Type<'hir>,
        span: Span,
    ) -> Result<(AdtId, &'hir [Type<'hir>], TryFamily), HirError<'hir>> {
        let TypeKind::Adt(id, args) = typ.kind() else {
            return Err(hir_error!(span, TryOnNonTryable { typ }));
        };

        let def = &self.scope[id];
        let family = match def.is_enum().then(|| self.scope.symbols.get(def.name)) {
            Some("Optional") => TryFamily::Optional,
            Some("Result") => TryFamily::Result,
            _ => return Err(hir_error!(span, TryOnNonTryable { typ })),
        };

        Ok((id, args, family))
    }

    /// the desugaring contains a `return`, so the enclosing signature has to be able to carry the failure onwards
    fn check_try_return(
        &self,
        id: AdtId,
        args: &'hir [Type<'hir>],
        family: TryFamily,
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        let found = self.return_type;
        let mismatch = |suggestion| Err(hir_error!(span, TryReturnMismatch { found, suggestion }));

        let TypeKind::Adt(return_id, return_args) = found.kind() else {
            return mismatch("Change the return type to an `Optional` or a `Result`");
        };

        if return_id != id {
            return mismatch(match family {
                TryFamily::Optional => {
                    "An absent `Optional` carries no failure value to return; return an `Optional` here, or match on it explicitly"
                },
                TryFamily::Result => {
                    "A `Result` failure carries a value this function cannot return; return a `Result` here, or match on it explicitly"
                },
            });
        }

        match family {
            TryFamily::Result if args.get(1) != return_args.get(1) => mismatch(
                "The failure types must match exactly; there is no conversion between them yet",
            ),
            _ => Ok(()),
        }
    }

    fn lower_variant(
        &mut self,
        path: &[&str],
        name: &'src str,
        args: &[expression::Expression<'src>],
        type_args: &[Spanned<statement::Type<'src>>],
        hint: Option<Type<'hir>>,
        span: Span,
    ) -> Result<Option<Lowered<'hir>>, HirError<'hir>> {
        if path.len() != 1 {
            return Ok(None);
        }
        let qualifier = path[0];
        let qualifier_symbol = self.scope.symbols.insert(qualifier);

        let Some(enum_type) =
            self.resolve_enum_type(qualifier, qualifier_symbol, type_args, hint, span)?
        else {
            return Ok(None);
        };
        let TypeKind::Adt(enum_id, generic_args) = enum_type.kind() else {
            unreachable!("resolved enum type must be an ADT")
        };

        let variant_symbol = self.scope.symbols.insert(name);
        let enum_def = &self.scope[enum_id];
        let Some(index) = enum_def.variants().iter().position(|v| v.name == variant_symbol) else {
            return Ok(None);
        };

        let payload_typ = enum_def.variants()[index]
            .payload
            .map(|payload| payload.subst(&self.scope.types, &self.scope.arrays, generic_args));
        let payload = self.lower_variant_payload(name, payload_typ, args, span)?;

        let callee = self
            .alloc(ExpressionKind::Path(variant_symbol), self.scope.types.common.unit, span)
            .expr;
        let arguments: &[&Expression] = payload.map_or(&[], |p| self.arena.alloc_slice_copy(&[p]));

        let lowered = self.alloc(ExpressionKind::Call { callee, args: arguments }, enum_type, span);
        self.typeck
            .type_dependent_defs
            .insert(lowered.expr.id, Res::Variant { id: enum_id, index });

        Ok(Some(lowered))
    }

    fn lower_variant_payload(
        &mut self,
        name: &'src str,
        typ: Option<Type<'hir>>,
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
            (Some(_), _) => {
                Err(hir_error!(span, ArityMismatch { name, expected: 1, found: 0, decl: None }))
            },
            (_, _) => Err(hir_error!(
                span,
                ArityMismatch { name, expected: 0, found: args.len(), decl: None }
            )),
        }
    }

    fn type_for_binary(
        &mut self,
        operator: &BinaryOperator,
        left: Type<'hir>,
        right: Type<'hir>,
        span: Span,
    ) -> Result<Type<'hir>, HirError<'hir>> {
        let i32_type = self.scope.types.common.i32;
        let type_mismatch = |found| hir_error!(span, TypeMismatch { expected: i32_type, found });

        match operator {
            BinaryOperator::Add
            | BinaryOperator::Sub
            | BinaryOperator::Mul
            | BinaryOperator::Div => {
                if !self.check_type_at(left, right, span, None)? {
                    return Ok(self.scope.types.common.error);
                }

                let left = self.infer.resolve_shallow(left);
                match left.is_number() || left.is_infer() {
                    true => Ok(left),
                    _ => Err(type_mismatch(left)),
                }
            },

            BinaryOperator::Eq | BinaryOperator::Ne => {
                self.assert_type(left, right, span)?;
                Ok(self.scope.types.common.bool)
            },

            BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq => {
                self.assert_type(left, right, span)?;
                let left = self.infer.resolve_shallow(left);
                match left.is_number() || left.is_infer() || left == self.scope.types.common.char {
                    true => Ok(self.scope.types.common.bool),
                    _ => Err(type_mismatch(left)),
                }
            },

            BinaryOperator::And | BinaryOperator::Or => {
                self.assert_type(TypeKind::Bool, left, span)?;
                self.assert_type(TypeKind::Bool, right, span)?;

                Ok(self.scope.types.common.bool)
            },

            BinaryOperator::BitAnd | BinaryOperator::BitOr | BinaryOperator::BitXor => {
                if !self.check_type_at(left, right, span, None)? {
                    return Ok(self.scope.types.common.error);
                }

                let left = self.infer.resolve_shallow(left);
                match left == self.scope.types.common.bool || left.is_integer() || left.is_infer() {
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

    fn resolve_enum_type(
        &mut self,
        qualifier: &str,
        symbol: SymbolId,
        type_args: &[Spanned<statement::Type<'src>>],
        hint: Option<Type<'hir>>,
        span: Span,
    ) -> Result<Option<Type<'hir>>, HirError<'hir>> {
        if let Some(&definition) = self.scope.adts.enum_map.get(&symbol)
            && !self.scope[definition].generics.is_empty()
        {
            // explicit generic arguments passed Enum::<Int>::Variant
            if !type_args.is_empty() {
                let mut resolved = Vec::with_capacity(type_args.len());
                for arg in type_args {
                    resolved.push(self.resolve_type(arg.value_ref(), arg.span())?);
                }

                let typ = self.scope.generic_adt(qualifier, &resolved, span)?;

                if let TypeKind::Adt(id, _) = typ.kind()
                    && self.scope[id].is_enum()
                {
                    return Ok(Some(typ));
                }
            }
        }

        // type infered via type hint: let x: Enum<Int> = Variant;
        if let Some(hint) = hint
            && let TypeKind::Adt(id, _) = hint.kind()
            && self.scope[id].is_enum()
        {
            return Ok(Some(hint));
        }

        if let Some(&id) = self.scope.adts.enum_map.get(&symbol)
            && self.scope[id].generics.is_empty()
        {
            return Ok(Some(self.scope.types.adt(id, &[])));
        }

        Ok(None)
    }

    fn coerce_array_to_slice(
        &mut self,
        operand: Type<'hir>,
        hint: Option<Type<'hir>>,
    ) -> Option<Type<'hir>> {
        let slice = hint?;
        let TypeKind::Slice { element, .. } = slice.kind() else {
            return None;
        };
        let TypeKind::Array(id) = operand.kind() else {
            return None;
        };

        let array_element = self.scope.arrays.get(id).element;
        self.infer.unify(array_element, element.into()).ok().map(|()| slice)
    }

    fn coerce_method_receiver(
        &mut self,
        receiver: Lowered<'hir>,
        lookup_type: Type<'hir>,
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

    #[inline]
    fn element_type(&self, typ: Type<'hir>) -> Option<Type<'hir>> {
        match typ.kind() {
            TypeKind::Array(id) => Some(self.scope.arrays.get(id).element),
            TypeKind::Slice { element, .. } => Some(element.into()),
            TypeKind::Ref { to, .. } => self.element_type(to.into()),
            _ => None,
        }
    }

    #[inline]
    fn array_type(&mut self, element: Type<'hir>, len: u32) -> Type<'hir> {
        let element = self.infer.resolve_shallow(element);
        let id = self.scope.arrays.intern(element, len);
        self.scope.types.array(id)
    }

    fn lookup_field(
        &mut self,
        current: Type<'hir>,
        name: &str,
        span: Span,
    ) -> Result<(SymbolId, Type<'hir>), HirError<'hir>> {
        #[rustfmt::skip]
        let (sid, generic_args) = match current.kind() {
            TypeKind::Adt(id, args) if self.scope[id].is_struct() => (id, args),
            TypeKind::Ref { to, .. } => match to.kind() {
                TypeKind::Adt(id, args) if self.scope[id].is_struct() => (id, args),
                _ => return Err(hir_error!(span, TypeMismatch {
                    expected: self.scope.types.adt(Default::default(), &[]),
                    found: current
                })),
            },
            _ => return Err(hir_error!(span, TypeMismatch {
                expected: self.scope.types.adt(Default::default(), &[]),
                found: current
            })),
        };

        let sym = self.scope.symbols.insert(name);
        let def = &self.scope[sid];
        let struct_name = self.arena.alloc_str(self.scope.symbols.get(def.name));

        let field = def.field(sym).ok_or_else(|| {
            let field = self.arena.alloc_str(name);
            hir_error!(span, UnknownField { struct_name, field })
        })?;

        Ok((sym, field.typ.subst(&self.scope.types, &self.scope.arrays, generic_args)))
    }
}

impl<'hir> Branches<'hir> {
    #[inline]
    const fn new(hint: Option<Type<'hir>>) -> Self {
        Self { unified: hint, divergent: None }
    }

    #[inline]
    const fn hint(&self) -> Option<Type<'hir>> {
        self.unified
    }

    #[inline]
    fn diverge(&mut self, never: Type<'hir>) {
        self.divergent = self.divergent.or(Some(never));
    }

    #[inline]
    fn resolve(self, unit: Type<'hir>) -> Type<'hir> {
        self.unified.or(self.divergent).unwrap_or(unit)
    }
}

impl TryFamily {
    #[inline]
    const fn success<'s>(self) -> &'s str {
        match self {
            Self::Optional => "Some",
            Self::Result => "Success",
        }
    }

    #[inline]
    const fn failure<'s>(self) -> &'s str {
        match self {
            Self::Optional => "None",
            Self::Result => "Failure",
        }
    }
}
