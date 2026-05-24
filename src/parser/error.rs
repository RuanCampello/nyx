use crate::lexer::{
    HasSpan,
    error::LexError,
    token::{Span, TokenKind},
};
use nyx_macros::Diagnostic;

#[derive(Debug, Clone, PartialEq)]
pub struct ParserError<'i> {
    pub(crate) kind: ParseErrorKind<'i>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, PartialEq, Diagnostic)]
pub enum ParseErrorKind<'i> {
    #[diagnostic(transparent)]
    Lexical(LexError),
    #[diagnostic(
        message = "expected {expected^}, found {found~}",
        primary = "expected {expected^} here",
        help = "add a {expected^} token here"
    )]
    Expected { expected: TokenKind<'i>, found: TokenKind<'i> },
    #[diagnostic(
        message = "expected identifier, found {found~}",
        primary = "an identifier was expected here"
    )]
    ExpectedIdentifier { found: TokenKind<'i> },
    #[diagnostic(
        message = "invalid assignment target",
        primary = "only identifiers and field paths can be assigned to"
    )]
    UnexpectedIdentifier,
    #[diagnostic(
        message = "unexpected token {found~} in expression",
        primary = "this is not a valid binary operator"
    )]
    InvalidBinaryOperator { found: TokenKind<'i> },
    #[diagnostic(
        message = "unexpected token {found~} in expression",
        primary = "this is not a valid prefix operator"
    )]
    InvalidUnaryOperator { found: TokenKind<'i> },
    #[diagnostic(
        message = "expected expression, found {found~}",
        primary = "an expression was expected here"
    )]
    ExpectedExpression { found: TokenKind<'i> },
    #[diagnostic(
        message = "expected type name, found {found!}",
        primary = "a type name was expected here"
    )]
    ExpectedTypeIdentifier { found: String },
    #[diagnostic(
        message = "unexpected end of file",
        primary = "the file ended prematurely"
    )]
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
