use crate::lexer::Spanned;
use crate::lexer::token::{Keyword, Punct, Span, TokenKind};
use crate::parser::error::{ParseErrorKind, ParserError};
use crate::parser::expression::Expression;
use crate::parser::{Parsable, Parser};
use std::num::NonZero;

#[derive(Debug, PartialEq, Clone)]
pub enum Statement<'i> {
    Let(Let<'i>),
    Const(Const<'i>),
    Return(Return<'i>),
    If(If<'i>),
    While(While<'i>),
    Fn(Function<'i>),
    Struct(Struct<'i>),
    Enum(Enum<'i>),
    Impl(Impl<'i>),
    Interface(Interface<'i>),
    Expr(Expression<'i>, Span),
    Block(Block<'i>),
    Use(UseDecl<'i>),
    Match(Match<'i>),
}

#[derive(Debug, PartialEq, Clone)]
pub struct Let<'i> {
    pub mutable: bool,
    pub name: &'i str,
    pub name_span: Span,
    pub typ: Option<Spanned<Type<'i>>>,
    pub value: Option<Expression<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Const<'i> {
    pub is_pub: bool,
    pub name: &'i str,
    pub typ: Spanned<Type<'i>>,
    pub value: Expression<'i>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Return<'i> {
    pub value: Option<Expression<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct If<'i> {
    pub condition: Expression<'i>,
    pub then_branch: Block<'i>,
    pub else_branch: Option<Box<Else<'i>>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct While<'i> {
    pub condition: Expression<'i>,
    pub body: Block<'i>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Match<'i> {
    pub scrutinee: Expression<'i>,
    pub arms: Vec<MatchArm<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct MatchArm<'i> {
    /// Single pattern; multiple `|` alternatives are wrapped in [`Pattern::Or`].
    pub pattern: Spanned<Pattern<'i>>,
    /// Optional `if <guard>` condition.
    pub guard: Option<Expression<'i>>,
    pub body: Expression<'i>,
    pub span: Span,
}

/// An inline literal value in a pattern position.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PatternLit {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
}

/// A match pattern
///
/// Bare identifiers (`Ident`) are resolved against the
/// scrutinee enum during lowering: a known variant name becomes a
/// fieldless variant match, otherwise it binds the payload
#[derive(Debug, PartialEq, Clone)]
pub enum Pattern<'i> {
    /// `_`
    Wildcard,
    /// An inline literal value, e.g. `42`, `true`, `'x'`
    Literal(PatternLit),
    /// `A | B | C` — or-pattern; alternatives are never empty and never nested
    Or(Vec<Spanned<Pattern<'i>>>),
    /// bare identifier, fieldless variant or a payload binding
    Ident(&'i str),
    /// `Qualifier::Name`, `Name(sub)`, or `Qualifier::Name(sub)`
    Variant {
        qualifier: Option<&'i str>,
        name: &'i str,
        sub: Option<Box<Spanned<Pattern<'i>>>>,
    },
}

#[derive(Debug, PartialEq, Clone)]
pub struct GenericBound<'i> {
    pub name: &'i str,
    pub bounds: Vec<Spanned<Type<'i>>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Function<'i> {
    pub name: &'i str,
    pub generics: Vec<GenericBound<'i>>,
    pub impl_type: Option<&'i str>,
    pub receiver: Option<Receiver>,
    pub params: Vec<Parameter<'i>>,
    pub return_type: Option<Spanned<Type<'i>>>,
    pub body: Block<'i>,
    pub is_const: bool,
    pub is_pub: bool,
    pub inline: bool,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Receiver {
    pub mutable: bool,
    pub by_ref: bool,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Struct<'i> {
    pub name: &'i str,
    pub generics: Vec<GenericBound<'i>>,
    pub fields: Vec<StructField<'i>>,
    pub repr: StructRepr,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct StructRepr {
    pub kind: StructReprKind,
    pub align: Option<NonZero<u32>>,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
#[repr(u8)]
pub enum StructReprKind {
    #[default]
    Default = 1 << 1,
    Extern = 1 << 2,
    Packed = 1 << 3,
}

#[derive(Debug, PartialEq, Clone)]
pub struct StructField<'i> {
    pub name: &'i str,
    pub typ: Spanned<Type<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Enum<'i> {
    pub name: &'i str,
    pub generics: Vec<GenericBound<'i>>,
    pub variants: Vec<EnumVariant<'i>>,
    pub repr: Spanned<Type<'i>>,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct EnumVariant<'i> {
    pub name: &'i str,
    pub payload: Option<Spanned<Type<'i>>>,
    pub value: Option<i64>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Impl<'i> {
    pub name: &'i str,
    pub receiver: Spanned<Type<'i>>,
    pub interface_type: Option<Spanned<Type<'i>>>,
    pub interface: Option<&'i str>,
    pub generics: Vec<GenericBound<'i>>,
    pub methods: Vec<Function<'i>>,
    pub constants: Vec<Const<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Interface<'i> {
    pub name: &'i str,
    pub generics: Vec<GenericBound<'i>>,
    pub superinterfaces: Vec<&'i str>,
    pub methods: Vec<InterfaceMethod<'i>>,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct InterfaceMethod<'i> {
    pub name: &'i str,
    pub generics: Vec<GenericBound<'i>>,
    pub receiver: Option<Receiver>,
    pub params: Vec<Parameter<'i>>,
    pub return_type: Option<Spanned<Type<'i>>>,
    pub body: Option<Block<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Parameter<'i> {
    pub name: &'i str,
    pub mutable: bool,
    pub typ: Spanned<Type<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Block<'i> {
    pub statements: Vec<Statement<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Else<'i> {
    If(If<'i>),
    Block(Block<'i>),
    Expr(Expression<'i>),
}

#[derive(Debug, PartialEq, Clone)]
pub struct UseDecl<'i> {
    pub path: UsePath<'i>,
    pub items: UseItems<'i>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct UsePath<'i> {
    pub segments: Vec<&'i str>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct UseItem<'i> {
    pub name: &'i str,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub enum UseItems<'i> {
    Namespace,
    Named(Vec<UseItem<'i>>),
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[non_exhaustive]
#[rustfmt::skip]
#[allow(clippy::enum_variant_names)]
pub enum Type<'i> {
    I8, U8, I16, U16,
    I32, U32, I64, U64,
    F32, F64,
    Bool,
    /// pointer-sized signed integer
    Uptr,
    /// pointer-sized unsigned integer
    Iptr,
    /// 32-bit unicode codepoint
    Char,
    /// borrowed string slice
    Str,
    /// owned heap string
    String,
    Named(&'i str),
    SelfType, RefSelf,
    Ref(Box<Type<'i>>),
    Generic(&'i str, Vec<Spanned<Type<'i>>>),
    #[allow(dead_code)]
    Unit,
    Never,
}

impl<'i> Parsable<'i> for Statement<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let (kind, is_fn_start) = match parser.peek() {
            Some(Ok(token)) => (token.kind, token.is_fn_start()),
            _ => {
                return Err(ParserError::new(ParseErrorKind::UnexpectedEof, Span::default()));
            },
        };

        if parser.is_const_decl() {
            return Ok(Statement::Const(parser.parse_node()?));
        }

        match kind {
            TokenKind::Keyword(Keyword::Let) => Ok(Statement::Let(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::If) => Ok(Statement::If(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Match) => Ok(Statement::Match(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::While) => Ok(Statement::While(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Return) => Ok(Statement::Return(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Use) => Ok(Statement::Use(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Struct) => Ok(Statement::Struct(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Enum) => Ok(Statement::Enum(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Impl) => Ok(Statement::Impl(parser.parse_node()?)),
            TokenKind::Punct(Punct::OpenBrace) => Ok(Statement::Block(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Interface) => {
                Ok(Statement::Interface(parser.parse_node()?))
            },

            TokenKind::Keyword(Keyword::Pub) => {
                let next_token = match parser.peek_nth(1) {
                    Some(Ok(t)) => t,
                    Some(Err(e)) => return Err((&e).into()),
                    None => {
                        return Err(ParserError::new(
                            ParseErrorKind::UnexpectedEof,
                            Span::default(),
                        ));
                    },
                };

                Ok(match next_token.kind {
                    TokenKind::Keyword(Keyword::Struct) => Statement::Struct(parser.parse_node()?),
                    TokenKind::Keyword(Keyword::Enum) => Statement::Enum(parser.parse_node()?),
                    TokenKind::Keyword(Keyword::Interface) => {
                        Statement::Interface(parser.parse_node()?)
                    },
                    _ if next_token.is_fn_start() => Statement::Fn(parser.parse_node()?),
                    found_kind => {
                        return Err(ParserError::new(
                            ParseErrorKind::Expected {
                                expected: TokenKind::Keyword(Keyword::Fn),
                                found: found_kind,
                            },
                            next_token.span,
                        ));
                    },
                })
            },

            TokenKind::Keyword(_) if is_fn_start => Ok(Statement::Fn(parser.parse_node()?)),
            TokenKind::Eof => Err(ParserError::new(ParseErrorKind::UnexpectedEof, Span::default())),
            _ => {
                let expr = parser.parse_node::<Expression>()?;
                let end_position = match parser.peek() {
                    Some(Ok(t)) if t.is_kind(Punct::CloseBrace) | t.is_kind(TokenKind::Eof) => {
                        expr.span().end
                    },
                    Some(Err(err)) => return Err(err.into()),
                    _ => {
                        parser.expect_token(Punct::Semicolon)?;
                        expr.span().end
                    },
                };

                let span = Span::new(expr.span().start, end_position);

                Ok(Statement::Expr(expr, span))
            },
        }
    }
}

impl<'i> Spanned<Type<'i>> {
    pub fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        if parser.consume_token(Punct::Ampersand)? {
            let start = parser.last_span().unwrap_or_default();
            let inner = parser.parse_node::<Spanned<Type<'i>>>()?;
            let span = start + inner.span();
            let kind = match inner.value() {
                Type::Str => Type::Str,
                Type::SelfType => Type::RefSelf,
                other => Type::Ref(Box::new(other)),
            };
            return Ok(Self::new(kind, span));
        }

        if parser.consume_token(Punct::Bang)? {
            let span = parser.last_span().unwrap_or_default();
            return Ok(Self::new(Type::Never, span));
        }

        let (name, span) = parser.expect_identifier()?;

        let mut generic_args = Vec::new();
        let mut type_span = span;
        if parser.consume_token(Punct::Lt)? {
            loop {
                let arg = parser.parse_node::<Spanned<Type<'i>>>()?;
                generic_args.push(arg);
                if parser.consume_generic_close()? {
                    break;
                }
                parser.expect_token(Punct::Comma)?;
                if parser.consume_generic_close()? {
                    break;
                }
            }

            type_span = span + parser.last_span().unwrap_or(span);
        }

        let value = if !generic_args.is_empty() {
            Type::Generic(name, generic_args)
        } else {
            Type::from_str(name).unwrap_or(Type::Named(name))
        };

        Ok(Self::new(value, type_span))
    }
}

impl<'i> Parsable<'i> for Let<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let let_token = parser.expect_token(Keyword::Let)?;
        let mutable = parser.consume_token(Keyword::Mut)?;
        let (name, name_span) = parser.expect_identifier()?;

        let typ = match parser.consume_token(Punct::Colon)? {
            true => Some(parser.parse_node::<Spanned<Type>>()?),
            false => None,
        };

        let value = match parser.consume_token(Punct::Eq)? {
            true => Some(parser.parse_node::<Expression>()?),
            false => None,
        };

        let semicolon = parser.expect_token(Punct::Semicolon)?;
        let span = let_token.span + semicolon.span;

        Ok(Let { mutable, name, typ, value, span, name_span })
    }
}

impl<'i> Parsable<'i> for Const<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let start_span = match parser.peek() {
            Some(Ok(token)) => token.span,
            Some(Err(err)) => return Err(err.into()),
            None => {
                return Err(ParserError::new(ParseErrorKind::UnexpectedEof, Span::default()));
            },
        };

        let is_pub = parser.consume_token(Keyword::Pub)?;
        let _const_token = parser.expect_token(Keyword::Const)?;
        let (name, _) = parser.expect_identifier()?;
        parser.expect_token(Punct::Colon)?;
        let typ = parser.parse_node::<Spanned<Type<'i>>>()?;
        parser.expect_token(Punct::Eq)?;
        let value = parser.parse_node::<Expression<'i>>()?;
        let semi = parser.expect_token(Punct::Semicolon)?;
        let span = start_span + semi.span;

        Ok(Const { is_pub, name, typ, value, span })
    }
}

impl<'i> Parsable<'i> for Return<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let return_token = parser.expect_token(Keyword::Return)?;

        let mut value = None;
        if let Some(Ok(token)) = parser.peek()
            && !token.is_kind(Punct::Semicolon)
        {
            value = Some(Expression::parse(parser)?);
        }
        let semi_token = parser.expect_token(Punct::Semicolon)?;
        let span = return_token.span + semi_token.span;

        Ok(Return { value, span })
    }
}

impl<'i> Parsable<'i> for If<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let if_token = parser.expect_token(Keyword::If)?;

        let condition = Expression::parse(parser)?;
        let has_block = matches!(parser.peek(), Some(Ok(token)) if token.is_kind(Punct::OpenBrace));

        let (then_branch, then_end) = match has_block {
            true => {
                let block = Block::parse(parser)?;
                let end = block.span.end;

                (block, end)
            },
            false => {
                let (statement, end) = match parser.peek() {
                    Some(Ok(token)) if token.is_kind(Keyword::Return) => {
                        let ret = Return::parse(parser)?;
                        let end = ret.span.end;

                        Ok((Statement::Return(ret), end))
                    },

                    Some(Ok(_)) => {
                        let expr = Expression::parse(parser)?;
                        let semi = parser.expect_token(Punct::Semicolon)?;
                        let span = expr.span() + semi.span;

                        Ok((Statement::Expr(expr, span), semi.span.end))
                    },

                    Some(Err(err)) => Err(err.into()),

                    _ => Err(ParserError::new(ParseErrorKind::UnexpectedEof, if_token.span)),
                }?;

                let span = Span::new(if_token.span.start, end);
                let block = Block { span, statements: vec![statement] };

                (block, span.end)
            },
        };

        let mut else_branch = None;
        let mut end_pos = then_end;

        if parser.consume_optional(TokenKind::Keyword(Keyword::Else)) {
            let Some(Ok(next_token)) = parser.peek() else {
                return Err(ParserError::new(ParseErrorKind::UnexpectedEof, Span::default()));
            };

            match next_token.kind {
                TokenKind::Keyword(Keyword::If) => {
                    let else_if = If::parse(parser)?;
                    end_pos = else_if.span.end;
                    else_branch = Some(Box::new(Else::If(else_if)));
                },

                TokenKind::Punct(Punct::OpenBrace) => {
                    let else_block = Block::parse(parser)?;
                    end_pos = else_block.span.end;
                    else_branch = Some(Box::new(Else::Block(else_block)));
                },

                // brace-less `else return x;`
                TokenKind::Keyword(Keyword::Return) => {
                    let ret = Return::parse(parser)?;
                    end_pos = ret.span.end;
                    let block = Block { span: ret.span, statements: vec![Statement::Return(ret)] };
                    else_branch = Some(Box::new(Else::Block(block)));
                },

                _ => {
                    let expr = Expression::parse(parser)?;
                    let semi = parser.expect_token(Punct::Semicolon)?;

                    end_pos = semi.span.end;
                    else_branch = Some(Box::new(Else::Expr(expr)));
                },
            }
        }

        let span = Span::new(if_token.span.start, end_pos);
        Ok(If { condition, then_branch, else_branch, span })
    }
}

impl<'i> Parsable<'i> for While<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let while_token = parser.expect_token(Keyword::While)?;
        let condition = Expression::parse(parser)?;
        let body = Block::parse(parser)?;
        let span = while_token.span + body.span;

        Ok(While { condition, body, span })
    }
}

impl<'i> Parsable<'i> for Match<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let match_token = parser.expect_token(Keyword::Match)?;
        let scrutinee = Expression::parse(parser)?;
        parser.expect_token(Punct::OpenBrace)?;

        let mut arms = Vec::new();
        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => break,
                Some(Err(err)) => return Err(err.into()),
                None => {
                    return Err(ParserError::new(ParseErrorKind::UnexpectedEof, match_token.span));
                },
                _ => {},
            }

            let first: Spanned<Pattern> = parser.parse_node()?;
            let pattern = if parser.consume_token(Punct::Pipe)? {
                let mut alts = vec![first];

                while {
                    alts.push(parser.parse_node()?);
                    parser.consume_token(Punct::Pipe)?
                } {}

                let span = alts.first().unwrap().span() + alts.last().unwrap().span();
                Spanned::new(Pattern::Or(alts), span)
            } else {
                first
            };

            let guard = parser
                .consume_token(Keyword::If)?
                .then(|| Expression::parse(parser))
                .transpose()?;

            parser.expect_token(Punct::Arrow)?;
            let body = Expression::parse(parser)?;
            let span = body.span();
            arms.push(MatchArm { pattern, guard, body, span });

            // arms are comma-separated, with an optional trailing comma
            if !parser.consume_token(Punct::Comma)? {
                break;
            }
        }

        let close = parser.expect_token(Punct::CloseBrace)?;
        Ok(Match { scrutinee, arms, span: match_token.span + close.span })
    }
}

impl<'i> Parsable<'i> for Pattern<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Pattern<'i>, ParserError<'i>> {
        // Literal patterns: integers, floats, bools, chars.
        if let Some(Ok(token)) = parser.peek() {
            let lit = match token.kind {
                TokenKind::Integer(n) => Some(PatternLit::Int(n)),
                TokenKind::Float(f) => Some(PatternLit::Float(f)),
                TokenKind::Bool(b) => Some(PatternLit::Bool(b)),
                TokenKind::Char(c) => Some(PatternLit::Char(c)),
                _ => None,
            };
            if let Some(lit) = lit {
                parser.expect_next()?;
                return Ok(Pattern::Literal(lit));
            }
        }

        let pattern_payload = |parser: &mut Parser<'i>| -> Result<_, ParserError<'i>> {
            if !parser.consume_token(Punct::OpenParen)? {
                return Ok(None);
            }

            let pattern: Spanned<Pattern> = parser.parse_node()?;
            parser.expect_token(Punct::CloseParen)?;
            Ok(Some(Box::new(pattern)))
        };

        let (ident, _) = parser.expect_identifier()?;

        // `Qualifier::Name` (optionally `Qualifier::Name(sub)`)
        if parser.consume_token(Punct::ColonColon)? {
            let (name, _) = parser.expect_identifier()?;
            let sub = pattern_payload(parser)?;
            return Ok(Pattern::Variant { qualifier: Some(ident), name, sub });
        }

        // `Name(sub)`
        if let Some(sub) = pattern_payload(parser)? {
            return Ok(Pattern::Variant { qualifier: None, name: ident, sub: Some(sub) });
        }

        Ok(match ident {
            "_" => Pattern::Wildcard,
            _ => Pattern::Ident(ident),
        })
    }
}

impl<'i> Parsable<'i> for Spanned<Pattern<'i>> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let start = parser
            .peek()
            .map(|t| t.as_ref().map(|tok| tok.span).unwrap_or_default())
            .unwrap_or_default();
        let value = Pattern::parse(parser)?;
        let end = parser.last_span().unwrap_or(start);
        Ok(Spanned::new(value, start + end))
    }
}

impl<'i> Parsable<'i> for Function<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let is_pub = parser.consume_token(Keyword::Pub)?;
        let inline = parser.consume_token(Keyword::Inline)?;
        let is_const = parser.consume_token(Keyword::Const)?;

        let fn_token = parser.expect_token(Keyword::Fn)?;
        let (name, _) = parser.expect_identifier()?;

        let mut generics = parse_generics::<GenericBound>(parser)?;

        parser.expect_token(Punct::OpenParen)?;

        let mut params = Vec::new();
        let mut receiver = None;

        loop {
            let token = parser
                .peek()
                .ok_or_else(|| ParserError::new(ParseErrorKind::UnexpectedEof, fn_token.span))?;

            match token {
                Ok(token) if token.is_kind(Punct::CloseParen) => {
                    parser.expect_token(Punct::CloseParen)?;
                    break;
                },

                Ok(_) => {
                    if !params.is_empty() || receiver.is_some() {
                        parser.expect_token(Punct::Comma)?;
                    }

                    if params.is_empty() && receiver.is_none() {
                        if parser.consume_token(Punct::Ampersand)? {
                            let amp_span = parser.last_span().unwrap_or_default();
                            receiver = Some(Receiver::parse_after_amp(parser, amp_span)?);
                            continue;
                        }
                        if peek_is_self(parser) {
                            receiver = Some(Receiver::parse_by_value(parser)?);
                            continue;
                        }
                    }

                    params.push(parser.parse_node()?);
                },

                Err(err) => return Err(err.into()),
            }
        }

        let return_type = match parser.consume_token(Punct::Colon)? {
            true => Some(parser.parse_node()?),
            false => None,
        };
        parse_where_clause(parser, &mut generics)?;
        let body = Block::parse(parser)?;
        let span = fn_token.span + body.span;

        Ok(Function {
            name,
            generics,
            impl_type: None,
            receiver,
            params,
            return_type,
            body,
            span,
            is_const,
            is_pub,
            inline,
        })
    }
}

impl<'i> Parsable<'i> for Impl<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let impl_token = parser.expect_token(Keyword::Impl)?;

        let receiver = Spanned::<Type>::parse(parser)?;
        let generics = Vec::new();
        let name = receiver.value().name().ok_or_else(|| {
            ParserError::new(
                ParseErrorKind::ExpectedTypeIdentifier { found: format!("{:?}", receiver.value()) },
                receiver.span(),
            )
        })?;

        let mut interface_type = None;
        let mut interface = None;
        if parser.consume_token(Keyword::With)? {
            let parsed_interface = Spanned::<Type>::parse(parser)?;
            let interface_name = parsed_interface.value().name().ok_or_else(|| {
                ParserError::new(
                    ParseErrorKind::ExpectedTypeIdentifier {
                        found: format!("{:?}", parsed_interface.value()),
                    },
                    parsed_interface.span(),
                )
            })?;
            interface_type = Some(parsed_interface);
            interface = Some(interface_name);
        }

        parser.expect_token(Punct::OpenBrace)?;

        let mut methods = Vec::new();
        let mut constants = Vec::new();

        loop {
            match parser.peek_nth(0) {
                Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                    let close = parser.expect_token(Punct::CloseBrace)?;
                    let span = impl_token.span + close.span;

                    return Ok(Self {
                        name,
                        receiver,
                        interface_type,
                        interface,
                        generics,
                        methods,
                        constants,
                        span,
                    });
                },

                Some(Ok(token)) if token.is_kind(TokenKind::Eof) => {
                    return Err(ParserError::new(ParseErrorKind::UnexpectedEof, token.span));
                },

                Some(Ok(_)) if parser.is_const_decl() => {
                    let constant = parser.parse_node::<Const>()?;
                    constants.push(constant);
                },

                Some(Ok(token)) if token.is_fn_start() => {
                    let mut method = parser.parse_node::<Function>()?;
                    method.impl_type = Some(name);
                    methods.push(method);
                },

                Some(Ok(token)) => {
                    return Err(ParserError::new(
                        ParseErrorKind::Expected {
                            expected: TokenKind::Keyword(Keyword::Fn),
                            found: token.kind,
                        },
                        token.span,
                    ));
                },

                Some(Err(err)) => return Err((&err).into()),
                None => {
                    return Err(ParserError::new(ParseErrorKind::UnexpectedEof, impl_token.span));
                },
            }
        }
    }
}

impl<'i> Impl<'i> {
    /// injects an interface's default method implementation into this `impl` block
    /// if the block hasn't overridden them
    pub fn inject_default_methods(&mut self, interface: &Interface<'i>) {
        let default_methods: Vec<_> = interface
            .methods
            .iter()
            .filter(|m| !self.methods.iter().any(|existing| existing.name == m.name))
            .filter_map(|m| {
                m.body.as_ref().map(|body| Function {
                    name: m.name,
                    generics: Vec::new(),
                    impl_type: Some(self.name),
                    receiver: m.receiver,
                    params: m.params.clone(),
                    return_type: m.return_type.clone(),
                    body: body.clone(),
                    is_const: false,
                    is_pub: false,
                    inline: false,
                    span: m.span,
                })
            })
            .collect();

        self.methods.extend(default_methods);
    }
}

impl<'i> Parsable<'i> for Struct<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let is_pub = parser.consume_token(Keyword::Pub)?;
        let struct_token = parser.expect_token(Keyword::Struct)?;
        let (name, _) = parser.expect_identifier()?;

        let generics = parse_generics::<GenericBound>(parser)?;

        parser.expect_token(Punct::OpenBrace)?;

        let mut fields = Vec::new();

        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                    let close = parser.expect_token(Punct::CloseBrace)?;
                    let repr = parser.parse_node()?;
                    let span = struct_token.span + parser.last_span().unwrap_or(close.span);

                    return Ok(Self { name, generics, fields, repr, is_pub, span });
                },

                Some(Ok(token)) if token.is_kind(TokenKind::Eof) => {
                    return Err(ParserError::new(ParseErrorKind::UnexpectedEof, token.span));
                },

                Some(Err(err)) => return Err(err.into()),
                _ => {},
            }

            if !fields.is_empty() {
                parser.expect_token(Punct::Comma)?;

                match parser.peek() {
                    Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                        continue;
                    },
                    _ => {},
                }
            }

            let (field_name, field_span) = parser.expect_identifier()?;
            parser.expect_token(Punct::Colon)?;
            let typ = parser.parse_node::<Spanned<Type<'i>>>()?;
            let span = field_span + typ.span();

            fields.push(StructField { name: field_name, typ, span });
        }
    }
}

impl<'i> Parsable<'i> for Enum<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let is_pub = parser.consume_token(Keyword::Pub)?;
        let enum_token = parser.expect_token(Keyword::Enum)?;
        let (name, _) = parser.expect_identifier()?;
        let generics = parse_generics::<GenericBound>(parser)?;
        parser.expect_token(Punct::OpenBrace)?;

        let mut variants = Vec::new();

        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                    parser.expect_token(Punct::CloseBrace)?;
                    break;
                },

                Some(Ok(token)) if token.is_kind(TokenKind::Eof) => {
                    return Err(ParserError::new(ParseErrorKind::UnexpectedEof, token.span));
                },

                Some(Err(err)) => return Err(err.into()),
                _ => {},
            }

            if !variants.is_empty() {
                parser.expect_token(Punct::Comma)?;

                match parser.peek() {
                    Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                        parser.expect_token(Punct::CloseBrace)?;
                        break;
                    },
                    _ => {},
                }
            }

            let (variant_name, variant_span) = parser.expect_identifier()?;

            let mut payload = None;
            if parser.consume_token(Punct::OpenParen)? {
                payload = Some(parser.parse_node::<Spanned<Type<'i>>>()?);
                parser.expect_token(Punct::CloseParen)?;
            }

            let (value, span) = match parser.consume_token(Punct::Eq)? {
                true => {
                    let negative = parser.consume_token(Punct::Minus)?;
                    let token = parser.expect_next()?;
                    match token.kind {
                        TokenKind::Integer(value) => {
                            let value = if negative {
                                -value
                            } else {
                                value
                            };
                            (Some(value), variant_span + token.span)
                        },
                        _ => {
                            return Err(ParserError::new(
                                ParseErrorKind::Expected {
                                    expected: TokenKind::Integer(0),
                                    found: token.kind,
                                },
                                token.span,
                            ));
                        },
                    }
                },
                false => (None, variant_span),
            };

            variants.push(EnumVariant { name: variant_name, payload, value, span });
        }

        let repr = parser
            .consume_token(Keyword::As)?
            .then(|| parser.parse_node())
            .transpose()?
            .unwrap_or_else(|| Spanned::new(Type::I32, enum_token.span));
        let span = enum_token.span + repr.span();

        Ok(Self { name, generics, variants, repr, is_pub, span })
    }
}

