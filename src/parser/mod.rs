//! Transformation of tokens into an abstract syntax tree.

use crate::{
    lexer::{
        HasSpan, Lexer,
        error::LexError,
        token::{Keyword, Position, Punct, Span, Token, TokenKind},
    },
    parser::{
        error::{ParseErrorKind, ParserError},
        statement::Statement,
    },
};
use std::collections::VecDeque;

pub mod error;
pub mod expression;
pub mod statement;
pub mod visitor;

/// Recursive-descent parser.
pub struct Parser<'i> {
    cursor: Lexer<'i>,
    buffer: VecDeque<Result<Token<'i>, LexError<'i>>>,
    /// Most recently used consumed token, used to place EOF diagnostics.
    last: Option<Span>,
}

pub trait Parsable<'i>: Sized {
    fn parse(parser: &mut Parser<'i>) -> Result<Self, ParserError<'i>>;
}

impl<'i> Parser<'i> {
    pub fn new(source: &'i str) -> Self {
        Self {
            cursor: Lexer::new(source),
            buffer: VecDeque::with_capacity(4),
            last: None,
        }
    }

    pub fn parse(mut self) -> Result<Vec<Statement<'i>>, ParserError<'i>> {
        let mut statements = Vec::new();

        loop {
            match self.peek() {
                Some(Ok(token)) if token.is_kind(TokenKind::Eof) => break,
                Some(Ok(_)) => statements.push(self.parse_node::<Statement>()?),
                None => break,
                Some(Err(err)) => return Err(ParserError::new(err.clone().into(), err.span())),
            }
        }

        Ok(statements)
    }

    fn parse_node<N: Parsable<'i>>(&mut self) -> Result<N, ParserError<'i>> {
        N::parse(self)
    }

    #[inline(always)]
    pub fn peek(&mut self) -> Option<&Result<Token<'i>, LexError<'i>>> {
        if self.buffer.is_empty() {
            if let Some(t) = self.cursor.next() {
                self.buffer.push_back(t);
            }
        }

        self.buffer.front()
    }

    #[inline(always)]
    pub fn peek_nth(&mut self, n: usize) -> Option<Result<Token<'i>, LexError<'i>>> {
        while self.buffer.len() <= n {
            match self.cursor.next() {
                Some(t) => self.buffer.push_back(t),
                None => break,
            }
        }

        self.buffer.get(n).copied()
    }

    #[inline(always)]
    pub fn next_token(&mut self) -> Result<Option<Token<'i>>, ParserError<'i>> {
        let token = self.buffer.pop_front().or_else(|| self.cursor.next());
        match token {
            Some(Ok(token)) => {
                self.last = Some(token.span);
                Ok(Some(token))
            },
            Some(Err(e)) => {
                let span = e.span();
                Err(ParserError::new(e.into(), span))
            },
            None => Ok(None),
        }
    }

    pub fn expect_next(&mut self) -> Result<Token<'i>, ParserError<'i>> {
        self.next_token()?.ok_or_else(|| {
            ParserError::new(ParseErrorKind::UnexpectedEof, self.last.unwrap_or_default())
        })
    }

    #[inline(always)]
    pub(crate) const fn last_span(&self) -> Option<Span> {
        self.last
    }

    #[inline(always)]
    pub fn expect_token(
        &mut self,
        expected: impl Into<TokenKind<'i>>,
    ) -> Result<Token<'i>, ParserError<'i>> {
        let expected = expected.into();
        let token = self.expect_next()?;
        match token.is_kind(expected) {
            true => Ok(token),
            false => Err(ParserError::new(
                ParseErrorKind::Expected { expected, found: token.kind },
                token.span,
            )),
        }
    }

    #[inline(always)]
    pub fn expect_identifier(&mut self) -> Result<(&'i str, Span), ParserError<'i>> {
        let token = self.expect_next()?;

        match token.kind {
            TokenKind::Identifier(id) => Ok((id, token.span)),
            _ => Err(ParserError::new(
                ParseErrorKind::ExpectedIdentifier { found: token.kind },
                token.span,
            )),
        }
    }

    #[inline(always)]
    pub fn consume_optional(&mut self, kind: TokenKind<'i>) -> bool {
        if let Some(Ok(token)) = self.peek() {
            if token.is_kind(kind) {
                let _ = self.next_token();
                return true;
            }
        }

        false
    }

    #[inline(always)]
    pub fn consume_keyword(&mut self, keyword: Keyword) -> Result<bool, ParserError<'i>> {
        self.consume_token(TokenKind::Keyword(keyword))
    }

