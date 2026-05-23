//! Error types for the Nyx lexer.
//! Produces human-readable diagnostics with source spans and help hints.

use crate::lexer::token::{Position, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub(crate) kind: LexErrorKind,
    pub(crate) span: Span,
    pub(crate) help: Option<String>,
}

/// The category of a lexer error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LexErrorKind {
    /// An unexpected character was encountered.
    UnexpectedChar(char),
    /// A string literal was opened but never closed.
    UnterminatedString,
    /// A char literal was opened but never closed.
    UnterminatedChar,
    /// A char literal was empty (i.e. '').
    EmptyChar,
    /// A char literal contains more than one character.
    OverlongChar,
    /// A block comment `/* ... */` was opened but never closed.
    UnterminatedComment,
    /// An invalid escape sequence in a string (e.g. `\q`).
    InvalidEscape(char),
    /// A numeric literal could not be parsed.
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
