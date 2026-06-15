use crate::lexer::Spanned;
use crate::lexer::token::{Keyword, Punct, Span, TokenKind};
use crate::parser::error::{ParseErrorKind, ParserError};
use crate::parser::statement::{self, Type};
use crate::parser::{Parsable, Parser};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq)]
#[rustfmt::skip]
pub enum Expression<'i> {
    Integer(i64, Span),
    Float(f64, Span),
    String(&'i str, Span),
    Char(char, Span),
    Bool(bool, Span),
    Identifier(&'i str, Span),
    Unary { operator: UnaryOperator, expr: Box<Expression<'i>>, span: Span },
    Binary {
        left: Box<Expression<'i>>,
        operator: BinaryOperator,
        right: Box<Expression<'i>>,
        span: Span,
    },
    Assignment { target: Box<Expression<'i>>, value: Box<Expression<'i>>, span: Span },
    Field { expr: Box<Expression<'i>>, field: &'i str, span: Span },
    Struct { 
        name: &'i str,
        fields: Vec<StructField<'i>>,
        type_args: Vec<Spanned<Type<'i>>>,
        span: Span,
    },
    Call {
        callee: Box<Expression<'i>>,
        args: Vec<Expression<'i>>,
        type_args: Vec<Spanned<Type<'i>>>,
        span: Span,
    },
    QualifiedCall {
        qualifier: &'i str,
        name: &'i str,
        args: Vec<Expression<'i>>,
        type_args: Vec<Spanned<Type<'i>>>,
        span: Span,
    },
    QualifiedName { qualifier: &'i str, name: &'i str, span: Span },
    /// Special compiler intrinsics that accept a type annotation as an argument
    /// these must be handled at the expression parser level because types are not value-level expressions,
    /// so standard function/intrinsic call parsing would fail on them
    TypeIntrinsic {
        kind: TypeIntrinsicKind,
        qualifier: Option<&'i str>,
        typ: Spanned<Type<'i>>,
        span: Span,
    },
    Cast { expr: Box<Expression<'i>>, target_type: Spanned<Type<'i>>, span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeIntrinsicKind {
    SizeOf,
    AlignOf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructField<'i> {
    pub name: &'i str,
    pub value: Expression<'i>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOperator {
    Neg,
    Not,
    Deref,
    Ref,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[rustfmt::skip]
pub enum BinaryOperator {
    Add, Sub, Div, Mul,
    Eq, Ne,
    Lt, LtEq, Gt, GtEq,
    And, Or,
    BitAnd, BitOr, BitXor, Shl,
    Shr,
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
            | Self::Char(_, span)
            | Self::Bool(_, span)
            | Self::Identifier(_, span)
            | Self::Unary { span, .. }
            | Self::Binary { span, .. }
            | Self::Assignment { span, .. }
            | Self::Struct { span, .. }
            | Self::Field { span, .. }
            | Self::Call { span, .. }
            | Self::QualifiedCall { span, .. }
            | Self::QualifiedName { span, .. }
            | Self::TypeIntrinsic { span, .. }
            | Self::Cast { span, .. } => *span,
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
            TokenKind::Char(c) => Ok(Expression::Char(c, token.span)),
            TokenKind::Bool(b) => Ok(Expression::Bool(b, token.span)),
            TokenKind::Identifier(ident) => {
                if Self::next_is_struct(parser) {
                    Self::parse_struct(parser, ident, Vec::new(), token.span)
                } else {
                    Ok(Expression::Identifier(ident, token.span))
                }
            },
            TokenKind::Punct(Punct::Minus)
            | TokenKind::Punct(Punct::Bang)
            | TokenKind::Punct(Punct::Star)
            | TokenKind::Punct(Punct::Ampersand) => {
                let operator = match token.kind {
                    TokenKind::Punct(Punct::Minus) => UnaryOperator::Neg,
                    TokenKind::Punct(Punct::Bang) => UnaryOperator::Not,
                    TokenKind::Punct(Punct::Star) => UnaryOperator::Deref,
                    TokenKind::Punct(Punct::Ampersand) => UnaryOperator::Ref,

                    _ => {
                        return Err(ParserError::new(
                            ParseErrorKind::InvalidUnaryOperator { found: token.kind },
                            token.span,
                        ));
                    },
                };

                let expr = Self::parse_expr(parser, 11)?;
                let span = token.span + expr.span();

                Ok(Expression::Unary { operator, expr: Box::new(expr), span })
            },

            TokenKind::Punct(Punct::OpenParen) => {
                let expr = parser.parse_node::<Expression<'i>>()?;
                parser.expect_token(Punct::CloseParen)?;
                Ok(expr)
            },

            _ => Err(ParserError::new(
                ParseErrorKind::ExpectedExpression { found: token.kind },
                token.span,
            )),
        }
    }

    fn parse_struct(
        parser: &mut Parser<'i>,
        name: &'i str,
        type_args: Vec<Spanned<Type<'i>>>,
        span: Span,
    ) -> Result<Self, ParserError<'i>> {
        parser.expect_token(Punct::OpenBrace)?;

        let (fields, close_span) =
            statement::parse_comma_separated(parser, Punct::CloseBrace, |parser| {
                let (name, field_span) = parser.expect_identifier()?;
                parser.expect_token(Punct::Colon)?;
                let value = parser.parse_node::<Expression>()?;
                let span = field_span + value.span();

                Ok(StructField { name, value, span })
            })?;

        let span = span + close_span;

        Ok(Expression::Struct { name, fields, type_args, span })
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
            TokenKind::Punct(Punct::Pipe) => 6,
            TokenKind::Punct(Punct::Caret) => 7,
            TokenKind::Punct(Punct::Ampersand) => 8,
            TokenKind::Punct(Punct::Shl) | TokenKind::Punct(Punct::Shr) => 9,
            TokenKind::Punct(Punct::Plus) | TokenKind::Punct(Punct::Minus) => 10,
            TokenKind::Punct(Punct::Star) | TokenKind::Punct(Punct::Slash) => 11,
            TokenKind::Punct(Punct::OpenParen) => 12, // function call
            TokenKind::Keyword(Keyword::As) => 12,    // casting
            TokenKind::Punct(Punct::ColonColon) => 13,
            TokenKind::Punct(Punct::Dot) => 14, // field access
            _ => 0,
        }
    }

    fn parse_call_args_after_paren(
        parser: &mut Parser<'i>,
        fallback_span: Span,
    ) -> Result<(Vec<Expression<'i>>, Span), ParserError<'i>> {
        let mut args = Vec::new();
        let mut first = true;
        let end_span;

        loop {
            let peeked = parser
                .peek()
                .and_then(|r| r.as_ref().ok())
                .ok_or_else(|| ParserError::new(ParseErrorKind::UnexpectedEof, fallback_span))?;

            if matches!(peeked.kind, TokenKind::Punct(Punct::CloseParen)) {
                end_span = parser.expect_token(Punct::CloseParen)?.span;
                break;
            }

            if !first {
                parser.expect_token(Punct::Comma)?;
            }

            first = false;
            args.push(parser.parse_node::<Expression>()?);
        }
        Ok((args, end_span))
    }

    fn parse_call_args(
        parser: &mut Parser<'i>,
        fallback_span: Span,
    ) -> Result<(Vec<Expression<'i>>, Span), ParserError<'i>> {
        parser.expect_token(Punct::OpenParen)?;
        Self::parse_call_args_after_paren(parser, fallback_span)
    }

    fn parse_infix(
        parser: &mut Parser<'i>,
        left: Expression<'i>,
        precedence: u8,
    ) -> Result<Self, ParserError<'i>> {
        let token = parser.expect_next()?;

        match token.kind {
            TokenKind::Punct(Punct::Dot) => {
                let (field, span) = parser.expect_identifier()?;
                let span = left.span() + span;

                Ok(Expression::Field { expr: Box::new(left), field, span })
            },
            TokenKind::Punct(Punct::ColonColon) => {
                let invalid_expr = || {
                    ParserError::new(
                        ParseErrorKind::ExpectedExpression { found: token.kind },
                        token.span,
                    )
                };

                // turbofish on `left` (e.g., `left::<T>`)
                if matches!(parser.peek(), Some(Ok(t)) if t.is_kind(Punct::Lt)) {
                    let type_args = statement::parse_generics(parser)?;

                    // struct literal (e.g., `SomeStruct::<T> { ... }`)
                    if matches!(parser.peek(), Some(Ok(t)) if t.is_kind(Punct::OpenBrace)) {
                        let Expression::Identifier(name, ident_span) = left else {
                            return Err(invalid_expr());
                        };

                        return Self::parse_struct(parser, name, type_args, ident_span);
                    }

                    // generic function call (e.g., `foo::<T>()`)
                    let (args, end_span) = Self::parse_call_args(parser, left.span())?;
                    return Ok(Expression::Call {
                        span: left.span() + end_span,
                        callee: Box::new(left),
                        args,
                        type_args,
                    });
                }

                // path / associated item access (e.g., `left::name...`)
                let Expression::Identifier(qualifier, _) = left else {
                    return Err(invalid_expr());
                };
                let (name, name_span) = parser.expect_identifier()?;

                // check for trailing turbofish (e.g., `:: <`)
                let has_turbofish = matches!(
                    (parser.peek_nth(0), parser.peek_nth(1)),
                    (Some(Ok(t1)), Some(Ok(t2))) if t1.is_kind(Punct::ColonColon) && t2.is_kind(Punct::Lt)
                );

                //associated call with turbofish (e.g., `container::method::<T>()`)
                if has_turbofish {
                    parser.expect_token(Punct::ColonColon)?;
                    let type_args = super::statement::parse_generics::<Spanned<Type>>(parser)?;

                    let (args, end_span) = Self::parse_call_args(parser, left.span())?;
                    return Ok(Expression::QualifiedCall {
                        span: left.span() + end_span,
                        qualifier,
                        name,
                        args,
                        type_args,
                    });
                }

                // methods, intrinsic types, or functions (e.g., `Container::method()`)
                if matches!(parser.peek(), Some(Ok(t)) if t.is_kind(Punct::OpenParen)) {
                    if let Ok(kind) = TypeIntrinsicKind::from_str(name) {
                        parser.expect_token(Punct::OpenParen)?;
                        let typ = parser.parse_node::<Spanned<Type>>()?;
                        let end_span = parser.expect_token(Punct::CloseParen)?.span;

                        return Ok(Expression::TypeIntrinsic {
                            kind,
                            qualifier: Some(qualifier),
                            typ,
                            span: left.span() + end_span,
                        });
                    }

                    let (args, end_span) = Self::parse_call_args(parser, name_span)?;
                    return Ok(Expression::QualifiedCall {
                        span: left.span() + end_span,
                        qualifier,
                        name,
                        args,
                        type_args: Vec::new(),
                    });
                }

                // plain associated path / variable (e.g., `Container::CONSTANT`)
                Ok(Expression::QualifiedName { span: left.span() + name_span, qualifier, name })
            },
            TokenKind::Punct(Punct::OpenParen) => {
                if let Expression::Identifier(name, _) = &left
                    && let Ok(kind) = TypeIntrinsicKind::from_str(name)
                {
                    let typ = parser.parse_node::<Spanned<Type>>()?;
                    let end_span = parser.expect_token(Punct::CloseParen)?.span;
                    let span = left.span() + end_span;

                    return Ok(Expression::TypeIntrinsic { kind, qualifier: None, typ, span });
                }

                let (args, end_span) = Self::parse_call_args_after_paren(parser, token.span)?;
                let span = Span::new(left.span().start, end_span.end);
                Ok(Expression::Call { callee: Box::new(left), args, type_args: Vec::new(), span })
            },

            TokenKind::Punct(Punct::Eq) => {
                let right = Self::parse_expr(parser, precedence - 1)?;
                let span = left.span() + right.span();

                match left {
                    Expression::Identifier { .. } | Expression::Field { .. } => {
                        Ok(Expression::Assignment {
                            target: Box::new(left),
                            value: Box::new(right),
                            span,
                        })
                    },

                    _ => Err(ParserError::new(ParseErrorKind::UnexpectedIdentifier, left.span())),
                }
            },

            TokenKind::Keyword(Keyword::As) => {
                let target_type = parser.parse_node::<Spanned<Type>>()?;
                let span = left.span() + target_type.span();

                Ok(Expression::Cast { expr: Box::new(left), target_type, span })
            },

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
                    TokenKind::Punct(Punct::Ampersand) => BinaryOperator::BitAnd,
                    TokenKind::Punct(Punct::Pipe) => BinaryOperator::BitOr,
                    TokenKind::Punct(Punct::Caret) => BinaryOperator::BitXor,
                    TokenKind::Punct(Punct::Shl) => BinaryOperator::Shl,
                    TokenKind::Punct(Punct::Shr) => BinaryOperator::Shr,
                    _ => {
                        return Err(ParserError::new(
                            ParseErrorKind::InvalidBinaryOperator { found: token.kind },
                            token.span,
                        ));
                    },
                };

                let right = Self::parse_expr(parser, precedence)?;
                let span = left.span() + right.span();

                Ok(Expression::Binary {
                    left: Box::new(left),
                    operator,
                    right: Box::new(right),
                    span,
                })
            },
        }
    }

    fn next_is_struct(parser: &mut Parser<'i>) -> bool {
        matches!(parser.peek_nth(0), Some(Ok(t)) if t.is_kind(TokenKind::Punct(Punct::OpenBrace)))
            && matches!(parser.peek_nth(1), Some(Ok(t)) if matches!(t.kind, TokenKind::Identifier(_)))
            && matches!(parser.peek_nth(2), Some(Ok(t)) if t.is_kind(TokenKind::Punct(Punct::Colon)))
    }
}

impl FromStr for TypeIntrinsicKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "size_of" => Ok(Self::SizeOf),
            "align_of" => Ok(Self::AlignOf),
            _ => Err(()),
        }
    }
}

impl From<&TypeIntrinsicKind> for &str {
    fn from(value: &TypeIntrinsicKind) -> Self {
        match value {
            TypeIntrinsicKind::SizeOf => "size_of",
            TypeIntrinsicKind::AlignOf => "align_of",
        }
    }
}