impl<'i> Parsable<'i> for Interface<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let is_pub = parser.consume_token(Keyword::Pub)?;
        let interface_token = parser.expect_token(Keyword::Interface)?;
        let (name, _) = parser.expect_identifier()?;

        let generics = parse_generics::<GenericBound>(parser)?;

        let mut superinterfaces = Vec::new();
        if parser.consume_token(Punct::Colon)? {
            loop {
                // parse as a full type so generic args (`PartialEq<Rhs>`) are consumed,
                // then keep only the bare interface name
                let bound = Spanned::<Type>::parse(parser)?;
                let name = match bound.value() {
                    Type::Named(name) | Type::Generic(name, _) => name,
                    _ => {
                        return Err(ParserError::new(
                            ParseErrorKind::ExpectedIdentifier {
                                found: TokenKind::Punct(Punct::Colon),
                            },
                            bound.span(),
                        ));
                    },
                };
                superinterfaces.push(name);

                if !parser.consume_token(Punct::Plus)? {
                    break;
                }
            }
        }

        parser.expect_token(Punct::OpenBrace)?;

        let mut methods = Vec::new();

        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                    let close = parser.expect_token(Punct::CloseBrace)?;
                    return Ok(Self {
                        name,
                        generics,
                        superinterfaces,
                        span: interface_token.span + close.span,
                        methods,
                        is_pub,
                    });
                },
                Some(Err(err)) => return Err(err.into()),
                _ => methods.push(InterfaceMethod::parse(parser)?),
            }
        }
    }
}

