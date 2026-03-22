use crate::lexer::token::{Position, Punct, Span, TokenKind};
use crate::parser::error::{ParseErrorKind, ParserError};
use crate::parser::{Parsable, Parser};
use std::hint::unreachable_unchecked;

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
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
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

    fn parse_expr(parser: &mut Parser<'i>, precedence: u8) -> Result<Self, ParserError> {
        todo!()
    }

    fn parse_prefix(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
        let token = parser.expect_next("expression")?;

        todo!()
    }

    #[inline(always)]
    const fn infix_precedence(kind: TokenKind) -> u8 {
        todo!()
    }

    fn parse_infix(
        parser: &mut Parser<'i>,
        left: Expression<'i>,
        precedence: u8,
    ) -> Result<Self, ParserError> {
        todo!()
    }
}
