//! Comment tokenizers (line and block).

use crate::lexer::cursor::Cursor;
use crate::lexer::error::{LexError, LexErrorKind};
use crate::lexer::token::{BytePos, Span};

/// Tokenizer for single-line comments (`// ...`).
pub struct LineComment;

/// Tokenizer for block comments (`/* ... */`) with nesting support
pub struct BlockComment {
    pub(in crate::lexer) open_offset: usize,
}

impl LineComment {
    pub fn skip(self, cursor: &mut Cursor<'_>) {
        cursor.consume_while(|ch| ch != '\n');
    }
}

impl BlockComment {
    pub fn skip<'src>(self, cursor: &mut Cursor<'src>) -> Result<(), LexError<'src>> {
        let mut depth = 1;

        while depth > 0 {
            match cursor.advance() {
                None => {
                    let span = Span::new(BytePos(self.open_offset as u32), cursor.position());
                    return Err(LexError::new(LexErrorKind::UnterminatedComment, span));
                },
                Some('/') if cursor.consume_optional('*') => depth += 1,
                Some('*') if cursor.consume_optional('/') => depth -= 1,
                _ => {},
            }
        }

        Ok(())
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
    fn block_comment_simple() {
        let mut cursor = Cursor::new(" hello */rest", BytePos(0));
        BlockComment { open_offset: 0 }.skip(&mut cursor).unwrap();
        assert_eq!(cursor.peek(), Some('r'));
    }

    #[test]
    fn block_comment_nested() {
        let mut cursor = Cursor::new(" /* inner */ */after", BytePos(0));
        BlockComment { open_offset: 0 }.skip(&mut cursor).unwrap();
        assert_eq!(cursor.peek(), Some('a'));
    }

    #[test]
    fn unterminated_block_comment() {
        let mut cursor = Cursor::new(" hello", BytePos(0));
        let err = BlockComment { open_offset: 0 }.skip(&mut cursor).unwrap_err();
        assert_eq!(err.kind, LexErrorKind::UnterminatedComment);
    }
}
