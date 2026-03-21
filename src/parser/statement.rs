use crate::lexer::token::Span;

#[derive(Debug)]
pub enum Statement<'i> {
    Let(LetStatement<'i>),
}

#[derive(Debug)]
pub struct LetStatement<'i> {
    mutable: bool,
    name: &'i str,
    typ: Option<Type>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Type {
    I32 { span: Span },
    I64 { span: Span },
    F32 { span: Span },
    F64 { span: Span },
    Bool { span: Span },
    String { span: Span },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expression<'src> {
    Integer(i64, Span),
    Float(f64, Span),
    String(&'src str, Span),
    Bool(bool, Span),
    Identifier(&'src str, Span),
    Unary {
        operator: UnaryOperator,
        expr: Box<Expression<'src>>,
        span: Span,
    },
    Binary {
        left: Box<Expression<'src>>,
        operator: BinaryOperator,
        right: Box<Expression<'src>>,
        span: Span,
    },
    Assignment {
        target: &'src str,
        value: Box<Expression<'src>>,
        span: Span,
    },
    Call {
        callee: Box<Expression<'src>>,
        args: Vec<Expression<'src>>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOperator {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinaryOperator {
    Add,
    Sub,
    Div,
    Mul,
    Eq,
    Ne,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
}

impl Type {
    #[inline(always)]
    pub const fn span(&self) -> Span {
        match self {
            Self::I32 { span }
            | Self::I64 { span }
            | Self::F32 { span }
            | Self::F64 { span }
            | Self::Bool { span }
            | Self::String { span } => *span,
        }
    }
}
