//! Number literal tokenizer.

use crate::lexer::cursor::Cursor;
use crate::lexer::error::{LexError, LexErrorKind};
use crate::lexer::token::{Position, Span, Token, TokenKind, Tokenize};

/// Tokenizer for numeric literals.
///
/// supports:
/// - decimal integers: `42`, `1_000`
/// - floating-point:   `3.14`, `0.5`
pub struct NumberLiteral;

impl<'src> Tokenize<'src> for NumberLiteral {
    fn lex(self, cursor: &mut Cursor<'src>, start: Position) -> Result<Token<'src>, LexError> {
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
            }
            _ => false,
        };

        let text = cursor.slice_from(start.offset());
        let span = Span::new(start, cursor.position());

        let clean: String;
        let parse_str = match text.contains('_') {
            true => {
                clean = text.replace('_', "");
                &clean
            }
            _ => text,
        };

        let kind = match is_float {
            true => {
                let value: f64 = parse_str.parse().map_err(|_| {
                    LexError::new(
                        LexErrorKind::InvalidNumber(format!("could not parse `{text}` as a float")),
                        span,
                    )
                })?;
                TokenKind::Float(value)
            }

            false => {
                let value: i64 = parse_str.parse().map_err(|_| {
                    LexError::new(
                        LexErrorKind::InvalidNumber(format!(
                            "could not parse `{text}` as an integer"
                        )),
                        span,
                    )
                })?;
                TokenKind::Integer(value)
            }
        };

        Ok(Token::new(kind, span))
    }
}

fn consume_digits(cursor: &mut Cursor<'_>) {
    cursor.consume_while(|ch| ch.is_ascii_digit() || ch == '_');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(src: &str) -> Token<'_> {
        let mut cursor = Cursor::new(src);
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
    fn floats() {
        assert_eq!(tok("3.14").kind, TokenKind::Float(3.14));
        assert_eq!(tok("0.5").kind, TokenKind::Float(0.5));
    }

    #[test]
    fn integer_followed_by_dot_no_digit() {
        // `42.` followed by non-digit should parse as integer 42.
        let mut cursor = Cursor::new("42.x");
        let start = cursor.position();
        let token = NumberLiteral.lex(&mut cursor, start).unwrap();

        assert_eq!(token.kind, TokenKind::Integer(42));
        assert_eq!(cursor.peek(), Some('.'));
    }
}
