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

mod char;
mod comment;
mod identifier;
mod number;
mod string;

use char::CharLiteral;
use comment::{DocComment, LineComment};
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

#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub struct Spanned<T> {
    span: Span,
    value: T,
}

pub trait HasSpan {
    fn span(&self) -> Option<Span>;
}

impl<'src> Lexer<'src> {
    #[inline]
    pub fn new(source: &'src str) -> Self {
        Self::with_base(source, token::BytePos::default())
    }

    /// Lex `source` whose first byte sits at global offset `base` in the
    /// [`SourceMap`](crate::source_map::SourceMap) address space
    #[inline]
    pub fn with_base(source: &'src str, base: token::BytePos) -> Self {
        Self { cursor: Cursor::new(source, base), finished: false }
    }

    /// Produces the next token, or `None` after EOF has been emitted.
    fn next_token(&mut self) -> Result<Option<Token<'src>>, LexError<'src>> {
        if self.finished {
            return Ok(None);
        }

        self.skip_whitespace_and_comments();
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
            '\'' => CharLiteral.lex(&mut self.cursor, start)?,

            '(' => self.single_punct(Punct::OpenParen),
            ')' => self.single_punct(Punct::CloseParen),
            '{' => self.single_punct(Punct::OpenBrace),
            '}' => self.single_punct(Punct::CloseBrace),
            '[' => self.single_punct(Punct::OpenBracket),
            ']' => self.single_punct(Punct::CloseBracket),
            ';' => self.single_punct(Punct::Semicolon),
            ',' => self.single_punct(Punct::Comma),
            '.' => {
                self.cursor.advance();
                match self.cursor.consume_optional('.') {
                    true if self.cursor.consume_optional('=') => self.token(Punct::RangeEq, start),
                    true => self.token(Punct::Range, start),
                    _ => self.token(Punct::Dot, start),
                }
            },
            '+' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::PlusEq, start),
                    _ => self.token(Punct::Plus, start),
                }
            },

            '*' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::StarEq, start),
                    _ => self.token(Punct::Star, start),
                }
            },

            ':' => {
                self.cursor.advance();
                match self.cursor.consume_optional(':') {
                    true => self.token(Punct::ColonColon, start),
                    _ => self.token(Punct::Colon, start),
                }
            },

            '-' => {
                self.cursor.advance();
                match self.cursor.consume_optional('>') {
                    true => self.token(Punct::Arrow, start),
                    _ => match self.cursor.consume_optional('=') {
                        true => self.token(Punct::MinusEq, start),
                        _ => self.token(Punct::Minus, start),
                    },
                }
            },

            // `//` was consumed while skipping; only `///` or a lone `/` reach here
            '/' => match self.is_doc_comment() {
                true => DocComment.lex(&mut self.cursor, start)?,
                _ => {
                    self.cursor.advance();
                    match self.cursor.consume_optional('=') {
                        true => self.token(Punct::SlashEq, start),
                        _ => self.token(Punct::Slash, start),
                    }
                },
            },

            '=' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::EqEq, start),
                    _ => self.token(Punct::Eq, start),
                }
            },

            '!' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::BangEq, start),
                    _ => self.token(Punct::Bang, start),
                }
            },

            '^' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::CaretEq, start),
                    _ => self.token(Punct::Caret, start),
                }
            },

            '@' => self.single_punct(Punct::At),

            '<' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::LtEq, start),
                    _ => match self.cursor.consume_optional('<') {
                        true => match self.cursor.consume_optional('=') {
                            true => self.token(Punct::ShlEq, start),
                            _ => self.token(Punct::Shl, start),
                        },
                        _ => self.token(Punct::Lt, start),
                    },
                }
            },

            '>' => {
                self.cursor.advance();
                match self.cursor.consume_optional('=') {
                    true => self.token(Punct::GtEq, start),
                    _ => match self.cursor.consume_optional('>') {
                        true => match self.cursor.consume_optional('=') {
                            true => self.token(Punct::ShrEq, start),
                            _ => self.token(Punct::Shr, start),
                        },
                        _ => self.token(Punct::Gt, start),
                    },
                }
            },

            '&' => {
                self.cursor.advance();
                match self.cursor.consume_optional('&') {
                    true => self.token(Punct::And, start),
                    _ => match self.cursor.consume_optional('=') {
                        true => self.token(Punct::AmpersandEq, start),
                        _ => self.token(Punct::Ampersand, start),
                    },
                }
            },

            '|' => {
                self.cursor.advance();
                match self.cursor.consume_optional('|') {
                    true => self.token(Punct::Or, start),
                    _ => match self.cursor.consume_optional('=') {
                        true => self.token(Punct::PipeEq, start),
                        _ => self.token(Punct::Pipe, start),
                    },
                }
            },

            // consumed before reporting so recovery is guaranteed to progress
            other => {
                self.cursor.advance();
                return Err(LexError::unexpected_char(other, start));
            },
        };

        Ok(Some(token))
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            self.cursor.consume_while(|ch| ch.is_ascii_whitespace());

            match (self.cursor.peek(), self.cursor.peek_until(2)) {
                // '///' is left for next_token to tokenise, '//' and '////'+ are skipped
                (Some('/'), Some('/')) if self.is_doc_comment() => break,
                (Some('/'), Some('/')) => {
                    self.cursor.advance();
                    self.cursor.advance();

                    LineComment.skip(&mut self.cursor);
                },
                _ => break,
            }
        }
    }

    /// Exactly three slashes, a `////`+ divider is an ordinary comment.
    #[inline]
    fn is_doc_comment(&self) -> bool {
        self.cursor.peek_until(2) == Some('/')
            && self.cursor.peek_until(3) == Some('/')
            && self.cursor.peek_until(4) != Some('/')
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
    fn token(&self, punct: Punct, start: token::BytePos) -> Token<'src> {
        Token::new(TokenKind::Punct(punct), Span::new(start, self.cursor.position()))
    }
}

