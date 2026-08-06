//! Translation of the parser AST into layout-independent [documents](Doc)
//!
//! The printer decides style, [render](crate::render) only picks between the
//! layouts a [Doc] already encodes

use crate::doc::Doc;
use crate::format::{FormatError, FormatOptions};
use crate::trivia::{Piece, Trivia, opens_with_blank_line};
use frontend::lexer::token::{BytePos, Span};
use frontend::parser::expression::{BinaryOperator, Expression, StructField, UnaryOperator};
use frontend::parser::statement::{
    Block, Const, Else, Function, If, Item, ItemKind, Let, Return, Statement, Struct,
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

/// Whether the grammar accepts a comma after the final item of a list
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrailingComma {
    Allowed,
    Rejected,
}

#[repr(u8)]
enum Precendence {
    Cast = 12,
    Unary = 13,
    Postfix = 14,
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
            let leading = self.trivia.leading(span.start);

            if index > 0 {
                parts.push(Doc::hard_line());
                let blank = match region {
                    Region::Module => true,
                    Region::Block => opens_with_blank_line(leading),
                };

                if blank {
                    parts.push(Doc::hard_line());
                }
            }

            for comment in self.comments_before(span.start) {
                parts.push(Doc::text(comment));
                parts.push(Doc::hard_line());
            }

            parts.push(self.statement(statement)?);
            self.push_trailing_comment(&mut parts, span);
        }

        Ok(Doc::concat(parts))
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
            other => return Err(FormatError::Unsupported { span: item_span(other) }),
        }

        Ok(Doc::concat(parts))
    }

    fn function(&mut self, function: &Function<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::new();

        if function.is_pub {
            parts.push(Doc::text("pub "));
        }
        if function.is_const {
            parts.push(Doc::text("const "));
        }
        if function.inline {
            parts.push(Doc::text("inline "));
        }

        parts.push(Doc::text("fn "));
        parts.push(Doc::text(function.name));

        let parameters = function
            .params
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

        parts.push(self.delimited("(", parameters, ")", TrailingComma::Rejected));

        if let Some(ref returned) = function.return_type {
            parts.push(Doc::text(": "));
            parts.push(Doc::text(self.slice(returned.span())));
        }

        parts.push(Doc::text(" "));
        parts.push(self.block(&function.body)?);

        Ok(Doc::concat(parts))
    }

    fn structure(&mut self, declaration: &Struct<'src>) -> Result<Doc<'src>, FormatError> {
        let mut parts = Vec::new();

        if declaration.is_pub {
            parts.push(Doc::text("pub "));
        }

        parts.push(Doc::text("struct "));
        parts.push(Doc::text(declaration.name));
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
                Else::Expr(expr) => parts.push(self.expression(expr)?),
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
                Doc::text(unary_operator(*operator)),
                self.operand(operand, Precendence::Unary as _)?,
            ])),
            Expression::Binary { left, operator, right, .. } => {
                let level = binary_precedence(*operator);

                Ok(Doc::group(Doc::concat([
                    self.operand(left, level)?,
                    Doc::text(" "),
                    Doc::text(binary_operator(*operator)),
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
                self.operand(base, Precendence::Postfix as _)?,
                Doc::text("."),
                Doc::text(*field),
            ])),
            Expression::Index { base, index, .. } => Ok(Doc::concat([
                self.operand(base, Precendence::Postfix as _)?,
                Doc::text("["),
                self.expression(index)?,
                Doc::text("]"),
            ])),
            Expression::Cast { expr: value, target_type, .. } => Ok(Doc::concat([
                self.operand(value, Precendence::Cast as _)?,
                Doc::text(" as "),
                Doc::text(self.slice(target_type.span())),
            ])),
            Expression::Call { callee, args, .. } => {
                let callee = self.operand(callee, Precendence::Postfix as _)?;
                let arguments = self.expressions(args)?;

                Ok(Doc::concat([
                    callee,
                    self.delimited("(", arguments, ")", TrailingComma::Rejected),
                ]))
            },
            Expression::QualifiedName { path, name, .. } => {
                Ok(Doc::text(self.qualified(path, name)))
            },
            Expression::QualifiedCall { path, name, args, .. } => {
                let arguments = self.expressions(args)?;

                Ok(Doc::concat([
                    Doc::text(self.qualified(path, name)),
                    self.delimited("(", arguments, ")", TrailingComma::Rejected),
                ]))
            },
            Expression::Array { elements, .. } => {
                let items = self.expressions(elements)?;

                Ok(self.delimited("[", items, "]", TrailingComma::Allowed))
            },
            Expression::Struct { name, fields, .. } => self.struct_literal(name, fields),

            other => Err(FormatError::Unsupported { span: other.span() }),
        }
    }

    fn struct_literal(
        &mut self,
        name: &'src str,
        fields: &[StructField<'src>],
    ) -> Result<Doc<'src>, FormatError> {
        match fields.is_empty() {
            true => Ok(Doc::concat([Doc::text(name), Doc::text(" {}")])),
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
    fn delimited(
        &self,
        open: &'src str,
        items: Vec<Doc<'src>>,
        close: &'src str,
        trailing: TrailingComma,
    ) -> Doc<'src> {
        let comma = match trailing {
            TrailingComma::Allowed => Doc::if_break(Doc::text(","), Doc::Empty),
            TrailingComma::Rejected => Doc::Empty,
        };

        match items.is_empty() {
            true => Doc::concat([Doc::text(open), Doc::text(close)]),
            false => Doc::group(Doc::concat([
                Doc::text(open),
                Doc::indent(self.indent_width(), Doc::concat([Doc::break_line(), join(items)])),
                comma,
                Doc::break_line(),
                Doc::text(close),
            ])),
        }
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
        Expression::Assignment { .. } => 1,
        Expression::Binary { operator, .. } => binary_precedence(*operator),
        Expression::Cast { .. } => Precendence::Cast as _,
        Expression::Unary { .. } => Precendence::Unary as _,
        _ => u8::MAX,
    }
}

