//! String literal tokenizer (double-quoted).

use crate::lexer::cursor::Cursor;
use crate::lexer::error::{LexError, LexErrorKind};
use crate::lexer::token::{BytePos, Span, Token, TokenKind, Tokenize};

/// Tokenizer for double-quoted string literals.
///
/// Validates escape sequences but does **not** allocate: the returned
/// [`TokenKind::String`] is a slice of the source between the quotes.
/// If the string contains no escapes, this is zero-cost.
pub struct StringLiteral;

impl<'src> Tokenize<'src> for StringLiteral {
    fn lex(
        self,
        cursor: &mut Cursor<'src>,
        start: BytePos,
    ) -> Result<Token<'src>, LexError<'src>> {
        // consume the opening `"`
        cursor.advance();

        let content_start = cursor.position();
        let mut has_invalid_escape = false;
        let mut invalid_escape_char = ' ';
        let mut invalid_escape_pos = cursor.position();

        loop {
            match cursor.peek() {
                None | Some('\n') => {
                    // unterminated string.
                    let span = Span::new(start, cursor.position());
                    return Err(LexError::new(LexErrorKind::UnterminatedString, span));
                },

                Some('"') => {
                    let content = cursor.slice_from(content_start);
                    cursor.advance(); // consume closing `"
                    let span = Span::new(start, cursor.position());
                    if has_invalid_escape {
                        return Err(LexError::new(
                            LexErrorKind::InvalidEscape(invalid_escape_char),
                            Span::new(invalid_escape_pos, invalid_escape_pos + 2),
                        ));
                    }

                    return Ok(Token::new(TokenKind::String(content), span));
                },

                Some('\\') => {
                    let esc_pos = cursor.position();
                    cursor.advance(); // consume `\`
                    match cursor.peek() {
                        Some('\\' | '"' | 'n' | 't' | 'r' | '0') => {
                            cursor.advance();
                        },

                        Some(c) => {
                            if !has_invalid_escape {
                                has_invalid_escape = true;
                                invalid_escape_char = c;
                                invalid_escape_pos = esc_pos;
                            }

                            cursor.advance();
                        },

                        None => {
                            let span = Span::new(start, cursor.position());
                            return Err(LexError::new(LexErrorKind::UnterminatedString, span));
                        },
                    }
                },

                Some(_) => {
                    cursor.advance();
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(src: &str) -> Result<Token<'_>, LexError<'_>> {
        let mut cursor = Cursor::new(src, BytePos(0));
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
