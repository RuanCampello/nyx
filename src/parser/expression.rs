use crate::lexer::token::{Span, TokenKind};
use crate::parser::error::ParserError;
use crate::parser::{Parsable, Parser};

#[derive(Debug, Clone, PartialEq)]
pub enum Expression<'i> {
    Integer(i64, Span),
    Float(f64, Span),
    String(&'i str, Span),
    Bool(bool, Span),
    Identifier(&'i str, Span),
    Unary {
        operator: UnaryOperator,
        expr: Box<Expression<'i>>,
        span: Span,
    },
    Binary {
        left: Box<Expression<'i>>,
        operator: BinaryOperator,
        right: Box<Expression<'i>>,
        span: Span,
    },
    Assignment {
        target: &'i str,
        value: Box<Expression<'i>>,
        span: Span,
    },
    Call {
        callee: Box<Expression<'i>>,
        args: Vec<Expression<'i>>,
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

impl<'i> Parsable<'i> for Expression<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        Self::parse_expr(parser, 0)
    }
}

impl<'i> Expression<'i> {
    #[inline(always)]
    pub const fn span(&self) -> Span {
        match self {
            Self::Integer(_, span)
            | Self::Float(_, span)
            | Self::String(_, span)
            | Self::Bool(_, span)
            | Self::Identifier(_, span)
            | Self::Unary { span, .. }
            | Self::Binary { span, .. }
            | Self::Assignment { span, .. }
            | Self::Call { span, .. } => *span,
        }
    }

    fn parse_expr(parser: &mut Parser<'i>, precedence: u8) -> Result<Self, ParserError<'i>> {
        todo!()
    }

    fn parse_prefix(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let token = parser.expect_next()?;

        todo!()
    }

    #[inline(always)]
    const fn infix_precedence(_kind: TokenKind<'i>) -> u8 {
        todo!()
    }

    fn parse_infix(
        parser: &mut Parser<'i>,
        left: Expression<'i>,
        precedence: u8,
    ) -> Result<Self, ParserError<'i>> {
        todo!()
    }
}