    #[inline(always)]
    pub fn consume_punct(&mut self, punct: Punct) -> Result<bool, ParserError<'i>> {
        self.consume_token(TokenKind::Punct(punct))
    }

    /// Consume a closing `>` for a generic argument list, splitting a `>>` (Shr) if necessary.
    ///
    /// Nested generics like `PartialEq<T>` produce a `>>` token at the boundary
    /// of the outer list. Rather than teaching the lexer about generic context,
    /// we split it here: consume `>>`, push the trailing `>` back into the
    /// buffer, and report success.
    pub(crate) fn consume_generic_close(&mut self) -> Result<bool, ParserError<'i>> {
        match self.peek() {
            Some(Ok(t)) if t.is_kind(Punct::Gt) => {
                self.next_token()?;
                Ok(true)
            },
            Some(Ok(t)) if t.is_kind(Punct::Shr) => {
                let shr = self.next_token()?.unwrap();
                // Split >>: push back a synthetic > for the second character
                let mid = Position::new(
                    shr.span.start.offset + 1,
                    shr.span.start.line,
                    shr.span.start.column + 1,
                );
                self.buffer.push_front(Ok(Token {
                    kind: TokenKind::Punct(Punct::Gt),
                    span: Span::new(mid, shr.span.end),
                }));
                Ok(true)
            },
            Some(Err(err)) => Err(err.into()),
            _ => Ok(false),
        }
    }

    fn consume_token(&mut self, kind: TokenKind<'i>) -> Result<bool, ParserError<'i>> {
        match self.peek() {
            Some(Ok(token)) if token.is_kind(kind) => {
                self.next_token()?;
                Ok(true)
            },
            Some(Err(err)) => return Err(err.into()),
            _ => Ok(false),
        }
    }

    pub(crate) fn is_const_decl(&mut self) -> bool {
        match self.peek_nth(0) {
            Some(Ok(t)) if t.is_kind(Keyword::Const) => {
                matches!(
                    self.peek_nth(1),
                    Some(Ok(t2)) if matches!(t2.kind, TokenKind::Identifier(id) if id != "fn")
                )
            },
            Some(Ok(t)) if t.is_kind(Keyword::Pub) => {
                matches!(
                    self.peek_nth(1),
                    Some(Ok(t2)) if t2.is_kind(Keyword::Const)
                ) && matches!(
                    self.peek_nth(2),
                    Some(Ok(t3)) if matches!(t3.kind, TokenKind::Identifier(id) if id != "fn")
                )
            },
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        lexer::token::Position,
        parser::{
            expression::{BinaryOperator, Expression, UnaryOperator},
            statement::{Let, Return, Type},
        },
    };

    use super::*;

    #[test]
    fn missing_semicolon() {
        let err = Parser::new("let value = 1").parse().unwrap_err();

        assert_eq!(
            err.kind,
            ParseErrorKind::Expected {
                expected: TokenKind::Punct(Punct::Semicolon),
                found: TokenKind::Eof
            }
        );

        assert_eq!(err.span.start.column, 14);
        assert_eq!(err.span.end.column, 14);
    }

    #[test]
    fn missing_expression() {
        let err = Parser::new("let value = ;").parse().unwrap_err();
        assert_eq!(
            err.kind,
            ParseErrorKind::ExpectedExpression { found: TokenKind::Punct(Punct::Semicolon) }
        );

        assert_eq!(err.span.start.column, 13);
        assert_eq!(err.span.end.column, 14);
    }

    #[test]
    fn invalid_identifier() {
        let err = Parser::new("let 123: i32 = 1;").parse().unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::ExpectedIdentifier { found: TokenKind::Integer(123) });

        assert_eq!(err.span.start.column, 5);
        assert_eq!(err.span.end.column, 8);
    }

    #[test]
    fn invalid_and_valid_return() {
        let err = Parser::new("return +1;").parse().unwrap_err();
        assert_eq!(
            err.kind,
            ParseErrorKind::ExpectedExpression { found: TokenKind::Punct(Punct::Plus) }
        );

        assert_eq!(err.span.start.column, 8);
        assert_eq!(err.span.end.column, 9);

        let statement = Parser::new("return -1;").parse().unwrap();
        assert_eq!(
            statement,
            vec![Statement::Return(Return {
                span: Span::new(Position::new(0, 1, 1), Position::new(10, 1, 11)),
                value: Some(Expression::Unary {
                    operator: UnaryOperator::Neg,
                    span: Span::new(Position::new(7, 1, 8), Position::new(9, 1, 10)),
                    expr: Box::new(Expression::Integer(
                        1,
                        Span::new(Position::new(8, 1, 9), Position::new(9, 1, 10))
                    )),
                }),
            },)]
        )
    }

    #[test]
    fn multiplication_is_left_associative() {
        let statements = Parser::new("a * b * c;").parse().unwrap();

        let a = Box::new(Expression::Identifier(
            "a",
            Span::new(Position::new(0, 1, 1), Position::new(1, 1, 2)),
        ));
        let b = Box::new(Expression::Identifier(
            "b",
            Span::new(Position::new(4, 1, 5), Position::new(5, 1, 6)),
        ));
        let c = Box::new(Expression::Identifier(
            "c",
            Span::new(Position::new(8, 1, 9), Position::new(9, 1, 10)),
        ));

        assert_eq!(
            statements,
            vec![Statement::Expr(
                Expression::Binary {
                    left: Box::new(Expression::Binary {
                        left: a,
                        operator: BinaryOperator::Mul,
                        right: b,
                        span: Span::new(Position::new(0, 1, 1), Position::new(5, 1, 6)),
                    }),
                    operator: BinaryOperator::Mul,
                    right: c,
                    span: Span::new(Position::new(0, 1, 1), Position::new(9, 1, 10)),
                },
                Span::new(Position::new(0, 1, 1), Position::new(9, 1, 10))
            )]
        );
    }

    #[test]
    fn assignment_is_right_associative() {
        let statements = Parser::new("a = b = c;").parse().unwrap();
        let b_eq_c = Box::new(Expression::Assignment {
            target: Box::new(Expression::Identifier(
                "b",
                Span::new(Position::new(4, 1, 5), Position::new(5, 1, 6)),
            )),
            value: Box::new(Expression::Identifier(
                "c",
                Span::new(Position::new(8, 1, 9), Position::new(9, 1, 10)),
            )),
            span: Span::new(Position::new(4, 1, 5), Position::new(9, 1, 10)),
        });

        assert_eq!(
            statements,
            vec![Statement::Expr(
                Expression::Assignment {
                    target: Box::new(Expression::Identifier(
                        "a",
                        Span::new(Position::new(0, 1, 1), Position::new(1, 1, 2)),
                    )),
                    value: b_eq_c,
                    span: Span::new(Position::new(0, 1, 1), Position::new(9, 1, 10)),
                },
                Span::new(Position::new(0, 1, 1), Position::new(9, 1, 10))
            )]
        );
    }

    #[test]
    fn unary_binds_after_method_call() {
        let statements = Parser::new("!rect.is_larger_than(15);").parse().unwrap();

        let [Statement::Expr(Expression::Unary { operator: UnaryOperator::Not, expr, .. }, _)] =
            statements.as_slice()
        else {
            panic!("expected unary expression statement, got {statements:?}");
        };

        let Expression::Call { callee, args, .. } = expr.as_ref() else {
            panic!("expected unary operand to be a method call, got {expr:?}");
        };

        assert!(matches!(
            callee.as_ref(),
            Expression::Field {
                expr,
                field: "is_larger_than",
                ..
            } if matches!(expr.as_ref(), Expression::Identifier("rect", _))
        ));
        assert!(matches!(args.as_slice(), [Expression::Integer(15, _)]));
    }

    #[test]
    fn parse_add_function_file() {
        let source = include_str!("../../tests/single/add.nyx");
        let statements = Parser::new(source).parse().unwrap();

        assert_eq!(statements.len(), 1);
        let function = match &statements[0] {
            Statement::Fn(function) => function,
            other => panic!("expected function, found {other:?}"),
        };

        assert_eq!(function.name, "add");
        assert_eq!(function.params.len(), 2);
        assert_eq!(function.params[0].name, "a");
        assert!(matches!(function.params[0].typ.value(), Type::I32));
        assert_eq!(function.params[1].name, "b");
        assert!(matches!(function.params[1].typ.value(), Type::I32));
    }

    #[test]
    fn parse_inference_file() {
        let source = include_str!("../../tests/single/inference.nyx");
        let statements = Parser::new(source).parse().unwrap();

        assert_eq!(statements.len(), 1);
        let function = match &statements[0] {
            Statement::Fn(function) => function,
            other => panic!("expected function, found {other:?}"),
        };

        assert_eq!(function.name, "main");
        assert!(function.params.is_empty());
        assert!(function.return_type.is_none());
        assert_eq!(function.body.statements.len(), 3);

        match &function.body.statements[0] {
            Statement::Let(Let { name, value, .. }) => {
                assert_eq!(*name, "x");
                assert!(matches!(value, Some(Expression::Integer(10, _))));
            },
            _ => unreachable!(),
        };

        match &function.body.statements[1] {
            Statement::Let(Let { name, value, .. }) => {
                assert_eq!(*name, "y");
                assert!(matches!(value, Some(Expression::Integer(20, _))));
            },
            _ => unreachable!(),
        };

        match &function.body.statements[2] {
            Statement::Let(Let { name, value, .. }) => {
                assert_eq!(*name, "z");
                assert!(matches!(
                    value,
                    Some(Expression::Binary { operator: BinaryOperator::Add, .. })
                ));
            },
            _ => unreachable!(),
        };
    }

    #[test]
    fn parses_struct_statement_and_expression() {
        let statements = Parser::new(
            r#"
            struct Point {
                y: i64,
                x: i32,
            }

            fn main() {
                let p: Point = Point { x: 1, y: 2 };
            }
        "#,
        )
        .parse()
        .unwrap();

        let declaration = match &statements[0] {
            Statement::Struct(declaration) => declaration,
            other => panic!("expected struct declaration, got {other:?}"),
        };
        assert_eq!(declaration.name, "Point");
        assert_eq!(declaration.fields.len(), 2);
        assert_eq!(declaration.fields[0].name, "y");
        assert!(matches!(declaration.fields[0].typ.value(), Type::I64));
        assert_eq!(declaration.fields[1].name, "x");
        assert!(matches!(declaration.fields[1].typ.value(), Type::I32));

        let function = match &statements[1] {
            Statement::Fn(function) => function,
            _ => panic!(),
        };
        let let_statement = match &function.body.statements[0] {
            Statement::Let(statement) => statement,
            _ => panic!(),
        };
        assert!(matches!(
            let_statement.typ.as_ref().map(|typ| typ.value()),
            Some(Type::Named("Point"))
        ));
        assert!(matches!(let_statement.value, Some(Expression::Struct { name: "Point", .. })));
    }

    #[test]
    fn parses_struct_representation_options() {
        let statements = Parser::new(
            r#"
            struct Flags {
                a: bool,
                b: bool,
            } as packed, align(4)
        "#,
        )
        .parse()
        .unwrap();

        let declaration = match &statements[0] {
            Statement::Struct(declaration) => declaration,
            other => panic!("expected struct declaration, got {other:?}"),
        };
        assert_eq!(declaration.repr.kind, statement::StructReprKind::Packed);
        assert_eq!(declaration.repr.align.unwrap().get(), 4);
    }

    #[test]
    fn parses_enum_statement_with_repr_and_values() {
        let statements = Parser::new(
            r#"
            pub enum Status {
                Ok = 0,
                Err = 1,
                Timeout,
            } as u16

            fn main(): Status {
                Status::Ok
            }
        "#,
        )
        .parse()
        .unwrap();

        let declaration = match &statements[0] {
            Statement::Enum(declaration) => declaration,
            other => panic!("expected enum declaration, got {other:?}"),
        };
        assert!(declaration.is_pub);
        assert_eq!(declaration.name, "Status");
        assert_eq!(declaration.variants.len(), 3);
        assert_eq!(declaration.variants[0].name, "Ok");
        assert_eq!(declaration.variants[0].value, Some(0));
        assert_eq!(declaration.variants[2].name, "Timeout");
        assert_eq!(declaration.variants[2].value, None);
        assert!(matches!(declaration.repr.value(), Type::U16));
    }

    #[test]
    fn bitwise_and_shifts_precedence() {
        let statements = Parser::new("!x & y | z ^ w << 2 >> 3;").parse().unwrap();
        let [Statement::Expr(expr, _)] = statements.as_slice() else {
            panic!("expected expression statement");
        };
        let Expression::Binary { left, operator, right, .. } = expr else {
            panic!("expected binary expression");
        };
        assert_eq!(*operator, BinaryOperator::BitOr);

        let Expression::Binary { left: l_l, operator: l_op, right: l_r, .. } = left.as_ref() else {
            panic!("expected left binary expression");
        };
        assert_eq!(*l_op, BinaryOperator::BitAnd);
        assert!(matches!(l_l.as_ref(), Expression::Unary { operator: UnaryOperator::Not, .. }));
        assert!(matches!(l_r.as_ref(), Expression::Identifier("y", _)));

        let Expression::Binary { left: r_l, operator: r_op, right: r_r, .. } = right.as_ref()
        else {
            panic!("expected right binary expression");
        };
        assert_eq!(*r_op, BinaryOperator::BitXor);
        assert!(matches!(r_l.as_ref(), Expression::Identifier("z", _)));

        let Expression::Binary { left: rr_l, operator: rr_op, right: rr_r, .. } = r_r.as_ref()
        else {
            panic!("expected shift-right binary expression");
        };
        assert_eq!(*rr_op, BinaryOperator::Shr);

        let Expression::Binary { left: rrl_l, operator: rrl_op, right: rrl_r, .. } = rr_l.as_ref()
        else {
            panic!("expected shift-left binary expression");
        };
        assert_eq!(*rrl_op, BinaryOperator::Shl);
        assert!(matches!(rrl_l.as_ref(), Expression::Identifier("w", _)));
        assert!(matches!(rrl_r.as_ref(), Expression::Integer(2, _)));
        assert!(matches!(rr_r.as_ref(), Expression::Integer(3, _)));
    }
}
