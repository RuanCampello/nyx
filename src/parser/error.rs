use crate::lexer::{
    error::LexError,
    token::{Span, TokenKind},
};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{kind}")]
pub struct ParserError<'i> {
    pub(in crate::parser) kind: ParseErrorKind<'i>,
    pub(in crate::parser) span: Span,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseErrorKind<'i> {
    #[error(transparent)]
    Lexical(#[from] LexError),

    #[error("expected {expected}, found {found}")]
    Expected {
        expected: TokenKind<'i>,
        found: TokenKind<'i>,
    },

    #[error("expected identifier, found {found}")]
    ExpectedIdentifier { found: TokenKind<'i> },

    #[error("unexpected identifier for assigment target")]
    UnexpectedIdentifier,

    #[error("unexpected binary operator: {found}")]
    InvalidBinaryOperator { found: TokenKind<'i> },

    #[error("expected type identifier, found `{found}`")]
    ExpectedTypeIdentifier { found: String },

    #[error("unexpected end of file")]
    UnexpectedEof,
}

impl<'i> ParserError<'i> {
    pub fn new(kind: ParseErrorKind<'i>, span: Span) -> Self {
        Self { kind, span }
    }
}