impl<'i> Parsable<'i> for InterfaceMethod<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        // accept (and ignore) `inline`/`const` modifiers on interface methods
        let _inline = parser.consume_token(Keyword::Inline)?;
        let _is_const = parser.consume_token(Keyword::Const)?;
        let fn_token = parser.expect_token(Keyword::Fn)?;
        let (name, _) = parser.expect_identifier()?;

        let mut generics = parse_generics::<GenericBound>(parser)?;

        parser.expect_token(Punct::OpenParen)?;

        let mut params = Vec::new();
        let mut receiver = None;

        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseParen) => {
                    parser.expect_token(Punct::CloseParen)?;
                    break;
                },

                Some(Ok(_)) => {
                    if !params.is_empty() || receiver.is_some() {
                        parser.expect_token(Punct::Comma)?;
                    }

                    if params.is_empty() && receiver.is_none() {
                        if parser.consume_token(Punct::Ampersand)? {
                            let amp_span = parser.last_span().unwrap_or_default();
                            receiver = Some(Receiver::parse_after_amp(parser, amp_span)?);
                            continue;
                        }
                        if peek_is_self(parser) {
                            receiver = Some(Receiver::parse_by_value(parser)?);
                            continue;
                        }
                    }

                    params.push(parser.parse_node()?);
                },

                Some(Err(err)) => return Err(err.into()),

                None => {
                    return Err(ParserError::new(ParseErrorKind::UnexpectedEof, fn_token.span));
                },
            }
        }

        let return_type =
            parser.consume_token(Punct::Colon)?.then(|| parser.parse_node()).transpose()?;
        parse_where_clause(parser, &mut generics)?;

        let (body, span) = match parser.consume_token(Punct::Semicolon)? {
            true => (None, fn_token.span + parser.last_span().unwrap_or_default()),
            _ => {
                let b = parser.parse_node::<Block>()?;
                let b_span = b.span;
                (Some(b), fn_token.span + b_span)
            },
        };

        Ok(Self { span, name, generics, receiver, params, return_type, body })
    }
}

