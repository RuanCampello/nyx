use crate::hir::error::ConstFnViolationKind;
use crate::hir::module::ModuleError;
use crate::lexer::HasSpan;
use crate::lexer::error::LexError;
use crate::lexer::token::Span;
use crate::mir::error::MirError;
use crate::parser::error::ParserError;
use crate::{NyxError, hir::error::HirError};
use ariadne::{Color as Colour, Label, Report, ReportKind, Source};
use std::cell::RefCell;

#[derive(Debug)]
pub struct Diagnostic {
    #[allow(dead_code)]
    pub(crate) message: String,
    pub(crate) rendered: String,
}

#[derive(Debug)]
pub struct Info {
    message: String,
    label: String,

    note: Option<String>,
    help: Option<String>,

    span: Span,
}

pub trait Diagnosticable {
    fn info(&self) -> Info;
}

thread_local! {
    static SOURCE: RefCell<(String, String)> = RefCell::new((String::new(), String::new()));
}

const RED: Colour = Colour::Fixed(203);

pub fn initialise(src: &str, filename: &str) {
    SOURCE.with_borrow_mut(|s| *s = (src.to_string(), filename.to_string()));
}

impl Diagnostic {
    pub fn display(self) -> String {
        self.rendered
    }

    fn from_info(info: Info) -> Self {
        let message = info.message.clone();
        let span: std::ops::Range<usize> = info.span.into();

        let rendered = SOURCE.with_borrow(|(src, filename)| {
            let id = filename.as_str();

            let mut builder = Report::build(ReportKind::Error, (id, span.start..span.start))
                .with_message(&info.message)
                .with_label(Label::new((id, span)).with_message(&info.label).with_color(RED));

            if let Some(note) = &info.note {
                builder = builder.with_note(note);
            }

            if let Some(help) = &info.help {
                builder = builder.with_help(help);
            }

            render(src, filename, builder.finish())
        });

        Self { message, rendered }
    }
}

#[inline(always)]
fn render<'s>(src: &'s str, filename: &str, report: Report<'_, (&str, std::ops::Range<usize>)>) -> String {
    let mut buf = Vec::with_capacity(src.len());
    report.write((filename, Source::from(src)), &mut buf).ok();

    // SAFETY: we know that the string is a valid utf8 cause it's from another valid str buffer
    unsafe { String::from_utf8_unchecked(buf) }
}

impl Diagnosticable for LexError {
    fn info(&self) -> Info {
        use crate::lexer::error::LexErrorKind as Kind;

        let (message, label) = match &self.kind {
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

        Info {
            message,
            label,
            span: self.span,
            help: self.help.clone(),
            note: None,
        }
    }
}

impl Diagnosticable for ParserError<'_> {
    fn info(&self) -> Info {
        use crate::parser::error::ParseErrorKind as Kind;

        let (message, label, note, help) = match &self.kind {
            Kind::Lexical(lex) => return lex.info(),

            Kind::Expected { expected, found } => (
                format!("expected `{expected}`, found `{found}`"),
                format!("expected `{expected}` here"),
                None,
                Some(format!("add a `{expected}` token here")),
            ),

            Kind::ExpectedIdentifier { found } => (
                format!("expected identifier, found `{found}`"),
                "an identifier was expected here".to_string(),
                None,
                None,
            ),

            Kind::UnexpectedIdentifier => (
                "invalid assignment target".to_string(),
                "only identifiers can be assigned to".to_string(),
                Some("assignment targets must be simple identifiers, not expressions".to_string()),
                None,
            ),

            Kind::InvalidBinaryOperator { found } => (
                format!("unexpected token `{found}` in expression"),
                format!("`{found}` cannot be used as a binary operator here"),
                None,
                None,
            ),

            Kind::InvalidUnaryOperator { found } => (
                format!("unexpected token `{found}` in unary expression"),
                format!("`{found}` cannot be used as a unary operator"),
                Some("valid unary operators are `-` (negation) and `!` (logical not)".to_string()),
                None,
            ),

            Kind::ExpectedExpression { found } => (
                format!("expected expression, found `{found}`"),
                "an expression was expected here".to_string(),
                None,
                None,
            ),

            Kind::ExpectedTypeIdentifier { found } => (
                format!("unknown type `{found}`"),
                format!("`{found}` is not a known type"),
                Some(
                    "valid types: i8, u8, i16, u16, i32, u32, i64, u64, f32, f64, bool, char, &str, String, iptr, uptr"
                        .to_string(),
                ),
                None,
            ),

            Kind::UnexpectedEof => (
                "unexpected end of file".to_string(),
                "the file ended here unexpectedly".to_string(),
                None,
                None,
            ),
        };

        Info {
            span: self.span,
            message,
            help,
            label,
            note,
        }
    }
}

