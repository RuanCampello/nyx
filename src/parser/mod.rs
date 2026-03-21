//! Transformation of tokens into an abstract syntax tree.

use crate::{
    lexer::{
        Lexer,
        token::{Position, Punct, Span, Token, TokenKind},
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
}

impl<'i> Parser<'i> {
    pub fn new(source: &'i str) -> Self {
        Self {
            cursor: Lexer::new(source).peekable(),
        }
    }

    pub fn parse(mut self) -> Result<Vec<statement::Statement<'i>>, ParserError> {
        let mut statements = Vec::new();

        while let Some(res) = self.cursor.peek() {
            match res {
                Ok(token) if token.kind == crate::lexer::token::TokenKind::Eof => break,
                Ok(_) => statements.push(Statement::parse(&mut self)?),
                Err(_) => {
                    let _ = self.next_token()?;
                    unreachable!()
                }
            }
        }

        Ok(statements)
    }

    #[inline(always)]
    pub fn peek(&mut self) -> Option<&Result<Token<'i>, crate::lexer::error::LexError>> {
        self.cursor.peek()
    }

    pub fn next_token(&mut self) -> Result<Option<Token<'i>>, ParserError> {
        match self.cursor.next() {
            Some(Ok(t)) => Ok(Some(t)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn expect_punct(&mut self, punct: Punct) -> Result<Token<'i>, ParserError> {
        let token = self.next_token()?.ok_or_else(|| ParserError {
            kind: ParseErrorKind::UnexpectedToken {
                expected: punct.as_str(),
                found: "EOF".into(),
            },
            span: Span::new(Position::new(0, 0, 0), Position::new(0, 0, 0)),
            help: None,
        })?;

        match token.kind == TokenKind::Punct(punct) {
            true => Ok(token),
            false => Err(ParserError {
                kind: error::ParseErrorKind::UnexpectedToken {
                    expected: punct.as_str(),
                    found: token.kind.to_string(),
                },
                span: token.span,
                help: None,
            }),
        }
    }

    pub fn expect_identifier(&mut self) -> Result<(&'i str, Span), ParserError> {
        let token = self.next_token()?.ok_or_else(|| ParserError {
            kind: ParseErrorKind::UnexpectedToken {
                expected: "identifier",
                found: "EOF".into(),
            },
            span: Span::new(Position::new(0, 0, 0), Position::new(0, 0, 0)),
            help: None,
        })?;

        match token.kind {
            TokenKind::Identifier(id) => Ok((id, token.span)),
            _ => Err(ParserError {
                kind: ParseErrorKind::ExpectedIdentifier {
                    found: token.kind.to_string(),
                },
                span: token.span,
                help: None,
            }),
        }
    }
}
