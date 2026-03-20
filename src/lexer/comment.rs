//! Comment tokenizers (line and block).

use crate::lexer::cursor::Cursor;
use crate::lexer::error::{LexError, LexErrorKind};
use crate::lexer::token::{Position, Span};

/// Tokenizer for single-line comments (`// ...`).
pub struct LineComment;

impl LineComment {
    pub fn skip(self, cursor: &mut Cursor<'_>) {
        cursor.consume_while(|ch| ch != '\n');
    }
}

/// Tokenizer for block comments (`/* ... */`) with nesting support.
pub struct BlockComment {
    pub(in crate::lexer) open_offset: usize,
}

impl BlockComment {
    pub fn skip(self, cursor: &mut Cursor<'_>) -> Result<(), LexError> {
        let start_pos = cursor.position();
        let mut depth = 1;

        while depth > 0 {
            match cursor.advance() {
                None => {
                    let span = Span::new(
                        Position::new(
                            self.open_offset,
                            start_pos.line,
                            start_pos.column.saturating_sub(2),
                        ),
                        cursor.position(),
                    );
                    return Err(LexError::new(LexErrorKind::UnterminatedComment, span)
                        .with_help("add a closing `*/`"));
                }
                Some('/') if cursor.consume_optional('*') => depth += 1,
                Some('*') if cursor.consume_optional('/') => depth -= 1,
                _ => {}
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
        let mut cursor = Cursor::new("this is a comment\ncode");
        LineComment.skip(&mut cursor);
        assert_eq!(cursor.peek(), Some('\n'));
    }

    #[test]
    fn block_comment_simple() {
        let mut cursor = Cursor::new(" hello */rest");
        BlockComment { open_offset: 0 }.skip(&mut cursor).unwrap();
        assert_eq!(cursor.peek(), Some('r'));
    }

    #[test]
    fn block_comment_nested() {
        let mut cursor = Cursor::new(" /* inner */ */after");
        BlockComment { open_offset: 0 }.skip(&mut cursor).unwrap();
        assert_eq!(cursor.peek(), Some('a'));
    }

    #[test]
    fn unterminated_block_comment() {
        let mut cursor = Cursor::new(" hello");
        let err = BlockComment { open_offset: 0 }
            .skip(&mut cursor)
            .unwrap_err();
        assert_eq!(err.kind, LexErrorKind::UnterminatedComment);
    }
}
