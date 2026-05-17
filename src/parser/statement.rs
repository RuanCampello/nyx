use crate::lexer::Spanned;
use crate::lexer::token::{Keyword, Punct, Span, TokenKind};
use crate::parser::error::{ParseErrorKind, ParserError};
use crate::parser::expression::Expression;
use crate::parser::{Parsable, Parser};

#[derive(Debug, PartialEq, Clone)]
pub enum Statement<'i> {
    Let(Let<'i>),
    Return(Return<'i>),
    If(If<'i>),
    While(While<'i>),
    Fn(Function<'i>),
    Struct(Struct<'i>),
    Impl(Impl<'i>),
    Interface(Interface<'i>),
    Expr(Expression<'i>, Span),
    Block(Block<'i>),
    Use(UseDecl<'i>),
}

#[derive(Debug, PartialEq, Clone)]
pub struct Let<'i> {
    pub mutable: bool,
    pub name: &'i str,
    pub typ: Option<Spanned<Type<'i>>>,
    pub value: Option<Expression<'i>>,
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
pub struct Function<'i> {
    pub name: &'i str,
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
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Struct<'i> {
    pub name: &'i str,
    pub fields: Vec<StructField<'i>>,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct StructField<'i> {
    pub name: &'i str,
    pub typ: Spanned<Type<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Impl<'i> {
    pub name: &'i str,
    pub interface: Option<&'i str>,
    pub methods: Vec<Function<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Interface<'i> {
    pub name: &'i str,
    pub methods: Vec<InterfaceMethod<'i>>,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, PartialEq, Clone)]
pub struct InterfaceMethod<'i> {
    name: &'i str,
    receiver: Option<Receiver>,
    params: Vec<Parameter<'i>>,
    return_type: Option<Spanned<Type<'i>>>,
    span: Span,
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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[non_exhaustive]
pub enum Type<'i> {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
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
    #[allow(dead_code)]
    Unit,
}

impl<'i> Parsable<'i> for Statement<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let (kind, is_fn_start) = match parser.peek() {
            Some(Ok(token)) => (token.kind, token.is_fn_start()),
            _ => {
                return Err(ParserError::new(
                    ParseErrorKind::UnexpectedEof,
                    Span::default(),
                ));
            }
        };

        match kind {
            TokenKind::Keyword(Keyword::Let) => Ok(Statement::Let(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::If) => Ok(Statement::If(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::While) => Ok(Statement::While(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Return) => Ok(Statement::Return(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Use) => Ok(Statement::Use(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Struct) => Ok(Statement::Struct(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Impl) => Ok(Statement::Impl(parser.parse_node()?)),
            TokenKind::Punct(Punct::OpenBrace) => Ok(Statement::Block(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Interface) => {
                Ok(Statement::Interface(parser.parse_node()?))
            }
            TokenKind::Keyword(Keyword::Pub) if parser.is_pub_struct() => {
                Ok(Statement::Struct(parser.parse_node()?))
            }
            TokenKind::Keyword(_) if is_fn_start => Ok(Statement::Fn(parser.parse_node()?)),
            TokenKind::Eof => Err(ParserError::new(
                ParseErrorKind::UnexpectedEof,
                Span::default(),
            )),
            _ => {
                let expr = parser.parse_node::<Expression>()?;
                let end_position = match parser.peek() {
                    Some(Ok(token))
                        if token.is_kind(Punct::CloseBrace) | token.is_kind(TokenKind::Eof) =>
                    {
                        expr.span().end
                    }
                    Some(Err(err)) => return Err(err.into()),
                    _ => {
                        parser.expect_token(Punct::Semicolon)?;
                        expr.span().end
                    }
                };

                let span = Span::new(expr.span().start, end_position);

                Ok(Statement::Expr(expr, span))
            }
        }
    }
}

impl<'i> Spanned<Type<'i>> {
    pub fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        if parser.consume_punct(Punct::Ampersand)? {
            let (name, span) = parser.expect_identifier()?;

            return match name {
                "str" => Ok(Self::new(Type::Str, span)),
                _ => Err(ParserError::new(
                    ParseErrorKind::ExpectedTypeIdentifier {
                        found: name.to_string(),
                    },
                    span,
                )),
            };
        };

        let (name, span) = parser.expect_identifier()?;
        let value = match name {
            "i8" => Type::I8,
            "i16" => Type::I16,
            "i32" => Type::I32,
            "i64" => Type::I64,
            "u8" => Type::U8,
            "u16" => Type::U16,
            "u32" => Type::U32,
            "u64" => Type::U64,
            "f32" => Type::F32,
            "f64" => Type::F64,
            "uptr" => Type::Uptr,
            "iptr" => Type::Iptr,
            "bool" => Type::Bool,
            "char" => Type::Char,
            "String" => Type::String,
            _ => Type::Named(name),
        };

        Ok(Self::new(value, span))
    }
}

impl<'i> Parsable<'i> for Let<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let let_token = parser.expect_token(Keyword::Let)?;
        let mutable = parser.consume_keyword(Keyword::Mut)?;
        let (name, _) = parser.expect_identifier()?;

        let typ = match parser.consume_punct(Punct::Colon)? {
            true => Some(parser.parse_node::<Spanned<Type>>()?),
            false => None,
        };

        let value = match parser.consume_punct(Punct::Eq)? {
            true => Some(parser.parse_node::<Expression>()?),
            false => None,
        };

        let semicolon = parser.expect_token(Punct::Semicolon)?;
        let span = let_token.span + semicolon.span;

        Ok(Let {
            mutable,
            name,
            typ,
            value,
            span,
        })
    }
}

impl<'i> Parsable<'i> for Return<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let return_token = parser.expect_token(Keyword::Return)?;

        let mut value = None;
        if let Some(Ok(token)) = parser.peek() {
            if !token.is_kind(Punct::Semicolon) {
                value = Some(Expression::parse(parser)?);
            }
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
            }
            false => {
                let (statement, end) = match parser.peek() {
                    Some(Ok(token)) if token.is_kind(Keyword::Return) => {
                        let ret = Return::parse(parser)?;
                        let end = ret.span.end;

                        Ok((Statement::Return(ret), end))
                    }

                    Some(Ok(_)) => {
                        let expr = Expression::parse(parser)?;
                        let semi = parser.expect_token(Punct::Semicolon)?;
                        let span = expr.span() + semi.span;

                        Ok((Statement::Expr(expr, span), semi.span.end))
                    }

                    Some(Err(err)) => Err(err.into()),

                    _ => Err(ParserError::new(
                        ParseErrorKind::UnexpectedEof,
                        if_token.span,
                    )),
                }?;

                let span = Span::new(if_token.span.start, end);
                let block = Block {
                    span,
                    statements: vec![statement],
                };

                (block, span.end)
            }
        };

        let mut else_branch = None;
        let mut end_pos = then_end;

        if parser.consume_optional(TokenKind::Keyword(Keyword::Else)) {
            let Some(Ok(next_token)) = parser.peek() else {
                return Err(ParserError::new(
                    ParseErrorKind::UnexpectedEof,
                    Span::default(),
                ));
            };

            match next_token.kind {
                TokenKind::Keyword(Keyword::If) => {
                    let else_if = If::parse(parser)?;
                    end_pos = else_if.span.end;
                    else_branch = Some(Box::new(Else::If(else_if)));
                }

                TokenKind::Punct(Punct::OpenBrace) => {
                    let else_block = Block::parse(parser)?;
                    end_pos = else_block.span.end;
                    else_branch = Some(Box::new(Else::Block(else_block)));
                }

                _ => {
                    let expr = Expression::parse(parser)?;
                    let semi = parser.expect_token(Punct::Semicolon)?;

                    end_pos = semi.span.end;
                    else_branch = Some(Box::new(Else::Expr(expr)));
                }
            }
        }

        let span = Span::new(if_token.span.start, end_pos);
        Ok(If {
            condition,
            then_branch,
            else_branch,
            span,
        })
    }
}

impl<'i> Parsable<'i> for While<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let while_token = parser.expect_token(Keyword::While)?;
        let condition = Expression::parse(parser)?;
        let body = Block::parse(parser)?;
        let span = while_token.span + body.span;

        Ok(While {
            condition,
            body,
            span,
        })
    }
}

impl<'i> Parsable<'i> for Function<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let is_pub = parser.consume_keyword(Keyword::Pub)?;
        let inline = parser.consume_keyword(Keyword::Inline)?;
        let is_const = parser.consume_keyword(Keyword::Const)?;

        let fn_token = parser.expect_token(Keyword::Fn)?;
        let (name, _) = parser.expect_identifier()?;
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
                }

                Ok(_) => {
                    if !params.is_empty() || receiver.is_some() {
                        parser.expect_token(Punct::Comma)?;
                    }

                    if params.is_empty()
                        && receiver.is_none()
                        && parser.consume_punct(Punct::Ampersand)?
                    {
                        let receiver_start = parser.last_span().unwrap_or(fn_token.span);
                        let mutable = parser.consume_keyword(Keyword::Mut)?;
                        let (name, name_span) = parser.expect_identifier()?;
                        if name != "self" {
                            return Err(ParserError::new(
                                ParseErrorKind::ExpectedIdentifier {
                                    found: TokenKind::Identifier(name),
                                },
                                name_span,
                            ));
                        }

                        receiver = Some(Receiver {
                            mutable,
                            span: receiver_start + name_span,
                        });

                        continue;
                    }

                    let mutable = parser.consume_keyword(Keyword::Mut)?;
                    let (param_name, param_span) = parser.expect_identifier()?;
                    parser.expect_token(Punct::Colon)?;
                    let typ = parser.parse_node()?;

                    params.push(Parameter {
                        name: param_name,
                        typ,
                        span: param_span + typ.span(),
                        mutable,
                    })
                }

                Err(err) => return Err(err.into()),
            }
        }

        let return_type = match parser.consume_punct(Punct::Colon)? {
            true => Some(parser.parse_node()?),
            false => None,
        };
        let body = Block::parse(parser)?;
        let span = fn_token.span + body.span;

        Ok(Function {
            params,
            impl_type: None,
            receiver,
            name,
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
        let (name, _) = parser.expect_identifier()?;
        parser.expect_token(Punct::OpenBrace)?;

        let interface = match parser.consume_keyword(Keyword::With)? {
            true => Some(parser.expect_identifier()?.0),
            false => None,
        };

        let mut methods = Vec::new();

        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                    let close = parser.expect_token(Punct::CloseBrace)?;
                    let span = impl_token.span + close.span;

                    return Ok(Self {
                        name,
                        methods,
                        span,
                        interface,
                    });
                }

                Some(Ok(token)) if token.is_kind(TokenKind::Eof) => {
                    return Err(ParserError::new(ParseErrorKind::UnexpectedEof, token.span));
                }

                Some(Ok(token)) if token.is_fn_start() => {
                    let mut method = parser.parse_node::<Function>()?;
                    method.impl_type = Some(name);
                    methods.push(method);
                }

                Some(Ok(token)) => {
                    return Err(ParserError::new(
                        ParseErrorKind::Expected {
                            expected: TokenKind::Keyword(Keyword::Fn),
                            found: token.kind,
                        },
                        token.span,
                    ));
                }

                Some(Err(err)) => return Err(err.into()),
                None => {
                    return Err(ParserError::new(
                        ParseErrorKind::UnexpectedEof,
                        impl_token.span,
                    ));
                }
            }
        }
    }
}

impl<'i> Parsable<'i> for Struct<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let is_pub = parser.consume_keyword(Keyword::Pub)?;
        let struct_token = parser.expect_token(Keyword::Struct)?;
        let (name, _) = parser.expect_identifier()?;
        parser.expect_token(Punct::OpenBrace)?;

        let mut fields = Vec::new();

        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                    let close = parser.expect_token(Punct::CloseBrace)?;
                    let span = struct_token.span + close.span;

                    return Ok(Self {
                        name,
                        fields,
                        is_pub,
                        span,
                    });
                }

                Some(Ok(token)) if token.is_kind(TokenKind::Eof) => {
                    return Err(ParserError::new(ParseErrorKind::UnexpectedEof, token.span));
                }

                Some(Err(err)) => return Err(err.into()),
                _ => {}
            }

            if !fields.is_empty() {
                parser.expect_token(Punct::Comma)?;

                match parser.peek() {
                    Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                        continue;
                    }
                    _ => {}
                }
            }

            let (field_name, field_span) = parser.expect_identifier()?;
            parser.expect_token(Punct::Colon)?;
            let typ = parser.parse_node::<Spanned<Type<'i>>>()?;
            let span = field_span + typ.span();

            fields.push(StructField {
                name: field_name,
                typ,
                span,
            });
        }
    }
}