#[inline(always)]
const fn binary_precedence(operator: BinaryOperator) -> u8 {
    match operator {
        BinaryOperator::Or => 2,
        BinaryOperator::And => 3,
        BinaryOperator::Eq | BinaryOperator::Ne => 4,
        BinaryOperator::Lt | BinaryOperator::LtEq | BinaryOperator::Gt | BinaryOperator::GtEq => 5,
        BinaryOperator::BitOr => 6,
        BinaryOperator::BitXor => 7,
        BinaryOperator::BitAnd => 8,
        BinaryOperator::Shl | BinaryOperator::Shr => 9,
        BinaryOperator::Add | BinaryOperator::Sub => 10,
        BinaryOperator::Mul | BinaryOperator::Div => 11,
    }
}

#[inline(always)]
const fn binary_operator<'s>(operator: BinaryOperator) -> &'s str {
    match operator {
        BinaryOperator::Add => "+",
        BinaryOperator::Sub => "-",
        BinaryOperator::Mul => "*",
        BinaryOperator::Div => "/",
        BinaryOperator::Eq => "==",
        BinaryOperator::Ne => "!=",
        BinaryOperator::Lt => "<",
        BinaryOperator::LtEq => "<=",
        BinaryOperator::Gt => ">",
        BinaryOperator::GtEq => ">=",
        BinaryOperator::And => "&&",
        BinaryOperator::Or => "||",
        BinaryOperator::BitAnd => "&",
        BinaryOperator::BitOr => "|",
        BinaryOperator::BitXor => "^",
        BinaryOperator::Shl => "<<",
        BinaryOperator::Shr => ">>",
    }
}

#[inline(always)]
const fn unary_operator<'s>(operator: UnaryOperator) -> &'s str {
    match operator {
        UnaryOperator::Neg => "-",
        UnaryOperator::Not => "!",
        UnaryOperator::Deref => "*",
        UnaryOperator::Ref => "&",
        UnaryOperator::RefMut => "&mut ",
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
        ItemKind::Impl(block) => block.span,
        ItemKind::Interface(interface) => interface.span,
        ItemKind::Use(declaration) => declaration.span,
    }
}
