//! Translation of the parser AST into layout-independent [documents](Doc)
//!
//! The printer decides style, [render](crate::render) only picks between the
//! layouts a [Doc] already encodes

use crate::doc::Doc;
use crate::format::{FormatError, FormatOptions};
use crate::trivia::{Piece, Trivia, opens_with_blank_line};
use frontend::lexer::Spanned;
use frontend::lexer::token::{BytePos, Punct, Span};
use frontend::parser::expression::{Expression, Precedence, StructField};
use frontend::parser::statement::{
    Block, Const, Else, Function, If, Impl, ImplType, Interface, InterfaceConst, InterfaceMethod,
    InterfaceType, Item, ItemKind, Let, Loop, LoopHeader, MODIFIER_ORDER, Parameter, Receiver,
    Return, Statement, Static, Struct, Type, UseDecl, UseItems,
};

/// Walks the AST once, emitting a [Doc] and tracking the comments it consumed
pub struct Printer<'src> {
    source: &'src str,
    trivia: Trivia<'src>,
    options: FormatOptions,
    printed_comments: usize,
}

/// Where a run of statements sits, which decides how they are separated
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    /// items at module level, always parted by a blank line
    Module,
    /// statements inside braces, where a blank line is kept only if written
    Block,
}

/// A member of an `impl` block
#[derive(Debug, Clone, Copy)]
enum Member<'a, 'src> {
    Method(&'a Function<'src>),
    Constant(&'a Const<'src>),
    Requirement(&'a InterfaceMethod<'src>),
    RequiredConstant(&'a InterfaceConst<'src>),
    Association(&'a ImplType<'src>),
    RequiredAssociation(&'a InterfaceType<'src>),
}

impl<'src> Printer<'src> {
    pub fn new(source: &'src str, options: FormatOptions) -> Self {
        Self {
            source,
            trivia: Trivia::scan(source),
            options,
            printed_comments: 0,
        }
    }

    #[inline(always)]
    pub fn into_document(
        mut self,
        statements: &[Statement<'src>],
    ) -> Result<Doc<'src>, FormatError> {
        let body = self.statements(statements, Region::Module)?;
        let scanned = self.trivia.comment_count();

        match self.printed_comments == scanned {
            true => Ok(Doc::concat([body, Doc::hard_line()])),
            false => Err(FormatError::CommentDropped { printed: self.printed_comments, scanned }),
        }
    }

    fn statements(
        &mut self,
        statements: &[Statement<'src>],
        region: Region,
    ) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::with_capacity(statements.len() * 3);

        for (index, statement) in statements.iter().enumerate() {
            let span = statement_span(statement);
            let start = self.declaration_start(span);
            let leading = self.trivia.leading(start);

            if index > 0 {
                parts.push(Doc::hard_line());
                let blank = match region {
                    Region::Module => match both_imports(&statements[index - 1], statement) {
                        true => opens_with_blank_line(leading),
                        false => true,
                    },
                    Region::Block => opens_with_blank_line(leading),
                };

                if blank {
                    parts.push(Doc::hard_line());
                }
            }

            for comment in self.comments_before(start) {
                parts.push(Doc::text(comment));
                parts.push(Doc::hard_line());
            }

            parts.push(self.statement(statement)?);
            self.push_trailing_comment(&mut parts, span);
        }

        Ok(Doc::concat(parts))
    }

    /// where a declaration really begins, ahead of the markers and modifiers
    fn declaration_start(&self, span: Span) -> BytePos {
        let mut start = span.start.offset();

        loop {
            let before = self.source[..start].trim_end();
            let word = match before.rfind(char::is_whitespace) {
                Some(at) => &before[at + 1..],
                None => before,
            };

            match word {
                "pub" | "inline" | "const" | "@unsafe" | "@intrinsic" => {
                    start = before.len() - word.len();
                },
                _ => return BytePos(start as u32),
            }
        }
    }

    fn comments_before(&mut self, at: BytePos) -> Vec<&'src str> {
        let comments: Vec<_> = self
            .trivia
            .leading(at)
            .iter()
            .filter_map(|piece| match piece {
                Piece::LineComment(text) => Some(*text),
                Piece::Newlines(_) => None,
            })
            .collect();

