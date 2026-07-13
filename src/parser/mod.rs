//! Transformation of tokens into an abstract syntax tree.

use crate::{
    lexer::{
        Lexer,
        error::LexError,
        token::{BytePos, Keyword, Punct, Span, Token, TokenKind},
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

    /// Parse `source` whose first byte sits at global offset `base` in the
    /// [`SourceMap`](crate::source_map::SourceMap) address space
    pub fn with_base(source: &'i str, base: BytePos) -> Self {
        Self {
            cursor: Lexer::with_base(source, base),
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
                Some(Err(err)) => return Err(ParserError::new((*err).into(), err.span)),
                None => break,
            }
        }

        Ok(statements)
    }

    fn parse_node<N: Parsable<'i>>(&mut self) -> Result<N, ParserError<'i>> {
        N::parse(self)
    }

    #[inline(always)]
    pub fn peek(&mut self) -> Option<&Result<Token<'i>, LexError<'i>>> {
        if self.buffer.is_empty()
            && let Some(t) = self.cursor.next()
        {
            self.buffer.push_back(t);
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
            Some(Err(e)) => Err(ParserError::new(e.into(), e.span)),
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
        if token.is_kind(expected) {
            Ok(token)
        } else {
            Err(ParserError::new(
                ParseErrorKind::Expected { expected, found: token.kind },
                token.span,
            ))
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
    pub fn expect_unsigned_literal(&mut self) -> Result<u64, ParserError<'i>> {
        let token = self.expect_next()?;
        match token.kind {
            TokenKind::Integer(n) if n >= 0 => Ok(n as u64),
            _ => Err(ParserError::new(
                ParseErrorKind::ExpectedExpression { found: token.kind },
                token.span,
            )),
        }
    }

    #[inline(always)]
    pub fn consume_optional(&mut self, kind: TokenKind<'i>) -> bool {
        if let Some(Ok(token)) = self.peek()
            && token.is_kind(kind)
        {
            let _ = self.next_token();
            return true;
        }

        false
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
                let mid = shr.span.start + 1;
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

    /// Consume the run of `///` lines that documents the upcoming item.
    pub(crate) fn parse_outer_docs(&mut self) -> Box<[&'i str]> {
        let mut docs = Vec::new();

        while let Some(Ok(token)) = self.peek()
            && let TokenKind::DocComment(text) = token.kind
        {
            docs.push(text);
            let _ = self.next_token();
        }

        docs.into_boxed_slice()
    }

    fn consume_token(&mut self, kind: impl Into<TokenKind<'i>>) -> Result<bool, ParserError<'i>> {
        match self.peek() {
            Some(Ok(token)) if token.is_kind(kind) => {
                self.next_token()?;
                Ok(true)
            },
            Some(Err(err)) => Err(err.into()),
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
        lexer::token::BytePos,
        parser::{
            expression::{BinaryOperator, Expression, UnaryOperator},
            statement::{Item, ItemKind, Let, Loop, LoopHeader, Return, Type},
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

        assert_eq!(err.span.start.0, 13);
        assert_eq!(err.span.end.0, 13);
    }

    #[test]
    fn missing_expression() {
        let err = Parser::new("let value = ;").parse().unwrap_err();
        assert_eq!(
            err.kind,
            ParseErrorKind::ExpectedExpression { found: TokenKind::Punct(Punct::Semicolon) }
        );

        assert_eq!(err.span.start.0, 12);
        assert_eq!(err.span.end.0, 13);
    }

    #[test]
    fn invalid_identifier() {
        let err = Parser::new("let 123: i32 = 1;").parse().unwrap_err();
        assert_eq!(err.kind, ParseErrorKind::ExpectedIdentifier { found: TokenKind::Integer(123) });

        assert_eq!(err.span.start.0, 4);
        assert_eq!(err.span.end.0, 7);
    }

    #[test]
    fn generic_impl_receiver_type_is_parsed() {
        let statements = Parser::new(
            r#"
            struct Pair<L, R> { left: L, right: R }
            impl Pair<L, R> {
                fn first(&self): L { self.left }
            }
            "#,
        )
        .parse()
        .unwrap();

        let Statement::Item(Item { kind: ItemKind::Impl(implementation), .. }) = &statements[1]
        else {
            panic!("expected impl block");
        };

        assert_eq!(implementation.name, "Pair");
        assert!(implementation.generics.is_empty());
        assert!(
            matches!(implementation.receiver.value_ref(), Type::Generic("Pair", args) if args.len() == 2)
        );
    }

    #[test]
    fn interface_method_doc_comments_are_captured() {
        let statements = Parser::new(
            r#"
            interface Clone {
                /// Returns a duplicate of the value
                fn clone(&self): Self;
            }
            "#,
        )
        .parse()
        .unwrap();

        let Statement::Item(Item { kind: ItemKind::Interface(interface), .. }) = &statements[0]
        else {
            panic!("expected interface");
        };

        assert_eq!(interface.methods.len(), 1);
        assert_eq!(interface.methods[0].name, "clone");

        assert_eq!(interface.member_docs.len(), 1);
        let (span, lines) = &interface.member_docs[0];
        assert_eq!(*span, interface.methods[0].span);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], " Returns a duplicate of the value");
    }

    #[test]
    fn rust_style_generic_impl_header_is_rejected() {
        let err = Parser::new(
            r#"
            struct Box<T> { val: T }
            impl<T> Box<T> {
                fn get(&self): T { self.val }
            }
            "#,
        )
        .parse()
        .unwrap_err();

        assert!(matches!(err.kind, ParseErrorKind::ExpectedIdentifier { .. }));
    }

    #[test]
    fn invalid_and_valid_return() {
        let err = Parser::new("return +1;").parse().unwrap_err();
        assert_eq!(
            err.kind,
            ParseErrorKind::ExpectedExpression { found: TokenKind::Punct(Punct::Plus) }
        );

        assert_eq!(err.span.start.0, 7);
        assert_eq!(err.span.end.0, 8);

        let statement = Parser::new("return -1;").parse().unwrap();
        assert_eq!(
            statement,
            vec![Statement::Return(Return {
                span: Span::new(BytePos(0), BytePos(10)),
                value: Some(Expression::Unary {
                    operator: UnaryOperator::Neg,
                    span: Span::new(BytePos(7), BytePos(9)),
                    expr: Box::new(Expression::Integer(1, Span::new(BytePos(8), BytePos(9)))),
                }),
            },)]
        )
    }

    #[test]
    fn multiplication_is_left_associative() {
        let statements = Parser::new("a * b * c;").parse().unwrap();

        let a = Box::new(Expression::Identifier("a", Span::new(BytePos(0), BytePos(1))));
        let b = Box::new(Expression::Identifier("b", Span::new(BytePos(4), BytePos(5))));
        let c = Box::new(Expression::Identifier("c", Span::new(BytePos(8), BytePos(9))));

        assert_eq!(
            statements,
            vec![Statement::Expr(
                Expression::Binary {
                    left: Box::new(Expression::Binary {
                        left: a,
                        operator: BinaryOperator::Mul,
                        right: b,
                        span: Span::new(BytePos(0), BytePos(5)),
                    }),
                    operator: BinaryOperator::Mul,
                    right: c,
                    span: Span::new(BytePos(0), BytePos(9)),
                },
                Span::new(BytePos(0), BytePos(9))
            )]
        );
    }

    #[test]
    fn assignment_is_right_associative() {
        let statements = Parser::new("a = b = c;").parse().unwrap();
        let b_eq_c = Box::new(Expression::Assignment {
            target: Box::new(Expression::Identifier("b", Span::new(BytePos(4), BytePos(5)))),
            value: Box::new(Expression::Identifier("c", Span::new(BytePos(8), BytePos(9)))),
            span: Span::new(BytePos(4), BytePos(9)),
        });

        assert_eq!(
            statements,
            vec![Statement::Expr(
                Expression::Assignment {
                    target: Box::new(Expression::Identifier(
                        "a",
                        Span::new(BytePos(0), BytePos(1)),
                    )),
                    value: b_eq_c,
                    span: Span::new(BytePos(0), BytePos(9)),
                },
                Span::new(BytePos(0), BytePos(9))
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
            Statement::Item(Item { kind: ItemKind::Fn(function), .. }) => function,
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
            Statement::Item(Item { kind: ItemKind::Fn(function), .. }) => function,
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
            Statement::Item(Item { kind: ItemKind::Struct(declaration), .. }) => declaration,
            other => panic!("expected struct declaration, got {other:?}"),
        };
        assert_eq!(declaration.name, "Point");
        assert_eq!(declaration.fields.len(), 2);
        assert_eq!(declaration.fields[0].name, "y");
        assert!(matches!(declaration.fields[0].typ.value(), Type::I64));
        assert_eq!(declaration.fields[1].name, "x");
        assert!(matches!(declaration.fields[1].typ.value(), Type::I32));

        let function = match &statements[1] {
            Statement::Item(Item { kind: ItemKind::Fn(function), .. }) => function,
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
            Statement::Item(Item { kind: ItemKind::Struct(declaration), .. }) => declaration,
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
            Statement::Item(Item { kind: ItemKind::Enum(declaration), .. }) => declaration,
            other => panic!("expected enum declaration, got {other:?}"),
        };
        assert!(declaration.is_pub);
        assert_eq!(declaration.name, "Status");
        assert_eq!(declaration.variants.len(), 3);
        assert_eq!(declaration.variants[0].name, "Ok");
        assert_eq!(declaration.variants[0].value, Some(0));
        assert_eq!(declaration.variants[2].name, "Timeout");
        assert_eq!(declaration.variants[2].value, None);
        assert!(matches!(declaration.repr.as_ref().map(|r| r.value()), Some(Type::U16)));
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

    #[test]
    fn doc_comments_attach_to_following_item() {
        let statements = Parser::new("/// first line\n/// second\nfn foo() {}").parse().unwrap();
        let Statement::Item(Item { docs, kind: ItemKind::Fn(_) }) = &statements[0] else {
            panic!("expected fn, got {:?}", statements[0]);
        };
        assert_eq!(&**docs, [" first line", " second"].as_slice());
    }

    #[test]
    fn plain_and_quad_slash_comments_are_not_docs() {
        let statements = Parser::new("// not a doc\n//// nor this\nfn foo() {}").parse().unwrap();
        let Statement::Item(Item { docs, kind: ItemKind::Fn(_) }) = &statements[0] else {
            panic!("expected fn");
        };
        assert!(docs.is_empty());
    }

    #[test]
    fn docs_do_not_leak_between_items() {
        let statements = Parser::new("/// documented\nfn a() {}\nfn b() {}").parse().unwrap();
        let (
            Statement::Item(Item { docs: a, kind: ItemKind::Fn(_) }),
            Statement::Item(Item { docs: b, kind: ItemKind::Fn(_) }),
        ) = (&statements[0], &statements[1])
        else {
            panic!("expected two fns");
        };
        assert_eq!(&**a, [" documented"].as_slice());
        assert!(b.is_empty());
    }

    #[test]
    fn docs_attach_to_struct_and_const() {
        let statements =
            Parser::new("/// a point\nstruct P { x: i32 }\n/// the answer\nconst N: i32 = 42;")
                .parse()
                .unwrap();
        let Statement::Item(Item { docs, kind: ItemKind::Struct(_) }) = &statements[0] else {
            panic!("expected struct");
        };
        assert_eq!(&**docs, [" a point"].as_slice());
        let Statement::Item(Item { docs, kind: ItemKind::Const(_) }) = &statements[1] else {
            panic!("expected const");
        };
        assert_eq!(&**docs, [" the answer"].as_slice());
    }

    #[test]
    fn docs_attach_to_impl_method() {
        let statements =
            Parser::new("impl P {\n  /// makes one\n  fn make() {}\n}").parse().unwrap();
        let Statement::Item(Item { kind: ItemKind::Impl(block), .. }) = &statements[0] else {
            panic!("expected impl");
        };
        assert_eq!(block.member_docs.len(), 1);
        assert_eq!(&*block.member_docs[0].1, [" makes one"].as_slice());
    }

    #[test]
    fn array_slice_and_ref_types_parse() {
        let stmts = Parser::new("fn f(a: [i32; 3], s: &[i32], m: &mut [i32], r: &mut i32){}")
            .parse()
            .unwrap();
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &stmts[0] else {
            panic!("expected fn");
        };

        let types: Vec<Type> = function.params.iter().map(|p| p.typ.value()).collect();
        assert!(matches!(&types[0], Type::Array(element, 3) if **element == Type::I32));
        assert!(matches!(&types[1], Type::Slice(element, false) if **element == Type::I32));
        assert!(matches!(&types[2], Type::Slice(element, true) if **element == Type::I32));
        assert!(matches!(&types[3], Type::Ref(element, true) if **element == Type::I32));
    }

    #[test]
    fn array_literals_and_indexing_parse() {
        let stmts =
            Parser::new("fn f(){let a = [1, 2, 3]; let b = [0; 4]; a[1];}").parse().unwrap();
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &stmts[0] else {
            panic!("expected fn");
        };
        let body = &function.body.statements;

        let Statement::Let(Let { value: Some(Expression::Array { elements, .. }), .. }) = &body[0]
        else {
            panic!("expected array literal");
        };
        assert_eq!(elements.len(), 3);

        let Statement::Let(Let { value: Some(Expression::ArrayRepeat { count, .. }), .. }) =
            &body[1]
        else {
            panic!("expected array repeat");
        };
        assert_eq!(*count, 4);

        assert!(matches!(&body[2], Statement::Expr(Expression::Index { .. }, _)));
    }

    #[test]
    fn loop_forms_parse() {
        let stmts = Parser::new(
            "fn f(){loop {} loop 0..10 {} loop value in 0..=10 {} loop item in values { continue; }}",
        )
        .parse()
        .unwrap();
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &stmts[0] else {
            panic!("expected function");
        };

        assert!(matches!(
            function.body.statements[0],
            Statement::Loop(Loop { header: LoopHeader::Infinite, .. })
        ));
        assert!(matches!(
            function.body.statements[1],
            Statement::Loop(Loop {
                header: LoopHeader::Range { binding: None, inclusive: false, .. },
                ..
            })
        ));
        assert!(matches!(
            function.body.statements[2],
            Statement::Loop(Loop {
                header: LoopHeader::Range { binding: Some(_), inclusive: true, .. },
                ..
            })
        ));
        assert!(matches!(
            function.body.statements[3],
            Statement::Loop(Loop { header: LoopHeader::Iterable { .. }, .. })
        ));
    }

    #[test]
    fn impl_slice_receiver_parses() {
        let stmts = Parser::new("impl [T] { fn is_empty(&self): bool { self.len() == 0 } }")
            .parse()
            .unwrap();
        let Statement::Item(Item { kind: ItemKind::Impl(block), .. }) = &stmts[0] else {
            panic!("expected impl");
        };

        assert_eq!(block.name, "[]");
        assert!(
            matches!(block.receiver.value_ref(), Type::Slice(element, false) if matches!(element.as_ref(), Type::Named("T")))
        );
    }
}