impl<'i> Parsable<'i> for Block<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let open_brace = parser.expect_token(Punct::OpenBrace)?;
        let mut statements = Vec::new();

        let close_brace = loop {
            let token = parser
                .peek()
                .and_then(|r| r.as_ref().ok())
                .ok_or_else(|| ParserError::new(ParseErrorKind::UnexpectedEof, open_brace.span))?;

            if token.is_kind(Punct::CloseBrace) {
                let close = parser.expect_token(Punct::CloseBrace)?;
                break close;
            }

            if token.is_kind(TokenKind::Eof) {
                return Err(ParserError::new(ParseErrorKind::UnexpectedEof, token.span));
            }

            statements.push(parser.parse_node::<Statement>()?);
        };

        let span = open_brace.span + close_brace.span;
        Ok(Self { span, statements })
    }
}

impl<'i> Parsable<'i> for UseDecl<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let use_token = parser.expect_token(Keyword::Use)?;
        let (first, _) = parser.expect_identifier()?;
        let mut segments = vec![first];

        // consume '::segment' pairs until we hit '{' or ';'
        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::ColonColon) => {
                    parser.expect_token(Punct::ColonColon)?;
                },
                _ => break,
            }

            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::OpenBrace) => break,
                _ => {},
            }

            let (segment, _) = parser.expect_identifier()?;
            segments.push(segment)
        }

        let items = match parser.peek() {
            Some(Ok(token)) if token.is_kind(Punct::OpenBrace) => {
                parser.expect_token(Punct::OpenBrace)?;
                let mut names = Vec::new();
                let mut first = true;

                loop {
                    match parser.peek() {
                        Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                            parser.expect_token(Punct::CloseBrace)?;
                            break;
                        },

                        _ => {},
                    }

                    if !first {
                        parser.expect_token(Punct::Comma)?;
                    }
                    first = false;

                    // trailing comma: '{a, b,}'
                    match parser.peek() {
                        Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                            parser.expect_token(Punct::CloseBrace)?;
                            break;
                        },
                        _ => {},
                    }

                    let (name, span) = parser.expect_identifier()?;
                    names.push(UseItem { name, span })
                }

                UseItems::Named(names)
            },

            _ => UseItems::Namespace,
        };

        let semi = parser.expect_token(Punct::Semicolon)?;
        let span = use_token.span + semi.span;

        Ok(UseDecl { path: UsePath { segments }, items, span })
    }
}

