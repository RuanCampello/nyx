//! Error types for the Nyx lexer.
//! Produces human-readable diagnostics with source spans and help hints.

use crate::lexer::token::{Position, Span};
use nyx_macros::Diagnostic;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub(crate) kind: LexErrorKind,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Diagnostic)]
pub(crate) enum LexErrorKind {
    #[diagnostic(message = "unexpected character {0~}", primary = "not valid here")]
    UnexpectedChar(char),
    #[diagnostic(
        message = "unterminated string literal",
        primary = "opened here, but never closed",
        help = "add a closing {`\"`} at the end of the string"
    )]
    UnterminatedString,
    #[diagnostic(
        message = "unterminated character literal",
        primary = "opened here, but never closed",
        help = "add a closing {`'`} at the end of the character literal"
    )]
    UnterminatedChar,
    #[diagnostic(
        message = "empty character literal",
        primary = "character literals cannot be empty",
        help = "provide a character inside the single quotes"
    )]
    EmptyChar,
    #[diagnostic(
        message = "character literal is too long",
        primary = "character literals must contain exactly one character",
        help = "use double quotes for string literals instead"
    )]
    OverlongChar,
    #[diagnostic(
        message = "unterminated block comment",
        primary = "block comment opened here, but never closed",
        help = "add a closing {`*/`}"
    )]
    UnterminatedComment,
    #[diagnostic(
        message = "invalid escape sequence {`\\{0}`}",
        primary = "{`\\{0}`} is not a recognised escape",
        help = "valid escapes are: {`\\\\`}, {`\\\"`}, {`\\n`}, {`\\t`}, {`\\r`}, {`\\0`}, {`\\xXX`}, {`\\u{{XXXXXX}}`}"
    )]
    InvalidEscape(char),
    #[diagnostic(
        message = "invalid number literal: {0}",
        primary = "could not parse this as a number"
    )]
    InvalidNumber(String),
}

impl LexError {
    #[inline]
    pub(in crate::lexer) fn new(kind: LexErrorKind, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn unexpected_char(ch: char, pos: Position) -> Self {
        let end = Position::new(pos.offset + ch.len_utf8() as u32, pos.line, pos.column + 1);
        Self::new(LexErrorKind::UnexpectedChar(ch), Span::new(pos, end))
    }
}
