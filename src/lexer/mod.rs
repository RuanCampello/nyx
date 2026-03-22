//! Nyx lexical analyzer (Lexer).
//!
//! Splits source code into a sequence of [`Token`]s.
//! The lexer skips whitespace and comments and produces meaningful errors with source spans.
//!
//! # Usage
//! ```ignore
//! use nyx::lexer::Lexer;
//!
//! let tokens: Vec<_> = Lexer::new(source).collect::<Result<_, _>>().unwrap();
//! ```

pub mod cursor;
pub mod error;
pub mod token;

mod comment;
mod identifier;
mod number;
mod string;

use comment::{BlockComment, LineComment};
use cursor::Cursor;
use error::LexError;
use identifier::Identifier;
use number::NumberLiteral;
use string::StringLiteral;
use token::{Punct, Span, Token, TokenKind, Tokenize};

/// The Nyx lexer.
///
/// Wraps a [`Cursor`] and exposes an [`Iterator`] of `Result<Token, LexError>`.
#[derive(Debug)]
pub struct Lexer<'src> {
    cursor: Cursor<'src>,
    /// set to `true` once we've emitted [`TokenKind::Eof`].
    finished: bool,
}

impl<'src> Lexer<'src> {
    #[inline]
    pub fn new(source: &'src str) -> Self {
        Self {
            cursor: Cursor::new(source),
            finished: false,
        }
    }

    /// Produces the next token, or `None` after EOF has been emitted.
    fn next_token(&mut self) -> Result<Option<Token<'src>>, LexError> {
        if self.finished {
            return Ok(None);
        }

        self.skip_whitespace_and_comments()?;
        let start = self.cursor.position();

        let Some(c) = self.cursor.peek() else {
            self.finished = true;
            let span = Span::new(start, start);

            return Ok(Some(Token::new(TokenKind::Eof, span)));
        };

