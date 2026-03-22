//! Transformation of tokens into an abstract syntax tree.

use crate::{
    lexer::{
        Lexer,
        token::{Punct, Span, Token, TokenKind},
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
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError>;
}

impl<'i> Parser<'i> {
    pub fn new(source: &'i str) -> Self {
        Self {
            cursor: Lexer::new(source).peekable(),
            last: None,
        }
    }

    pub fn parse(mut self) -> Result<Vec<Statement<'i>>, ParserError> {
        let mut statements = Vec::new();

        loop {
            match self.peek() {
                Some(Ok(token)) if token.kind == TokenKind::Eof => break,
                Some(Ok(_)) => statements.push(self.parse_node::<Statement>()?),
                None => break,
                Some(Err(err)) => return Err(err.clone().into()),
            }
        }

        Ok(statements)
    }

    fn parse_node<N: Parsable<'i>>(&mut self) -> Result<N, ParserError> {
        N::parse(self)
    }

    #[inline(always)]
    pub fn peek(&mut self) -> Option<&Result<Token<'i>, crate::lexer::error::LexError>> {
        self.cursor.peek()
    }

    pub fn next_token(&mut self) -> Result<Option<Token<'i>>, ParserError> {
        match self.cursor.next() {
            Some(Ok(token)) => {
                self.last = Some(token.span);
                Ok(Some(token))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn unexpected(&self, expected: &str, found: &str) -> ParserError {
        ParserError {
            kind: ParseErrorKind::Unexpected {
                expected: expected.to_string(),
                found: found.to_string(),
            },
            span: self.last.unwrap_or_default(),
        }
    }

    pub fn expect_next(&mut self, expected: &str) -> Result<Token<'i>, ParserError> {
        self.next_token()?
            .ok_or_else(|| self.unexpected(expected, "EOF"))
    }

    pub fn expect_punct(&mut self, punct: Punct) -> Result<Token<'i>, ParserError> {
        let expected = format!("punctiation `{punct}`");
        let token = self.expect_next(expected.as_ref())?;

        if TokenKind::Punct(punct) == token.kind {
            return Ok(token);
        }

        Err(ParserError {
            kind: ParseErrorKind::Unexpected {
                expected,
                found: token.kind.to_string(),
            },
            span: token.span,
        })
    }

    pub fn expect_identifier(&mut self) -> Result<(&'i str, Span), ParserError> {
        let token = self.expect_next("identifier")?;

        match token.kind {
            TokenKind::Identifier(id) => Ok((id, token.span)),
            _ => Err(ParserError {
                kind: ParseErrorKind::Unexpected {
                    expected: "identifier".to_string(),
                    found: token.kind.to_string(),
                },
                span: token.span,
            }),
        }
    }
}