        self.printed_comments += comments.len();
        comments
    }

    /// comments sitting between the last statement of a block and its closing
    /// brace, which belong to no statement
    fn dangling(&mut self, closing_brace: Span) -> Doc<'src> {
        let before_brace = BytePos(closing_brace.end.0.saturating_sub(1));
        let mut parts = Vec::new();

        for comment in self.comments_before(before_brace) {
            parts.push(Doc::hard_line());
            parts.push(Doc::text(comment));
        }

        Doc::concat(parts)
    }

    fn push_trailing_comment(&mut self, parts: &mut Vec<Doc<'src>>, span: Span) {
        if let Some(comment) = self.trivia.trailing(span.end) {
            parts.push(Doc::text(" "));
            parts.push(Doc::text(comment));
            self.printed_comments += 1;
        }
    }

    fn statement(&mut self, statement: &Statement<'src>) -> Result<Doc<'src>, FormatError> {
        match statement {
            Statement::Let(binding) => self.binding(binding),
            Statement::Return(returned) => self.returned(returned),
            Statement::If(conditional) => self.conditional(conditional),
            Statement::Loop(repeated) => self.repetition(repeated),
            Statement::Block(block) => self.block(block),
            Statement::Break(_) => Ok(Doc::text("break;")),
            Statement::Continue(_) => Ok(Doc::text("continue;")),
            Statement::Expr(expr, _) => {
                let printed = self.expression(expr)?;

                match self.is_terminated(expr.span().end) {
                    true => Ok(Doc::concat([printed, Doc::text(";")])),
                    false => Ok(printed),
                }
            },
            Statement::Item(item) => self.item(item),
            other => Err(FormatError::Unsupported { span: statement_span(other) }),
        }
    }

    fn item(&mut self, item: &Item<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::new();

        for line in &item.docs {
            parts.push(Doc::text("///"));
            parts.push(Doc::text(*line));
            parts.push(Doc::hard_line());
        }

        match &item.kind {
            ItemKind::Fn(function) => parts.push(self.function(function)?),
            ItemKind::Struct(declaration) => parts.push(self.structure(declaration)?),
            ItemKind::Const(constant) => parts.push(self.constant(constant)?),
            ItemKind::Static(item) => parts.push(self.static_item(item)?),
            ItemKind::Impl(block) => parts.push(self.implementation(block)?),
            ItemKind::Interface(interface) => parts.push(self.interface(interface)?),
            ItemKind::Use(declaration) => parts.push(import(declaration)),
            other => return Err(FormatError::Unsupported { span: item_span(other) }),
        }

        Ok(Doc::concat(parts))
    }

    fn function(&mut self, function: &Function<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::new();

        for marker in function.markers.iter() {
            parts.push(Doc::text(Punct::At.as_str()));
            parts.push(Doc::text(marker.as_str()));
            parts.push(Doc::hard_line());
        }

        for (present, keyword) in function.modifiers().into_iter().zip(MODIFIER_ORDER) {
            if present {
                parts.push(Doc::text(keyword.as_str()));
                parts.push(Doc::text(" "));
            }
        }

        parts.push(self.signature(
            function.name,
            function.name_span,
            function.receiver,
            &function.params,
            function.return_type.as_ref(),
            function.body.span.start,
        ));
        parts.push(self.body(&function.body)?);

        Ok(Doc::concat(parts))
    }

    /// `fn name<...>(...): Type where ...` all a declaration holds before its body
    fn signature(
        &mut self,
        name: &'src str,
        name_span: Span,
        receiver: Option<Receiver>,
        params: &[Parameter<'src>],
        return_type: Option<&Spanned<Type<'src>>>,
        body_start: BytePos,
    ) -> Doc<'src> {
        let mut parts =
            vec![Doc::text("fn "), Doc::text(name), Doc::text(self.generics_at(name_span.end))];

        let mut parameters: Vec<_> = params
            .iter()
            .map(|parameter| {
                let mutable = match parameter.mutable {
                    true => "mut ",
                    false => "",
                };

                Doc::concat([
                    Doc::text(mutable),
                    Doc::text(parameter.name),
                    Doc::text(": "),
                    Doc::text(self.slice(parameter.typ.span())),
                ])
            })
            .collect();

        if let Some(receiver) = receiver {
            parameters.insert(0, Doc::text(receiver_name(receiver)));
        }

        parts.push(self.delimited("(", parameters, ")"));

        let signature_end = match return_type {
            Some(returned) => {
                parts.push(Doc::text(": "));
                parts.push(Doc::text(self.slice(returned.span())));
                returned.span().end
            },
            None => match params.last() {
                Some(parameter) => parameter.span.end,
                None => name_span.end,
            },
        };

        parts.push(self.where_clause(signature_end, body_start));
        Doc::concat(parts)
    }

    fn interface(&mut self, interface: &Interface<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::new();

        if interface.is_pub {
            parts.push(Doc::text("pub "));
        }

        parts.push(Doc::text("interface "));
        parts.push(Doc::text(interface.name));
        parts.push(Doc::text(self.generics_at(interface.name_span.end)));

        for (index, superinterface) in interface.superinterfaces.iter().enumerate() {
            parts.push(Doc::text(match index {
                0 => ": ",
                _ => " + ",
            }));
            parts.push(Doc::text(*superinterface));
        }

        parts.push(Doc::text(" {"));

        let mut members: Vec<_> = interface
            .methods
            .iter()
            .map(Member::Requirement)
            .chain(interface.constants.iter().map(Member::RequiredConstant))
            .chain(interface.types.iter().map(Member::RequiredAssociation))
            .collect();

        members.sort_by_key(|member| member.span().start);

        let body = match members.is_empty() {
            true => Doc::Empty,
            false => self.members(&members, &interface.member_docs)?,
        };

        parts.push(Doc::indent(
            self.indent_width(),
            Doc::concat([body, self.dangling(interface.span)]),
        ));
        parts.push(Doc::hard_line());
        parts.push(Doc::text("}"));

        Ok(Doc::concat(parts))
    }

    /// a method an interface requires, which carries a body only when it supplies a default
    fn interface_method(
        &mut self,
        method: &InterfaceMethod<'src>,
    ) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::new();

        for marker in method.markers.iter() {
            parts.push(Doc::text(Punct::At.as_str()));
            parts.push(Doc::text(marker.as_str()));
            parts.push(Doc::hard_line());
        }

        if method.inline {
            parts.push(Doc::text("inline "));
        }

        if method.is_const {
            parts.push(Doc::text("const "));
        }

        let terminator = match method.body {
            Some(ref body) => body.span.start,
            None => method.span.end,
        };

        parts.push(self.signature(
            method.name,
            method.name_span,
            method.receiver,
            &method.params,
            method.return_type.as_ref(),
            terminator,
        ));

        match method.body {
            Some(ref body) => parts.push(self.body(body)?),
            None => parts.push(Doc::text(";")),
        }

        Ok(Doc::concat(parts))
    }

    fn interface_constant(&mut self, constant: &InterfaceConst<'src>) -> Doc<'src> {
        Doc::concat([
            Doc::text("const "),
            Doc::text(constant.name),
            Doc::text(": "),
            Doc::text(self.slice(constant.typ.span())),
            Doc::text(";"),
        ])
    }

    fn associated_type(&mut self, associated: &ImplType<'src>) -> Doc<'src> {
        Doc::concat([
            Doc::text("type "),
            Doc::text(associated.name),
            Doc::text(" = "),
            Doc::text(self.slice(associated.typ.span())),
            Doc::text(";"),
        ])
    }

    fn required_associated_type(&mut self, associated: &InterfaceType<'src>) -> Doc<'src> {
        let mut parts = vec![Doc::text("type "), Doc::text(associated.name)];

        for (index, bound) in associated.bounds.iter().enumerate() {
            parts.push(Doc::text(match index {
                0 => ": ",
                _ => " + ",
            }));
            parts.push(Doc::text(self.slice(bound.span())));
        }

        parts.push(Doc::text(";"));

        Doc::concat(parts)
    }

    fn body(&mut self, block: &Block<'src>) -> Result<Doc<'src>, FormatError> {
        if !self.slice(block.span).starts_with('=') {
            return Ok(Doc::concat([Doc::text(" "), self.block(block)?]));
        }

        let [statement] = block.statements.as_slice() else {
            return Err(FormatError::Unsupported { span: block.span });
        };

        let printed = self.statement(statement)?;
        let terminated = matches!(
            statement,
            Statement::Expr(expr, _) if self.is_terminated(expr.span().end)
        );

        match terminated {
            true => Ok(Doc::concat([Doc::text(" = "), printed])),
            false => Ok(Doc::concat([Doc::text(" = "), printed, Doc::text(";")])),
        }
    }

    /// The `<...>` or `::<...>` list written at `after`
    fn generics_at(&self, after: BytePos) -> &'src str {
        let rest = &self.source[after.offset()..];
        let mut depth = 0usize;

        for (offset, character) in rest.char_indices() {
            match character {
                '<' => depth += 1,
                '>' => depth = depth.saturating_sub(1),
                // a `:` at depth zero opens a superinterface list, never a bound
                '(' | '{' | ';' | '=' | ':' if depth == 0 => return rest[..offset].trim(),
                _ => {},
            }
        }

        ""
    }

    /// The `where ...` clause between the end of a signature and its body
    fn where_clause(&self, from: BytePos, to: BytePos) -> Doc<'src> {
        let between = &self.source[from.offset()..to.offset()];

        let Some(at) = between.find("where") else {
            return Doc::Empty;
        };

        let clause = &between[at..];
        let clause = clause.split([';', '{']).next().unwrap_or(clause);

        Doc::text(format!(" {}", normalise_spaces(clause)))
    }

    fn structure(&mut self, declaration: &Struct<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::new();

        if declaration.is_pub {
            parts.push(Doc::text("pub "));
        }

        parts.push(Doc::text("struct "));
        parts.push(Doc::text(declaration.name));
        parts.push(Doc::text(self.generics_at(declaration.name_span.end)));
        parts.push(Doc::text(" {"));

        let mut fields = Vec::new();

        for field in &declaration.fields {
            fields.push(Doc::hard_line());

            for line in docs_for(&declaration.member_docs, field.name_span) {
                fields.push(Doc::text("///"));
                fields.push(Doc::text(*line));
                fields.push(Doc::hard_line());
            }

            fields.push(Doc::text(field.name));
            fields.push(Doc::text(": "));
            fields.push(Doc::text(self.slice(field.typ.span())));
            fields.push(Doc::text(","));
        }

        parts.push(Doc::indent(self.indent_width(), Doc::concat(fields)));
        parts.push(Doc::hard_line());
        parts.push(Doc::text("}"));

        Ok(Doc::concat(parts))
    }

    fn constant(&mut self, constant: &Const<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::new();

        if constant.is_pub {
            parts.push(Doc::text("pub "));
        }

        parts.push(Doc::text("const "));
        parts.push(Doc::text(constant.name));
        parts.push(Doc::text(": "));
        parts.push(Doc::text(self.slice(constant.typ.span())));
        parts.push(Doc::text(" = "));
        parts.push(self.expression(&constant.value)?);
        parts.push(Doc::text(";"));

        Ok(Doc::concat(parts))
    }

    fn static_item(&mut self, item: &Static<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::new();

        if item.is_pub {
            parts.push(Doc::text("pub "));
        }

        parts.push(Doc::text("static "));
        if item.is_mut {
            parts.push(Doc::text("mut "));
        }

        parts.push(Doc::text(item.name));
        parts.push(Doc::text(": "));
        parts.push(Doc::text(self.slice(item.typ.span())));
        parts.push(Doc::text(" = "));
        parts.push(self.expression(&item.value)?);
        parts.push(Doc::text(";"));

        Ok(Doc::concat(parts))
    }

    fn implementation(&mut self, block: &Impl<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = vec![Doc::text("impl "), Doc::text(self.slice(block.receiver.span()))];

        if let Some(ref interface) = block.interface_type {
            parts.push(Doc::text(" with "));
            parts.push(Doc::text(self.slice(interface.span())));
        }

        parts.push(Doc::text(" {"));

        let mut members: Vec<_> = block
            .methods
            .iter()
            .map(Member::Method)
            .chain(block.constants.iter().map(Member::Constant))
            .chain(block.types.iter().map(Member::Association))
            .collect();

        members.sort_by_key(|member| member.span().start);

        match members.is_empty() {
            true => {
                let dangling = self.dangling(block.span);
                parts.push(Doc::indent(self.indent_width(), dangling));
            },
            false => {
                let body = self.members(&members, &block.member_docs)?;
                parts.push(Doc::indent(
                    self.indent_width(),
                    Doc::concat([body, self.dangling(block.span)]),
                ));
            },
        }

        parts.push(Doc::hard_line());
        parts.push(Doc::text("}"));

        Ok(Doc::concat(parts))
    }

    fn members(
        &mut self,
        members: &[Member<'_, 'src>],
        docs: &[(Span, Box<[&'src str]>)],
    ) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::with_capacity(members.len() * 3);

        for (index, member) in members.iter().enumerate() {
            let span = member.span();
            let start = self.declaration_start(span);
            let leading = self.trivia.leading(start);

            parts.push(Doc::hard_line());
            if index > 0 && opens_with_blank_line(leading) {
                parts.push(Doc::hard_line());
            }

            for comment in self.comments_before(start) {
                parts.push(Doc::text(comment));
                parts.push(Doc::hard_line());
            }

            for line in docs_for(docs, span) {
                parts.push(Doc::text("///"));
                parts.push(Doc::text(*line));
                parts.push(Doc::hard_line());
            }

            parts.push(match member {
                Member::Method(method) => self.function(method)?,
                Member::Constant(constant) => self.constant(constant)?,
                Member::Requirement(method) => self.interface_method(method)?,
                Member::RequiredConstant(constant) => self.interface_constant(constant),
                Member::Association(associated) => self.associated_type(associated),
                Member::RequiredAssociation(associated) => {
                    self.required_associated_type(associated)
                },
            });

            self.push_trailing_comment(&mut parts, span);
        }

        Ok(Doc::concat(parts))
    }

    fn repetition(&mut self, repeated: &Loop<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = vec![Doc::text("loop")];

        match repeated.header {
            LoopHeader::Infinite => {},
            LoopHeader::Range { ref binding, ref start, ref end, inclusive } => {
                if let Some(binding) = binding {
                    parts.push(Doc::text(" "));
                    parts.push(Doc::text(binding.name));
                    parts.push(Doc::text(" in"));
                }

                let separator = match inclusive {
                    true => "..=",
                    false => "..",
                };

                parts.push(Doc::text(" "));
                parts.push(self.expression(start)?);
                parts.push(Doc::text(separator));
                parts.push(self.expression(end)?);
            },
            LoopHeader::Iterable { ref binding, ref iterable } => {
                parts.push(Doc::text(" "));
                parts.push(Doc::text(binding.name));
                parts.push(Doc::text(" in "));
                parts.push(self.expression(iterable)?);
            },
        }

        parts.push(Doc::text(" "));
        parts.push(self.block(&repeated.body)?);

        Ok(Doc::concat(parts))
    }

    fn binding(&mut self, binding: &Let<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = vec![Doc::text("let ")];

        if binding.mutable {
            parts.push(Doc::text("mut "));
        }

        parts.push(Doc::text(binding.name));

        if let Some(ref declared) = binding.typ {
            parts.push(Doc::text(": "));
            parts.push(Doc::text(self.slice(declared.span())));
        }

        if let Some(ref value) = binding.value {
            parts.push(Doc::text(" = "));
            parts.push(self.expression(value)?);
        }

        parts.push(Doc::text(";"));

        Ok(Doc::concat(parts))
    }

    fn returned(&mut self, returned: &Return<'src>) -> Result<Doc<'src>, FormatError> {
        if let Some(ref value) = returned.value {
            let parts = [Doc::text("return "), self.expression(value)?, Doc::text(";")];
            return Ok(Doc::concat(parts));
        };

        Ok(Doc::text("return;"))
    }

    fn conditional(&mut self, conditional: &If<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = vec![
            Doc::text("if "),
            self.expression(&conditional.condition)?,
            Doc::text(" "),
            self.block(&conditional.then_branch)?,
        ];

        if let Some(branch) = conditional.else_branch.as_deref() {
            parts.push(Doc::text(" else "));

            match branch {
                Else::If(nested) => parts.push(self.conditional(nested)?),
                Else::Block(block) => parts.push(self.block(block)?),
                // the grammar closes an expression-bodied `else` with a semicolon
                Else::Expr(expr) => {
                    parts.push(self.expression(expr)?);
                    parts.push(Doc::text(";"));
                },
            }
        }

        Ok(Doc::concat(parts))
    }

    fn block(&mut self, block: &Block<'src>) -> Result<Doc<'src>, FormatError> {
        let body = match block.statements.is_empty() {
            true => Doc::Empty,
            false => {
                Doc::concat([Doc::hard_line(), self.statements(&block.statements, Region::Block)?])
            },
        };

        let dangling = self.dangling(block.span);

        match (&body, &dangling) {
            (Doc::Empty, Doc::Empty) => {
                Ok(Doc::concat([Doc::text("{"), Doc::hard_line(), Doc::text("}")]))
            },
            _ => Ok(Doc::concat([
                Doc::text("{"),
                Doc::indent(self.indent_width(), Doc::concat([body, dangling])),
                Doc::hard_line(),
                Doc::text("}"),
            ])),
        }
    }

    fn expression(&mut self, expr: &Expression<'src>) -> Result<Doc<'src>, FormatError> {
        match expr {
            Expression::Integer(_, span)
            | Expression::Float(_, span)
            | Expression::String(_, span)
            | Expression::Char(_, span)
            | Expression::Bool(_, span) => Ok(Doc::text(self.slice(*span))),
            Expression::Identifier(name, _) => Ok(Doc::text(*name)),
            Expression::Unary { operator, expr: operand, .. } => Ok(Doc::concat([
                Doc::text(operator.as_str()),
                Doc::text(match operator.needs_separator() {
                    true => " ",
                    false => "",
                }),
                // the operand keeps its parentheses unless it binds at least as
                // tightly as the cast a prefix operator would otherwise absorb
                self.operand(operand, Precedence::Suffix.level())?,
            ])),
            Expression::Binary { left, operator, right, .. } => {
                let level = operator.precedence().level();

                Ok(Doc::group(Doc::concat([
                    self.operand(left, level)?,
                    Doc::text(" "),
                    Doc::text(operator.as_str()),
                    Doc::soft_line(),
                    self.operand(right, level + 1)?,
                ])))
            },
            Expression::Assignment { target, value, .. } => Ok(Doc::concat([
                self.expression(target)?,
                Doc::text(" = "),
                self.expression(value)?,
            ])),
            Expression::Field { expr: base, field, .. } => Ok(Doc::concat([
                self.operand(base, Precedence::Field.level())?,
                Doc::text("."),
                Doc::text(*field),
            ])),
            Expression::Index { base, index, .. } => Ok(Doc::concat([
                self.operand(base, Precedence::Field.level())?,
                Doc::text("["),
                self.expression(index)?,
                Doc::text("]"),
            ])),
            Expression::Cast { expr: value, target_type, .. } => Ok(Doc::concat([
                self.operand(value, Precedence::Suffix.level())?,
                Doc::text(" as "),
                Doc::text(self.slice(target_type.span())),
            ])),
            Expression::Call { callee, args, type_args, .. } => {
                let turbofish = self.turbofish(type_args);
                let callee = self.operand(callee, Precedence::Field.level())?;
                let arguments = self.expressions(args)?;

                Ok(Doc::concat([callee, turbofish, self.delimited("(", arguments, ")")]))
            },
            Expression::QualifiedName { path, name, .. } => {
                Ok(Doc::text(self.qualified(path, name)))
            },
            Expression::QualifiedCall { path, name, args, type_args, .. } => {
                let turbofish = self.turbofish(type_args);
                let arguments = self.expressions(args)?;

                Ok(Doc::concat([
                    Doc::text(self.qualified(path, name)),
                    turbofish,
                    self.delimited("(", arguments, ")"),
                ]))
            },
            Expression::Array { elements, .. } => {
                let items = self.expressions(elements)?;

                Ok(self.delimited("[", items, "]"))
            },
            Expression::ArrayRepeat { value, span, .. } => Ok(Doc::concat([
                Doc::text("["),
                self.expression(value)?,
                Doc::text("; "),
                Doc::text(self.repeat_count(*span)),
                Doc::text("]"),
            ])),
            Expression::Struct { name, fields, type_args, .. } => {
                self.struct_literal(name, fields, type_args)
            },

            other => Err(FormatError::Unsupported { span: other.span() }),
        }
    }

    fn struct_literal(
        &mut self,
        name: &'src str,
        fields: &[StructField<'src>],
        type_args: &[Spanned<Type<'src>>],
    ) -> Result<Doc<'src>, FormatError> {
        let turbofish = self.turbofish(type_args);

        match fields.is_empty() {
            true => Ok(Doc::concat([Doc::text(name), turbofish, Doc::text(" {}")])),
            false => {
                let mut printed = Vec::with_capacity(fields.len());

                for field in fields {
                    printed.push(match self.is_shorthand(field) {
                        true => Doc::text(field.name),
                        false => Doc::concat([
                            Doc::text(field.name),
                            Doc::text(": "),
                            self.expression(&field.value)?,
                        ]),
                    });
                }

                Ok(Doc::group(Doc::concat([
                    Doc::text(name),
                    turbofish,
                    Doc::text(" {"),
                    Doc::indent(
                        self.indent_width(),
                        Doc::concat([Doc::soft_line(), join(printed)]),
                    ),
                    Doc::if_break(Doc::text(","), Doc::Empty),
                    Doc::soft_line(),
                    Doc::text("}"),
                ])))
            },
        }
    }

    /// whether `field` prints as `name` rather than `name: value`
    fn is_shorthand(&self, field: &StructField<'src>) -> bool {
        let Expression::Identifier(name, span) = field.value else {
            return false;
        };

        if name != field.name {
            return false;
        }

        self.options.initialise_short_hand() || span.start == field.span.start
    }

    fn expressions(
        &mut self,
        expressions: &[Expression<'src>],
    ) -> Result<Vec<Doc<'src>>, FormatError> {
        expressions.iter().map(|expr| self.expression(expr)).collect()
    }

    /// prints `expr` as the child of a construct binding at `level`, adding the
    /// parentheses the AST does not record
    fn operand(&mut self, expr: &Expression<'src>, level: u8) -> Result<Doc<'src>, FormatError> {
        let printed = self.expression(expr)?;

        match precedence(expr) < level {
            true => Ok(Doc::concat([Doc::text("("), printed, Doc::text(")")])),
            false => Ok(printed),
        }
    }

    /// a comma-separated list that breaks as one unit, with a trailing comma only in the broken form
    fn delimited(&self, open: &'src str, items: Vec<Doc<'src>>, close: &'src str) -> Doc<'src> {
        match items.is_empty() {
            true => Doc::concat([Doc::text(open), Doc::text(close)]),
            false => Doc::group(Doc::concat([
                Doc::text(open),
                Doc::indent(self.indent_width(), Doc::concat([Doc::break_line(), join(items)])),
                Doc::if_break(Doc::text(","), Doc::Empty),
                Doc::break_line(),
                Doc::text(close),
            ])),
        }
    }

    /// The `::<...>` arguments naming which instantiation is called
    fn turbofish(&self, arguments: &[Spanned<Type<'src>>]) -> Doc<'src> {
        if arguments.is_empty() {
            return Doc::Empty;
        }

        let mut written = String::from("::<");

        for (index, argument) in arguments.iter().enumerate() {
            if index > 0 {
                written.push_str(", ");
            }
            written.push_str(self.slice(argument.span()));
        }

        written.push('>');
        Doc::text(written)
    }

    /// the repeat count of `[value; count]` as written
    fn repeat_count(&self, span: Span) -> &'src str {
        self.slice(span)
            .rsplit_once(';')
            .map_or("", |(_, tail)| tail.trim().trim_end_matches(']').trim())
    }

    fn qualified(&self, path: &[&'src str], name: &'src str) -> String {
        let mut written = String::new();

        for segment in path {
            written.push_str(segment);
            written.push_str("::");
        }

        written.push_str(name);
        written
    }

    #[inline]
    fn is_terminated(&self, end: BytePos) -> bool {
        self.source[end.offset()..].trim_start().starts_with(';')
    }

    #[inline]
    fn slice(&self, span: Span) -> &'src str {
        &self.source[span.start.offset()..span.end.offset()]
    }

    #[inline]
    const fn indent_width(&self) -> u8 {
        self.options.indent_width()
    }
}

impl Member<'_, '_> {
    #[inline]
    const fn span(&self) -> Span {
        match self {
            Self::Method(method) => method.span,
            Self::Constant(constant) => constant.span,
            Self::Requirement(method) => method.span,
            Self::RequiredConstant(constant) => constant.span,
            Self::Association(associated) => associated.span,
            Self::RequiredAssociation(associated) => associated.span,
        }
    }
}

/// whether two adjacent top-level items are both imports
fn both_imports(previous: &Statement<'_>, current: &Statement<'_>) -> bool {
    let is_import = |statement: &Statement<'_>| matches!(statement, Statement::Item(item) if matches!(item.kind, ItemKind::Use(_)));

    is_import(previous) && is_import(current)
}

fn import<'src>(declaration: &UseDecl<'src>) -> Doc<'src> {
    let mut written = String::from("use ");

    for (index, segment) in declaration.path.segments.iter().enumerate() {
        if index > 0 {
            written.push_str("::");
        }
        written.push_str(segment);
    }

    match declaration.items {
        UseItems::Namespace => written.push(';'),
        UseItems::Named(ref names) => {
            written.push_str("::{");

            for (index, item) in names.iter().enumerate() {
                if index > 0 {
                    written.push_str(", ");
                }
                written.push_str(item.name);
            }

            written.push_str("};");
        },
    }

    Doc::text(written)
}

/// collapses every run of whitespace to a single space
fn normalise_spaces(text: &str) -> String {
    let mut written = String::with_capacity(text.len());

    for (index, word) in text.split_whitespace().enumerate() {
        if index > 0 {
            written.push(' ');
        }
        written.push_str(word);
    }

    written
}

fn join(items: Vec<Doc<'_>>) -> Doc<'_> {
    let last = items.len().saturating_sub(1);
    let mut parts = Vec::with_capacity(items.len() * 3);

    for (index, item) in items.into_iter().enumerate() {
        parts.push(item);
        if index < last {
            parts.push(Doc::text(","));
            parts.push(Doc::soft_line());
        }
    }

    Doc::concat(parts)
}

fn docs_for<'a, 'src>(member_docs: &'a [(Span, Box<[&'src str]>)], span: Span) -> &'a [&'src str] {
    member_docs
        .iter()
        .find(|(recorded, _)| *recorded == span)
        .map_or(&[], |(_, lines)| lines)
}

#[inline(always)]
const fn precedence(expr: &Expression<'_>) -> u8 {
    match expr {
        Expression::Assignment { .. } => Precedence::Assignment.level(),
        Expression::Binary { operator, .. } => operator.precedence().level(),
        Expression::Cast { .. } => Precedence::Suffix.level(),
        Expression::Unary { .. } => Precedence::UNARY_OPERAND.level(),
        _ => u8::MAX,
    }
}

#[inline(always)]
const fn receiver_name<'s>(receiver: Receiver) -> &'s str {
    match (receiver.by_ref, receiver.mutable) {
        (true, true) => "&mut self",
        (true, false) => "&self",
        (false, true) => "mut self",
        (false, false) => "self",
    }
}

#[inline(always)]
const fn statement_span(statement: &Statement<'_>) -> Span {
    match statement {
        Statement::Let(binding) => binding.span,
        Statement::Return(returned) => returned.span,
        Statement::If(conditional) => conditional.span,
        Statement::Loop(repeated) => repeated.span,
        Statement::Break(span) | Statement::Continue(span) | Statement::Expr(_, span) => *span,
        Statement::Block(block) => block.span,
        Statement::Unsafe { block, .. } => block.span,
        Statement::Match(matched) => matched.span,
        Statement::Item(item) => item_span(&item.kind),
    }
}

#[inline(always)]
const fn item_span(kind: &ItemKind<'_>) -> Span {
    match kind {
        ItemKind::Fn(function) => function.span,
        ItemKind::Struct(declaration) => declaration.span,
        ItemKind::Enum(enumeration) => enumeration.span,
        ItemKind::Const(constant) => constant.span,
        ItemKind::Static(item) => item.span,
        ItemKind::Impl(block) => block.span,
        ItemKind::Interface(interface) => interface.span,
        ItemKind::Use(declaration) => declaration.span,
    }
}