impl<'src> Iterator for Lexer<'src> {
    type Item = Result<Token<'src>, LexError<'src>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_token().transpose()
    }
}

impl<T> Spanned<T> {
    pub fn new(value: T, span: Span) -> Self {
        Self { span, value }
    }

    pub const fn span(&self) -> Span {
        self.span
    }
}

impl<T: Clone> Spanned<T> {
    pub fn value(&self) -> T {
        self.value.clone()
    }

    #[allow(unused)]
    pub fn value_ref(&self) -> &T {
        &self.value
    }
}

/// whether `word` is a reserved nyx keyword, the single source of truth for
/// editor tooling that classifies identifiers without a full lex
#[inline]
pub fn is_keyword(word: &str) -> bool {
    use std::str::FromStr;
    token::Keyword::from_str(word).is_ok()
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
    fn compound_assignment_operators() {
        let ks = kinds("+= -= *= /= &= |= ^= <<= >>=");
        let expected = [
            Punct::PlusEq,
            Punct::MinusEq,
            Punct::StarEq,
            Punct::SlashEq,
            Punct::AmpersandEq,
            Punct::PipeEq,
            Punct::CaretEq,
            Punct::ShlEq,
            Punct::ShrEq,
        ];

        assert_eq!(ks, expected.map(TokenKind::Punct));
    }

    #[test]
    fn compound_assignment_does_not_swallow_its_neighbours() {
        let ks = kinds("a <= b >= c << d >> e -> f && g || h");
        let puncts: Vec<_> = ks
            .into_iter()
            .filter_map(|kind| match kind {
                TokenKind::Punct(punct) => Some(punct),
                _ => None,
            })
            .collect();

        assert_eq!(
            puncts,
            [
                Punct::LtEq,
                Punct::GtEq,
                Punct::Shl,
                Punct::Shr,
                Punct::Arrow,
                Punct::And,
                Punct::Or,
            ]
        );
    }

    #[test]
    fn equals_followed_by_an_operator_stays_two_tokens() {
        let ks = kinds("a =- 1");
        assert_eq!(ks[1], TokenKind::Punct(Punct::Eq));
        assert_eq!(ks[2], TokenKind::Punct(Punct::Minus));
    }

    #[test]
    fn punctuation() {
        let ks = kinds("( ) { } [ ] : ; , . + - * / = == != < > <= >= -> & && || @");
        let expected = [
            Punct::OpenParen,
            Punct::CloseParen,
            Punct::OpenBrace,
            Punct::CloseBrace,
            Punct::OpenBracket,
            Punct::CloseBracket,
            Punct::Colon,
            Punct::Semicolon,
            Punct::Comma,
            Punct::Dot,
            Punct::Plus,
            Punct::Minus,
            Punct::Star,
            Punct::Slash,
            Punct::Eq,
            Punct::EqEq,
            Punct::BangEq,
            Punct::Lt,
            Punct::Gt,
            Punct::LtEq,
            Punct::GtEq,
            Punct::Arrow,
            Punct::Ampersand,
            Punct::And,
            Punct::Or,
            Punct::At,
        ]
        .map(TokenKind::Punct)
        .to_vec();

        assert_eq!(ks, expected);
    }

    #[test]
    fn and_vs_ampersand() {
        let ks = kinds("& && && &");

        assert_eq!(
            [Punct::Ampersand, Punct::And, Punct::And, Punct::Ampersand]
                .map(TokenKind::Punct)
                .to_vec(),
            ks
        )
    }

    #[test]
    fn keywords_and_identifiers() {
        let ks = kinds("fn let mut if else return loop break continue in for struct foo _bar x1");
        assert_eq!(
            ks,
            vec![
                TokenKind::Keyword(Keyword::Fn),
                TokenKind::Keyword(Keyword::Let),
                TokenKind::Keyword(Keyword::Mut),
                TokenKind::Keyword(Keyword::If),
                TokenKind::Keyword(Keyword::Else),
                TokenKind::Keyword(Keyword::Return),
                TokenKind::Keyword(Keyword::Loop),
                TokenKind::Keyword(Keyword::Break),
                TokenKind::Keyword(Keyword::Continue),
                TokenKind::Keyword(Keyword::In),
                TokenKind::Keyword(Keyword::For),
                TokenKind::Keyword(Keyword::Struct),
                TokenKind::Identifier("foo"),
                TokenKind::Identifier("_bar"),
                TokenKind::Identifier("x1"),
            ]
        );
    }

    #[test]
    fn range_punctuation() {
        assert_eq!(
            kinds(". .. ..="),
            vec![
                TokenKind::Punct(Punct::Dot),
                TokenKind::Punct(Punct::Range),
                TokenKind::Punct(Punct::RangeEq),
            ]
        );
    }

    #[test]
    fn boolean_literals() {
        assert_eq!(kinds("true false"), vec![TokenKind::Bool(true), TokenKind::Bool(false)]);
    }

    #[test]
    #[allow(clippy::approx_constant)]
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
            vec![TokenKind::String("hello"), TokenKind::String(""), TokenKind::String("a\\nb"),]
        );
    }

    #[test]
    fn line_comment_skipped() {
        let ks = kinds("42 // this is a comment\n7");
        assert_eq!(ks, vec![TokenKind::Integer(42), TokenKind::Integer(7)]);
    }

    #[test]
    fn trailing_line_comment_skipped() {
        let ks = kinds("let x = 1; // trailing");
        assert_eq!(
            ks,
            vec![
                TokenKind::Keyword(Keyword::Let),
                TokenKind::Identifier("x"),
                TokenKind::Punct(Punct::Eq),
                TokenKind::Integer(1),
                TokenKind::Punct(Punct::Semicolon),
            ]
        );
    }

    #[test]
    fn doc_comment_is_tokenised() {
        let ks = kinds("/// docs\nfn");
        assert_eq!(ks, vec![TokenKind::DocComment(" docs"), TokenKind::Keyword(Keyword::Fn)]);
    }

    #[test]
    fn consecutive_doc_comments() {
        let ks = kinds("/// one\n/// two\nfn");
        assert_eq!(
            ks,
            vec![
                TokenKind::DocComment(" one"),
                TokenKind::DocComment(" two"),
                TokenKind::Keyword(Keyword::Fn),
            ]
        );
    }

    #[test]
    fn quad_slash_is_an_ordinary_comment() {
        let ks = kinds("//// not docs\n7");
        assert_eq!(ks, vec![TokenKind::Integer(7)]);
    }

    #[test]
    fn slash_is_still_division() {
        let ks = kinds("a / b");
        assert_eq!(
            ks,
            vec![
                TokenKind::Identifier("a"),
                TokenKind::Punct(Punct::Slash),
                TokenKind::Identifier("b"),
            ]
        );
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
        let result: Result<Vec<_>, _> = Lexer::new("42 ` 7").collect();
        let err = result.unwrap_err();
        assert_eq!(err.kind, error::LexErrorKind::UnexpectedChar('`'));
        assert_eq!(err.span.start.0, 3);
    }

    #[test]
    fn unterminated_string_error() {
        let result: Result<Vec<_>, _> = Lexer::new(r#"let x = "hello"#).collect();
        let err = result.unwrap_err();
        assert_eq!(err.kind, error::LexErrorKind::UnterminatedString);
    }

    #[test]
    fn bang_without_eq() {
        let ks = kinds("!x");
        assert_eq!(ks, vec![TokenKind::Punct(Punct::Bang), TokenKind::Identifier("x")]);
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
        let tokens: Vec<_> = Lexer::new("let x = 42;").collect::<Result<Vec<_>, _>>().unwrap();

        // "let" occupies bytes 0..3
        assert_eq!(tokens[0].span.start.0, 0);
        assert_eq!(tokens[0].span.end.0, 3);

        // "x" at byte 4
        assert_eq!(tokens[1].span.start.0, 4);
        assert_eq!(tokens[1].span.end.0, 5);

        // "42" at bytes 8..10
        assert_eq!(tokens[3].span.start.0, 8);
        assert_eq!(tokens[3].span.end.0, 10);
    }

    #[test]
    fn bitwise_and_shifts_lexing() {
        let ks = kinds("x & y | z ^ w << 2 >> 3");
        assert_eq!(
            ks,
            vec![
                TokenKind::Identifier("x"),
                TokenKind::Punct(Punct::Ampersand),
                TokenKind::Identifier("y"),
                TokenKind::Punct(Punct::Pipe),
                TokenKind::Identifier("z"),
                TokenKind::Punct(Punct::Caret),
                TokenKind::Identifier("w"),
                TokenKind::Punct(Punct::Shl),
                TokenKind::Integer(2),
                TokenKind::Punct(Punct::Shr),
                TokenKind::Integer(3),
            ]
        );
    }

    #[test]
    fn lexer_resumes_after_every_bad_char() {
        let items: Vec<_> = Lexer::new("a ` b $ c").collect();
        let errors = items.iter().filter(|item| item.is_err()).count();
        let identifiers: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                Ok(Token { kind: TokenKind::Identifier(name), .. }) => Some(*name),
                _ => None,
            })
            .collect();

        assert_eq!(errors, 2, "both bad characters are reported: {items:?}");
        assert_eq!(identifiers, ["a", "b", "c"]);
        assert!(
            matches!(items.last(), Some(Ok(Token { kind: TokenKind::Eof, .. }))),
            "recovery still terminates at eof: {items:?}"
        );
    }

    #[test]
    fn lexer_terminates_on_an_unterminated_string() {
        let items: Vec<_> = Lexer::new("let x = \"oops").collect();
        assert!(items.iter().any(|item| item.is_err()));
        assert!(items.len() < 16, "the stream must not spin: {items:?}");
    }
}
