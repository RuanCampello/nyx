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
    Expr(Expression<'i>, Span),
    Block(Block<'i>),
}

#[derive(Debug, PartialEq)]
pub struct Let<'i> {
    pub mutable: bool,
    pub name: &'i str,
    pub typ: Option<Spanned<Type>>,
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
    pub return_type: Option<Spanned<Type>>,
    pub body: Block<'i>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub struct Parameter<'i> {
    pub name: &'i str,
    pub typ: Spanned<Type>,
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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[non_exhaustive]
#[allow(unused)]
pub enum Type {
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
    Uptr,
    Iptr,
    Bool,
    Char,
    Str,
    String,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Spanned<T> {
    span: Span,
    value: T,
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
            TokenKind::Keyword(Keyword::Fn) => Ok(Statement::Fn(parser.parse_node()?)),
            TokenKind::Punct(Punct::OpenBrace) => Ok(Statement::Block(parser.parse_node()?)),
            TokenKind::Eof => Err(ParserError::new(
                ParseErrorKind::UnexpectedEof,
                Span::default(),
            )),
            _ => {
                let expr = parser.parse_node::<Expression>()?;
                let end_position = match parser.peek() {
                    Some(Ok(token))
                        if matches!(
                            token.kind,
                            TokenKind::Punct(Punct::CloseBrace) | TokenKind::Eof
                        ) =>
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

impl<T: Clone + Copy> Spanned<T> {
    pub const fn value(&self) -> T {
        self.value
    }

    pub const fn span(&self) -> Span {
        self.span
    }
}

impl Spanned<Type> {
    pub fn parse<'i>(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        if parser.consume_punct(Punct::Ampersand)? {
            let (name, span) = parser.expect_identifier()?;

            return match name {
                "str" => Ok(Self {
                    span,
                    value: Type::Str,
                }),
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
            _ => {
                return Err(ParserError::new(
                    ParseErrorKind::ExpectedTypeIdentifier {
                        found: name.to_string(),
                    },
                    span,
                ));
            }
        };

        Ok(Self { value, span })
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
            if token.kind != TokenKind::Punct(Punct::Semicolon) {
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

        let has_block = matches!(parser.peek(), Some(Ok(token)) if token.kind == TokenKind::Punct(Punct::OpenBrace));

        let (then_branch, then_end) = match has_block {
            true => {
                let expr = Expression::parse(parser)?;
                let semi = parser.expect_punct(Punct::Semicolon)?;
                let span = Span::new(expr.span().start, semi.span.end);
                let block = Block {
                    span,
                    statements: vec![Statement::Expr(expr, span)],
                };

                (block, span.end)
            }
            false => {
                let block = Block::parse(parser)?;
                let end = block.span.end;

                (block, end)
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
        let fn_token = parser.expect_keyword(Keyword::Fn)?;
        let (name, _) = parser.expect_identifier()?;
        parser.expect_punct(Punct::OpenParen)?;

        let mut params = Vec::new();

        loop {
            let token = parser
                .peek()
                .ok_or_else(|| ParserError::new(ParseErrorKind::UnexpectedEof, fn_token.span))?;

            match token {
                Ok(token) if token.kind == TokenKind::Punct(Punct::CloseParen) => {
                    parser.expect_punct(Punct::CloseParen)?;
                    break;
                }

                Ok(_) => {
                    if !params.is_empty() {
                        parser.expect_punct(Punct::Comma)?;
                    }

                    let (param_name, param_span) = parser.expect_identifier()?;
                    parser.expect_punct(Punct::Colon)?;
                    let typ = parser.parse_node::<Spanned<Type>>()?;
                    let span = Span::new(param_span.start, typ.span().end);

                    params.push(Parameter {
                        name: param_name,
                        typ,
                        span,
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
        })
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

            if let TokenKind::Punct(Punct::CloseBrace) = token.kind {
                let close = parser.expect_punct(Punct::CloseBrace)?;
                break close;
            }

            if let TokenKind::Eof = token.kind {
                return Err(ParserError::new(ParseErrorKind::UnexpectedEof, token.span));
            }

            statements.push(parser.parse_node::<Statement>()?);
        };

        let span = Span::new(open_brace.span.start, close_brace.span.end);
        Ok(Self { span, statements })
    }
}

impl<'i> Parsable<'i> for Spanned<Type> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>> {
        Spanned::<Type>::parse(parser)
    }
}