impl Receiver {
    fn parse_after_amp<'i>(parser: &mut Parser<'i>, start: Span) -> Result<Self, ParserError<'i>> {
        let mutable = parser.consume_token(Keyword::Mut)?;
        let (name, span) = parser.expect_identifier()?;

        if name != "self" {
            return Err(ParserError::new(
                ParseErrorKind::ExpectedIdentifier { found: TokenKind::Identifier(name) },
                span,
            ));
        }

        Ok(Self { mutable, by_ref: true, span: start + span })
    }

    fn parse_by_value<'i>(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let start = parser.last_span().unwrap_or_default();
        let mutable = parser.consume_token(Keyword::Mut)?;
        let (name, span) = parser.expect_identifier()?;

        if name != "self" {
            return Err(ParserError::new(
                ParseErrorKind::ExpectedIdentifier { found: TokenKind::Identifier(name) },
                span,
            ));
        }

        Ok(Self { mutable, by_ref: false, span: start + span })
    }
}

impl<'i> Parsable<'i> for Parameter<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let mutable = parser.consume_token(Keyword::Mut)?;
        let (name, span) = parser.expect_identifier()?;
        parser.expect_token(Punct::Colon)?;
        let typ = parser.parse_node::<Spanned<Type>>()?;
        let typ_span = typ.span();

        Ok(Self { mutable, name, typ, span: span + typ_span })
    }
}

