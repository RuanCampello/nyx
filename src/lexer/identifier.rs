//! Identifier and keyword tokenizer.

use crate::lexer::cursor::Cursor;
use crate::lexer::error::LexError;
use crate::lexer::token::{Keyword, Position, Span, Token, TokenKind, Tokenize};

/// Tokenizer for identifiers, keywords, and boolean literals.
///
/// Reads `[a-zA-Z_][a-zA-Z0-9_]*` and then checks the result against
/// the keyword table and boolean literals.
pub struct Identifier;

impl<'src> Tokenize<'src> for Identifier {
    fn lex(self, cursor: &mut Cursor<'src>, start: Position) -> Result<Token<'src>, LexError> {
        cursor.advance();
        cursor.consume_while(|ch| ch.is_ascii_alphanumeric() || ch == '_');

        let text = cursor.slice_from(start.offset);
        let span = Span::new(start, cursor.position());

        let kind = match text {
            "true" => TokenKind::Bool(true),
            "false" => TokenKind::Bool(false),
            other => match Keyword::from_str(other) {
                Some(keyword) => TokenKind::Keyword(keyword),
                None => TokenKind::Identifier(other),
            },
        };

        Ok(Token::new(kind, span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(src: &str) -> Token<'_> {
        let mut cursor = Cursor::new(src);
        let start = cursor.position();
        Identifier.lex(&mut cursor, start).unwrap()
    }

    #[test]
    fn identifiers() {
        assert_eq!(tok("foo").kind, TokenKind::Identifier("foo"));
        assert_eq!(tok("_bar").kind, TokenKind::Identifier("_bar"));
        assert_eq!(tok("x123").kind, TokenKind::Identifier("x123"));
    }

    #[test]
    fn keywords() {
        assert_eq!(tok("fn").kind, TokenKind::Keyword(Keyword::Fn));
        assert_eq!(tok("let").kind, TokenKind::Keyword(Keyword::Let));
        assert_eq!(tok("return").kind, TokenKind::Keyword(Keyword::Return));
        assert_eq!(tok("struct").kind, TokenKind::Keyword(Keyword::Struct));
    }

    #[test]
    fn booleans() {
        assert_eq!(tok("true").kind, TokenKind::Bool(true));
        assert_eq!(tok("false").kind, TokenKind::Bool(false));
    }
}
