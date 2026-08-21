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
    errors: Vec<ParserError<'i>>,
    /// tokens pulled off the stream so far
    consumed: usize,
    /// whether a `{` ahead opens the body of the construct being parsed rather
    /// than a struct literal, as in the head of an `if`, `loop` or `match`
    no_struct_literal: bool,
}

/// The complete result of parsing one source file
///
/// Sound statements survive syntax errors, allowing later compiler stages and
/// editor features to proceed while every parser diagnostic is reported
#[derive(Debug)]
pub struct ParseOutput<'s> {
    pub statements: Vec<Statement<'s>>,
    pub diagnostics: Vec<ParserError<'s>>,
}

/// Where [Parser::synchronise] stops scanning after an error
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    /// the next top-level item, a stray closing brace is discarded as garbage
    Item,
    /// the next statement inside a block, a closing brace is left for the block
    Statement,
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
            errors: Vec::new(),
            consumed: 0,
            no_struct_literal: false,
        }
    }

    /// Parse `source` whose first byte sits at global offset `base` in the
    /// [`SourceMap`](crate::source_map::SourceMap) address space
    pub fn with_base(source: &'i str, base: BytePos) -> Self {
        Self {
            cursor: Lexer::with_base(source, base),
            buffer: VecDeque::with_capacity(4),
            last: None,
            errors: Vec::new(),
            consumed: 0,
            no_struct_literal: false,
        }
    }

    /// Parse every sound statement and collect every diagnostic in one pass.
    pub fn parse(mut self) -> ParseOutput<'i> {
        let statements = self.parse_items();
        ParseOutput { statements, diagnostics: self.errors }
    }

    fn parse_items(&mut self) -> Vec<Statement<'i>> {
        let mut statements = Vec::new();

        loop {
            let mark = self.mark();
            let error = match self.peek() {
                Some(Ok(token)) if token.is_kind(TokenKind::Eof) => break,
                Some(Ok(_)) => match self.parse_node::<Statement>() {
                    Ok(statement) => {
                        statements.push(statement);
                        continue;
                    },
                    Err(error) => error,
                },
                Some(Err(err)) => ParserError::new((*err).into(), err.span),
                None => break,
            };

            self.record(error);
            self.synchronise(Boundary::Item, mark);
        }

        statements
    }

    pub(crate) fn in_construct_head<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, ParserError<'i>>,
    ) -> Result<T, ParserError<'i>> {
        self.with_struct_literal(false, parse)
    }

    pub(crate) fn in_delimiter<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, ParserError<'i>>,
    ) -> Result<T, ParserError<'i>> {
        self.with_struct_literal(true, parse)
    }

    #[inline]
    pub(crate) const fn struct_literal_allowed(&self) -> bool {
        !self.no_struct_literal
    }

    fn with_struct_literal<T>(
        &mut self,
        allowed: bool,
        parse: impl FnOnce(&mut Self) -> Result<T, ParserError<'i>>,
    ) -> Result<T, ParserError<'i>> {
        let previous = std::mem::replace(&mut self.no_struct_literal, !allowed);
        let parsed = parse(self);
        self.no_struct_literal = previous;

        parsed
    }

    /// Whether the `@name` ahead opens a block rather than annotating a declaration
    pub(crate) fn at_marked_block(&mut self) -> bool {
        matches!(self.peek_nth(2), Some(Ok(token)) if token.is_kind(Punct::OpenBrace))
    }

    /// The current stream position, to hand back to [Parser::synchronise_statement]
    #[inline]
    pub(crate) const fn mark(&self) -> usize {
        self.consumed
    }

    /// Record an error, unless the identical one is already there: a failure
    /// surfaces twice when a block reports it and the enclosing item rethrows it
    pub(crate) fn record(&mut self, error: ParserError<'i>) {
        if !self.errors.contains(&error) {
            self.errors.push(error);
        }
    }

    /// Skip tokens until the next statement inside the enclosing block
    pub(crate) fn synchronise_statement(&mut self, mark: usize) {
        self.synchronise(Boundary::Statement, mark);
    }

    /// Skip tokens until the next plausible `boundary`
    fn synchronise(&mut self, boundary: Boundary, mark: usize) {
        let stalled = self.consumed == mark;
        let mut depth = 0usize;
        let mut consumed = 0usize;

        loop {
            let token = match self.take_token() {
                Some(Ok(token)) => token,
                Some(Err(error)) => {
                    self.record(ParserError::new(error.into(), error.span));
                    continue;
                },
                None => return,
            };
            self.last = Some(token.span);

            let settled = !stalled || consumed > 0;
            match token.kind {
                TokenKind::Eof => return self.push_back(token),
                TokenKind::Punct(Punct::OpenBrace) => depth += 1,
                TokenKind::Punct(Punct::CloseBrace) if depth > 0 => depth -= 1,
                TokenKind::Punct(Punct::CloseBrace) if boundary == Boundary::Statement => {
                    return self.push_back(token);
                },
                TokenKind::Punct(Punct::Semicolon)
                    if depth == 0 && boundary == Boundary::Statement =>
                {
                    return;
                },
                _ if depth == 0 && settled && boundary.starts_at(&token) => {
                    return self.push_back(token);
                },
                _ => {},
            }

            consumed += 1;
        }
    }

    /// Pull the next item off the stream, counting it as consumed
    #[inline]
    fn take_token(&mut self) -> Option<Result<Token<'i>, LexError<'i>>> {
        let token = self.buffer.pop_front().or_else(|| self.cursor.next());
        if token.is_some() {
            self.consumed += 1;
        }

        token
    }

    /// Return `token` to the front of the stream, undoing its consumption
    #[inline]
    pub(crate) fn push_back(&mut self, token: Token<'i>) {
        assert!(self.consumed > 0, "a token can only be pushed back after it was consumed");

        self.buffer.push_front(Ok(token));
        self.consumed -= 1;
    }

    fn parse_node<N: Parsable<'i>>(&mut self) -> Result<N, ParserError<'i>> {
        N::parse(self)
    }

    #[inline(always)]
    pub fn peek(&mut self) -> Option<&Result<Token<'i>, LexError<'i>>> {
        self.skip_lexical_errors();
        if self.buffer.is_empty()
            && let Some(t) = self.cursor.next()
        {
            self.buffer.push_back(t);
        }

        self.buffer.front()
    }

    /// Record and drop the lexical errors at the head of the stream, so the rest
    /// of a parse only ever sees real tokens and can keep going
    fn skip_lexical_errors(&mut self) {
        loop {
            if self.buffer.is_empty() {
                match self.cursor.next() {
                    Some(token) => self.buffer.push_back(token),
                    None => return,
                }
            }

            let Some(Err(error)) = self.buffer.front().copied() else {
                return;
            };
            self.buffer.pop_front();
            self.record(ParserError::new(error.into(), error.span));
        }
    }

    #[inline(always)]
    pub fn peek_nth(&mut self, n: usize) -> Option<Result<Token<'i>, LexError<'i>>> {
        self.skip_lexical_errors();
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
        self.skip_lexical_errors();
        let token = self.take_token();
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

    /// A statement terminator, recovered when it is absent
    pub(crate) fn expect_semicolon(&mut self) -> Span {
        if let Ok(true) = self.consume_token(Punct::Semicolon) {
            return self.last_span().unwrap_or_default();
        }

        let at = self.last_span().map_or_else(BytePos::default, |span| span.end);
        let found = match self.peek() {
            Some(Ok(token)) => token.kind,
            _ => TokenKind::Eof,
        };

        self.record(ParserError::new(
            ParseErrorKind::Expected { expected: TokenKind::Punct(Punct::Semicolon), found },
            Span::new(at, at),
        ));

        Span::new(at, at)
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
            TokenKind::Integer(n) => Ok(n),
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

    pub(crate) fn is_static_decl(&mut self) -> bool {
        match self.peek_nth(0) {
            Some(Ok(t)) if t.is_kind(Keyword::Static) => true,
            Some(Ok(t)) if t.is_kind(Keyword::Pub) => {
                matches!(self.peek_nth(1), Some(Ok(t2)) if t2.is_kind(Keyword::Static))
            },
            _ => false,
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

impl<'i> ParseOutput<'i> {
    pub fn unwrap(self) -> Vec<Statement<'i>> {
        self.expect("source did not parse")
    }

    pub fn expect(self, message: &str) -> Vec<Statement<'i>> {
        assert!(self.diagnostics.is_empty(), "{message}: {:?}", self.diagnostics);
        self.statements
    }

    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub fn unwrap_err(self) -> Vec<ParserError<'i>> {
        assert!(!self.diagnostics.is_empty(), "source parsed without diagnostics");
        self.diagnostics
    }
}

impl Boundary {
    /// Keywords that open a statement but never a top-level item
    const STATEMENT: &[Keyword] =
        &[Keyword::Let, Keyword::If, Keyword::Match, Keyword::Loop, Keyword::Return];

    fn starts_at(self, token: &Token<'_>) -> bool {
        opens_item(token)
            || token.is_fn_start()
            || (self == Self::Statement
                && Self::STATEMENT.iter().any(|&keyword| token.is_kind(keyword)))
    }
}

pub(crate) fn opens_item(token: &Token<'_>) -> bool {
    /// Keywords that can only open a top-level item
    const ITEM: &[Keyword] = &[
        Keyword::Fn,
        Keyword::Pub,
        Keyword::Struct,
        Keyword::Enum,
        Keyword::Impl,
        Keyword::Interface,
        Keyword::Use,
    ];

    // `@` only reaches a boundary at depth zero, where a pattern binding cannot appear
    token.is_kind(Punct::At) || ITEM.iter().any(|&keyword| token.is_kind(keyword))
}

#[cfg(test)]
mod tests {
    use crate::{
        lexer::{Spanned, token::BytePos},
        parser::{
            expression::{BinaryOperator, Expression, UnaryOperator},
            statement::{Item, ItemKind, Let, Loop, LoopHeader, Pattern, PatternLit, Return, Type},
        },
    };

    use super::*;

    fn recovered(source: &str) -> (Vec<Statement<'_>>, Vec<ParserError<'_>>) {
        let parsed = Parser::new(source).parse();
        (parsed.statements, parsed.diagnostics)
    }

    #[test]
    fn a_trailing_comma_closes_every_comma_separated_list() {
        let sources = [
            "fn takes(a: i32, b: i32,) {}",
            "fn calls() { takes(1, 2,); }",
            "struct Point { x: i32, y: i32, }",
            "fn literal() { let p = Point { x: 1, y: 2, }; }",
            "fn array() { let xs = [1, 2, 3,]; }",
            "fn method(&self, a: i32,) {}",
        ];

        for source in sources {
            assert!(
                Parser::new(source).parse().is_ok(),
                "a trailing comma must be accepted everywhere: {source}"
            );
        }
    }

    #[test]
    fn a_prefix_operator_binds_looser_than_a_cast() {
        let statements = Parser::new("fn main() { let n = *x as u32; }").parse().unwrap();

        // `*x as u32` is `*(x as u32)`, which is what the formatter re-derives
        // its parentheses from
        let printed = format!("{statements:?}");
        let cast_inside_deref = printed.find("Unary").expect("a deref is parsed")
            < printed.find("Cast").expect("a cast is parsed");

        assert!(cast_inside_deref, "the cast must sit inside the dereference: {printed}");
    }

    fn item_names<'i>(statements: &[Statement<'i>]) -> Vec<&'i str> {
        statements
            .iter()
            .filter_map(|statement| match statement {
                Statement::Item(Item { kind: ItemKind::Fn(function), .. }) => Some(function.name),
                Statement::Item(Item { kind: ItemKind::Struct(declaration), .. }) => {
                    Some(declaration.name)
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn recovery_reports_every_broken_item() {
        let (statements, errors) = recovered(
            "fn a(): i32 { 1 }
             fn b(: i32 { 2 }
             fn c(): i32 { 3 }
             fn d(] { }
             fn e(): i32 { 5 }",
        );

        assert_eq!(errors.len(), 2, "one error per broken item: {errors:?}");
        assert_eq!(item_names(&statements), ["a", "c", "e"], "the sound items survive");
    }

    #[test]
    fn recovery_keeps_sibling_statements_in_a_block() {
        let (statements, errors) =
            recovered("fn main() { let a = 1; let b = ; let c = 3; let d = ; let e = 5; }");

        assert_eq!(errors.len(), 2, "both broken bindings are reported: {errors:?}");
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &statements[0] else {
            panic!("expected fn main, got {statements:?}");
        };

        let names: Vec<_> = function
            .body
            .statements
            .iter()
            .filter_map(|statement| match statement {
                Statement::Let(Let { name, .. }) => Some(*name),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["a", "c", "e"], "the sound bindings survive");
    }

    #[test]
    fn recovery_surfaces_a_lexical_error_and_carries_on() {
        let (statements, errors) = recovered("fn a() { let x = 1 % 2; }\nstruct P { x: i32 }");

        assert!(
            errors.iter().any(|error| matches!(error.kind, ParseErrorKind::Lexical(_))),
            "the bad character is reported: {errors:?}"
        );
        assert!(item_names(&statements).contains(&"P"), "the later struct still parses");
    }

    #[test]
    fn recovery_closes_an_unterminated_block() {
        let (statements, errors) = recovered("fn main() { let x = 1;");

        assert!(
            errors.iter().any(|error| error.kind == ParseErrorKind::UnexpectedEof),
            "the missing brace is reported: {errors:?}"
        );
        assert_eq!(item_names(&statements), ["main"], "the partial function survives");
    }

    #[test]
    fn an_unterminated_block_does_not_swallow_the_next_item() {
        let (statements, errors) =
            recovered("fn unfinished() { let z = 1; let w =\n\nfn main() { let ok = 2; }");

        assert!(!errors.is_empty(), "the broken binding is reported");
        assert_eq!(
            item_names(&statements),
            ["unfinished", "main"],
            "both functions survive as separate items"
        );
    }

    #[test]
    fn a_half_typed_field_access_keeps_its_block() {
        let (statements, errors) =
            recovered("fn a() { p.\n}\nfn b() { let ok = 2; }\nstruct P { x: i32 }");

        assert!(
            errors
                .iter()
                .any(|error| matches!(error.kind, ParseErrorKind::ExpectedIdentifier { .. })),
            "the missing field name is reported: {errors:?}"
        );
        assert_eq!(
            item_names(&statements),
            ["a", "b", "P"],
            "a `.` with nothing after it must not consume the closing brace and \
             turn the rest of the file into one runaway item"
        );
    }

    fn body_expression(source: &str) -> crate::parser::expression::Expression<'_> {
        let parsed = Parser::new(source).parse();
        assert!(parsed.diagnostics.is_empty(), "{source:?}: {:?}", parsed.diagnostics);

        let Some(Statement::Item(Item { kind: ItemKind::Fn(function), .. })) =
            parsed.statements.into_iter().next()
        else {
            panic!("{source:?} must parse as one function");
        };

        match function.body.statements.into_iter().next() {
            Some(Statement::Expr(expr, _)) => expr,
            other => panic!("{source:?} must have one expression body, got {other:?}"),
        }
    }

    #[test]
    fn a_struct_field_may_be_written_as_its_own_name() {
        use crate::parser::expression::Expression;

        let Expression::Struct { fields, .. } = body_expression("fn main() { Point { x, y: 2 } }")
        else {
            panic!("a shorthand field still opens a struct literal");
        };

        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "x");
        assert_eq!(
            fields[0].value,
            Expression::Identifier("x", fields[0].span),
            "the shorthand stands for the binding of the same name, spanned where              it is written so it still resolves and type-checks like any other value"
        );
        assert_eq!(fields[1].name, "y");
        assert_eq!(fields[1].value, Expression::Integer(2, fields[1].value.span()));
    }

    #[test]
    fn a_lone_shorthand_field_still_opens_a_literal() {
        use crate::parser::expression::Expression;

        let Expression::Struct { fields, .. } = body_expression("fn main() { Point { x } }") else {
            panic!("`Point {{ x }}` is a struct literal, not a name beside a block");
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "x");
    }

    #[test]
    fn a_construct_head_reads_a_brace_as_its_body() {
        use crate::parser::expression::Expression;

        let parsed = Parser::new("fn main() { if flag { x } }").parse();
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

        let Some(Statement::Item(Item { kind: ItemKind::Fn(function), .. })) =
            parsed.statements.into_iter().next()
        else {
            panic!("expected fn main");
        };
        assert!(
            matches!(function.body.statements.first(), Some(Statement::If(_))),
            "got {:?}",
            function.body.statements
        );

        let parsed = Parser::new("fn main() { if take(Point { x, y }) { z } }").parse();
        assert!(
            parsed.diagnostics.is_empty(),
            "a literal inside a call in a construct head still parses: {:?}",
            parsed.diagnostics
        );

        let Expression::Call { args, .. } = body_expression("fn main() { take(Point { x, y }) }")
        else {
            panic!("expected a call");
        };
        assert!(matches!(args.first(), Some(Expression::Struct { .. })), "got {args:?}");
    }

    #[test]
    fn recovery_terminates_on_pathological_input() {
        for source in ["}}}", "fn", "pub pub pub", "{{{{", "fn f(){{{{", "))))", "@@@@"] {
            let (_, errors) = recovered(source);
            assert!(!errors.is_empty(), "{source:?} must report something");
        }
    }

    #[test]
    fn parse_reports_every_error() {
        let errors = Parser::new("fn a(: i32 { 2 }\nfn b(] { }").parse().unwrap_err();
        assert_eq!(errors.len(), 2, "got {errors:?}");
        assert!(
            errors
                .iter()
                .all(|error| matches!(error.kind, ParseErrorKind::ExpectedIdentifier { .. }))
        );
    }

    #[test]
    fn missing_semicolon() {
        let err = Parser::new("let value = 1").parse().unwrap_err().remove(0);

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
    fn a_missing_semicolon_keeps_the_statement_and_its_neighbours() {
        let (statements, diagnostics) =
            recovered("fn main(): i32 {\n    let x = 1\n    return x;\n}\n");

        assert_eq!(diagnostics.len(), 1, "got {diagnostics:?}");
        assert!(matches!(
            diagnostics[0].kind,
            ParseErrorKind::Expected { expected: TokenKind::Punct(Punct::Semicolon), .. }
        ));

        let [Statement::Item(Item { kind: ItemKind::Fn(function), .. })] = statements.as_slice()
        else {
            panic!("expected one function, got {statements:?}")
        };

        assert_eq!(
            function.body.statements.len(),
            2,
            "recovery keeps both the let and the return: {:?}",
            function.body.statements
        );
    }

    #[test]
    fn every_missing_semicolon_in_a_block_is_reported() {
        let (_, diagnostics) =
            recovered("fn main(): i32 {\n    let a = 1\n    let b = 2\n    return a;\n}\n");

        assert_eq!(diagnostics.len(), 2, "got {diagnostics:?}");
    }

    #[test]
    fn missing_expression() {
        let err = Parser::new("let value = ;").parse().unwrap_err().remove(0);
        assert_eq!(
            err.kind,
            ParseErrorKind::ExpectedExpression { found: TokenKind::Punct(Punct::Semicolon) }
        );

        assert_eq!(err.span.start.0, 12);
        assert_eq!(err.span.end.0, 13);
    }

    #[test]
    fn invalid_identifier() {
        let err = Parser::new("let 123: i32 = 1;").parse().unwrap_err().remove(0);
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
    fn interface_associated_constants_are_parsed_and_documented() {
        let statements = Parser::new(
            r#"
            interface Buffer {
                /// Size in bytes
                const SIZE: uptr;
            }
            "#,
        )
        .parse()
        .unwrap();

        let Statement::Item(Item { kind: ItemKind::Interface(interface), .. }) = &statements[0]
        else {
            panic!("expected interface");
        };

        assert_eq!(interface.constants.len(), 1);
        assert_eq!(interface.constants[0].name, "SIZE");
        assert_eq!(interface.member_docs[0].0, interface.constants[0].span);
        assert_eq!(interface.member_docs[0].1.as_ref(), [" Size in bytes"]);
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
        .unwrap_err()
        .remove(0);

        assert!(matches!(err.kind, ParseErrorKind::ExpectedIdentifier { .. }));
    }

    #[test]
    fn invalid_and_valid_return() {
        let err = Parser::new("return +1;").parse().unwrap_err().remove(0);
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
    fn compound_assignment_keeps_its_operator() {
        let statements = Parser::new("a += b;").parse().unwrap();
        let [Statement::Expr(Expression::CompoundAssignment { target, operator, value, .. }, _)] =
            statements.as_slice()
        else {
            panic!("expected a compound assignment, got {statements:?}");
        };

        assert_eq!(**target, Expression::Identifier("a", Span::new(BytePos(0), BytePos(1))));
        assert_eq!(*operator, BinaryOperator::Add);
        assert_eq!(**value, Expression::Identifier("b", Span::new(BytePos(5), BytePos(6))));
    }

    #[test]
    fn compound_assignment_binds_looser_than_arithmetic() {
        let statements = Parser::new("a += b * c;").parse().unwrap();
        let [Statement::Expr(Expression::CompoundAssignment { value, .. }, _)] =
            statements.as_slice()
        else {
            panic!("expected a compound assignment, got {statements:?}");
        };

        assert!(matches!(**value, Expression::Binary { operator: BinaryOperator::Mul, .. }));
    }

    #[test]
    fn every_compound_operator_parses() {
        let cases = [
            ("a += b;", BinaryOperator::Add),
            ("a -= b;", BinaryOperator::Sub),
            ("a *= b;", BinaryOperator::Mul),
            ("a /= b;", BinaryOperator::Div),
            ("a &= b;", BinaryOperator::BitAnd),
            ("a |= b;", BinaryOperator::BitOr),
            ("a ^= b;", BinaryOperator::BitXor),
            ("a <<= b;", BinaryOperator::Shl),
            ("a >>= b;", BinaryOperator::Shr),
        ];

        for (source, expected) in cases {
            let statements = Parser::new(source).parse().unwrap();
            let [Statement::Expr(Expression::CompoundAssignment { operator, .. }, _)] =
                statements.as_slice()
            else {
                panic!("expected a compound assignment for {source:?}, got {statements:?}");
            };

            assert_eq!(*operator, expected, "wrong operator for {source:?}");
        }
    }

    #[test]
    fn compound_assignment_rejects_a_non_place_target() {
        let errors = Parser::new("f() += 1;").parse().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| matches!(error.kind, ParseErrorKind::InvalidAssignmentTarget)),
            "expected an invalid-target error, got {errors:?}"
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
        let source = include_str!("../../../tests/single/add.nyx");
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
    fn expression_body_becomes_a_tail_expression() {
        let statements = Parser::new("fn is_even(n: i32): bool = n & 1 == 0;").parse().unwrap();
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &statements[0] else {
            panic!("expected a function item");
        };

        assert!(matches!(function.return_type.as_ref().map(Spanned::value), Some(Type::Bool)));
        assert!(matches!(
            function.body.statements.as_slice(),
            [Statement::Expr(Expression::Binary { operator: BinaryOperator::Eq, .. }, _)]
        ));
    }

    #[test]
    fn expression_body_requires_a_return_type() {
        let error =
            Parser::new("fn double(value: i32) = value * 2;").parse().unwrap_err().remove(0);

        assert_eq!(error.kind, ParseErrorKind::ExpressionBodyNeedsReturnType);
        assert_eq!(error.span, Span::new(BytePos(22), BytePos(23)));
    }

    #[test]
    fn expression_body_requires_a_semicolon() {
        let error = Parser::new("fn answer(): i32 = 42").parse().unwrap_err().remove(0);

        assert_eq!(
            error.kind,
            ParseErrorKind::Expected {
                expected: TokenKind::Punct(Punct::Semicolon),
                found: TokenKind::Eof,
            }
        );
    }

    #[test]
    fn methods_and_interface_defaults_accept_expression_bodies() {
        let statements = Parser::new(
            r#"
            interface Named {
                fn value(&self): i32 = 40;
            }

            struct Number { value: i32 }

            impl Number {
                fn doubled(&self): i32 = self.value * 2;
            }
            "#,
        )
        .parse()
        .unwrap();

        let Statement::Item(Item { kind: ItemKind::Interface(interface), .. }) = &statements[0]
        else {
            panic!("expected an interface item");
        };
        assert!(interface.methods[0].body.is_some());

        let Statement::Item(Item { kind: ItemKind::Impl(implementation), .. }) = &statements[2]
        else {
            panic!("expected an impl item");
        };
        assert_eq!(implementation.methods[0].body.statements.len(), 1);
    }

    #[test]
    fn expression_body_can_follow_a_trailing_where_bound() {
        let statements = Parser::new("fn identity<T>(value: T): T where T: Copy, = value;")
            .parse()
            .unwrap();
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &statements[0] else {
            panic!("expected a function item");
        };

        assert_eq!(function.generics.len(), 1);
        assert_eq!(function.generics[0].bounds.len(), 1);
        assert_eq!(function.body.statements.len(), 1);
    }

    #[test]
    fn branching_expressions_can_be_function_bodies() {
        let statements = Parser::new(
            r#"
            fn absolute(value: i32): i32 = if value < 0 { -value } else { value };
            fn classify(value: i32): i32 = match value { 0 -> 1, _ -> value, };
            "#,
        )
        .parse()
        .unwrap();

        let Statement::Item(Item { kind: ItemKind::Fn(absolute), .. }) = &statements[0] else {
            panic!("expected a function item");
        };
        assert!(matches!(absolute.body.statements.as_slice(), [Statement::If(_)]));

        let Statement::Item(Item { kind: ItemKind::Fn(classify), .. }) = &statements[1] else {
            panic!("expected a function item");
        };
        assert!(matches!(classify.body.statements.as_slice(), [Statement::Match(_)]));
    }

    #[test]
    fn parse_inference_file() {
        let source = include_str!("../../../tests/single/inference.nyx");
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
    fn adjacent_ampersands_are_nested_references_in_types() {
        let stmts = Parser::new("fn f(value: &&i32){}").parse().unwrap();
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &stmts[0] else {
            panic!("expected fn")
        };
        assert!(matches!(
            function.params[0].typ.value(),
            Type::Ref(outer, false)
                if matches!(&*outer, Type::Ref(inner, false) if **inner == Type::I32)
        ));
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

    #[test]
    fn struct_pattern_with_rest() {
        let pattern = Pattern::parse(&mut Parser::new("Colour { r, g: 0, .. }")).unwrap();
        let Pattern::Struct { name, fields, rest } = pattern else {
            panic!("expected struct pattern");
        };

        assert_eq!(name, "Colour");
        assert!(rest);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "r");
        assert!(fields[0].pattern.is_none());
        assert_eq!(fields[1].name, "g");
        assert!(matches!(
            fields[1].pattern.as_ref().unwrap().value_ref(),
            Pattern::Literal(PatternLit::Int(0))
        ));
    }

    #[test]
    fn range_patterns() {
        let pattern = Pattern::parse(&mut Parser::new("1..=5")).unwrap();
        assert_eq!(
            pattern,
            Pattern::Range {
                start: PatternLit::Int(1),
                end: PatternLit::Int(5),
                inclusive: true
            }
        );

        let pattern = Pattern::parse(&mut Parser::new("'a'..'z'")).unwrap();
        assert_eq!(
            pattern,
            Pattern::Range {
                start: PatternLit::Char('a'),
                end: PatternLit::Char('z'),
                inclusive: false
            }
        );
    }

    #[test]
    fn range_pattern_requires_literal_endpoint() {
        let err = Pattern::parse(&mut Parser::new("1..=x")).unwrap_err();
        assert!(matches!(err.kind, ParseErrorKind::ExpectedPatternLiteral { .. }));
    }

    #[test]
    fn at_binding_pattern() {
        let pattern = Pattern::parse(&mut Parser::new("id @ 3..=7")).unwrap();
        let Pattern::Binding { name, sub } = pattern else {
            panic!("expected binding pattern");
        };

        assert_eq!(name, "id");
        assert!(matches!(sub.value_ref(), Pattern::Range { inclusive: true, .. }));
    }

    #[test]
    fn a_marker_precedes_the_visibility_and_modifier_keywords() {
        let statements = Parser::new("@unsafe pub inline fn go() {}").parse().unwrap();
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &statements[0] else {
            panic!("expected a function item");
        };

        assert!(function.is_unsafe());
        assert!(function.is_pub);
        assert!(function.inline);
    }

    #[test]
    fn an_interface_requirement_carries_inline() {
        let src = "interface Speak { fn tone(&self): i32; inline fn loud(&self): i32 { 1 } }";
        let statements = Parser::new(src).parse().unwrap();
        let Statement::Item(Item { kind: ItemKind::Interface(interface), .. }) = &statements[0]
        else {
            panic!("expected an interface item");
        };

        assert!(!interface.methods[0].inline);
        assert!(interface.methods[1].inline);
    }

    #[test]
    fn an_interface_separates_const_requirements_from_constants() {
        let src = "interface Bounded { const LIMIT: i32; const fn peak(&self): i32; }";
        let statements = Parser::new(src).parse().unwrap();
        let Statement::Item(Item { kind: ItemKind::Interface(interface), .. }) = &statements[0]
        else {
            panic!("expected an interface item");
        };

        assert_eq!(interface.constants.len(), 1);
        assert_eq!(interface.constants[0].name, "LIMIT");
        assert_eq!(interface.methods.len(), 1);
        assert!(interface.methods[0].is_const);
    }

    #[test]
    fn an_unknown_marker_is_rejected() {
        let err = Parser::new("@fast fn go() {}").parse().unwrap_err().remove(0);
        assert!(matches!(err.kind, ParseErrorKind::UnknownMarker { name: "fast" }));
    }

    #[test]
    fn markers_stack_on_one_declaration() {
        let statements =
            Parser::new("@intrinsic @unsafe pub const fn go(): i32 {}").parse().unwrap();
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &statements[0] else {
            panic!("expected a function item");
        };

        assert!(function.is_intrinsic());
        assert!(function.is_unsafe());
        assert!(function.is_const);
    }

    #[test]
    fn only_unsafe_opens_a_block() {
        let err = Parser::new("fn go() { @intrinsic { let x = 1; } }")
            .parse()
            .unwrap_err()
            .remove(0);
        assert!(matches!(err.kind, ParseErrorKind::MarkerIsNotABlock { name: "intrinsic" }));
    }

    #[test]
    fn a_marker_followed_by_a_brace_opens_a_block() {
        let statements = Parser::new("fn go() { @unsafe { let x = 1; } }").parse().unwrap();
        let Statement::Item(Item { kind: ItemKind::Fn(function), .. }) = &statements[0] else {
            panic!("expected a function item");
        };

        assert!(!function.is_unsafe(), "the block must not mark the function");
        assert!(matches!(
            function.body.statements[0],
            Statement::Unsafe { ref block, .. } if block.statements.len() == 1
        ));
    }

    #[test]
    fn a_raw_pointer_type_mirrors_a_reference() {
        let mut parser = Parser::new("*mut i32");
        let typ = parser.parse_node::<Spanned<Type>>().unwrap().value();
        assert!(matches!(typ, Type::Raw(inner, true) if *inner == Type::I32));

        let mut parser = Parser::new("*i32");
        let typ = parser.parse_node::<Spanned<Type>>().unwrap().value();
        assert!(matches!(typ, Type::Raw(inner, false) if *inner == Type::I32));
    }

    #[test]
    fn variant_with_struct_subpattern() {
        let pattern =
            Pattern::parse(&mut Parser::new("Msg::ChangeColour(Colour { r, g, b })")).unwrap();
        let Pattern::Variant { qualifier: Some("Msg"), name: "ChangeColour", sub: Some(sub) } =
            pattern
        else {
            panic!("expected qualified variant pattern");
        };

        assert!(matches!(
            sub.value_ref(),
            Pattern::Struct { name: "Colour", fields, rest: false } if fields.len() == 3
        ));
    }

    #[test]
    fn enum_variants_and_struct_fields_carry_docs() {
        let source = r#"
            enum Msg {
                /// nothing to say
                Quiet,
                Loud(i32),
            }
            struct Point {
                /// the horizontal coordinate
                x: i32,
                y: i32,
            }
        "#;
        let (statements, errors) = recovered(source);
        assert!(errors.is_empty(), "a documented member parses: {errors:?}");

        let mut members = statements.iter().filter_map(|statement| match statement {
            Statement::Item(Item { kind: ItemKind::Enum(e), .. }) => {
                Some((e.member_docs.clone(), e.variants[0].name_span))
            },
            Statement::Item(Item { kind: ItemKind::Struct(s), .. }) => {
                Some((s.member_docs.clone(), s.fields[0].name_span))
            },
            _ => None,
        });

        let (docs, quiet) = members.next().expect("the enum");
        assert_eq!(docs.len(), 1, "only the documented variant is filed: {docs:?}");
        assert_eq!(docs[0], (quiet, Box::from([" nothing to say"])));

        let (docs, x) = members.next().expect("the struct");
        assert_eq!(docs.len(), 1, "only the documented field is filed: {docs:?}");
        assert_eq!(docs[0], (x, Box::from([" the horizontal coordinate"])));
    }

    #[test]
    fn declarations_span_their_name_alone() {
        let source = "fn add(): i32 { 1 }\nstruct Point { x: i32 }\nenum Msg { Quiet }";
        let (statements, errors) = recovered(source);
        assert!(errors.is_empty(), "{errors:?}");

        let named: Vec<_> = statements
            .iter()
            .filter_map(|statement| match statement {
                Statement::Item(Item { kind, .. }) => match kind {
                    ItemKind::Fn(f) => Some(f.name_span),
                    ItemKind::Struct(s) => Some(s.name_span),
                    ItemKind::Enum(e) => Some(e.name_span),
                    _ => None,
                },
                _ => None,
            })
            .map(|span| &source[span.start.0 as usize..span.end.0 as usize])
            .collect();

        assert_eq!(named, ["add", "Point", "Msg"], "the name alone, not the keyword");
    }
}