        let token = match c {
            'a'..='z' | 'A'..='Z' | '_' => Identifier.lex(&mut self.cursor, start)?,
            '0'..='9' => NumberLiteral.lex(&mut self.cursor, start)?,
            '"' => StringLiteral.lex(&mut self.cursor, start)?,

            '(' => self.single_punct(Punct::OpenParen),
            ')' => self.single_punct(Punct::CloseParen),
            '{' => self.single_punct(Punct::OpenBrace),
            '}' => self.single_punct(Punct::CloseBrace),
            '[' => self.single_punct(Punct::OpenBracket),
            ']' => self.single_punct(Punct::CloseBracket),
            ':' => self.single_punct(Punct::Colon),
            ';' => self.single_punct(Punct::Semicolon),
            ',' => self.single_punct(Punct::Comma),
            '.' => self.single_punct(Punct::Dot),
            '+' => self.single_punct(Punct::Plus),
            '*' => self.single_punct(Punct::Star),

            '-' => {
                self.cursor.advance();
                match self.cursor.consume_optional('>') {
                    true => self.token(Punct::Arrow, start),
                    _ => self.token(Punct::Minus, start),
                }
            }

            '/' => {
                // The comment cases (// and /*) are already handled in
                // skip_whitespace_and_comments, so if we get here it's
                // a plain division operator :D
                self.single_punct(Punct::Slash)
            }

            '=' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::EqEq, start),
                    _ => self.token(Punct::Eq, start),
                }
            }

            '!' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::BangEq, start),
                    false => self.token(Punct::Bang, start),
                }
            }

            '<' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::LtEq, start),
                    false => self.token(Punct::Lt, start),
                }
            }

            '>' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::GtEq, start),
                    _ => self.token(Punct::Gt, start),
                }
            }

            '&' => {
                self.cursor.advance();
                match self.cursor.consume_optional('&') {
                    true => self.token(Punct::And, start),
                    _ => {
                        return Err(LexError::unexpected_char('&', start)
                            .with_help("did you mean `&&` (logical and)?"));
                    }
                }
            }

            '|' => {
                self.cursor.advance();
                match self.cursor.consume_optional('|') {
                    true => self.token(Punct::Or, start),
                    _ => {
                        return Err(LexError::unexpected_char('|', start)
                            .with_help("did you mean `||` (logical or)?"));
                    }
                }
            }

            other => return Err(LexError::unexpected_char(other, start)),
        };

        Ok(Some(token))
    }

    fn skip_whitespace_and_comments(&mut self) -> Result<(), LexError> {
        loop {
            self.cursor.consume_while(|ch| ch.is_ascii_whitespace());

            match (self.cursor.peek(), self.cursor.peek_until(2)) {
                (Some('/'), Some('/')) => {
                    self.cursor.advance(); // consume first `/`
                    self.cursor.advance(); // consume second `/`

                    LineComment.skip(&mut self.cursor);
                }
                (Some('/'), Some('*')) => {
                    let offset = self.cursor.position().offset();
                    self.cursor.advance(); // consume `/`
                    self.cursor.advance(); // consume `*`

                    BlockComment {
                        open_offset: offset as usize,
                    }
                    .skip(&mut self.cursor)?;
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// Consumes a single character and returns a punctuation token.
    #[inline]
    fn single_punct(&mut self, punct: Punct) -> Token<'src> {
        let start = self.cursor.position();
        self.cursor.advance();
        self.token(punct, start)
    }

    /// Builds a punctuation token from `start` to the current cursor position.
    #[inline]
    fn token(&self, punct: Punct, start: token::Position) -> Token<'src> {
        Token::new(
            TokenKind::Punct(punct),
            Span::new(start, self.cursor.position()),
        )
    }
}

impl<'src> Iterator for Lexer<'src> {
    type Item = Result<Token<'src>, LexError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_token() {
            Ok(Some(token)) => Some(Ok(token)),
            Ok(None) => None,
            Err(e) => {
                self.finished = true; // stop after first error
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use token::{Keyword, Punct, TokenKind};

    fn kinds(src: &str) -> Vec<TokenKind<'_>> {
        Lexer::new(src)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| *k != TokenKind::Eof)
            .collect()
    }

    #[test]
    fn empty_source() {
        let kinds = kinds("");
        assert!(kinds.is_empty());
    }

    #[test]
    fn whitespace_only() {
        let kinds = kinds("   \n\t  \n  ");
        assert!(kinds.is_empty());
    }

    #[test]
    fn punctuation() {
        let ks = kinds("( ) { } [ ] : ; , . + - * / = == != < > <= >= -> && ||");
        let expected = vec![
            TokenKind::Punct(Punct::OpenParen),
            TokenKind::Punct(Punct::CloseParen),
            TokenKind::Punct(Punct::OpenBrace),
            TokenKind::Punct(Punct::CloseBrace),
            TokenKind::Punct(Punct::OpenBracket),
            TokenKind::Punct(Punct::CloseBracket),
            TokenKind::Punct(Punct::Colon),
            TokenKind::Punct(Punct::Semicolon),
            TokenKind::Punct(Punct::Comma),
            TokenKind::Punct(Punct::Dot),
            TokenKind::Punct(Punct::Plus),
            TokenKind::Punct(Punct::Minus),
            TokenKind::Punct(Punct::Star),
            TokenKind::Punct(Punct::Slash),
            TokenKind::Punct(Punct::Eq),
            TokenKind::Punct(Punct::EqEq),
            TokenKind::Punct(Punct::BangEq),
            TokenKind::Punct(Punct::Lt),
            TokenKind::Punct(Punct::Gt),
            TokenKind::Punct(Punct::LtEq),
            TokenKind::Punct(Punct::GtEq),
            TokenKind::Punct(Punct::Arrow),
            TokenKind::Punct(Punct::And),
            TokenKind::Punct(Punct::Or),
        ];
        assert_eq!(ks, expected);
    }

    #[test]
    fn keywords_and_identifiers() {
        let ks = kinds("fn let mut if else return while for struct foo _bar x1");
        assert_eq!(
            ks,
            vec![
                TokenKind::Keyword(Keyword::Fn),
                TokenKind::Keyword(Keyword::Let),
                TokenKind::Keyword(Keyword::Mut),
                TokenKind::Keyword(Keyword::If),
                TokenKind::Keyword(Keyword::Else),
                TokenKind::Keyword(Keyword::Return),
                TokenKind::Keyword(Keyword::While),
                TokenKind::Keyword(Keyword::For),
                TokenKind::Keyword(Keyword::Struct),
                TokenKind::Identifier("foo"),
                TokenKind::Identifier("_bar"),
                TokenKind::Identifier("x1"),
            ]
        );
    }

    #[test]
    fn boolean_literals() {
        assert_eq!(
            kinds("true false"),
            vec![TokenKind::Bool(true), TokenKind::Bool(false)]
        );
    }

    #[test]
    fn number_literals() {
        assert_eq!(
            kinds("42 3.14 0 1_000"),
            vec![
                TokenKind::Integer(42),
                TokenKind::Float(3.14),
                TokenKind::Integer(0),
                TokenKind::Integer(1000),
            ]
        );
    }

    #[test]
    fn string_literals() {
        assert_eq!(
            kinds(r#""hello" "" "a\nb""#),
            vec![
                TokenKind::String("hello"),
                TokenKind::String(""),
                TokenKind::String("a\\nb"),
            ]
        );
    }

    #[test]
    fn line_comment_skipped() {
        let ks = kinds("42 // this is a comment\n7");
        assert_eq!(ks, vec![TokenKind::Integer(42), TokenKind::Integer(7)]);
    }

    #[test]
    fn block_comment_skipped() {
        let ks = kinds("42 /* block comment */ 7");
        assert_eq!(ks, vec![TokenKind::Integer(42), TokenKind::Integer(7)]);
    }

    #[test]
    fn nested_block_comment() {
        let ks = kinds("1 /* outer /* inner */ still comment */ 2");
        assert_eq!(ks, vec![TokenKind::Integer(1), TokenKind::Integer(2)]);
    }

    #[test]
    fn add_nyx_file() {
        // fn add(a: i32, b: i32): i32 { a + b }
        let ks = kinds("fn add(a: i32, b: i32): i32 {\n  a + b\n}");
        assert_eq!(
            ks,
            vec![
                TokenKind::Keyword(Keyword::Fn),
                TokenKind::Identifier("add"),
                TokenKind::Punct(Punct::OpenParen),
                TokenKind::Identifier("a"),
                TokenKind::Punct(Punct::Colon),
                TokenKind::Identifier("i32"),
                TokenKind::Punct(Punct::Comma),
                TokenKind::Identifier("b"),
                TokenKind::Punct(Punct::Colon),
                TokenKind::Identifier("i32"),
                TokenKind::Punct(Punct::CloseParen),
                TokenKind::Punct(Punct::Colon),
                TokenKind::Identifier("i32"),
                TokenKind::Punct(Punct::OpenBrace),
                TokenKind::Identifier("a"),
                TokenKind::Punct(Punct::Plus),
                TokenKind::Identifier("b"),
                TokenKind::Punct(Punct::CloseBrace),
            ]
        );
    }

    #[test]
    fn inference_nyx_file() {
        let src = "fn main() {\n  let x = 10;\n  let y = 20;\n\n  let z = x + y;\n}";
        let ks = kinds(src);
        assert_eq!(
            ks,
            vec![
                TokenKind::Keyword(Keyword::Fn),
                TokenKind::Identifier("main"),
                TokenKind::Punct(Punct::OpenParen),
                TokenKind::Punct(Punct::CloseParen),
                TokenKind::Punct(Punct::OpenBrace),
                TokenKind::Keyword(Keyword::Let),
                TokenKind::Identifier("x"),
                TokenKind::Punct(Punct::Eq),
                TokenKind::Integer(10),
                TokenKind::Punct(Punct::Semicolon),
                TokenKind::Keyword(Keyword::Let),
                TokenKind::Identifier("y"),
                TokenKind::Punct(Punct::Eq),
                TokenKind::Integer(20),
                TokenKind::Punct(Punct::Semicolon),
                TokenKind::Keyword(Keyword::Let),
                TokenKind::Identifier("z"),
                TokenKind::Punct(Punct::Eq),
                TokenKind::Identifier("x"),
                TokenKind::Punct(Punct::Plus),
                TokenKind::Identifier("y"),
                TokenKind::Punct(Punct::Semicolon),
                TokenKind::Punct(Punct::CloseBrace),
            ]
        );
    }

    #[test]
    fn unexpected_char_error() {
        let result: Result<Vec<_>, _> = Lexer::new("42 @ 7").collect();
        let err = result.unwrap_err();
        assert_eq!(err.kind, error::LexErrorKind::UnexpectedChar('@'));
        assert_eq!(err.span.start.column, 4);
    }

    #[test]
    fn unterminated_string_error() {
        let result: Result<Vec<_>, _> = Lexer::new(r#"let x = "hello"#).collect();
        let err = result.unwrap_err();
        assert_eq!(err.kind, error::LexErrorKind::UnterminatedString);
        assert!(err.help.is_some());
    }

    #[test]
    fn unterminated_block_comment_error() {
        let result: Result<Vec<_>, _> = Lexer::new("42 /* never closed").collect();
        let err = result.unwrap_err();
        assert_eq!(err.kind, error::LexErrorKind::UnterminatedComment);
    }

    #[test]
    fn bang_without_eq() {
        let ks = kinds("!x");
        assert_eq!(
            ks,
            vec![TokenKind::Punct(Punct::Bang), TokenKind::Identifier("x")]
        );
    }

    #[test]
    fn arrow_vs_minus() {
        let ks = kinds("a -> b - c");
        assert_eq!(
            ks,
            vec![
                TokenKind::Identifier("a"),
                TokenKind::Punct(Punct::Arrow),
                TokenKind::Identifier("b"),
                TokenKind::Punct(Punct::Minus),
                TokenKind::Identifier("c"),
            ]
        );
    }

    #[test]
    fn spans_are_correct() {
        let tokens: Vec<_> = Lexer::new("let x = 42;")
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        // "let" starts at col 1, ends at col 4
        assert_eq!(tokens[0].span.start.column, 1);
        assert_eq!(tokens[0].span.end.column, 4);

        // "x" at col 5
        assert_eq!(tokens[1].span.start.column, 5);
        assert_eq!(tokens[1].span.end.column, 6);

        // "42" at col 9
        assert_eq!(tokens[3].span.start.column, 9);
        assert_eq!(tokens[3].span.end.column, 11);
    }
}
