use crate::parser::expression::{Expression, Segment};
use crate::parser::statement::{
    self, Block, Const, Function, If, Impl, Interface, ItemKind, Let, Loop, Match, Return,
    Statement,
};

pub trait Visitor<'i>: Sized {
    fn visit_statement(&mut self, stmt: &Statement<'i>) {
        match stmt {
            Statement::Let(let_stmt) => self.visit_let(let_stmt),
            Statement::Return(ret_stmt) => self.visit_return(ret_stmt),
            Statement::Loop(loop_stmt) => self.visit_loop(loop_stmt),
            Statement::Break(_) | Statement::Continue(_) => {},
            Statement::Expr { expr, .. } => self.visit_expression(expr),
            Statement::Unsafe { block, .. } => self.visit_block(block),
            Statement::Item(item) => match &item.kind {
                ItemKind::Fn(func) => self.visit_function(func),
                ItemKind::Const(const_stmt) => self.visit_const(const_stmt),
                ItemKind::Static(static_item) => self.visit_expression(&static_item.value),
                ItemKind::Impl(impl_block) => self.visit_impl(impl_block),
                ItemKind::Interface(interface) => self.visit_interface(interface),
                ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Use(_) => {},
            },
        }
    }

    fn visit_match(&mut self, match_stmt: &Match<'i>) {
        self.visit_expression(&match_stmt.scrutinee);
        for arm in &match_stmt.arms {
            if let Some(body) = arm.body.value() {
                self.visit_expression(body);
            }
        }
    }

    fn visit_expression(&mut self, expr: &Expression<'i>) {
        walk_expression(self, expr);
    }

    fn visit_block(&mut self, block: &Block<'i>) {
        for stmt in &block.statements {
            self.visit_statement(stmt);
        }
    }

    fn visit_let(&mut self, let_stmt: &Let<'i>) {
        if let Some(val) = &let_stmt.value {
            self.visit_expression(val);
        }
    }

    fn visit_const(&mut self, const_stmt: &Const<'i>) {
        self.visit_expression(&const_stmt.value);
    }

    fn visit_return(&mut self, ret_stmt: &Return<'i>) {
        if let Some(val) = &ret_stmt.value {
            self.visit_expression(val);
        }
    }

    fn visit_if(&mut self, if_stmt: &If<'i>) {
        self.visit_expression(&if_stmt.condition);
        self.visit_block(&if_stmt.then_branch);
        if let Some(else_branch) = &if_stmt.else_branch {
            match else_branch.as_ref() {
                crate::parser::statement::Else::If(nested_if) => {
                    self.visit_if(nested_if);
                },
                crate::parser::statement::Else::Block(block) => {
                    self.visit_block(block);
                },
                crate::parser::statement::Else::Expr(expr) => {
                    self.visit_expression(expr);
                },
            }
        }
    }

    fn visit_loop(&mut self, loop_stmt: &Loop<'i>) {
        use statement::LoopHeader::*;
        match &loop_stmt.header {
            Infinite => {},
            Range { start, end, .. } => {
                self.visit_expression(start);
                self.visit_expression(end);
            },
            Iterable { iterable, .. } => self.visit_expression(iterable),
        }

        self.visit_block(&loop_stmt.body);
    }

    fn visit_function(&mut self, func: &Function<'i>) {
        self.visit_block(&func.body);
    }

    fn visit_impl(&mut self, impl_block: &Impl<'i>) {
        for method in &impl_block.methods {
            self.visit_function(method);
        }
        for constant in &impl_block.constants {
            self.visit_const(constant);
        }
    }

    fn visit_interface(&mut self, interface: &Interface<'i>) {
        for method in &interface.methods {
            if let Some(body) = &method.body {
                self.visit_block(body);
            }
        }
    }
}

pub fn walk_expression<'i, V: Visitor<'i>>(visitor: &mut V, expr: &Expression<'i>) {
    use Expression as E;
    match expr {
        E::Unary { expr, .. } | E::Cast { expr, .. } => visitor.visit_expression(expr),
        E::Binary { left, right, .. } => {
            visitor.visit_expression(left);
            visitor.visit_expression(right);
        },
        E::Assignment { target, value, .. } | E::CompoundAssignment { target, value, .. } => {
            visitor.visit_expression(target);
            visitor.visit_expression(value);
        },
        E::Field { expr, .. } => visitor.visit_expression(expr),
        E::Try { value, .. } => visitor.visit_expression(value),
        E::Interpolated { segments, .. } => {
            for segment in segments {
                if let Segment::Value(expr) = segment {
                    visitor.visit_expression(expr);
                }
            }
        },
        E::Block { block, .. } => visitor.visit_block(block),
        E::If { inner, .. } => visitor.visit_if(inner),
        E::Match { inner, .. } => visitor.visit_match(inner),
        E::Struct { fields, .. } => {
            for field in fields {
                visitor.visit_expression(&field.value);
            }
        },
        E::Call { callee, args, .. } => {
            visitor.visit_expression(callee);
            for arg in args {
                visitor.visit_expression(arg);
            }
        },
        E::QualifiedCall { args, .. } => {
            for arg in args {
                visitor.visit_expression(arg);
            }
        },
        E::Array { elements, .. } => {
            for element in elements {
                visitor.visit_expression(element);
            }
        },
        E::ArrayRepeat { value, .. } => visitor.visit_expression(value),
        E::Index { base, index, .. } => {
            visitor.visit_expression(base);
            visitor.visit_expression(index);
        },
        E::TypeIntrinsic { .. }
        | E::Integer(_, _)
        | E::Float(_, _)
        | E::String(_, _)
        | E::Char(_, _)
        | E::Bool(_, _)
        | E::Identifier(_, _)
        | E::QualifiedName { .. } => {},
    }
}