impl Diagnosticable for HirError<'_> {
    fn info(&self) -> Info {
        use crate::hir::error::HirErrorKind as Kind;

        let (message, label, note, help) = match &self.kind {
            Kind::Parser(_) => unreachable!(),

            Kind::TopLevelNonFunction => (
                "only function declarations are allowed at the top level".to_string(),
                "this is not a function declaration".to_string(),
                Some("move this into a function body, or wrap it in `fn main()`".to_string()),
                None,
            ),

            Kind::DuplicateFunction { name } => (
                format!("duplicate function `{name}`"),
                format!("`{name}` is defined here again"),
                None,
                Some(format!("rename one of the `{name}` functions")),
            ),

            Kind::UndeclaredIdentifier { name } => (
                format!("use of undeclared identifier `{name}`"),
                format!("`{name}` is not declared in this scope"),
                None,
                Some(format!("declare `{name}` with `let {name} = ...` before using it")),
            ),

            Kind::UnknownFunction { name } => (
                format!("call to unknown function `{name}`"),
                format!("`{name}` is not a known function"),
                None,
                Some(format!("declare `fn {name}(...)` before calling it")),
            ),

            Kind::ArityMismatch { name, expected, found } => (
                format!("wrong number of arguments to `{name}`"),
                format!(
                    "{found} argument{} provided, but `{name}` expects {expected}",
                    if *found == 1 { "" } else { "s" }
                ),
                None,
                None,
            ),

            Kind::DuplicateBind { name } => (
                format!("duplicate binding `{name}`"),
                format!("`{name}` is already bound in this scope"),
                Some("re-declaring the same name in the same scope is not allowed".to_string()),
                Some("use a different name, or shadow it in a nested block".to_string()),
            ),

            Kind::MissingInitialiser { name } => (
                format!("missing initialiser for `{name}`"),
                format!("`{name}` has no initialiser and no type annotation"),
                Some("Nyx cannot infer the type without a value to check against".to_string()),
                Some(format!(
                    "add a type annotation: `let {name}: <type>;` or provide an initial value"
                )),
            ),

            Kind::TypeMismatch { expected, found } => (
                format!("type mismatch: expected `{expected}`, found `{found}`"),
                format!("this has type `{found}`"),
                None,
                Some(format!("expected `{expected}` here")),
            ),

            Kind::ImmutableBind { name } => (
                format!("cannot assign to immutable binding `{name}`"),
                format!("`{name}` is immutable and cannot be reassigned"),
                Some("bindings are immutable by default".to_string()),
                Some(format!("declare it as mutable: `let mut {name} = ...`")),
            ),

            Kind::ConstFnViolation(ConstFnViolationKind::NonConstCall { name }) => (
                format!("cannot call non-const function `{name}` in a const context"),
                format!("`{name}` is not declared `const`"),
                Some("const functions may only call other const functions".to_string()),
                Some(format!("mark `fn {name}` as `const fn {name}` if it qualifies")),
            ),
        };

        Info {
            span: self.span(),
            message,
            label,
            help,
            note,
        }
    }
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
        self.span
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

impl std::fmt::Display for NyxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compile(d) => writeln!(f, "{d}"),
            Self::Io(io) => writeln!(f, "i/o error: {io}"),
            Self::Assembler(code) => writeln!(f, "assembler failed with exit code: {code}"),
            Self::Linker(code) => writeln!(f, "linker failed with exit code: {code}"),
            Self::ToolNotFound(msg) => writeln!(f, "tool not found — is binutils installed? ({msg})"),
        }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered)
    }
}

impl<T: Diagnosticable> From<T> for Diagnostic {
    fn from(value: T) -> Self {
        Self::from_info(value.info())
    }
}

impl<T: Into<Diagnostic>> From<T> for NyxError {
    fn from(value: T) -> Self {
        Self::Compile(value.into())
    }
}

impl From<MirError> for Diagnostic {
    fn from(value: MirError) -> Self {
        let message = value.to_string();
        let rendered = format!("error: {message}\n");
        // TODO: better errors for MIR :X

        Self { message, rendered }
    }
}

impl From<ModuleError> for Diagnostic {
    fn from(value: ModuleError) -> Self {
        match value {
            ModuleError::Diagnostic(diagnostic) => diagnostic,
            ModuleError::FileNotFound { path, span } => Self::from_info(Info {
                message: format!("module file not found: `{}`", path.display()),
                label: "imported here".to_string(),
                span: span.unwrap_or_default(),
                help: Some(format!("make sure the file `{}` exists", path.display())),
                note: None,
            }),
            ModuleError::CircularImport { path, span } => Self::from_info(Info {
                message: format!("circular import: `{}` is already being loaded", path.display()),
                label: "this import creates a cycle".to_string(),
                span,
                help: Some("remove the circular dependency between modules".to_string()),
                note: None,
            }),
            ModuleError::EmptyPath => Self::from_info(Info {
                message: "empty import path".to_string(),
                label: "this path has no segments".to_string(),
                span: Span::default(),
                help: Some("use paths like `use project::module;`".to_string()),
                note: None,
            }),
            ModuleError::UnknownRoot { name, span } => Self::from_info(Info {
                message: format!("unknown module root `{name}`"),
                label: format!("`{name}` is not a known module root"),
                span,
                help: Some("the root must match the project name".to_string()),
                note: None,
            }),
            ModuleError::UnknownExport { path, name, span } => Self::from_info(Info {
                message: format!("module `{}` has no exported symbol `{name}`", path.display()),
                label: format!("`{name}` is not exported from this module"),
                span,
                help: Some(format!("add `pub` to `fn {name}` in `{}`", path.display())),
                note: None,
            }),
            ModuleError::TopLevelNonFunction { path, span } => Self::from_info(Info {
                message: format!(
                    "only function declarations are allowed at top level in `{}`",
                    path.display()
                ),
                label: "this is not a function declaration".to_string(),
                span,
                help: Some("move this into a function body, or wrap it in `fn main()`".to_string()),
                note: None,
            }),
        }
    }
}

impl From<std::io::Error> for NyxError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
