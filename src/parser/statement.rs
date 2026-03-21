use crate::lexer::token::Span;

#[derive(Debug, PartialEq)]
pub enum Statement<'i> {
    Let(Let<'i>),
    Return(Return<'i>),
    If(If<'i>),
    While(While<'i>),
    Expr(Expression<'i>, Span),
    Block(Block<'i>),
}

#[derive(Debug, PartialEq)]
pub struct Let<'i> {
    mutable: bool,
    name: &'i str,
    typ: Option<Type>,
    value: Option<Expression<'i>>,
    span: Span,
}

#[derive(Debug, PartialEq)]
pub struct Return<'i> {
    value: Option<Expression<'i>>,
    span: Span,
}

#[derive(Debug, PartialEq)]
pub struct If<'i> {
    condition: Expression<'i>,
    then_branch: Block<'i>,
    else_branch: Option<Box<Else<'i>>>,
    span: Span,
}

#[derive(Debug, PartialEq)]
pub struct While<'i> {
    condition: Expression<'i>,
    body: Expression<'i>,
    span: Span,
}

#[derive(Debug, PartialEq)]
pub struct Block<'i> {
    statements: Vec<Statement<'i>>,
    span: Span,
}

#[derive(Debug, PartialEq)]
pub enum Else<'i> {
    If(If<'i>),
    Block(Block<'i>),
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
