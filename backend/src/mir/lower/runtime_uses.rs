use crate::mir::lower::{ExpressionKind, LocalId};
use frontend::{
    hir::{self, IndexVec, Statement},
    parser::expression::UnaryOperator,
};

pub(in crate::mir::lower) fn collect(function: &hir::Function<'_>) -> IndexVec<LocalId, bool> {
    let mut uses = IndexVec::from_elem(false, function.locals.len());
    visit_block(&function.body, &mut uses);
    uses
}

fn visit_block(block: &hir::Block<'_>, uses: &mut IndexVec<LocalId, bool>) {
    for statement in block.statements {
        visit_statement(statement, uses);
    }
}

fn visit_statement(statement: &Statement<'_>, uses: &mut IndexVec<LocalId, bool>) {
    use Statement::*;
    match statement {
        LetUninit { .. } | Return(None) => {},
        LetInit { init, .. } | Expr(init) | Return(Some(init)) => visit_expr(init, uses),
        Loop { kind, body } => {
            match kind {
                hir::LoopKind::Infinite => {},
                hir::LoopKind::Range { binding, start, end, .. } => {
                    if let Some(binding) = binding {
                        uses[*binding] = true;
                    }
                    visit_expr(start, uses);
                    visit_expr(end, uses);
                },
                hir::LoopKind::Iterable { binding, iterable } => {
                    uses[*binding] = true;
                    visit_expr(iterable, uses);
                },
            }
            visit_block(body, uses);
        },
        Break | Continue => {},
    }
}

fn visit_expr(expr: &hir::Expression<'_>, uses: &mut IndexVec<LocalId, bool>) {
    use ExpressionKind::*;
    match &expr.kind {
        Local(id) => uses[*id] = true,
        // Constants have their own local space and cannot refer to this body.
        Const(_) | ParamConst { .. } | Static(_) => {},
        Unary { expr, .. } | Cast { from: expr, .. } | ArrayRepeat { value: expr, .. } => {
            visit_expr(expr, uses)
        },
        Binary { left, right, .. } => {
            visit_expr(left, uses);
            visit_expr(right, uses);
        },
        Block { statements, tail } => {
            for statement in *statements {
                visit_statement(statement, uses);
            }
            if let Some(tail) = tail {
                visit_expr(tail, uses);
            }
        },
        If { condition, then_block, else_block } => {
            visit_expr(condition, uses);
            visit_expr(then_block, uses);
            if let Some(else_block) = else_block {
                visit_expr(else_block, uses);
            }
        },
        Assign { target, value } => {
            if !matches!(&target.kind, Local(_)) {
                visit_place(target, uses);
            }
            visit_expr(value, uses);
        },
        // Compound assignment reads its target before writing it.
        CompoundAssign { target, value, .. } => {
            visit_expr(target, uses);
            visit_expr(value, uses);
        },
        Struct { fields, .. } => {
            for &(_, value) in *fields {
                visit_expr(value, uses);
            }
        },
        Call { args, .. } => {
            for arg in *args {
                visit_expr(arg, uses);
            }
        },
        MethodCall { receiver, args, .. } => {
            let receiver = *receiver;
            match matches!(&receiver.kind, Local(_) | Field { .. }) {
                true => visit_place(receiver, uses),
                _ => visit_expr(receiver, uses),
            }

            for arg in *args {
                visit_expr(arg, uses);
            }
        },
        Field { .. } => visit_place(expr, uses),
        Array { elements } => {
            for element in *elements {
                visit_expr(element, uses);
            }
        },
        Index { base, index } => {
            visit_place(base, uses);
            visit_expr(index, uses);
        },
        TypeIntrinsic { .. } | Literal(_) | Path(_) => {},
        Match { scrutinee, arms } => {
            visit_expr(scrutinee, uses);
            for arm in *arms {
                if let Some(guard) = arm.guard {
                    visit_expr(guard, uses);
                }
                if let Some(body) = arm.body.value() {
                    visit_expr(body, uses);
                }
            }
        },
    }
}

fn visit_place(expr: &hir::Expression<'_>, uses: &mut IndexVec<LocalId, bool>) {
    use ExpressionKind::*;
    match &expr.kind {
        Local(local) => uses[*local] = true,
        Field { base, .. } => visit_place(base, uses),
        Index { base, index } => {
            visit_place(base, uses);
            visit_expr(index, uses);
        },
        Unary { operator: UnaryOperator::Deref, expr } => visit_expr(expr, uses),
        _ => visit_expr(expr, uses),
    }
}
