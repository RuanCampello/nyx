use std::hint::unreachable_unchecked;

use crate::lexer::token::{Position, Punct, Span, TokenKind as Kind};
use crate::parser::Parser;
use crate::parser::error::{ParseErrorKind, ParserError};

#[derive(Debug, Clone, PartialEq)]
pub enum Expression<'i> {
    Integer(i64, Span),
    Float(f64, Span),
    String(&'i str, Span),
    Bool(bool, Span),
    Identifier(&'i str, Span),
    Unary {
        operator: UnaryOperator,
        expr: Box<Expression<'i>>,
        span: Span,
    },
    Binary {
        left: Box<Expression<'i>>,
        operator: BinaryOperator,
        right: Box<Expression<'i>>,
        span: Span,
    },
    Assignment {
        target: &'i str,
        value: Box<Expression<'i>>,
        span: Span,
    },
    Call {
        callee: Box<Expression<'i>>,
        args: Vec<Expression<'i>>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOperator {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinaryOperator {
    Add,
    Sub,
    Div,
    Mul,
    Eq,
    Ne,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
}

impl<'i> Expression<'i> {
    #[inline(always)]
    pub const fn span(&self) -> Span {
        match self {
            Self::Integer(_, span)
            | Self::Float(_, span)
            | Self::String(_, span)
            | Self::Bool(_, span)
            | Self::Identifier(_, span)
            | Self::Unary { span, .. }
            | Self::Binary { span, .. }
            | Self::Assignment { span, .. }
            | Self::Call { span, .. } => *span,
        }
    }

    pub fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
        Self::parse_expr(parser, 0)
    }

    fn parse_expr(parser: &mut Parser<'i>, precedence: u8) -> Result<Self, ParserError> {
        let mut left = Self::parse_prefix(parser)?;

        while let Some(Ok(token)) = parser.peek() {
            let next_precedence = Self::infix_precedence(&token.kind);
            if next_precedence <= precedence {
                break;
            }

            left = Self::parse_infix(parser, left, next_precedence)?;
        }

        Ok(left)
    }

    fn parse_prefix(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
        let token = parser.next_token()?;
        let token = token.ok_or_else(|| ParserError {
            kind: ParseErrorKind::UnexpectedToken {
                expected: "expression",
                found: "EOF".into(),
            },
            span: Span::new(Position::new(0, 0, 0), Position::new(0, 0, 0)),
            help: None,
        })?;

        match token.kind {
            Kind::Integer(n) => Ok(Expression::Integer(n, token.span)),
            Kind::Float(f) => Ok(Expression::Float(f, token.span)),
            Kind::String(s) => Ok(Expression::String(s, token.span)),
            Kind::Bool(b) => Ok(Expression::Bool(b, token.span)),
            Kind::Identifier(id) => Ok(Expression::Identifier(id, token.span)),
            Kind::Punct(Punct::Minus) | Kind::Punct(Punct::Bang) => {
                let operator = match token.kind {
                    Kind::Punct(Punct::Minus) => UnaryOperator::Neg,
                    Kind::Punct(Punct::Bang) => UnaryOperator::Not,
                    _ => unsafe { unreachable_unchecked() },
                };

                let expr = Self::parse_expr(parser, 10)?;
                let span = Span::new(token.span.start, expr.span().end);

                Ok(Expression::Unary {
                    operator,
                    expr: Box::new(expr),
                    span,
                })
            }
            Kind::Punct(Punct::OpenParen) => {
                let expr = Expression::parse(parser)?;
                parser.expect_punct(Punct::CloseParen)?;
                Ok(expr)
            }
            _ => Err(ParserError {
                kind: ParseErrorKind::UnexpectedToken {
                    expected: "expression",
                    found: token.kind.to_string(),
                },
                span: token.span,
                help: None,
            }),
        }
    }

    #[inline(always)]
    const fn infix_precedence(kind: &crate::lexer::token::TokenKind) -> u8 {
        match kind {
            Kind::Punct(Punct::Eq) => 1, // assignment
            Kind::Punct(Punct::Or) => 2,
            Kind::Punct(Punct::And) => 3,
            Kind::Punct(Punct::EqEq) | Kind::Punct(Punct::BangEq) => 4,
            Kind::Punct(Punct::Lt)
            | Kind::Punct(Punct::LtEq)
            | Kind::Punct(Punct::Gt)
            | Kind::Punct(Punct::GtEq) => 5,
            Kind::Punct(Punct::Plus) | Kind::Punct(Punct::Minus) => 6,
            Kind::Punct(Punct::Star) | Kind::Punct(Punct::Slash) => 7,
            Kind::Punct(Punct::OpenParen) => 8, // function call
            _ => 0,
        }
    }

    fn parse_infix(
        parser: &mut Parser<'i>,
        left: Expression<'i>,
        precedence: u8,
    ) -> Result<Self, ParserError> {
        let token = parser.next_token()?.unwrap();

        match token.kind {
            Kind::Punct(Punct::OpenParen) => {
                let mut args = Vec::new();
                let end_pos;
                let mut first = true;

                loop {
                    match parser.peek() {
                        Some(Ok(token)) => {
                            if matches!(token.kind, Kind::Punct(Punct::CloseParen)) {
                                end_pos = parser.next_token()?.unwrap().span.end;
                                break;
                            }
                        }
                        _ => {
                            return Err(ParserError {
                                kind: crate::parser::error::ParseErrorKind::UnexpectedToken {
                                    expected: "`)`",
                                    found: "EOF".into(),
                                },
                                span: token.span,
                                help: None,
                            });
                        }
                    };

                    match !first {
                        true => {
                            parser.expect_punct(Punct::Comma)?;
                        }
                        _ => first = false,
                    }

                    args.push(Expression::parse(parser)?);
                }

                let span = Span::new(left.span().start, end_pos);
                Ok(Expression::Call {
                    callee: Box::new(left),
                    args,
                    span,
                })
            }
            Kind::Punct(Punct::Eq) => {
                let right = Self::parse_expr(parser, precedence - 1)?;
                let span = Span::new(left.span().start, right.span().end);

                match left {
                    Expression::Identifier(name, _) => Ok(Expression::Assignment {
                        target: name,
                        value: Box::new(right),
                        span,
                    }),

                    _ => Err(ParserError {
                        kind: ParseErrorKind::InvalidAssignmentTarget,
                        span: left.span(),
                        help: None,
                    }),
                }
            }
            _ => {
                let operator = match token.kind {
                    Kind::Punct(Punct::Plus) => BinaryOperator::Add,
                    Kind::Punct(Punct::Minus) => BinaryOperator::Sub,
                    Kind::Punct(Punct::Star) => BinaryOperator::Mul,
                    Kind::Punct(Punct::Slash) => BinaryOperator::Div,
                    Kind::Punct(Punct::EqEq) => BinaryOperator::Eq,
                    Kind::Punct(Punct::BangEq) => BinaryOperator::Ne,
                    Kind::Punct(Punct::Lt) => BinaryOperator::Lt,
                    Kind::Punct(Punct::LtEq) => BinaryOperator::LtEq,
                    Kind::Punct(Punct::Gt) => BinaryOperator::Gt,
                    Kind::Punct(Punct::GtEq) => BinaryOperator::GtEq,
                    Kind::Punct(Punct::And) => BinaryOperator::And,
                    Kind::Punct(Punct::Or) => BinaryOperator::Or,
                    _ => unsafe { unreachable_unchecked() },
                };

                let right = Self::parse_expr(parser, precedence)?;
                let span = Span::new(left.span().start, right.span().end);
                Ok(Expression::Binary {
                    left: Box::new(left),
                    operator,
                    right: Box::new(right),
                    span,
                })
            }
        }
    }
}
