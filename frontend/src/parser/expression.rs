use crate::lexer::Spanned;
use crate::lexer::token::{Keyword, Punct, Span, TokenKind};
use crate::parser::error::{ParseErrorKind, ParserError};
use crate::parser::statement::{self, Type};
use crate::parser::{Parsable, Parser};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq)]
#[rustfmt::skip]
pub enum Expression<'i> {
    Integer(u64, Span),
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
    /// `target <op>= value`, where the target is evaluated once
    CompoundAssignment {
        target: Box<Expression<'i>>,
        operator: BinaryOperator,
        value: Box<Expression<'i>>,
        span: Span,
    },
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
        path: Vec<&'i str>,
        name: &'i str,
        args: Vec<Expression<'i>>,
        type_args: Vec<Spanned<Type<'i>>>,
        span: Span,
    },
    QualifiedName { path: Vec<&'i str>, name: &'i str, span: Span },
    /// Special compiler intrinsics that accept a type annotation as an argument
    /// these must be handled at the expression parser level because types are not value-level expressions,
    /// so standard function/intrinsic call parsing would fail on them
    TypeIntrinsic {
        kind: TypeIntrinsicKind,
        path: Option<Vec<&'i str>>,
        typ: Spanned<Type<'i>>,
        span: Span,
    },
    Cast { expr: Box<Expression<'i>>, target_type: Spanned<Type<'i>>, span: Span },
    /// array literal `[a, b, c]`
    Array { elements: Vec<Expression<'i>>, span: Span },
    /// array repeat literal `[value; count]`
    ArrayRepeat { value: Box<Expression<'i>>, count: u64, span: Span },
    /// indexing `base[index]`
    Index { base: Box<Expression<'i>>, index: Box<Expression<'i>>, span: Span },
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
    RefMut,
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

/// How tightly each operator binds, as the Pratt parser climbs
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Precedence {
    /// not an infix operator at all
    None = 0,
    Assignment = 1,
    Or = 2,
    And = 3,
    Equality = 4,
    Comparison = 5,
    BitOr = 6,
    BitXor = 7,
    BitAnd = 8,
    Shift = 9,
    Sum = 10,
    Product = 11,
    /// a call, an index and an `as`, which all bind equally
    Suffix = 12,
    Path = 13,
    Field = 14,
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
            | Self::CompoundAssignment { span, .. }
            | Self::Struct { span, .. }
            | Self::Field { span, .. }
            | Self::Call { span, .. }
            | Self::QualifiedCall { span, .. }
            | Self::QualifiedName { span, .. }
            | Self::TypeIntrinsic { span, .. }
            | Self::Cast { span, .. }
            | Self::Array { span, .. }
            | Self::ArrayRepeat { span, .. }
            | Self::Index { span, .. } => *span,
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
                    TokenKind::Punct(Punct::Ampersand) => {
                        match parser.consume_token(Keyword::Mut)? {
                            true => UnaryOperator::RefMut,
                            false => UnaryOperator::Ref,
                        }
                    },

                    _ => {
                        return Err(ParserError::new(
                            ParseErrorKind::InvalidUnaryOperator { found: token.kind },
                            token.span,
                        ));
                    },
                };

                let expr = Self::parse_expr(parser, Precedence::UNARY_OPERAND.level())?;
                let span = token.span + expr.span();

                Ok(Expression::Unary { operator, expr: Box::new(expr), span })
            },

            TokenKind::Punct(Punct::OpenParen) => {
                let expr = parser.in_delimiter(|parser| parser.parse_node::<Expression<'i>>())?;
                parser.expect_token(Punct::CloseParen)?;
                Ok(expr)
            },

            TokenKind::Punct(Punct::OpenBracket) => {
                parser.in_delimiter(|parser| Self::parse_array_literal(parser, token.span))
            },

            _ => {
                parser.push_back(token);
                Err(ParserError::new(
                    ParseErrorKind::ExpectedExpression { found: token.kind },
                    token.span,
                ))
            },
        }
    }

    fn parse_struct(
        parser: &mut Parser<'i>,
        name: &'i str,
        type_args: Vec<Spanned<Type<'i>>>,
        span: Span,
    ) -> Result<Self, ParserError<'i>> {
        parser.expect_token(Punct::OpenBrace)?;

        let (fields, close_span) = parser.in_delimiter(|parser| {
            statement::parse_comma_separated(parser, Punct::CloseBrace, |parser| {
                let (name, field_span) = parser.expect_identifier()?;

                if !parser.consume_token(Punct::Colon)? {
                    let value = Expression::Identifier(name, field_span);
                    return Ok(StructField { name, value, span: field_span });
                }

                let value = parser.parse_node::<Expression>()?;
                let span = field_span + value.span();

                Ok(StructField { name, value, span })
            })
        })?;

        let span = span + close_span;

        Ok(Expression::Struct { name, fields, type_args, span })
    }

    fn parse_array_literal(parser: &mut Parser<'i>, open: Span) -> Result<Self, ParserError<'i>> {
        if let Some(Ok(token)) = parser.peek()
            && token.is_kind(Punct::CloseBracket)
        {
            let close = parser.expect_token(Punct::CloseBracket)?.span;
            return Ok(Expression::Array { elements: Vec::new(), span: open + close });
        }

        let first = parser.parse_node()?;

        if parser.consume_token(Punct::Semicolon)? {
            let count = parser.expect_unsigned_literal()?;
            let close = parser.expect_token(Punct::CloseBracket)?.span;
            return Ok(Expression::ArrayRepeat {
                value: Box::new(first),
                count,
                span: open + close,
            });
        }

        let mut elements = vec![first];
        while parser.consume_token(Punct::Comma)? {
            if let Some(Ok(token)) = parser.peek()
                && token.is_kind(Punct::CloseBracket)
            {
                break;
            }
            elements.push(parser.parse_node()?);
        }

        let close = parser.expect_token(Punct::CloseBracket)?.span;
        Ok(Expression::Array { elements, span: open + close })
    }

    /// The level `kind` binds at as an infix operator, zero when it is not one
    #[inline(always)]
    const fn infix_precedence(kind: &TokenKind) -> u8 {
        let punct = match kind {
            TokenKind::Punct(punct) => *punct,
            TokenKind::Keyword(Keyword::As) => return Precedence::Suffix.level(),
            _ => return Precedence::None.level(),
        };

        if let Some(operator) = BinaryOperator::from_punct(punct) {
            return operator.precedence().level();
        }

        if BinaryOperator::from_compound_assignment(punct).is_some() {
            return Precedence::Assignment.level();
        }

        let precedence = match punct {
            Punct::Eq => Precedence::Assignment,
            Punct::OpenParen | Punct::OpenBracket => Precedence::Suffix,
            Punct::ColonColon => Precedence::Path,
            Punct::Dot => Precedence::Field,
            _ => Precedence::None,
        };

        precedence.level()
    }

    fn parse_call_args_after_paren(
        parser: &mut Parser<'i>,
        fallback_span: Span,
    ) -> Result<(Vec<Expression<'i>>, Span), ParserError<'i>> {
        parser.in_delimiter(|parser| Self::call_args_body(parser, fallback_span))
    }

    fn call_args_body(
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

                if matches!(parser.peek(), Some(Ok(token)) if token.is_kind(Punct::CloseParen)) {
                    end_span = parser.expect_token(Punct::CloseParen)?.span;
                    break;
                }
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
                if let Some(Ok(ahead)) = parser.peek()
                    && !matches!(ahead.kind, TokenKind::Identifier(_))
                {
                    let (kind, span) = (ahead.kind, ahead.span);
                    return Err(ParserError::new(
                        ParseErrorKind::ExpectedIdentifier { found: kind },
                        span,
                    ));
                }

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

                // path / associated item access (e.g., `std::io::println...`)
                let (path, start_span) = match left {
                    Expression::Identifier(name, span) => (vec![name], span),
                    Expression::QualifiedName { mut path, name, span } => {
                        path.push(name);
                        (path, span)
                    },
                    _ => return Err(invalid_expr()),
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

                    let (args, end_span) = Self::parse_call_args(parser, start_span)?;
                    return Ok(Expression::QualifiedCall {
                        span: start_span + end_span,
                        path,
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
                            path: Some(path),
                            typ,
                            span: start_span + end_span,
                        });
                    }

                    let (args, end_span) = Self::parse_call_args(parser, name_span)?;
                    return Ok(Expression::QualifiedCall {
                        span: start_span + end_span,
                        path,
                        name,
                        args,
                        type_args: Vec::new(),
                    });
                }

                // plain associated path / variable (e.g., `Container::CONSTANT`)
                Ok(Expression::QualifiedName { span: start_span + name_span, path, name })
            },
            TokenKind::Punct(Punct::OpenParen) => {
                if let Expression::Identifier(name, _) = &left
                    && let Ok(kind) = TypeIntrinsicKind::from_str(name)
                {
                    let typ = parser.parse_node::<Spanned<Type>>()?;
                    let end_span = parser.expect_token(Punct::CloseParen)?.span;
                    let span = left.span() + end_span;

                    return Ok(Expression::TypeIntrinsic { kind, path: None, typ, span });
                }

                let (args, end_span) = Self::parse_call_args_after_paren(parser, token.span)?;
                let span = Span::new(left.span().start, end_span.end);
                Ok(Expression::Call { callee: Box::new(left), args, type_args: Vec::new(), span })
            },

            TokenKind::Punct(Punct::Eq) => {
                let right = Self::parse_expr(parser, precedence - 1)?;
                let span = left.span() + right.span();

                match is_place(&left) {
                    true => Ok(Expression::Assignment {
                        target: Box::new(left),
                        value: Box::new(right),
                        span,
                    }),
                    _ => {
                        Err(ParserError::new(ParseErrorKind::InvalidAssignmentTarget, left.span()))
                    },
                }
            },

            // `a += b` keeps its operator rather than expanding to `a = a + b`, so the target is evaluated once when it is lowered
            TokenKind::Punct(punct)
                if let Some(operator) = BinaryOperator::from_compound_assignment(punct) =>
            {
                let right = Self::parse_expr(parser, precedence - 1)?;
                let span = left.span() + right.span();

                match is_place(&left) {
                    true => Ok(Expression::CompoundAssignment {
                        target: Box::new(left),
                        operator,
                        value: Box::new(right),
                        span,
                    }),
                    _ => {
                        Err(ParserError::new(ParseErrorKind::InvalidAssignmentTarget, left.span()))
                    },
                }
            },

            TokenKind::Punct(Punct::OpenBracket) => {
                let index = parser.in_delimiter(|parser| parser.parse_node::<Expression<'i>>())?;
                let close = parser.expect_token(Punct::CloseBracket)?.span;
                let span = left.span() + close;

                Ok(Expression::Index { base: Box::new(left), index: Box::new(index), span })
            },

            TokenKind::Keyword(Keyword::As) => {
                let target_type = parser.parse_node::<Spanned<Type>>()?;
                let span = left.span() + target_type.span();

                Ok(Expression::Cast { expr: Box::new(left), target_type, span })
            },

            _ => {
                let operator = match token.kind {
                    TokenKind::Punct(punct) => BinaryOperator::from_punct(punct),
                    _ => None,
                };

                let Some(operator) = operator else {
                    return Err(ParserError::new(
                        ParseErrorKind::InvalidBinaryOperator { found: token.kind },
                        token.span,
                    ));
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
        if !parser.struct_literal_allowed() {
            return false;
        }

        let opens = matches!(parser.peek_nth(0), Some(Ok(t)) if t.is_kind(TokenKind::Punct(Punct::OpenBrace)));
        let named =
            matches!(parser.peek_nth(1), Some(Ok(t)) if matches!(t.kind, TokenKind::Identifier(_)));
        let separated = matches!(parser.peek_nth(2), Some(Ok(t)) if {
            t.is_kind(TokenKind::Punct(Punct::Colon))
                || t.is_kind(TokenKind::Punct(Punct::Comma))
                || t.is_kind(TokenKind::Punct(Punct::CloseBrace))
        });

        opens && named && separated
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

impl Precedence {
    /// the level a prefix operator parses its operand at
    pub const UNARY_OPERAND: Self = Self::Product;

    #[inline]
    pub const fn level(self) -> u8 {
        self as u8
    }
}

impl UnaryOperator {
    #[inline]
    pub const fn as_str<'s>(self) -> &'s str {
        match self {
            Self::Neg => Punct::Minus.as_str(),
            Self::Not => Punct::Bang.as_str(),
            Self::Deref => Punct::Star.as_str(),
            Self::Ref => Punct::Ampersand.as_str(),
            Self::RefMut => "&mut",
        }
    }

    #[inline]
    pub const fn needs_separator(self) -> bool {
        matches!(self, Self::RefMut)
    }
}

impl BinaryOperator {
    #[inline]
    pub const fn punct(self) -> Punct {
        match self {
            Self::Add => Punct::Plus,
            Self::Sub => Punct::Minus,
            Self::Mul => Punct::Star,
            Self::Div => Punct::Slash,
            Self::Eq => Punct::EqEq,
            Self::Ne => Punct::BangEq,
            Self::Lt => Punct::Lt,
            Self::LtEq => Punct::LtEq,
            Self::Gt => Punct::Gt,
            Self::GtEq => Punct::GtEq,
            Self::And => Punct::And,
            Self::Or => Punct::Or,
            Self::BitAnd => Punct::Ampersand,
            Self::BitOr => Punct::Pipe,
            Self::BitXor => Punct::Caret,
            Self::Shl => Punct::Shl,
            Self::Shr => Punct::Shr,
        }
    }

    #[inline]
    pub const fn from_compound_assignment(punct: Punct) -> Option<Self> {
        let operator = match punct {
            Punct::PlusEq => Self::Add,
            Punct::MinusEq => Self::Sub,
            Punct::StarEq => Self::Mul,
            Punct::SlashEq => Self::Div,
            Punct::AmpersandEq => Self::BitAnd,
            Punct::PipeEq => Self::BitOr,
            Punct::CaretEq => Self::BitXor,
            Punct::ShlEq => Self::Shl,
            Punct::ShrEq => Self::Shr,
            _ => return None,
        };

        Some(operator)
    }

    pub const fn from_punct(punct: Punct) -> Option<Self> {
        let operator = match punct {
            Punct::Plus => Self::Add,
            Punct::Minus => Self::Sub,
            Punct::Star => Self::Mul,
            Punct::Slash => Self::Div,
            Punct::EqEq => Self::Eq,
            Punct::BangEq => Self::Ne,
            Punct::Lt => Self::Lt,
            Punct::LtEq => Self::LtEq,
            Punct::Gt => Self::Gt,
            Punct::GtEq => Self::GtEq,
            Punct::And => Self::And,
            Punct::Or => Self::Or,
            Punct::Ampersand => Self::BitAnd,
            Punct::Pipe => Self::BitOr,
            Punct::Caret => Self::BitXor,
            Punct::Shl => Self::Shl,
            Punct::Shr => Self::Shr,
            _ => return None,
        };

        Some(operator)
    }

    #[inline]
    pub const fn precedence(self) -> Precedence {
        match self {
            Self::Or => Precedence::Or,
            Self::And => Precedence::And,
            Self::Eq | Self::Ne => Precedence::Equality,
            Self::Lt | Self::LtEq | Self::Gt | Self::GtEq => Precedence::Comparison,
            Self::BitOr => Precedence::BitOr,
            Self::BitXor => Precedence::BitXor,
            Self::BitAnd => Precedence::BitAnd,
            Self::Shl | Self::Shr => Precedence::Shift,
            Self::Add | Self::Sub => Precedence::Sum,
            Self::Mul | Self::Div => Precedence::Product,
        }
    }

    #[inline]
    pub const fn as_str<'s>(self) -> &'s str {
        self.punct().as_str()
    }
}

#[inline]
const fn is_place(expr: &Expression<'_>) -> bool {
    matches!(
        expr,
        Expression::Identifier { .. }
            | Expression::Field { .. }
            | Expression::Index { .. }
            | Expression::Unary { operator: UnaryOperator::Deref, .. }
    )
}
