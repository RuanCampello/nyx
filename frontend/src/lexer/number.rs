//! Number literal tokenizer.

use crate::lexer::cursor::Cursor;
use crate::lexer::error::{LexError, LexErrorKind};
use crate::lexer::token::{BytePos, Span, Token, TokenKind, Tokenize};

/// Tokenizer for numeric literals.
///
/// supports:
/// - decimal integers: `42`, `1_000`
/// - hexadecimal:      `0xFF`, `0xdead_beef`
/// - binary:           `0b1100`, `0b1010_0001`
/// - floating-point:   `3.14`, `0.5`
pub struct NumberLiteral;

/// The bases a literal may be written in besides decimal
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Radix {
    Binary = 2,
    Hex = 16,
}

impl<'src> Tokenize<'src> for NumberLiteral {
    fn lex(self, cursor: &mut Cursor<'src>, start: BytePos) -> Result<Token<'src>, LexError<'src>> {
        fn consume_digits(cursor: &mut Cursor<'_>) {
            cursor.consume_while(|ch| ch.is_ascii_digit() || ch == '_');
        }

        let start_with_zero = matches!(cursor.peek(), Some('0'));
        if start_with_zero && let Some(radix) = cursor.peek_until(2).and_then(Radix::from_prefix) {
            return radix.lex(cursor, start);
        }

        // consume leading digits and underscores
        consume_digits(cursor);

        // check for fractional part
        let is_float = match cursor.peek() == Some('.')
            && cursor.peek_until(2).is_some_and(|c| c.is_ascii_digit())
        {
            true => {
                cursor.advance();
                consume_digits(cursor);
                true
            },
            _ => false,
        };

        let text = cursor.slice_from(start);
        let span = Span::new(start, cursor.position());

        let clean: String;
        let parse_str = match text.contains('_') {
            true => {
                clean = text.replace('_', "");
                &clean
            },
            _ => text,
        };

        let kind = match is_float {
            true => {
                let value: f64 = parse_str
                    .parse()
                    .map_err(|_| LexError::new(LexErrorKind::InvalidFloat(text), span))?;
                TokenKind::Float(value)
            },
            _ => {
                let value: u64 = parse_str
                    .parse()
                    .map_err(|_| LexError::new(LexErrorKind::InvalidInteger(text), span))?;
                TokenKind::Integer(value)
            },
        };

        Ok(Token::new(kind, span))
    }
}

impl Radix {
    #[inline]
    const fn from_prefix(ch: char) -> Option<Self> {
        match ch {
            'b' => Some(Self::Binary),
            'x' => Some(Self::Hex),
            _ => None,
        }
    }

    #[inline]
    const fn accepts(self, ch: char) -> bool {
        match self {
            Self::Binary => matches!(ch, '0' | '1'),
            Self::Hex => ch.is_ascii_hexdigit(),
        }
    }
}

impl<'src> Tokenize<'src> for Radix {
    fn lex(self, cursor: &mut Cursor<'src>, start: BytePos) -> Result<Token<'src>, LexError<'src>> {
        cursor.advance();
        cursor.advance();

        let digits_start = cursor.position();
        cursor.consume_while(|ch| self.accepts(ch) || ch == '_');

        let digits = cursor.slice_from(digits_start).replace('_', "");
        let trailing = cursor.peek().is_some_and(|ch| ch.is_ascii_alphanumeric());

        let text = cursor.slice_from(start);
        let span = Span::new(start, cursor.position());
        let invalid = || LexError::new(LexErrorKind::InvalidInteger(text), span);

        if digits.is_empty() || trailing {
            return Err(invalid());
        }

        let value = u64::from_str_radix(&digits, self as u32).map_err(|_| invalid())?;

        Ok(Token::new(TokenKind::Integer(value), span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(src: &str) -> Token<'_> {
        let mut cursor = Cursor::new(src, BytePos(0));
        let start = cursor.position();
        NumberLiteral.lex(&mut cursor, start).unwrap()
    }

    #[test]
    fn integers() {
        assert_eq!(tok("42").kind, TokenKind::Integer(42));
        assert_eq!(tok("0").kind, TokenKind::Integer(0));
        assert_eq!(tok("1_000").kind, TokenKind::Integer(1000));
    }

    #[test]
    fn full_u64_range() {
        // values above i64::MAX must lex
        assert_eq!(tok("14695981039346656037").kind, TokenKind::Integer(14695981039346656037));
        assert_eq!(tok("18446744073709551615").kind, TokenKind::Integer(u64::MAX));
    }

    #[test]
    fn integer_overflow_is_rejected() {
        // one past u64::MAX
        let mut cursor = Cursor::new("18446744073709551616", BytePos(0));
        let start = cursor.position();
        assert!(NumberLiteral.lex(&mut cursor, start).is_err());
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn floats() {
        assert_eq!(tok("3.14").kind, TokenKind::Float(3.14));
        assert_eq!(tok("0.5").kind, TokenKind::Float(0.5));
    }

    #[test]
    fn integer_followed_by_dot_no_digit() {
        // `42.` followed by non-digit should parse as integer 42.
        let mut cursor = Cursor::new("42.x", BytePos(0));
        let start = cursor.position();
        let token = NumberLiteral.lex(&mut cursor, start).unwrap();

        assert_eq!(token.kind, TokenKind::Integer(42));
        assert_eq!(cursor.peek(), Some('.'));
    }

    #[test]
    fn hexadecimal_and_binary_literals() {
        assert_eq!(tok("0xFF").kind, TokenKind::Integer(255));
        assert_eq!(tok("0xff").kind, TokenKind::Integer(255));
        assert_eq!(tok("0b1100").kind, TokenKind::Integer(12));
        assert_eq!(tok("0xdead_beef").kind, TokenKind::Integer(0xdead_beef));
        assert_eq!(tok("0b1010_0001").kind, TokenKind::Integer(0b1010_0001));
    }

    #[test]
    fn a_leading_zero_is_still_decimal() {
        assert_eq!(tok("0").kind, TokenKind::Integer(0));
        assert_eq!(tok("07").kind, TokenKind::Integer(7));
        assert_eq!(tok("0.5").kind, TokenKind::Float(0.5));
    }

    #[test]
    fn a_digit_the_base_rejects_is_an_error() {
        let mut cursor = Cursor::new("0b12", BytePos(0));
        let start = cursor.position();
        assert!(NumberLiteral.lex(&mut cursor, start).is_err());

        let mut cursor = Cursor::new("0x", BytePos(0));
        let start = cursor.position();
        assert!(NumberLiteral.lex(&mut cursor, start).is_err());

        let mut cursor = Cursor::new("0xfg", BytePos(0));
        let start = cursor.position();
        assert!(NumberLiteral.lex(&mut cursor, start).is_err());
    }
}
