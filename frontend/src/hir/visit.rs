use crate::hir::{
    Arm, ArmBody, Block, Constant, Expression, ExpressionKind as ExprKind, Function, FunctionKind,
    Local, LoopKind, Method, Parameter, Pattern, PatternKind as PattKind, Res, Statement as Stmt,
    Type, TypeckResults,
};

pub trait Visitor<'hir>: Sized {
    fn visit_function(&mut self, function: &'hir Function<'hir>) {
        walk_function(self, function);
    }

    fn visit_constant(&mut self, constant: &'hir Constant<'hir>) {
        walk_constant(self, constant);
    }

    fn visit_block(&mut self, block: &'hir Block<'hir>) {
        walk_block(self, block);
    }

    fn visit_statement(&mut self, statement: &'hir Stmt<'hir>) {
        walk_statement(self, statement);
    }

    fn visit_expression(&mut self, expression: &'hir Expression<'hir>) {
        walk_expression(self, expression);
    }

    fn visit_pattern(&mut self, pattern: &'hir Pattern<'hir>) {
        walk_pattern(self, pattern);
    }
}

pub trait Folder<'hir>: Sized {
    fn arena(&self) -> &'hir bumpalo::Bump;

    fn fold_type(&mut self, typ: Type<'hir>) -> Type<'hir> {
        typ
    }

    fn fold_res(&mut self, resolution: Res) -> Res {
        resolution
    }

    fn fold_function(&mut self, function: &Function<'hir>) -> Function<'hir> {
        fold_function(self, function)
    }

    fn fold_constant(&mut self, constant: &Constant<'hir>) -> Constant<'hir> {
        fold_constant(self, constant)
    }

    fn fold_block(&mut self, block: Block<'hir>) -> Block<'hir> {
        fold_block(self, block)
    }

    fn fold_statement(&mut self, statement: Stmt<'hir>) -> Stmt<'hir> {
        fold_statement(self, statement)
    }

    fn fold_expression(&mut self, expression: &'hir Expression<'hir>) -> &'hir Expression<'hir> {
        fold_expression(self, expression)
    }

    fn fold_pattern(&mut self, pattern: &'hir Pattern<'hir>) -> &'hir Pattern<'hir> {
        fold_pattern(self, pattern)
    }
}

pub fn walk_function<'hir, V: Visitor<'hir>>(visitor: &mut V, function: &'hir Function<'hir>) {
    visitor.visit_block(&function.body);
}

pub fn walk_constant<'hir, V: Visitor<'hir>>(visitor: &mut V, constant: &'hir Constant<'hir>) {
    visitor.visit_expression(constant.value);
}

pub fn walk_block<'hir, V: Visitor<'hir>>(visitor: &mut V, block: &'hir Block<'hir>) {
    for statement in block.statements {
        visitor.visit_statement(statement);
    }
}

pub fn walk_statement<'hir, V: Visitor<'hir>>(visitor: &mut V, statement: &'hir Stmt<'hir>) {
    match statement {
        Stmt::LetInit { init, .. } | Stmt::Expr(init) => visitor.visit_expression(init),
        Stmt::Return(value) => {
            if let Some(value) = value {
                visitor.visit_expression(value);
            }
        },
        Stmt::If { condition, then_block, else_block } => {
            visitor.visit_expression(condition);
            visitor.visit_block(then_block);
            if let Some(else_block) = else_block {
                visitor.visit_block(else_block);
            }
        },
        Stmt::Loop { kind, body } => {
            match kind {
                LoopKind::Range { start, end, .. } => {
                    visitor.visit_expression(start);
                    visitor.visit_expression(end);
                },
                LoopKind::Iterable { iterable, .. } => visitor.visit_expression(iterable),
                LoopKind::Infinite => {},
            }
            visitor.visit_block(body);
        },
        Stmt::Block(block) => visitor.visit_block(block),
        Stmt::LetUninit { .. } | Stmt::Break | Stmt::Continue => {},
    }
}

pub fn walk_expression<'hir, V: Visitor<'hir>>(
    visitor: &mut V,
    expression: &'hir Expression<'hir>,
) {
    match expression.kind {
        ExprKind::Unary { expr, .. }
        | ExprKind::Field { base: expr, .. }
        | ExprKind::ArrayRepeat { value: expr, .. }
        | ExprKind::Cast { from: expr, .. } => visitor.visit_expression(expr),
        ExprKind::Binary { left, right, .. }
        | ExprKind::Assign { target: left, value: right }
        | ExprKind::CompoundAssign { target: left, value: right, .. }
        | ExprKind::Index { base: left, index: right } => {
            visitor.visit_expression(left);
            visitor.visit_expression(right);
        },
        ExprKind::Struct { fields, .. } => {
            for (_, field) in fields {
                visitor.visit_expression(field);
            }
        },
        ExprKind::Array { elements } => {
            for element in elements {
                visitor.visit_expression(element);
            }
        },
        ExprKind::Call { callee, args } => {
            visitor.visit_expression(callee);
            for argument in args {
                visitor.visit_expression(argument);
            }
        },
        ExprKind::MethodCall { receiver, args, .. } => {
            visitor.visit_expression(receiver);
            for argument in args {
                visitor.visit_expression(argument);
            }
        },
        ExprKind::Match { scrutinee, arms } => {
            visitor.visit_expression(scrutinee);
            for arm in arms {
                visitor.visit_pattern(arm.pattern);
                if let Some(guard) = arm.guard {
                    visitor.visit_expression(guard);
                }
                if let Some(body) = arm.body.value() {
                    visitor.visit_expression(body);
                }
            }
        },
        ExprKind::Literal(_)
        | ExprKind::Local(_)
        | ExprKind::Path(_)
        | ExprKind::Const(_)
        | ExprKind::ParamConst { .. }
        | ExprKind::Static(_)
        | ExprKind::TypeIntrinsic { .. } => {},
    }
}

pub fn walk_pattern<'hir, V: Visitor<'hir>>(visitor: &mut V, pattern: &'hir Pattern<'hir>) {
    match pattern.kind {
        PattKind::Bind { sub, .. } => visitor.visit_pattern(sub),
        PattKind::Variant { sub, .. } => {
            if let Some(sub) = sub {
                visitor.visit_pattern(sub);
            }
        },
        PattKind::Struct { fields, .. } => {
            for (_, field) in fields {
                visitor.visit_pattern(field);
            }
        },
        PattKind::Or(patterns) => {
            for pattern in patterns {
                visitor.visit_pattern(pattern);
            }
        },
        PattKind::Wildcard
        | PattKind::Binding(_)
        | PattKind::Literal(_)
        | PattKind::Range { .. } => {},
    }
}

pub fn fold_function<'hir, F: Folder<'hir>>(
    folder: &mut F,
    function: &Function<'hir>,
) -> Function<'hir> {
    let params = function
        .params
        .iter()
        .map(|parameter| Parameter { typ: folder.fold_type(parameter.typ), ..*parameter })
        .collect();
    let locals = function
        .locals
        .iter()
        .map(|local| Local { typ: folder.fold_type(local.typ), ..*local })
        .collect();
    let kind = match function.kind {
        FunctionKind::Method(method) => {
            FunctionKind::Method(Method { receiver: folder.fold_type(method.receiver), ..method })
        },
        other => other,
    };
    let owner = function.owner.map_type(|on| folder.fold_type(on));

    Function {
        id: function.id,
        name: function.name,
        decl_span: function.decl_span,
        name_span: function.name_span,
        kind,
        owner,
        params,
        locals,
        return_type: folder.fold_type(function.return_type),
        is_const: function.is_const,
        is_pub: function.is_pub,
        inline: function.inline,
        is_unsafe: function.is_unsafe,
        typeck: fold_typeck(folder, &function.typeck),
        body: folder.fold_block(function.body),
        generics: function.generics.clone(),
    }
}

pub fn fold_constant<'hir, F: Folder<'hir>>(
    folder: &mut F,
    constant: &Constant<'hir>,
) -> Constant<'hir> {
    Constant {
        name: constant.name,
        typ: folder.fold_type(constant.typ),
        owner: constant.owner.map_type(|on| folder.fold_type(on)),
        value: folder.fold_expression(constant.value),
        typeck: fold_typeck(folder, &constant.typeck),
        is_pub: constant.is_pub,
        decl_span: constant.decl_span,
        name_span: constant.name_span,
    }
}

pub fn fold_block<'hir, F: Folder<'hir>>(folder: &mut F, block: Block<'hir>) -> Block<'hir> {
    let statements = block
        .statements
        .iter()
        .map(|statement| folder.fold_statement(*statement))
        .collect::<Vec<_>>();
    Block {
        statements: folder.arena().alloc_slice_copy(&statements),
        span: block.span,
    }
}

pub fn fold_statement<'hir, F: Folder<'hir>>(folder: &mut F, statement: Stmt<'hir>) -> Stmt<'hir> {
    match statement {
        Stmt::LetInit { id, init } => Stmt::LetInit { id, init: folder.fold_expression(init) },
        Stmt::Expr(expression) => Stmt::Expr(folder.fold_expression(expression)),
        Stmt::Return(value) => Stmt::Return(value.map(|value| folder.fold_expression(value))),
        Stmt::If { condition, then_block, else_block } => Stmt::If {
            condition: folder.fold_expression(condition),
            then_block: folder.fold_block(then_block),
            else_block: else_block.map(|block| folder.fold_block(block)),
        },
        Stmt::Loop { kind, body } => {
            let kind = match kind {
                LoopKind::Range { binding, start, end, inclusive } => LoopKind::Range {
                    binding,
                    start: folder.fold_expression(start),
                    end: folder.fold_expression(end),
                    inclusive,
                },
                LoopKind::Iterable { binding, iterable } => {
                    LoopKind::Iterable { binding, iterable: folder.fold_expression(iterable) }
                },
                LoopKind::Infinite => LoopKind::Infinite,
            };
            Stmt::Loop { kind, body: folder.fold_block(body) }
        },
        Stmt::Block(block) => Stmt::Block(folder.fold_block(block)),
        other => other,
    }
}

pub fn fold_expression<'hir, F: Folder<'hir>>(
    folder: &mut F,
    expression: &'hir Expression<'hir>,
) -> &'hir Expression<'hir> {
    use ExprKind::*;

    let kind = match expression.kind {
        Unary { operator, expr } => Unary { operator, expr: folder.fold_expression(expr) },
        Binary { operator, left, right } => Binary {
            operator,
            left: folder.fold_expression(left),
            right: folder.fold_expression(right),
        },
        Field { base, field } => Field { base: folder.fold_expression(base), field },
        Assign { target, value } => Assign {
            target: folder.fold_expression(target),
            value: folder.fold_expression(value),
        },
        CompoundAssign { target, operator, value } => CompoundAssign {
            target: folder.fold_expression(target),
            operator,
            value: folder.fold_expression(value),
        },
        Struct { id, fields } => {
            let fields = fields
                .iter()
                .map(|(name, value)| (*name, folder.fold_expression(value)))
                .collect::<Vec<_>>();
            Struct { id, fields: folder.arena().alloc_slice_copy(&fields) }
        },
        Array { elements } => {
            let elements = elements
                .iter()
                .map(|element| folder.fold_expression(element))
                .collect::<Vec<_>>();
            Array { elements: folder.arena().alloc_slice_copy(&elements) }
        },
        ArrayRepeat { value, count } => ArrayRepeat { value: folder.fold_expression(value), count },
        Index { base, index } => Index {
            base: folder.fold_expression(base),
            index: folder.fold_expression(index),
        },
        Call { callee, args } => Call {
            callee: folder.fold_expression(callee),
            args: fold_expressions(folder, args),
        },
        MethodCall { name, receiver, args } => MethodCall {
            name,
            receiver: folder.fold_expression(receiver),
            args: fold_expressions(folder, args),
        },
        TypeIntrinsic { kind, typ } => TypeIntrinsic { kind, typ: folder.fold_type(typ) },
        Cast { from, to } => Cast { from: folder.fold_expression(from), to: folder.fold_type(to) },
        Match { scrutinee, arms } => {
            use ArmBody::*;
            let arms: Vec<_> = arms
                .iter()
                .map(|arm| Arm {
                    pattern: folder.fold_pattern(arm.pattern),
                    guard: arm.guard.map(|guard| folder.fold_expression(guard)),
                    body: match arm.body {
                        Expr(body) => Expr(folder.fold_expression(body)),
                        Return(value) => Return(value.map(|value| folder.fold_expression(value))),
                        Break => Break,
                        Continue => Continue,
                    },
                    span: arm.span,
                })
                .collect();
            Match {
                scrutinee: folder.fold_expression(scrutinee),
                arms: folder.arena().alloc_slice_copy(&arms),
            }
        },
        other => other,
    };

    folder
        .arena()
        .alloc(Expression { id: expression.id, kind, span: expression.span })
}

pub fn fold_pattern<'hir, F: Folder<'hir>>(
    folder: &mut F,
    pattern: &'hir Pattern<'hir>,
) -> &'hir Pattern<'hir> {
    let kind = match pattern.kind {
        PattKind::Bind { local, sub } => PattKind::Bind { local, sub: folder.fold_pattern(sub) },
        PattKind::Variant { id, variant_idx, sub } => PattKind::Variant {
            id,
            variant_idx,
            sub: sub.map(|sub| folder.fold_pattern(sub)),
        },
        PattKind::Struct { id, fields } => {
            let fields = fields
                .iter()
                .map(|(name, pattern)| (*name, folder.fold_pattern(pattern)))
                .collect::<Vec<_>>();
            PattKind::Struct { id, fields: folder.arena().alloc_slice_copy(&fields) }
        },
        PattKind::Or(patterns) => {
            let patterns =
                patterns.iter().map(|pattern| *folder.fold_pattern(pattern)).collect::<Vec<_>>();
            PattKind::Or(folder.arena().alloc_slice_copy(&patterns))
        },
        other => other,
    };
    folder.arena().alloc(Pattern { kind, span: pattern.span })
}

fn fold_typeck<'hir, F: Folder<'hir>>(
    folder: &mut F,
    typeck: &TypeckResults<'hir>,
) -> TypeckResults<'hir> {
    TypeckResults {
        node_types: typeck.node_types.iter().map(|typ| folder.fold_type(*typ)).collect(),
        type_dependent_defs: typeck
            .type_dependent_defs
            .iter()
            .map(|(expression, resolution)| (*expression, folder.fold_res(*resolution)))
            .collect(),
        node_args: typeck
            .node_args
            .iter()
            .map(|(expression, arguments)| {
                (*expression, arguments.iter().map(|typ| folder.fold_type(*typ)).collect())
            })
            .collect(),
        const_uses: typeck.const_uses.clone(),
    }
}

fn fold_expressions<'hir, F: Folder<'hir>>(
    folder: &mut F,
    expressions: &[&'hir Expression<'hir>],
) -> &'hir [&'hir Expression<'hir>] {
    let expressions = expressions
        .iter()
        .map(|expression| folder.fold_expression(expression))
        .collect::<Vec<_>>();
    folder.arena().alloc_slice_copy(&expressions)
}
