use crate::parser::expression::Expression;
use crate::parser::statement::{
    Block, Const, Function, If, Impl, Interface, Let, Match, Return, Statement, While,
};

pub trait Visitor<'i>: Sized {
    fn visit_statement(&mut self, stmt: &Statement<'i>) {
        match stmt {
            Statement::Let(let_stmt) => self.visit_let(let_stmt),
            Statement::Const(const_stmt) => self.visit_const(const_stmt),
            Statement::Return(ret_stmt) => self.visit_return(ret_stmt),
            Statement::If(if_stmt) => self.visit_if(if_stmt),
            Statement::While(while_stmt) => self.visit_while(while_stmt),
            Statement::Fn(func) => self.visit_function(func),
            Statement::Impl(impl_block) => self.visit_impl(impl_block),
            Statement::Interface(interface) => self.visit_interface(interface),
            Statement::Expr(expr, _) => self.visit_expression(expr),
            Statement::Block(block) => self.visit_block(block),
            Statement::Match(match_stmt) => self.visit_match(match_stmt),
            Statement::Struct(_) | Statement::Enum(_) | Statement::Use(_) => {},
        }
    }

    fn visit_match(&mut self, match_stmt: &Match<'i>) {
        self.visit_expression(&match_stmt.scrutinee);
        for arm in &match_stmt.arms {
            self.visit_expression(&arm.body);
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

    fn visit_while(&mut self, while_stmt: &While<'i>) {
        self.visit_expression(&while_stmt.condition);
        self.visit_block(&while_stmt.body);
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

pub fn walk_expression<'i, V: Visitor<'i> + ?Sized>(visitor: &mut V, expr: &Expression<'i>) {
    match expr {
        Expression::Unary { expr, .. } | Expression::Cast { expr, .. } => {
            visitor.visit_expression(expr);
        },
        Expression::Binary { left, right, .. } => {
            visitor.visit_expression(left);
            visitor.visit_expression(right);
        },
        Expression::Assignment { target, value, .. } => {
            visitor.visit_expression(target);
            visitor.visit_expression(value);
        },
        Expression::Field { expr, .. } => {
            visitor.visit_expression(expr);
        },
        Expression::Struct { fields, .. } => {
            for field in fields {
                visitor.visit_expression(&field.value);
            }
        },
        Expression::Call { callee, args, .. } => {
            visitor.visit_expression(callee);
            for arg in args {
                visitor.visit_expression(arg);
            }
        },
        Expression::QualifiedCall { args, .. } => {
            for arg in args {
                visitor.visit_expression(arg);
            }
        },
        Expression::TypeIntrinsic { .. }
        | Expression::Integer(_, _)
        | Expression::Float(_, _)
        | Expression::String(_, _)
        | Expression::Char(_, _)
        | Expression::Bool(_, _)
        | Expression::Identifier(_, _)
        | Expression::QualifiedName { .. } => {},
    }
}