impl<'i> Parsable<'i> for StructRepr {
    fn parse(parser: &mut Parser<'i>) -> Result<StructRepr, ParserError<'i>> {
        if !parser.consume_token(Keyword::As)? {
            return Ok(StructRepr::default());
        }

        let mut repr = StructRepr::default();
        let mut first = true;

        loop {
            if !first {
                parser.expect_token(Punct::Comma)?;
            }
            first = false;

            let (name, span) = parser.expect_identifier()?;
            match name {
                "extern" => repr.kind = StructReprKind::Extern,
                "packed" => repr.kind = StructReprKind::Packed,
                "align" => {
                    parser.expect_token(Punct::OpenParen)?;
                    let token = parser.expect_next()?;
                    let value = match token.kind {
                        TokenKind::Integer(value) => value as u32,
                        _ => {
                            return Err(ParserError::new(
                                ParseErrorKind::Expected {
                                    expected: TokenKind::Integer(1),
                                    found: token.kind,
                                },
                                token.span,
                            ));
                        },
                    };
                    parser.expect_token(Punct::CloseParen)?;
                    repr.align = NonZero::new(value);
                },
                _ => {
                    return Err(ParserError::new(
                        ParseErrorKind::ExpectedIdentifier { found: TokenKind::Identifier(name) },
                        span,
                    ));
                },
            }

            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::Comma) => continue,
                _ => break,
            }
        }

