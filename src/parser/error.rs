use crate::lexer::{
    HasSpan,
    error::LexError,
    token::{Span, TokenKind},
};

#[derive(Debug, Clone, PartialEq)]
pub struct ParserError<'i> {
    pub(crate) kind: ParseErrorKind<'i>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind<'i> {
    Lexical(LexError),
    Expected { expected: TokenKind<'i>, found: TokenKind<'i> },
    ExpectedIdentifier { found: TokenKind<'i> },
    UnexpectedIdentifier,
    InvalidBinaryOperator { found: TokenKind<'i> },
    InvalidUnaryOperator { found: TokenKind<'i> },
    ExpectedExpression { found: TokenKind<'i> },
    ExpectedTypeIdentifier { found: String },
    UnexpectedEof,
}

impl<'i> ParserError<'i> {
    pub fn new(kind: ParseErrorKind<'i>, span: Span) -> Self {
        Self { kind, span }
    }
}

impl<'i> From<&LexError> for ParserError<'i> {
    fn from(value: &LexError) -> Self {
        Self::new(ParseErrorKind::Lexical(value.clone()), value.span())
    }
}

impl<'i> From<LexError> for ParseErrorKind<'i> {
    fn from(value: LexError) -> Self {
        Self::Lexical(value)
    }
}
