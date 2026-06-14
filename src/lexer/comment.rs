//! Comment tokenizers: `//` lines are skipped, `///` lines become doc tokens.

use crate::lexer::cursor::Cursor;
use crate::lexer::error::LexError;
use crate::lexer::token::{BytePos, Span, Token, TokenKind, Tokenize};

/// Tokenizer for single-line comments (`// ...`).
pub struct LineComment;

/// Tokenizer for documentation comments (`/// ...`).
pub struct DocComment;

impl LineComment {
    pub fn skip(self, cursor: &mut Cursor<'_>) {
        cursor.consume_while(|ch| ch != '\n');
    }
}

impl<'src> Tokenize<'src> for DocComment {
    fn lex(self, cursor: &mut Cursor<'src>, start: BytePos) -> Result<Token<'src>, LexError<'src>> {
        for _ in 0..3 {
            cursor.advance();
        }

        let text_start = cursor.position();
        cursor.consume_while(|ch| ch != '\n');
        let text = cursor.slice_from(text_start);

        Ok(Token::new(TokenKind::DocComment(text), Span::new(start, cursor.position())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_comment() {
        let mut cursor = Cursor::new("this is a comment\ncode", BytePos(0));
        LineComment.skip(&mut cursor);
        assert_eq!(cursor.peek(), Some('\n'));
    }

    #[test]
    fn doc_comment_captures_text_after_slashes() {
        let mut cursor = Cursor::new("/// the answer\nfn", BytePos(0));
        let token = DocComment.lex(&mut cursor, BytePos(0)).unwrap();

        assert_eq!(token.kind, TokenKind::DocComment(" the answer"));
        assert_eq!(cursor.peek(), Some('\n'));
    }
}