        Ok(repr)
    }
}

impl<'i> Parsable<'i> for GenericBound<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let (param_name, param_span) = parser.expect_identifier()?;
        let mut bounds = Vec::new();
        let mut bound_span = param_span;
        if parser.consume_token(Punct::Colon)? {
            loop {
                let bound = Spanned::<Type>::parse(parser)?;
                bound_span = bound_span + bound.span();
                bounds.push(bound);
                if !parser.consume_token(Punct::Plus)? {
                    break;
                }
            }
        }
        Ok(GenericBound { name: param_name, bounds, span: bound_span })
    }
}

fn peek_is_self<'i>(parser: &mut Parser<'i>) -> bool {
    let head = match parser.peek_nth(0) {
        Some(Ok(t)) => t,
        _ => return false,
    };

    match head.kind {
        TokenKind::Identifier("self") => true,
        TokenKind::Keyword(Keyword::Mut) => {
            matches!(parser.peek_nth(1), Some(Ok(t)) if t.kind == TokenKind::Identifier("self"))
        },
        _ => false,
    }
}

fn parse_where_clause<'i>(
    parser: &mut Parser<'i>,
    generics: &mut Vec<GenericBound<'i>>,
) -> Result<(), ParserError<'i>> {
    if !parser.consume_token(Keyword::Where)? {
        return Ok(());
    }

    loop {
        let entry = GenericBound::parse(parser)?;
        match generics.iter_mut().find(|g| g.name == entry.name) {
            Some(existing) => {
                existing.span = existing.span + entry.span;
                existing.bounds.extend(entry.bounds);
            },
            None => generics.push(entry),
        }

        if !parser.consume_token(Punct::Comma)? {
            break;
        }

        // accept a trailing comma before the body's `{`
        match parser.peek() {
            Some(Ok(t)) if t.is_kind(Punct::OpenBrace) => break,
            _ => {},
        }
    }

    Ok(())
}

