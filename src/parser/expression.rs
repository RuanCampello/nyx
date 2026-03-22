use crate::lexer::token::{Punct, Span, TokenKind};
use crate::parser::error::{ParseErrorKind, ParserError};
use crate::parser::{Parsable, Parser};

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

impl<'i> Parsable<'i> for Expression<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        Self::parse_expr(parser, 0)
    }
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

    fn parse_expr(parser: &mut Parser<'i>, precedence: u8) -> Result<Self, ParserError<'i>> {
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

    fn parse_prefix(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let token = parser.expect_next()?;

        match token.kind {
            TokenKind::Integer(n) => Ok(Expression::Integer(n, token.span)),
            TokenKind::Float(f) => Ok(Expression::Float(f, token.span)),
            TokenKind::String(s) => Ok(Expression::String(s, token.span)),
            TokenKind::Bool(b) => Ok(Expression::Bool(b, token.span)),
            TokenKind::Identifier(id) => Ok(Expression::Identifier(id, token.span)),
            TokenKind::Punct(Punct::Minus) | TokenKind::Punct(Punct::Bang) => {
                let operator = match token.kind {
                    TokenKind::Punct(Punct::Minus) => UnaryOperator::Neg,
                    TokenKind::Punct(Punct::Bang) => UnaryOperator::Not,

                    _ => {
                        return Err(ParserError::new(
                            ParseErrorKind::InvalidUnaryOperator { found: token.kind },
                            token.span,
                        ));
                    }
                };

                let expr = Self::parse_expr(parser, 10)?;
                let span = Span::new(token.span.start, expr.span().end);

                Ok(Expression::Unary {
                    operator,
                    expr: Box::new(expr),
                    span,
                })
            }

            TokenKind::Punct(Punct::OpenParen) => {
                let expr = parser.parse_node::<Expression<'i>>()?;
                parser.expect_punct(Punct::CloseParen)?;
                Ok(expr)
            }

            _ => Err(ParserError::new(
                ParseErrorKind::ExpectedExpression { found: token.kind },
                token.span,
            )),
        }
    }

    #[inline(always)]
    const fn infix_precedence(kind: &TokenKind) -> u8 {
        match kind {
            TokenKind::Punct(Punct::Eq) => 1, // assignment
            TokenKind::Punct(Punct::Or) => 2,
            TokenKind::Punct(Punct::And) => 3,
            TokenKind::Punct(Punct::EqEq) | TokenKind::Punct(Punct::BangEq) => 4,
            TokenKind::Punct(Punct::Lt)
            | TokenKind::Punct(Punct::LtEq)
            | TokenKind::Punct(Punct::Gt)
            | TokenKind::Punct(Punct::GtEq) => 5,
            TokenKind::Punct(Punct::Plus) | TokenKind::Punct(Punct::Minus) => 6,
            TokenKind::Punct(Punct::Star) | TokenKind::Punct(Punct::Slash) => 7,
            TokenKind::Punct(Punct::OpenParen) => 8, // function call
            _ => 0,
        }
    }

    fn parse_infix(
        parser: &mut Parser<'i>,
        left: Expression<'i>,
        precedence: u8,
    ) -> Result<Self, ParserError<'i>> {
        let token = parser.expect_next()?;

        match token.kind {
            TokenKind::Punct(Punct::OpenParen) => {
                let mut args = Vec::new();
                let end_position;
                let mut first = true;

                loop {
                    let token = match parser.peek() {
                        Some(Ok(token)) => token,
                        _ => {
                            return Err(ParserError::new(
                                ParseErrorKind::UnexpectedEof,
                                token.span,
                            ));
                        }
                    };

                    if matches!(token.kind, TokenKind::Punct(Punct::CloseParen)) {
                        end_position = parser.expect_punct(Punct::CloseParen)?.span.end;
                        break;
                    }

                    match first {
                        true => first = false,
                        _ => {
                            parser.expect_punct(Punct::Comma)?;
                        }
                    };

                    args.push(parser.parse_node::<Expression>()?)
                }

                let span = Span::new(left.span().start, end_position);
                Ok(Expression::Call {
                    callee: Box::new(left),
                    args,
                    span,
                })
            }

            TokenKind::Punct(Punct::Eq) => {
                let right = Self::parse_expr(parser, precedence - 1)?;
                let span = Span::new(left.span().start, right.span().end);

                match left {
                    Expression::Identifier(name, _) => Ok(Expression::Assignment {
                        target: name,
                        value: Box::new(right),
                        span,
                    }),

                    _ => Err(ParserError::new(
                        ParseErrorKind::UnexpectedIdentifier,
                        left.span(),
                    )),
                }
            }

            _ => {
                let operator = match token.kind {
                    TokenKind::Punct(Punct::Plus) => BinaryOperator::Add,
                    TokenKind::Punct(Punct::Minus) => BinaryOperator::Sub,
                    TokenKind::Punct(Punct::Star) => BinaryOperator::Mul,
                    TokenKind::Punct(Punct::Slash) => BinaryOperator::Div,
                    TokenKind::Punct(Punct::EqEq) => BinaryOperator::Eq,
                    TokenKind::Punct(Punct::BangEq) => BinaryOperator::Ne,
                    TokenKind::Punct(Punct::Lt) => BinaryOperator::Lt,
                    TokenKind::Punct(Punct::LtEq) => BinaryOperator::LtEq,
                    TokenKind::Punct(Punct::Gt) => BinaryOperator::Gt,
                    TokenKind::Punct(Punct::GtEq) => BinaryOperator::GtEq,
                    TokenKind::Punct(Punct::And) => BinaryOperator::And,
                    TokenKind::Punct(Punct::Or) => BinaryOperator::Or,
                    _ => {
                        return Err(ParserError::new(
                            ParseErrorKind::InvalidBinaryOperator { found: token.kind },
                            token.span,
                        ));
                    }
                };

                let right = Self::parse_expr(parser, precedence - 1)?;
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
