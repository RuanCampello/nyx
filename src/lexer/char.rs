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
            }

            Some('\'') => {
                cursor.advance(); // consume closing `'`
                let span = Span::new(start, cursor.position());
                return Err(LexError::new(LexErrorKind::EmptyChar, span)
                    .with_help("character literals cannot be empty"));
            }

            Some('\\') => {
                let esc_pos = cursor.position();
                cursor.advance(); // consume `\`
                let escaped = match cursor.peek() {
                    Some('n') => {
                        cursor.advance();
                        '\n'
                    }
                    Some('r') => {
                        cursor.advance();
                        '\r'
                    }
                    Some('t') => {
                        cursor.advance();
                        '\t'
                    }
                    Some('0') => {
                        cursor.advance();
                        '\0'
                    }
                    Some('\\') => {
                        cursor.advance();
                        '\\'
                    }
                    Some('\'') => {
                        cursor.advance();
                        '\''
                    }
                    Some('"') => {
                        cursor.advance();
                        '"'
                    }
                    Some('x') => {
                        cursor.advance(); // consume 'x'
                        let h1 = cursor.advance();
                        let h2 = cursor.advance();
                        match (h1, h2) {
                            (Some(c1), Some(c2))
                                if c1.is_ascii_hexdigit() && c2.is_ascii_hexdigit() =>
                            {
                                let hex = format!("{}{}", c1, c2);
                                let val = u32::from_str_radix(&hex, 16).unwrap();
                                char::from_u32(val).unwrap()
                            }
                            _ => {
                                let span = Span::new(esc_pos, cursor.position());
                                return Err(LexError::new(LexErrorKind::InvalidEscape('x'), span)
                                    .with_help("invalid hex escape: \\x must be followed by two hex digits"));
                            }
                        }
                    }
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
                    }
                    Some(c) => {
                        cursor.advance();
                        let span = Span::new(esc_pos, cursor.position());
                        return Err(LexError::new(LexErrorKind::InvalidEscape(c), span)
                            .with_help("valid character escapes are: \\\\, \\', \\n, \\t, \\r, \\0, \\xXX, \\u{XXXXXX}"));
                    }
                    None => {
                        let span = Span::new(start, cursor.position());
                        return Err(LexError::new(LexErrorKind::UnterminatedChar, span)
                            .with_help("add a closing `'` at the end of the character literal"));
                    }
                };
                escaped
            }

            Some(c) => {
                cursor.advance();
                c
            }
        };

        match cursor.peek() {
            Some('\'') => {
                cursor.advance(); // consume closing `'`
                let span = Span::new(start, cursor.position());
                Ok(Token::new(TokenKind::Char(content_char), span))
            }
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
            }
        }
    }
}
