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
    Expr(Expression<'i>, Span),
    Block(Block<'i>),
}

#[derive(Debug, PartialEq)]
pub struct Let<'i> {
    pub mutable: bool,
    pub name: &'i str,
    pub typ: Option<Type>,
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
pub struct Block<'i> {
    pub statements: Vec<Statement<'i>>,
    pub span: Span,
}

#[derive(Debug, PartialEq)]
pub enum Else<'i> {
    If(If<'i>),
    Block(Block<'i>),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Type {
    I32 { span: Span },
    I64 { span: Span },
    F32 { span: Span },
    F64 { span: Span },
    Bool { span: Span },
    String { span: Span },
}

impl<'i> Parsable<'i> for Statement<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
        let token = match parser.peek() {
            Some(Ok(token)) => token,
            _ => return Err(parser.unexpected("statement", "EOF")),
        };

        Ok(match token.kind {
            TokenKind::Keyword(Keyword::Let) => Statement::Let(parser.parse_node()?),
            TokenKind::Keyword(Keyword::If) => Statement::If(parser.parse_node()?),
            TokenKind::Keyword(Keyword::While) => Statement::While(parser.parse_node()?),
            TokenKind::Keyword(Keyword::Return) => Statement::Return(parser.parse_node()?),
            TokenKind::Punct(Punct::OpenBrace) => Statement::Block(parser.parse_node()?),
            TokenKind::Eof => return Err(parser.unexpected("statement", "EOF")),

            _ => {
                let expr = parser.parse_node::<Expression>()?;
                parser.expect_punct(Punct::Semicolon)?;
                let span = Span::new(expr.span().start, expr.span().end);

                return Ok(Statement::Expr(expr, span));
            }
        })
    }
}

impl Type {
    #[inline(always)]
    pub const fn span(&self) -> Span {
        match self {
            Self::I32 { span }
            | Self::I64 { span }
            | Self::F32 { span }
            | Self::F64 { span }
            | Self::Bool { span }
            | Self::String { span } => *span,
        }
    }

    pub fn parse(parser: &mut Parser<'_>) -> Result<Self, ParserError> {
        let (name, span) = parser.expect_identifier()?;
        match name {
            "i32" => Ok(Type::I32 { span }),
            "i64" => Ok(Type::I64 { span }),
            "f32" => Ok(Type::F32 { span }),
            "f64" => Ok(Type::F64 { span }),
            "bool" => Ok(Type::Bool { span }),
            "String" => Ok(Type::String { span }),
            _ => Err(ParserError {
                kind: ParseErrorKind::Unexpected {
                    expected: "type identifier".to_string(),
                    found: name.to_string(),
                },
                span,
            }),
        }
    }
}

impl<'i> Parsable<'i> for Let<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
        let let_token = parser.next_token()?.unwrap();
        let mut mutable = false;

        if let Some(Ok(token)) = parser.peek() {
            if token.kind == TokenKind::Keyword(Keyword::Mut) {
                parser.next_token()?;
                mutable = true;
            }
        }

        let (name, _) = parser.expect_identifier()?;

        let mut typ = None;
        if let Some(Ok(token)) = parser.peek() {
            if token.kind == TokenKind::Punct(Punct::Colon) {
                parser.expect_punct(Punct::Colon)?;
                typ = Some(Type::parse(parser)?);
            }
        }

        let mut value = None;
        if let Some(Ok(token)) = parser.peek() {
            if token.kind == TokenKind::Punct(Punct::Eq) {
                parser.expect_punct(Punct::Eq)?;
                value = Some(Expression::parse(parser)?);
            }
        }

        let semi_token = parser.expect_punct(Punct::Semicolon)?;
        let span = Span::new(let_token.span.start, semi_token.span.end);

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
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
        let return_token = parser.next_token()?.unwrap();
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
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
        let if_token = parser.next_token()?.unwrap();

        let condition = Expression::parse(parser)?;
        let then_branch = Block::parse(parser)?;

        let mut else_branch = None;
        let mut end_pos = then_branch.span.end;

        if let Some(Ok(token)) = parser.peek() {
            if token.kind == TokenKind::Keyword(Keyword::Else) {
                parser.next_token()?;

                if let Some(Ok(next_token)) = parser.peek() {
                    if next_token.kind == TokenKind::Keyword(Keyword::If) {
                        let else_if = If::parse(parser)?;
                        end_pos = else_if.span.end;
                        else_branch = Some(Box::new(Else::If(else_if)));
                    } else if next_token.kind == TokenKind::Punct(Punct::OpenBrace) {
                        let else_block = Block::parse(parser)?;
                        end_pos = else_block.span.end;
                        else_branch = Some(Box::new(Else::Block(else_block)));
                    } else {
                        return Err(ParserError {
                            kind: ParseErrorKind::Unexpected {
                                expected: "`if` or `{`".to_string(),
                                found: next_token.kind.to_string(),
                            },
                            span: next_token.span,
                        });
                    }
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
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
        let while_token = parser.next_token()?.unwrap();
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

impl<'i> Parsable<'i> for Block<'i> {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError> {
        let open_brace = parser.expect_punct(Punct::OpenBrace)?;

        todo!()
    }
}
