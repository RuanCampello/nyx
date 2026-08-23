use crate::lexer::{
    Lexer, Spanned,
    token::{BytePos, Keyword, Punct, Span, TokenKind},
};
use crate::parser::{
    Parsable, Parser,
    error::{ParseErrorKind, ParserError},
    statement::{self, Block, If, Match, Type},
};
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
    /// `value?`, unwraps a success value or returns the failure early
    Try { value: Box<Expression<'i>>, span: Span },
    /// A `{ … }` block in value position, its value is the tail expression
    Block { block: Block<'i>, span: Span },
    /// An `if`/`else` chain in value position, every branch yields the value
    If { inner: Box<If<'i>>, span: Span },
    /// A `match` in value position, every arm yields the value
    Match { inner: Box<Match<'i>>, span: Span },
    /// A string literal carrying `{...}` interpolations
    /// Plain literals stay [Expression::String], so only strings that interpolate pay for it
    Interpolated { segments: Vec<Segment<'i>>, span: Span },
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
    Add, Sub, Div, Mul, Rem,
    Eq, Ne,
    Lt, LtEq, Gt, GtEq,
    And, Or,
    BitAnd, BitOr, BitXor, Shl,
    Shr,
}

/// One piece of an interpolated string literal
#[derive(Debug, PartialEq, Clone)]
pub enum Segment<'i> {
    /// A run of literal text, exactly as written in the source
    Text(&'i str),
    /// An expression whose value is printed in place of the braces
    Value(Expression<'i>),
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
            | Self::Try { span, .. }
            | Self::Block { span, .. }
            | Self::If { span, .. }
            | Self::Match { span, .. }
            | Self::Interpolated { span, .. }
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

    pub(super) fn parse_prefix(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        use TokenKind as T;
        let token = parser.expect_next()?;

        Ok(match token.kind {
            T::Integer(n) => Expression::Integer(n, token.span),
            T::Float(f) => Expression::Float(f, token.span),
            T::String(s) => match Self::interpolate(s, token.span)? {
                Some(segments) => Expression::Interpolated { segments, span: token.span },
                _ => Expression::String(s, token.span),
            },
            T::Char(c) => Expression::Char(c, token.span),
            T::Bool(b) => Expression::Bool(b, token.span),
            T::Identifier(ident) => match Self::next_is_struct(parser) {
                true => return Self::parse_struct(parser, ident, Vec::new(), token.span),
                _ => Expression::Identifier(ident, token.span),
            },
            T::Punct(Punct::OpenBrace) => {
                parser.push_back(token);
                let block = parser.parse_node::<Block>()?;

                Expression::Block { span: block.span, block }
            },
            T::Keyword(Keyword::If) => {
                parser.push_back(token);
                let inner = parser.parse_node::<If>()?;

                Expression::If { span: inner.span, inner: Box::new(inner) }
            },
            T::Keyword(Keyword::Match) => {
                parser.push_back(token);
                let inner = parser.parse_node::<Match>()?;

                Expression::Match { span: inner.span, inner: Box::new(inner) }
            },
            T::Punct(Punct::Minus)
            | T::Punct(Punct::Bang)
            | T::Punct(Punct::Star)
            | T::Punct(Punct::Ampersand) => {
                let operator = match token.kind {
                    T::Punct(Punct::Minus) => UnaryOperator::Neg,
                    T::Punct(Punct::Bang) => UnaryOperator::Not,
                    T::Punct(Punct::Star) => UnaryOperator::Deref,
                    T::Punct(Punct::Ampersand) => match parser.consume_token(Keyword::Mut)? {
                        true => UnaryOperator::RefMut,
                        false => UnaryOperator::Ref,
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

                Expression::Unary { operator, expr: Box::new(expr), span }
            },
            T::Punct(Punct::OpenParen) => {
                let expr = parser.in_delimiter(|parser| parser.parse_node::<Expression<'i>>())?;
                parser.expect_token(Punct::CloseParen)?;
                expr
            },

            T::Punct(Punct::OpenBracket) => {
                return parser.in_delimiter(|parser| Self::parse_array_literal(parser, token.span));
            },
            _ => {
                parser.push_back(token);
                return Err(ParserError::new(
                    ParseErrorKind::ExpectedExpression { found: token.kind },
                    token.span,
                ));
            },
        })
    }

    /// splits a string literal into literal text and the expressions written between braces
    fn interpolate(
        content: &'i str,
        span: Span,
    ) -> Result<Option<Vec<Segment<'i>>>, ParserError<'i>> {
        if !content.contains('{') {
            return Ok(None);
        }

        // the literal's own span covers the quotes, so the first byte of `content` sits one past its start
        let base = span.start + 1;
        let bytes = content.as_bytes();

        let mut segments = Vec::new();
        let (mut text_start, mut index) = (0, 0);

        let push_text = |segments: &mut Vec<Segment<'i>>, text: &'i str| {
            if !text.is_empty() {
                segments.push(Segment::Text(text));
            }
        };

        while index < bytes.len() {
            use ParseErrorKind as E;

            let doubled = |ch: u8| bytes.get(index + 1) == Some(&ch);
            match bytes[index] {
                ch @ (b'{' | b'}') if doubled(ch) => {
                    push_text(&mut segments, &content[text_start..index]);
                    segments.push(Segment::Text(match ch {
                        b'{' => "{",
                        _ => "}",
                    }));

                    index += 2;
                    text_start = index;
                },

                b'{' => {
                    push_text(&mut segments, &content[text_start..index]);

                    let open = index;
                    let start = index + 1;
                    let end = Self::interpolation_end(content, start, base)?.ok_or_else(|| {
                        let at = base + open as u32;
                        ParserError::new(E::UnterminatedInterpolation, Span::new(at, at + 1))
                    })?;

                    if content[start..end].trim().is_empty() {
                        let at = base + open as u32;
                        return Err(ParserError::new(
                            E::EmptyInterpolation,
                            Span::new(at, base + end as u32 + 1),
                        ));
                    }

                    let mut inner = Parser::with_base(&content[start..end], base + start as u32);
                    segments.push(Segment::Value(inner.parse_node::<Expression>()?));

                    match inner.peek() {
                        Some(Ok(token)) if !token.is_kind(TokenKind::Eof) => {
                            let (found, at) = (token.kind, token.span);
                            return Err(ParserError::new(E::ExpectedExpression { found }, at));
                        },
                        Some(Err(error)) => return Err(error.into()),
                        _ => {},
                    }

                    index = end + 1;
                    text_start = index;
                },

                _ => index += 1,
            }
        }

        push_text(&mut segments, &content[text_start..]);

        Ok(Some(segments))
    }

    /// The offset of the `}` closing an interpolation opened at `start`, or `None` when it is never closed
    fn interpolation_end(
        content: &'i str,
        start: usize,
        base: BytePos,
    ) -> Result<Option<usize>, ParserError<'i>> {
        let (mut lexer, mut depth) = (Lexer::with_base(&content[start..], base + start as u32), 0);

        loop {
            let Some(token) = lexer.next() else {
                return Ok(None);
            };

            let token = token.map_err(|error| ParserError::from(&error))?;
            match token.kind {
                TokenKind::Punct(Punct::CloseBrace) if depth == 0 => {
                    return Ok(Some(token.span.start.offset() - base.offset()));
                },
                TokenKind::Punct(Punct::OpenBrace) => depth += 1,
                TokenKind::Punct(Punct::CloseBrace) => depth -= 1,
                TokenKind::Eof => return Ok(None),
                _ => {},
            }
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
            Punct::Dot | Punct::Question => Precedence::Field,
            _ => Precedence::None,
        };

        precedence.level()
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
        parser.in_delimiter(|parser| Self::call_args_body(parser, fallback_span))
    }

    fn parse_infix(
        parser: &mut Parser<'i>,
        left: Expression<'i>,
        precedence: u8,
    ) -> Result<Self, ParserError<'i>> {
        use Expression::*;
        use ParseErrorKind as E;

        let token = parser.expect_next()?;

        Ok(match token.kind {
            TokenKind::Punct(Punct::Dot) => {
                if let Some(Ok(ahead)) = parser.peek()
                    && !matches!(ahead.kind, TokenKind::Identifier(_))
                {
                    let (kind, span) = (ahead.kind, ahead.span);
                    return Err(ParserError::new(E::ExpectedIdentifier { found: kind }, span));
                }

                let (field, span) = parser.expect_identifier()?;
                Field { span: left.span() + span, expr: Box::new(left), field }
            },
            TokenKind::Punct(Punct::Question) => {
                Try { span: left.span() + token.span, value: Box::new(left) }
            },
            TokenKind::Punct(Punct::ColonColon) => {
                let invalid_expr =
                    || ParserError::new(E::ExpectedExpression { found: token.kind }, token.span);

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
                    return Ok(Call {
                        span: left.span() + end_span,
                        callee: Box::new(left),
                        args,
                        type_args,
                    });
                }

                // path / associated item access (e.g., `std::io::println...`)
                let (path, start_span) = match left {
                    Identifier(name, span) => (vec![name], span),
                    QualifiedName { mut path, name, span } => {
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
                    let type_args = statement::parse_generics::<Spanned<Type>>(parser)?;

                    let (args, end_span) = Self::parse_call_args(parser, start_span)?;
                    return Ok(QualifiedCall {
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
                        let span = start_span + end_span;

                        return Ok(TypeIntrinsic { kind, path: Some(path), typ, span });
                    }

                    let (args, end_span) = Self::parse_call_args(parser, name_span)?;
                    let span = start_span + end_span;
                    return Ok(QualifiedCall { span, path, name, args, type_args: Vec::new() });
                }

                // plain associated path / variable (e.g., `Container::CONSTANT`)
                QualifiedName { span: start_span + name_span, path, name }
            },
            TokenKind::Punct(Punct::OpenParen) => {
                if let Identifier(name, _) = &left
                    && let Ok(kind) = TypeIntrinsicKind::from_str(name)
                {
                    let typ = parser.parse_node::<Spanned<Type>>()?;
                    let end_span = parser.expect_token(Punct::CloseParen)?.span;
                    let span = left.span() + end_span;

                    return Ok(TypeIntrinsic { kind, path: None, typ, span });
                }

                let (args, end_span) =
                    parser.in_delimiter(|parser| Self::call_args_body(parser, token.span))?;
                let span = Span::new(left.span().start, end_span.end);
                Call { callee: Box::new(left), args, type_args: Vec::new(), span }
            },

            TokenKind::Punct(Punct::Eq) => {
                let right = Self::parse_expr(parser, precedence - 1)?;
                let span = left.span() + right.span();

                match is_place(&left) {
                    true => Assignment { target: Box::new(left), value: Box::new(right), span },
                    _ => return Err(ParserError::new(E::InvalidAssignmentTarget, left.span())),
                }
            },

            // `a += b` keeps its operator rather than expanding to `a = a + b`, so the target is evaluated once when it is lowered
            TokenKind::Punct(punct)
                if let Some(operator) = BinaryOperator::from_compound_assignment(punct) =>
            {
                let right = Self::parse_expr(parser, precedence - 1)?;
                let span = left.span() + right.span();
                let value = Box::new(right);

                match is_place(&left) {
                    true => CompoundAssignment { target: Box::new(left), operator, value, span },
                    _ => return Err(ParserError::new(E::InvalidAssignmentTarget, left.span())),
                }
            },

            TokenKind::Punct(Punct::OpenBracket) => {
                let index = parser.in_delimiter(|parser| parser.parse_node::<Expression<'i>>())?;
                let close = parser.expect_token(Punct::CloseBracket)?.span;
                let span = left.span() + close;

                Index { base: Box::new(left), index: Box::new(index), span }
            },

            TokenKind::Keyword(Keyword::As) => {
                let target_type = parser.parse_node::<Spanned<Type>>()?;
                let span = left.span() + target_type.span();

                Cast { expr: Box::new(left), target_type, span }
            },

            _ => {
                let operator = match token.kind {
                    TokenKind::Punct(punct) => BinaryOperator::from_punct(punct),
                    _ => None,
                };

                let Some(operator) = operator else {
                    let (found, span) = (token.kind, token.span);
                    return Err(ParserError::new(E::InvalidBinaryOperator { found }, span));
                };

                let right = Self::parse_expr(parser, precedence)?;
                let span = left.span() + right.span();
                Binary { left: Box::new(left), operator, right: Box::new(right), span }
            },
        })
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
            Self::Rem => Punct::Percent,
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
            Punct::PercentEq => Self::Rem,
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
            Punct::Percent => Self::Rem,
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
            Self::Mul | Self::Div | Self::Rem => Precedence::Product,
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
