//! String literal tokenizer (double-quoted).

use crate::lexer::cursor::Cursor;
use crate::lexer::error::{LexError, LexErrorKind};
use crate::lexer::token::{Position, Span, Token, TokenKind, Tokenize};

/// Tokenizer for double-quoted string literals.
///
/// Validates escape sequences but does **not** allocate: the returned
/// [`TokenKind::String`] is a slice of the source between the quotes.
/// If the string contains no escapes, this is zero-cost.
pub struct StringLiteral;

impl<'src> Tokenize<'src> for StringLiteral {
    fn lex(self, cursor: &mut Cursor<'src>, start: Position) -> Result<Token<'src>, LexError> {
        // consume the opening `"`
        cursor.advance();

        let content_start = cursor.position().offset();
        let mut has_invalid_escape = false;
        let mut invalid_escape_char = ' ';
        let mut invalid_escape_pos = cursor.position();

        loop {
            match cursor.peek() {
                None | Some('\n') => {
                    // unterminated string.
                    let span = Span::new(start, cursor.position());
                    return Err(LexError::new(LexErrorKind::UnterminatedString, span)
                        .with_help("add a closing `\"` at the end of the string"));
                }

                Some('"') => {
                    let content_end = cursor.position().offset();
                    cursor.advance(); // consume closing `"
                    let span = Span::new(start, cursor.position());
                    let content = &cursor.source()[content_start..content_end];
                    return match has_invalid_escape {
                        true => Err(LexError::new(
                            LexErrorKind::InvalidEscape(invalid_escape_char),
                            Span::new(
                                invalid_escape_pos,
                                Position::new(
                                    invalid_escape_pos.offset + 2,
                                    invalid_escape_pos.line,
                                    invalid_escape_pos.column + 2,
                                ),
                            ),
                        )
                        .with_help("valid escapes are: \\\\, \\\", \\n, \\t, \\r, \\0")),

                        _ => Ok(Token::new(TokenKind::String(content), span)),
                    };
                }

                Some('\\') => {
                    let esc_pos = cursor.position();
                    cursor.advance(); // consume `\`
                    match cursor.peek() {
                        Some('\\' | '"' | 'n' | 't' | 'r' | '0') => {
                            cursor.advance();
                        }

                        Some(c) => {
                            if !has_invalid_escape {
                                has_invalid_escape = true;
                                invalid_escape_char = c;
                                invalid_escape_pos = esc_pos;
                            }

                            cursor.advance();
                        }

                        None => {
                            let span = Span::new(start, cursor.position());
                            return Err(LexError::new(LexErrorKind::UnterminatedString, span)
                                .with_help("add a closing `\"` at the end of the string"));
                        }
                    }
                }

                Some(_) => {
                    cursor.advance();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(src: &str) -> Result<Token<'_>, LexError> {
        let mut cursor = Cursor::new(src);
        let start = cursor.position();
        StringLiteral.lex(&mut cursor, start)
    }

    #[test]
    fn simple_string() {
        let t = tok(r#""hello""#).unwrap();
        assert_eq!(t.kind, TokenKind::String("hello"));
    }

    #[test]
    fn empty_string() {
        let t = tok(r#""""#).unwrap();
        assert_eq!(t.kind, TokenKind::String(""));
    }

    #[test]
    fn escape_sequences() {
        let t = tok(r#""a\nb""#).unwrap();
        assert_eq!(t.kind, TokenKind::String("a\\nb"));
    }

    #[test]
    fn unterminated_string() {
        let err = tok(r#""hello"#).unwrap_err();
        assert_eq!(err.kind, LexErrorKind::UnterminatedString);
    }

    #[test]
    fn invalid_escape() {
        let err = tok(r#""a\qb""#).unwrap_err();
        assert_eq!(err.kind, LexErrorKind::InvalidEscape('q'));
    }
}
