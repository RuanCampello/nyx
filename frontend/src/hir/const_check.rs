//! Post-lowering check that every call from a const context resolves to another
//! const function
//!
//! Must run *after* [super::freeze_function_ids]: before that point a
//! `Res::Function`'s id and a function's position in the `functions` list are
//! two different numbering schemes, freezing is what finally makes
//! `functions[id].id == id` hold for every entry, which this pass depends
//! on to look a callee up at all.

use crate::{
    hir::{
        Block, Constant, Expression, ExpressionKind, Function, FunctionId, LoopKind, Res,
        Statement, TypeckResults,
        collect::ItemTable,
        error::{ConstFnViolationKind, hir_error},
        ids::IndexVec,
    },
    lexer::token::Span,
};

struct Checker<'a, 'hir> {
    scope: &'a ItemTable<'hir>,
    functions: &'a IndexVec<FunctionId, Function<'hir>>,
    is_const: bool,
    typeck: &'a TypeckResults<'hir>,
}

pub(in crate::hir) fn check<'hir>(
    scope: &ItemTable<'hir>,
    functions: &IndexVec<FunctionId, Function<'hir>>,
) {
    functions.iter().for_each(|function| {
        Checker {
            scope,
            functions,
            is_const: function.is_const,
            typeck: &function.typeck,
        }
        .check_block(&function.body);
    });

    scope
        .values
        .constants
        .values()
        .for_each(|constant| check_constant(scope, functions, constant));
}

impl<'a, 'hir> Checker<'a, 'hir> {
    fn check_block(&mut self, block: &Block<'hir>) {
        for statement in block.statements {
            self.check_statement(statement);
        }
    }

    fn check_statement(&mut self, statement: &Statement<'hir>) {
        use Statement::*;

        match statement {
            LetInit { init, .. } | Expr(init) => self.check_expr(init),
            Return(Some(value)) => self.check_expr(value),
            If { condition, then_block, else_block } => {
                self.check_expr(condition);
                self.check_block(then_block);
                if let Some(else_block) = else_block {
                    self.check_block(else_block);
                }
            },
            Loop { kind, body } => {
                match kind {
                    LoopKind::Range { start, end, .. } => {
                        self.check_expr(start);
                        self.check_expr(end);
                    },
                    LoopKind::Iterable { iterable, .. } => self.check_expr(iterable),
                    LoopKind::Infinite => {},
                }
                self.check_block(body);
            },
            Block(block) => self.check_block(block),
            Return(None) | LetUninit { .. } | Break | Continue => {},
        }
    }

    fn check_expr(&mut self, expression: &Expression<'hir>) {
        use ExpressionKind::*;

        if let Call { .. } | MethodCall { .. } = expression.kind
            && let Some(Res::Function(id)) = self.typeck.type_dependent_def(expression.id)
        {
            self.check_call(id, expression.span);
        }

        match expression.kind {
            Const(constant) => check_constant(self.scope, self.functions, constant),
            Unary { expr, .. }
            | Field { base: expr, .. }
            | ArrayRepeat { value: expr, .. }
            | Cast { from: expr, .. } => self.check_expr(expr),
            Binary { left, right, .. }
            | Assign { target: left, value: right }
            | CompoundAssign { target: left, value: right, .. }
            | Index { base: left, index: right } => {
                self.check_expr(left);
                self.check_expr(right);
            },
            Struct { fields, .. } => {
                for (_, field) in fields {
                    self.check_expr(field);
                }
            },
            Array { elements } => {
                for element in elements {
                    self.check_expr(element);
                }
            },
            Call { callee, args } => {
                self.check_expr(callee);
                for argument in args {
                    self.check_expr(argument);
                }
            },
            MethodCall { receiver, args, .. } => {
                self.check_expr(receiver);
                for argument in args {
                    self.check_expr(argument);
                }
            },
            Match { scrutinee, arms } => {
                self.check_expr(scrutinee);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        self.check_expr(guard);
                    }
                    if let Some(body) = arm.body.value() {
                        self.check_expr(body);
                    }
                }
            },
            Literal(_)
            | Local(_)
            | Path(_)
            | ParamConst { .. }
            | Static(_)
            | TypeIntrinsic { .. } => {},
        }
    }

    fn check_call(&self, callee: FunctionId, span: Span) {
        if !self.is_const {
            return;
        }

        let target = self
            .functions
            .get(callee)
            .expect("Res::Function always names a lowered function once frozen");
        if !target.is_const {
            let name = self.scope.arena.alloc_str(self.scope.symbols.get(target.name));
            self.scope.soft(hir_error!(
                span,
                ConstFnViolation(ConstFnViolationKind::NonConstCall { name })
            ));
        }
    }
}

/// a constant's initialiser is always a const context, whether it's a top-level
/// constant (from [check]) or a body-local one (found while walking a
/// function's block, below), each gets its own [Checker] since each has its
/// own [TypeckResults], independent of whatever body it's declared inside
fn check_constant<'hir>(
    scope: &ItemTable<'hir>,
    functions: &IndexVec<FunctionId, Function<'hir>>,
    constant: &'hir Constant<'hir>,
) {
    Checker { scope, functions, is_const: true, typeck: &constant.typeck }
        .check_expr(constant.value);
}