impl<'i> Parsable<'i> for Interface<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let is_pub = parser.consume_keyword(Keyword::Pub)?;
        let interface_token = parser.expect_token(Keyword::Interface)?;
        let (name, _) = parser.expect_identifier()?;
        parser.expect_token(Punct::OpenBrace)?;

        let mut methods = Vec::new();

        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                    let close = parser.expect_token(Punct::CloseBrace)?;
                    return Ok(Self {
                        name,
                        span: interface_token.span + close.span,
                        methods,
                        is_pub,
                    });
                }
                Some(Err(err)) => return Err(err.into()),
                _ => methods.push(InterfaceMethod::parse(parser)?),
            }
        }
    }
}

impl<'i> Parsable<'i> for InterfaceMethod<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let fn_token = parser.expect_token(Keyword::Fn)?;
        let (name, _) = parser.expect_identifier()?;
        parser.expect_token(Punct::OpenParen)?;

        let mut params = Vec::new();
        let mut receiver = None;

        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseParen) => {
                    parser.expect_token(Punct::CloseParen)?;
                    break;
                }

                Some(Ok(_)) => {
                    if !params.is_empty() || receiver.is_none() {
                        parser.expect_token(Punct::Comma)?;
                    }

                    if params.is_empty()
                        && receiver.is_none()
                        && parser.consume_punct(Punct::Ampersand)?
                    {
                        let receiver_start = parser.last_span().unwrap_or(fn_token.span);
                        let mutable = parser.consume_keyword(Keyword::Mut)?;
                        let (receiver_name, receiver_span) = parser.expect_identifier()?;

                        if receiver_name != "self" {
                            return Err(ParserError::new(
                                ParseErrorKind::ExpectedIdentifier {
                                    found: TokenKind::Identifier(receiver_name.into()),
                                },
                                receiver_span,
                            ));
                        }

                        receiver = Some(Receiver {
                            mutable,
                            span: receiver_start + receiver_span,
                        });

                        continue;
                    }

                    let mutable = parser.consume_keyword(Keyword::Mut)?;
                    let (param_name, param_span) = parser.expect_identifier()?;
                    parser.expect_token(Punct::Colon)?;
                    let typ = parser.parse_node()?;

                    params.push(Parameter {
                        mutable,
                        name: param_name,
                        typ,
                        span: param_span + typ.span(),
                    })
                }

                Some(Err(err)) => return Err(err.into()),

                None => {
                    return Err(ParserError::new(
                        ParseErrorKind::UnexpectedEof,
                        fn_token.span,
                    ));
                }
            }
        }

        let return_type = match parser.consume_punct(Punct::Colon)? {
            true => Some(parser.parse_node()?),
            _ => None,
        };

        let semi = parser.expect_token(Punct::Semicolon)?;

        Ok(Self {
            span: fn_token.span + semi.span,
            name,
            receiver,
            params,
            return_type,
        })
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
                }
                _ => break,
            }

            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::OpenBrace) => break,
                _ => {}
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
                        }

                        _ => {}
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
                        }
                        _ => {}
                    }

                    let (name, span) = parser.expect_identifier()?;
                    names.push(UseItem { name, span })
                }

                UseItems::Named(names)
            }

            _ => UseItems::Namespace,
        };

        let semi = parser.expect_token(Punct::Semicolon)?;
        let span = use_token.span + semi.span;

        Ok(UseDecl {
            path: UsePath { segments },
            items,
            span,
        })
    }
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
            Self::Return(s) => s.span,
            Self::If(s) => s.span,
            Self::While(s) => s.span,
            Self::Fn(s) => s.span,
            Self::Struct(s) => s.span,
            Self::Impl(s) => s.span,
            Self::Interface(i) => i.span,
            Self::Use(s) => s.span,
            Self::Expr(_, span) => *span,
            Self::Block(b) => b.span,
        }
    }
}
