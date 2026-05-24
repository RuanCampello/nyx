//! Error types for the Nyx lexer.
//! Produces human-readable diagnostics with source spans and help hints.

use crate::lexer::token::{Position, Span};
use nyx_macros::Diagnostic;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub(crate) kind: LexErrorKind,
    pub(crate) span: Span,
    pub(crate) help: Option<String>,
}

/// The category of a lexer error.
#[derive(Debug, Clone, PartialEq, Eq, Diagnostic)]
pub(crate) enum LexErrorKind {
    /// An unexpected character was encountered.
    #[diagnostic(
        message = "unexpected character {0~}",
        primary = "not valid here"
    )]
    UnexpectedChar(char),
    /// A string literal was opened but never closed.
    #[diagnostic(
        message = "unterminated string literal",
        primary = "opened here, but never closed",
        help = "add a closing {`\"`} at the end of the string"
    )]
    UnterminatedString,
    /// A char literal was opened but never closed.
    #[diagnostic(
        message = "unterminated character literal",
        primary = "opened here, but never closed",
        help = "add a closing {`'`} at the end of the character literal"
    )]
    UnterminatedChar,
    /// A char literal was empty (i.e. '').
    #[diagnostic(
        message = "empty character literal",
        primary = "character literals cannot be empty",
        help = "provide a character inside the single quotes"
    )]
    EmptyChar,
    /// A char literal contains more than one character.
    #[diagnostic(
        message = "character literal is too long",
        primary = "character literals must contain exactly one character",
        help = "use double quotes for string literals instead"
    )]
    OverlongChar,
    /// A block comment `/* ... */` was opened but never closed.
    #[diagnostic(
        message = "unterminated block comment",
        primary = "block comment opened here, but never closed",
        help = "add a closing {`*/`}"
    )]
    UnterminatedComment,
    /// An invalid escape sequence in a string (e.g. `\q`).
    #[diagnostic(
        message = "invalid escape sequence {`\\{0}`}",
        primary = "{`\\{0}`} is not a recognised escape",
        help = "valid escapes: {`\\\\`}  {`\\\"`}  {`\\n`}  {`\\t`}  {`\\r`}  {`\\0`}"
    )]
    InvalidEscape(char),
    /// A numeric literal could not be parsed.
    #[diagnostic(
        message = "invalid number literal: {0}",
        primary = "could not parse this as a number"
    )]
    InvalidNumber(String),
}

impl LexError {
    #[inline]
    pub(in crate::lexer) fn new(kind: LexErrorKind, span: Span) -> Self {
        Self { kind, span, help: None }
    }

    #[inline]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn unexpected_char(ch: char, pos: Position) -> Self {
        let end = Position::new(pos.offset + ch.len_utf8() as u32, pos.line, pos.column + 1);
        Self::new(LexErrorKind::UnexpectedChar(ch), Span::new(pos, end))
    }
}
