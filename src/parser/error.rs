use crate::lexer::{error::LexError, token::Span};
use std::error::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct ParserError {
    pub(in crate::parser) kind: ParseErrorKind,
    pub(in crate::parser) span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind {
    Lexical(LexError),
    Unexpected { expected: String, found: String },
}

impl Error for ParserError {}

impl std::fmt::Display for ParserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match &self.kind {
            ParseErrorKind::Lexical(lex) => lex.to_string(),
            ParseErrorKind::Unexpected { expected, found } => {
                format!("expected {expected}, found {found}")
            }
        };

        write!(
            f,
            "error: {msg}\n --> {}:{}\n",
            self.span.start.line, self.span.start.column
        )
    }
}

impl From<LexError> for ParserError {
    fn from(value: LexError) -> Self {
        Self {
            kind: ParseErrorKind::Lexical(value.to_owned()),
            span: value.span(),
        }
    }
}
