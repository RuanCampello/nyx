//! Zero-copy cursor over source code
//!
//! Tracks a single global byte offset as it advances through the source one
//! character at a time. Line and column are no longer tracked here, they are
//! derived on demand from the [`SourceMap`](crate::source_map::SourceMap)

use crate::lexer::token::BytePos;
use std::iter::Peekable;
use std::str::Chars;

/// A cursor that walks through one file's source, reporting global byte
/// positions relative to the file's `base` in the source address space
#[derive(Debug, Clone)]
pub struct Cursor<'src> {
    /// The full source text of this file
    source: &'src str,
    /// Peekable char iterator.
    chars: Peekable<Chars<'src>>,
    /// Global offset of this file's first byte
    base: BytePos,
    /// Current global byte offset
    position: BytePos,
}

impl<'src> Cursor<'src> {
    #[inline]
    pub fn new(source: &'src str, base: BytePos) -> Self {
        Self {
            source,
            chars: source.chars().peekable(),
            base,
            position: base,
        }
    }

    #[inline]
    pub const fn position(&self) -> BytePos {
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

    /// Consumes and returns the next character, updating position.
    pub fn advance(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        self.position.0 += c.len_utf8() as u32;
        Some(c)
    }

    /// Consumes the next character only if it equals `expected`.
    #[inline]
    pub fn consume_optional(&mut self, expected: char) -> bool {
        self.peek()
            .filter(|&ch| ch == expected)
            .inspect(|_| {
                self.advance();
            })
            .is_some()
    }

    /// Advances while `predicate` returns `true` for the peeked character.
    pub fn consume_while(&mut self, mut predicate: impl FnMut(char) -> bool) {
        while self.chars.peek().is_some_and(|&ch| predicate(ch)) {
            self.advance();
        }
    }

    /// Slice of source between the global offset `from` and the current
    /// position, rebased to this file's own text
    #[inline]
    pub fn slice_from(&self, from: BytePos) -> &'src str {
        let lo = (from.0 - self.base.0) as usize;
        let hi = (self.position.0 - self.base.0) as usize;

        &self.source[lo..hi]
    }
}