pub(crate) fn parse_generics<'i, T: Parsable<'i>>(
    parser: &mut Parser<'i>,
) -> Result<Vec<T>, ParserError<'i>> {
    let mut items = Vec::new();
    if parser.consume_token(Punct::Lt)? {
        loop {
            items.push(parser.parse_node::<T>()?);
            if parser.consume_generic_close()? {
                break;
            }
            parser.expect_token(Punct::Comma)?;
            if parser.consume_generic_close()? {
                break;
            }
        }
    }
    Ok(items)
}

impl<'i> Parsable<'i> for Spanned<Type<'i>> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        Spanned::<Type>::parse(parser)
    }
}

impl<'s> Statement<'s> {
    pub const fn span(&self) -> Span {
        match self {
            Self::Let(s) => s.span,
            Self::Const(c) => c.span,
            Self::Return(s) => s.span,
            Self::If(s) => s.span,
            Self::While(s) => s.span,
            Self::Fn(s) => s.span,
            Self::Struct(s) => s.span,
            Self::Enum(s) => s.span,
            Self::Impl(s) => s.span,
            Self::Interface(i) => i.span,
            Self::Use(s) => s.span,
            Self::Expr(_, span) => *span,
            Self::Block(b) => b.span,
            Self::Match(m) => m.span,
        }
    }
}

macro_rules! primitive_spellings {
    ($($variant:ident => $spelling:literal),+ $(,)?) => {
        impl<'i> Type<'i> {
            pub fn from_str(name: &'i str) -> Option<Self> {
                Some(match name {
                    $($spelling => Type::$variant,)+
                    _ => return None,
                })
            }

            fn primitive_name<'s>(&self) -> Option<&'s str> {
                Some(match self {
                    $(Type::$variant => $spelling,)+
                    _ => return None,
                })
            }
        }
    };
}

primitive_spellings! {
    I8 => "i8", U8 => "u8", I16 => "i16", U16 => "u16",
    I32 => "i32", U32 => "u32", I64 => "i64", U64 => "u64",
    F32 => "f32", F64 => "f64", Bool => "bool", Char => "char",
    Uptr => "uptr", Iptr => "iptr", Str => "str", String => "String",
    SelfType => "Self",
}

impl<'i> Type<'i> {
    pub fn name(&self) -> Option<&'i str> {
        match self {
            Type::Named(name) | Type::Generic(name, _) => Some(name),
            Type::Ref(inner) => inner.name(),
            Type::RefSelf => Some("Self"),
            Type::Unit => Some("unit"),
            Type::Never => Some("!"),
            other => other.primitive_name(),
        }
    }
}

pub fn inject_default_methods<'a, 'b>(
    statements: &mut [Statement<'a>],
    lookup_interface: impl Fn(&str) -> Option<&'b Interface<'a>>,
) where
    'a: 'b,
{
    for stmt in statements {
        if let Statement::Impl(imp) = stmt
            && let Some(interface) = imp.interface.and_then(&lookup_interface)
        {
            imp.inject_default_methods(interface);
        }
    }
}
