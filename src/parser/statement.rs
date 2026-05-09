use crate::lexer::Spanned;
use crate::lexer::token::{Keyword, Punct, Span, TokenKind};
use crate::parser::error::{ParseErrorKind, ParserError};
use crate::parser::expression::Expression;
use crate::parser::{Parsable, Parser};

#[derive(Debug, PartialEq)]
pub enum Statement<'i> {
    Let(Let<'i>),
    Return(Return<'i>),
    If(If<'i>),
    While(While<'i>),
    Fn(Function<'i>),
    Struct(Struct<'i>),
    Expr(Expression<'i>, Span),
    Block(Block<'i>),
    Use(UseDecl<'i>),
}

#[derive(Debug, PartialEq)]
pub struct Let<'i> {
    pub mutable: bool,
    pub name: &'i str,
    pub typ: Option<Spanned<Type<'i>>>,
    pub value: Option<Expression<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct Return<'i> {
    pub value: Option<Expression<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct If<'i> {
    pub condition: Expression<'i>,
    pub then_branch: Block<'i>,
    pub else_branch: Option<Box<Else<'i>>>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct While<'i> {
    pub condition: Expression<'i>,
    pub body: Block<'i>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct Function<'i> {
    pub name: &'i str,
    pub params: Vec<Parameter<'i>>,
    pub return_type: Option<Spanned<Type<'i>>>,
    pub body: Block<'i>,
    pub is_const: bool,
    pub is_pub: bool,
    pub inline: bool,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct Struct<'i> {
    pub name: &'i str,
    pub fields: Vec<StructField<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct StructField<'i> {
    pub name: &'i str,
    pub typ: Spanned<Type<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct Parameter<'i> {
    pub name: &'i str,
    pub mutable: bool,
    pub typ: Spanned<Type<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct Block<'i> {
    pub statements: Vec<Statement<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub enum Else<'i> {
    If(If<'i>),
    Block(Block<'i>),
    Expr(Expression<'i>),
}

#[derive(Debug, PartialEq)]
pub struct UseDecl<'i> {
    pub path: UsePath<'i>,
    pub items: UseItems<'i>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct UsePath<'i> {
    pub segments: Vec<&'i str>,
}

#[derive(Debug, PartialEq)]
pub struct UseItem<'i> {
    pub name: &'i str,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
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
        let kind = match parser.peek() {
            Some(Ok(token)) => &token.kind,
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
            TokenKind::Punct(Punct::OpenBrace) => Ok(Statement::Block(parser.parse_node()?)),
            TokenKind::Keyword(Keyword::Fn | Keyword::Pub | Keyword::Inline | Keyword::Const) => {
                Ok(Statement::Fn(parser.parse_node()?))
            }
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
                        parser.expect_punct(Punct::Semicolon)?;
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
        let let_token = parser.expect_keyword(Keyword::Let)?;
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

        let semicolon = parser.expect_punct(Punct::Semicolon)?;
        let span = Span::new(let_token.span.start, semicolon.span.end);

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
        let return_token = parser.expect_keyword(Keyword::Return)?;

        let mut value = None;
        if let Some(Ok(token)) = parser.peek() {
            if !token.is_kind(Punct::Semicolon) {
                value = Some(Expression::parse(parser)?);
            }
        }
        let semi_token = parser.expect_punct(Punct::Semicolon)?;
        let span = Span::new(return_token.span.start, semi_token.span.end);

        Ok(Return { value, span })
    }
}

impl<'i> Parsable<'i> for If<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let if_token = parser.expect_keyword(Keyword::If)?;

        let condition = Expression::parse(parser)?;
        let has_block = matches!(parser.peek(), Some(Ok(token)) if token.is_kind(Punct::OpenBrace));

        let (then_branch, then_end) = match has_block {
            true => {
                let block = Block::parse(parser)?;
                let end = block.span.end;

                (block, end)
            }
            false => {
                let expr = Expression::parse(parser)?;
                let semi = parser.expect_punct(Punct::Semicolon)?;
                let span = Span::new(expr.span().start, semi.span.end);
                let block = Block {
                    span,
                    statements: vec![Statement::Expr(expr, span)],
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
                    let semi = parser.expect_punct(Punct::Semicolon)?;

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
        let while_token = parser.expect_keyword(Keyword::While)?;
        let condition = Expression::parse(parser)?;
        let body = Block::parse(parser)?;
        let span = Span::new(while_token.span.start, body.span.end);

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

        let fn_token = parser.expect_keyword(Keyword::Fn)?;
        let (name, _) = parser.expect_identifier()?;
        parser.expect_punct(Punct::OpenParen)?;

        let mut params = Vec::new();

        loop {
            let token = parser
                .peek()
                .ok_or_else(|| ParserError::new(ParseErrorKind::UnexpectedEof, fn_token.span))?;

            match token {
                Ok(token) if token.is_kind(Punct::CloseParen) => {
                    parser.expect_punct(Punct::CloseParen)?;
                    break;
                }

                Ok(_) => {
                    if !params.is_empty() {
                        parser.expect_punct(Punct::Comma)?;
                    }

                    let mutable = parser.consume_keyword(Keyword::Mut)?;
                    let (param_name, param_span) = parser.expect_identifier()?;
                    parser.expect_punct(Punct::Colon)?;
                    let typ = parser.parse_node::<Spanned<Type>>()?;
                    let span = Span::new(param_span.start, typ.span().end);

                    params.push(Parameter {
                        name: param_name,
                        typ,
                        span,
                        mutable,
                    })
                }

                Err(err) => return Err(err.into()),
            }
        }

        let return_type = match parser.consume_punct(Punct::Colon)? {
            true => Some(parser.parse_node::<Spanned<Type>>()?),
            false => None,
        };
        let body = Block::parse(parser)?;
        let span = Span::new(fn_token.span.start, body.span.end);

        Ok(Function {
            params,
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

impl<'i> Parsable<'i> for Struct<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let struct_token = parser.expect_keyword(Keyword::Struct)?;
        let (name, _) = parser.expect_identifier()?;
        parser.expect_punct(Punct::OpenBrace)?;

        let mut fields = Vec::new();

        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                    let close = parser.expect_punct(Punct::CloseBrace)?;
                    let span = Span::new(struct_token.span.start, close.span.end);

                    return Ok(Self { name, fields, span });
                }

                Some(Ok(token)) if token.is_kind(TokenKind::Eof) => {
                    return Err(ParserError::new(ParseErrorKind::UnexpectedEof, token.span));
                }

                Some(Err(err)) => return Err(err.into()),
                _ => {}
            }

            if !fields.is_empty() {
                parser.expect_punct(Punct::Comma)?;

                match parser.peek() {
                    Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                        continue;
                    }
                    _ => {}
                }
            }

            let (field_name, field_span) = parser.expect_identifier()?;
            parser.expect_punct(Punct::Colon)?;
            let typ = parser.parse_node::<Spanned<Type<'i>>>()?;
            let span = Span::new(field_span.start, typ.span().end);

            fields.push(StructField {
                name: field_name,
                typ,
                span,
            });
        }
    }
}

impl<'i> Parsable<'i> for Block<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let open_brace = parser.expect_punct(Punct::OpenBrace)?;
        let mut statements = Vec::new();

        let close_brace = loop {
            let token = parser
                .peek()
                .and_then(|r| r.as_ref().ok())
                .ok_or_else(|| ParserError::new(ParseErrorKind::UnexpectedEof, open_brace.span))?;

            if token.is_kind(Punct::CloseBrace) {
                let close = parser.expect_punct(Punct::CloseBrace)?;
                break close;
            }

            if token.is_kind(TokenKind::Eof) {
                return Err(ParserError::new(ParseErrorKind::UnexpectedEof, token.span));
            }

            statements.push(parser.parse_node::<Statement>()?);
        };

        let span = Span::new(open_brace.span.start, close_brace.span.end);
        Ok(Self { span, statements })
    }
}

impl<'i> Parsable<'i> for UseDecl<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        let use_token = parser.expect_keyword(Keyword::Use)?;
        let (first, _) = parser.expect_identifier()?;
        let mut segments = vec![first];

        // consume '::segment' pairs until we hit '{' or ';'
        loop {
            match parser.peek() {
                Some(Ok(token)) if token.is_kind(Punct::ColonColon) => {
                    parser.expect_punct(Punct::ColonColon)?;
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
                parser.expect_punct(Punct::OpenBrace)?;
                let mut names = Vec::new();
                let mut first = true;

                loop {
                    match parser.peek() {
                        Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                            parser.expect_punct(Punct::CloseBrace)?;
                            break;
                        }

                        _ => {}
                    }

                    if !first {
                        parser.expect_punct(Punct::Comma)?;
                    }
                    first = false;

                    // trailing comma: '{a, b,}'
                    match parser.peek() {
                        Some(Ok(token)) if token.is_kind(Punct::CloseBrace) => {
                            parser.expect_punct(Punct::CloseBrace)?;
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

        let semi = parser.expect_punct(Punct::Semicolon)?;
        let span = Span::new(use_token.span.start, semi.span.end);

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
            Self::Use(s) => s.span,
            Self::Expr(_, span) => *span,
            Self::Block(b) => b.span,
        }
    }
}
