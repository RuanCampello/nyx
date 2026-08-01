//! Error types for the Nyx lexer.
//! Produces human-readable diagnostics with source spans and help hints.

use crate::lexer::token::{BytePos, Span};
use nyx_macros::Diagnostic;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexError<'src> {
    pub(crate) kind: LexErrorKind<'src>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Diagnostic)]
pub(crate) enum LexErrorKind<'src> {
    #[diagnostic(code = "E001", message = "Unexpected character {0!}", primary = "not valid here")]
    UnexpectedChar(char),
    #[diagnostic(
        code = "E002",
        message = "Unterminated string literal",
        primary = "opened here, but never closed",
        help = "Add a closing {`\"`} at the end of the string"
    )]
    UnterminatedString,
    #[diagnostic(
        code = "E003",
        message = "Unterminated character literal",
        primary = "opened here, but never closed",
        help = "Add a closing {`'`} at the end of the character literal"
    )]
    UnterminatedChar,
    #[diagnostic(
        code = "E004",
        message = "Empty character literal",
        primary = "must contain exactly one character",
        help = "Write the character between the quotes, e.g. {`'a'`}"
    )]
    EmptyChar,
    #[diagnostic(
        code = "E005",
        message = "Character literal contains more than one character",
        primary = "too many characters",
        help = "Use double quotes for string literals: {`\"…\"`}"
    )]
    OverlongChar,
    #[diagnostic(
        code = "E006",
        message = "Invalid escape sequence {`\\{0}`}",
        primary = "not a recognised escape",
        help = "Valid escapes are {`\\\\`}, {`\\\"`}, {`\\n`}, {`\\t`}, {`\\r`}, {`\\0`}, {`\\xXX`} and {`\\u{{XXXXXX}}`}"
    )]
    InvalidEscape(char),
    #[diagnostic(
        code = "E007",
        message = "Invalid float literal {0!}",
        primary = "cannot be parsed as a float"
    )]
    InvalidFloat(&'src str),
    #[diagnostic(
        code = "E008",
        message = "Invalid integer literal {0!}",
        primary = "cannot be parsed as an integer",
        note = "Integer literals must fit in 64 bits"
    )]
    InvalidInteger(&'src str),
}

impl<'src> LexError<'src> {
    #[inline]
    pub(in crate::lexer) fn new(kind: LexErrorKind<'src>, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn unexpected_char(ch: char, pos: BytePos) -> LexError<'static> {
        LexError {
            kind: LexErrorKind::UnexpectedChar(ch),
            span: Span::new(pos, pos + ch.len_utf8() as u32),
        }
    }
}
