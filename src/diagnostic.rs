#![allow(unused)]

use crate::hir::error::HirError;
use crate::lexer::HasSpan;
use crate::lexer::error::LexError;
use crate::lexer::token::Span;
use crate::parser::error::ParserError;
use ariadne::{Color as Colour, Config, Label, Report, ReportKind, Source};

pub struct Diagnostic {
    message: String,
    rendered: String,
}

struct Error<'s, E: std::error::Error + HasSpan> {
    src: &'s str,
    error: E,
}

const RED: Colour = Colour::Fixed(203);
const YELLOW: Colour = Colour::Fixed(221);
const CYAN: Colour = Colour::Fixed(117);
const MAGENTA: Colour = Colour::Fixed(183);

impl From<Error<'_, LexError>> for Diagnostic {
    fn from(value: Error<'_, LexError>) -> Self {
        use crate::lexer::error::LexErrorKind as Kind;

        let error = value.error;
        let span = error.span();

        let (message, label_msg) = match &error.kind {
            Kind::UnexpectedChar(c) => (
                format!("unexpected character `{c}`"),
                format!("this character is not valid here"),
            ),
            Kind::UnterminatedString => (
                "unterminated string literal".to_string(),
                "string opened here but never closed".to_string(),
            ),
            Kind::UnterminatedComment => (
                "unterminated block comment".to_string(),
                "block comment opened here but never closed".to_string(),
            ),
            Kind::InvalidEscape(c) => (
                format!("invalid escape sequence `\\{c}`"),
                format!("`\\{c}` is not a recognised escape"),
            ),
            Kind::InvalidNumber(detail) => (
                format!("invalid number literal: {detail}"),
                "could not parse this as a number".to_string(),
            ),
        };

        let mut builder = Report::build(ReportKind::Error, span.into())
            .with_message(&message)
            .with_label(Label::new(span.into()).with_message(&label_msg).with_color(RED));

        if let Some(help) = error.help {
            builder = builder.with_help(help);
        }

        let rendered = render(value.src, builder.finish());

        Self { message, rendered }
    }
}

#[inline(always)]
fn render<'s>(src: &'s str, report: Report<'_, std::ops::Range<usize>>) -> String {
    let mut buf = Vec::with_capacity(src.len());

    report.write(Source::from(src), &mut buf).ok();

    // SAFETY: we know that the string is a valid utf8 cause it's from another valid str buffer
    unsafe { String::from_utf8_unchecked(buf) }
}

impl HasSpan for LexError {
    fn span(&self) -> Span {
        self.span
    }
}

impl<'p> HasSpan for ParserError<'p> {
    fn span(&self) -> Span {
        self.span
    }
}

impl<'h> HasSpan for HirError<'h> {
    fn span(&self) -> Span {
        todo!("add spans to hir errors")
    }
}

impl From<Span> for std::ops::Range<usize> {
    fn from(value: Span) -> Self {
        Self {
            start: value.start.offset(),
            end: value.end.offset(),
        }
    }
}
