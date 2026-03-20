//! Error types for the Nyx lexer.
//! Produces human-readable diagnostics with source spans and help hints.

use crate::lexer::token::{Position, Span};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub(in crate::lexer) kind: LexErrorKind,
    pub(in crate::lexer) span: Span,
    pub(in crate::lexer) help: Option<String>,
}

/// The category of a lexer error.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::lexer) enum LexErrorKind {
    /// An unexpected character was encountered.
    UnexpectedChar(char),
    /// A string literal was opened but never closed.
    UnterminatedString,
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
        Self {
            kind,
            span,
            help: None,
        }
    }

    #[inline]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn unexpected_char(ch: char, pos: Position) -> Self {
        let end = Position::new(pos.offset + ch.len_utf8(), pos.line, pos.column + 1);
        Self::new(LexErrorKind::UnexpectedChar(ch), Span::new(pos, end))
    }
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match &self.kind {
            LexErrorKind::UnexpectedChar(ch) => format!("unexpected character `{ch}`"),
            LexErrorKind::UnterminatedString => "unterminated string literal".into(),
            LexErrorKind::UnterminatedComment => "unterminated block comment".into(),
            LexErrorKind::InvalidEscape(ch) => format!("invalid escape sequence `\\{ch}`"),
            LexErrorKind::InvalidNumber(detail) => format!("invalid number literal: {detail}"),
        };

        write!(
            f,
            "error: {msg}\n --> {}:{}\n",
            self.span.start.line, self.span.start.column,
        )?;

        if let Some(help) = &self.help {
            write!(f, " help: {help}\n")?;
        }
        Ok(())
    }
}

impl std::error::Error for LexError {}
