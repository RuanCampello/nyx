//! Transformation of tokens into an abstract syntax tree.

use crate::{
    lexer::{
        Lexer,
        error::LexError,
        token::{Keyword, Punct, Span, Token, TokenKind},
    },
    parser::{
        error::{ParseErrorKind, ParserError},
        statement::Statement,
    },
};
use std::iter::Peekable;

pub mod error;
pub mod expression;
pub mod statement;

/// Recursive-descent parser.
pub struct Parser<'i> {
    cursor: Peekable<Lexer<'i>>,
    /// Most recently used consumed token, used to place EOF diagnostics.
    last: Option<Span>,
}

pub trait Parsable<'i>: Sized {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>>;
}

impl<'i> Parser<'i> {
    pub fn new(source: &'i str) -> Self {
        Self {
            cursor: Lexer::new(source).peekable(),
            last: None,
        }
    }

    pub fn parse(mut self) -> Result<Vec<Statement<'i>>, ParserError<'i>> {
        let mut statements = Vec::new();

        loop {
            match self.peek() {
                Some(Ok(token)) if token.kind == TokenKind::Eof => break,
                Some(Ok(_)) => statements.push(self.parse_node::<Statement>()?),
                None => break,
                Some(Err(err)) => return Err(ParserError::new(err.clone().into(), err.span())),
            }
        }

        Ok(statements)
    }

    fn parse_node<N: Parsable<'i>>(&mut self) -> Result<N, ParserError<'i>> {
        N::parse(self)
    }

    #[inline(always)]
    pub fn peek(&mut self) -> Option<&Result<Token<'i>, LexError>> {
        self.cursor.peek()
    }

    #[inline(always)]
    pub fn next_token(&mut self) -> Result<Option<Token<'i>>, ParserError<'i>> {
        match self.cursor.next() {
            Some(Ok(token)) => {
                self.last = Some(token.span);
                Ok(Some(token))
            }
            Some(Err(e)) => {
                let span = e.span();
                Err(ParserError::new(e.into(), span))
            }
            None => Ok(None),
        }
    }

    #[inline(always)]
    pub fn unexpected(&self, found: TokenKind<'i>, expected: TokenKind<'i>) -> ParserError<'i> {
        ParserError::new(
            ParseErrorKind::Expected { expected, found },
            self.last.unwrap_or_default(),
        )
    }

    #[inline(always)]
    pub fn expect_next(&mut self) -> Result<Token<'i>, ParserError<'i>> {
        self.next_token()?.ok_or_else(|| {
            ParserError::new(ParseErrorKind::UnexpectedEof, self.last.unwrap_or_default())
        })
    }

    #[inline(always)]
    pub fn expect_token(&mut self, expected: TokenKind<'i>) -> Result<Token<'i>, ParserError<'i>> {
        let token = self.expect_next()?;
        match token.kind == expected {
            true => Ok(token),
            false => Err(ParserError::new(
                ParseErrorKind::Expected {
                    expected,
                    found: token.kind,
                },
                token.span,
            )),
        }
    }

    #[inline(always)]
    pub fn expect_keyword(&mut self, expected: Keyword) -> Result<Token<'i>, ParserError<'i>> {
        self.expect_token(TokenKind::Keyword(expected))
    }

    #[inline(always)]
    pub fn expect_punct(&mut self, punct: Punct) -> Result<Token<'i>, ParserError<'i>> {
        self.expect_token(TokenKind::Punct(punct))
    }

    #[inline(always)]
    pub fn expect_identifier(&mut self) -> Result<(&'i str, Span), ParserError<'i>> {
        let token = self.expect_next()?;

        match token.kind {
            TokenKind::Identifier(id) => Ok((id, token.span)),
            _ => Err(ParserError::new(
                ParseErrorKind::ExpectedIdentifier { found: token.kind },
                token.span,
            )),
        }
    }

    #[inline(always)]
    pub fn consume_optional(&mut self, kind: TokenKind<'i>) -> bool {
        if let Some(Ok(token)) = self.peek() {
            if token.kind == kind {
                let _ = self.next_token();
                return true;
            }
        }

        false
    }

    #[inline(always)]
    pub fn consume_keyword(&mut self, keyword: Keyword) -> Result<bool, ParserError> {
        self.consume_token(TokenKind::Keyword(keyword))
    }

    #[inline(always)]
    pub fn consume_punct(&mut self, punct: Punct) -> Result<bool, ParserError> {
        self.consume_token(TokenKind::Punct(punct))
    }

    fn consume_token(&mut self, kind: TokenKind) -> Result<bool, ParserError> {
        match self.peek() {
            Some(Ok(token)) if token.kind == kind => {
                self.next_token()?;
                Ok(true)
            }

            Some(Err(err)) => {
                return Err(ParserError::new(
                    ParseErrorKind::Lexical(err.clone()),
                    err.span(),
                ));
            }

            _ => Ok(false),
        }
    }
}
