//! Zero-copy cursor over source code.
//!
//! Tracks byte offset and human-readable line:column as it advances
//! through the source character by character.

use crate::lexer::token::Position;
use std::iter::Peekable;
use std::str::Chars;

/// A cursor that walks through source code one character at a time,
/// maintaining the current [`Position`].
#[derive(Debug)]
pub struct Cursor<'src> {
    /// The full source text.
    source: &'src str,
    /// Peekable char iterator.
    chars: Peekable<Chars<'src>>,
    /// Current position (offset + line + column).
    position: Position,
}

impl<'src> Cursor<'src> {
    #[inline]
    pub fn new(source: &'src str) -> Self {
        Self {
            source,
            chars: source.chars().peekable(),
            position: Position::new(0, 1, 1),
        }
    }

    #[inline]
    pub const fn position(&self) -> Position {
        self.position
    }

    #[inline]
    pub fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    #[inline]
    pub fn peek_until(&self, n: usize) -> Option<char> {
        self.chars.clone().nth(n - 1)
    }

    #[inline]
    pub const fn source(&self) -> &'src str {
        self.source
    }

    /// Consumes and returns the next character, updating position.
    pub fn advance(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        let len = c.len_utf8();

        match c == '\n' {
            true => {
                self.position.line += 1;
                self.position.column = 1;
            }
            _ => self.position.column += 1,
        }

        self.position.offset += len as u32;

        Some(c)
    }

    /// Consumes the next character only if it equals `expected`.
    #[inline]
    pub fn consume_optional(&mut self, expected: char) -> bool {
        match self.peek() == Some(expected) {
            true => {
                self.advance();
                true
            }
            _ => false,
        }
    }

    /// Advances while `predicate` returns `true` for the peeked character.
    pub fn consume_while(&mut self, mut predicate: impl FnMut(char) -> bool) {
        while let Some(&ch) = self.chars.peek() {
            match predicate(ch) {
                true => self.advance(),
                _ => break,
            };
        }
    }

    #[inline]
    pub fn slice_from(&self, from: usize) -> &'src str {
        &self.source[from..self.position.offset()]
    }
}
