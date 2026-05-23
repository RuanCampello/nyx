//! Character literal tokenizer (single-quoted)

use crate::lexer::cursor::Cursor;
use crate::lexer::error::{LexError, LexErrorKind};
use crate::lexer::token::{Position, Span, Token, TokenKind, Tokenize};

pub struct CharLiteral;

impl<'src> Tokenize<'src> for CharLiteral {
    fn lex(self, cursor: &mut Cursor<'src>, start: Position) -> Result<Token<'src>, LexError> {
        // consume the opening `'`
        cursor.advance();

        let content_char = match cursor.peek() {
            None | Some('\n') => {
                let span = Span::new(start, cursor.position());
                return Err(LexError::new(LexErrorKind::UnterminatedChar, span)
                    .with_help("add a closing `'` at the end of the character literal"));
            },

            Some('\'') => {
                cursor.advance(); // consume closing `'`
                let span = Span::new(start, cursor.position());
                return Err(LexError::new(LexErrorKind::EmptyChar, span)
                    .with_help("character literals cannot be empty"));
            },

            Some('\\') => {
                let esc_pos = cursor.position();
                cursor.advance(); // consume `\`
                let escaped = match cursor.peek() {
                    Some('n') => {
                        cursor.advance();
                        '\n'
                    },
                    Some('r') => {
                        cursor.advance();
                        '\r'
                    },
                    Some('t') => {
                        cursor.advance();
                        '\t'
                    },
                    Some('0') => {
                        cursor.advance();
                        '\0'
                    },
                    Some('\\') => {
                        cursor.advance();
                        '\\'
                    },
                    Some('\'') => {
                        cursor.advance();
                        '\''
                    },
                    Some('"') => {
                        cursor.advance();
                        '"'
                    },
                    Some('x') => {
                        cursor.advance(); // consume 'x'
                        let mut hex = String::with_capacity(2);

                        for _ in 0..2 {
                            match cursor.peek() {
                                Some(c) if c.is_ascii_hexdigit() => {
                                    hex.push(c);
                                    cursor.advance();
                                },
                                _ => {
                                    let span = Span::new(esc_pos, cursor.position());
                                    return Err(LexError::new(
                                        LexErrorKind::InvalidEscape('x'),
                                        span,
                                    )
                                    .with_help("invalid hex escape: \\x must be followed by two hex digits"));
                                },
                            }
                        }

                        let val = u32::from_str_radix(&hex, 16).unwrap();
                        match char::from_u32(val) {
                            Some(ch) => ch,
                            None => {
                                let span = Span::new(esc_pos, cursor.position());
                                return Err(LexError::new(LexErrorKind::InvalidEscape('x'), span)
                                    .with_help("invalid hex escape scalar value"));
                            },
                        }
                    },
                    Some('u') => {
                        cursor.advance(); // consume 'u'
                        if cursor.peek() == Some('{') {
                            cursor.advance(); // consume '{'
                            let mut hex = String::new();
                            while let Some(ch) = cursor.peek() {
                                if ch == '}' {
                                    break;
                                }
                                if ch.is_ascii_hexdigit() {
                                    hex.push(ch);
                                    cursor.advance();
                                } else {
                                    break;
                                }
                            }
                            if cursor.peek() == Some('}') && !hex.is_empty() && hex.len() <= 6 {
                                cursor.advance(); // consume '}'
                                let val = u32::from_str_radix(&hex, 16).unwrap();
                                if let Some(valid_char) = char::from_u32(val) {
                                    valid_char
                                } else {
                                    let span = Span::new(esc_pos, cursor.position());
                                    return Err(LexError::new(
                                        LexErrorKind::InvalidEscape('u'),
                                        span,
                                    )
                                    .with_help("invalid unicode scalar value"));
                                }
                            } else {
                                let span = Span::new(esc_pos, cursor.position());
                                return Err(LexError::new(LexErrorKind::InvalidEscape('u'), span)
                                    .with_help("invalid unicode escape: must be \\u{hex_digits} with 1 to 6 hex digits"));
                            }
                        } else {
                            let span = Span::new(esc_pos, cursor.position());
                            return Err(LexError::new(LexErrorKind::InvalidEscape('u'), span)
                                .with_help("invalid unicode escape: \\u must be followed by {"));
                        }
                    },
                    Some(c) => {
                        cursor.advance();
                        let span = Span::new(esc_pos, cursor.position());
                        return Err(LexError::new(LexErrorKind::InvalidEscape(c), span)
                            .with_help("valid character escapes are: \\\\, \\', \\n, \\t, \\r, \\0, \\xXX, \\u{XXXXXX}"));
                    },
                    None => {
                        let span = Span::new(start, cursor.position());
                        return Err(LexError::new(LexErrorKind::UnterminatedChar, span)
                            .with_help("add a closing `'` at the end of the character literal"));
                    },
                };
                escaped
            },

            Some(c) => {
                cursor.advance();
                c
            },
        };

        match cursor.peek() {
            Some('\'') => {
                cursor.advance(); // consume closing `'`
                let span = Span::new(start, cursor.position());
                Ok(Token::new(TokenKind::Char(content_char), span))
            },
            _ => {
                // read until we see a closing quote or newline, so we can report an overlong char literal
                let mut overlong_span_end = cursor.position();
                while let Some(ch) = cursor.peek() {
                    if ch == '\'' {
                        cursor.advance();
                        overlong_span_end = cursor.position();
                        break;
                    }
                    if ch == '\n' {
                        break;
                    }
                    cursor.advance();
                    overlong_span_end = cursor.position();
                }
                let span = Span::new(start, overlong_span_end);
                Err(LexError::new(LexErrorKind::OverlongChar, span)
                    .with_help("character literals must contain exactly one character"))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(src: &str) -> Result<Token<'_>, LexError> {
        let mut cursor = Cursor::new(src);
        let start = cursor.position();
        CharLiteral.lex(&mut cursor, start)
    }

    #[test]
    fn hex_escape() {
        let token = tok(r"'\x7f'").unwrap();
        assert_eq!(token.kind, TokenKind::Char('\x7f'));
    }

    #[test]
    fn invalid_hex_escape_does_not_consume_literal_boundary() {
        let mut cursor = Cursor::new(r"'\x' + 1");
        let start = cursor.position();

        let err = CharLiteral.lex(&mut cursor, start).unwrap_err();

        assert_eq!(err.kind, LexErrorKind::InvalidEscape('x'));
        assert_eq!(err.span.start.column, 2);
        assert_eq!(err.span.end.column, 4);
        assert_eq!(cursor.peek(), Some('\''));
    }
}
