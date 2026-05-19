use crate::hir::error::ConstFnViolationKind;
use crate::hir::module::ModuleError;
use crate::lexer::HasSpan;
use crate::lexer::error::LexError;
use crate::lexer::token::Span;
use crate::mir::error::{MirError, MirErrorKind};
use crate::parser::error::ParserError;
use crate::{NyxError, hir::error::HirError};
use ariadne::{Color, Config, Fmt, Label, Report, ReportKind, Source};
use std::cell::RefCell;

const PRIMARY: Color = Color::Rgb(243, 139, 168);
const SECONDARY: Color = Color::Rgb(180, 190, 254);
const HIGHLIGHT: Color = Color::Rgb(137, 180, 250);

#[inline(always)]
fn hi(s: impl std::fmt::Display) -> impl std::fmt::Display {
    s.fg(HIGHLIGHT)
}

thread_local! {
    static SOURCE: RefCell<(String, String)> = RefCell::new((String::new(), String::new()));
}

pub fn initialise(src: &str, filename: &str) {
    SOURCE.with_borrow_mut(|s| *s = (src.to_string(), filename.to_string()));
}

#[derive(Debug)]
pub struct Diagnostic {
    pub(crate) rendered: String,
}

impl Diagnostic {
    pub fn display(self) -> String {
        self.rendered
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered)
    }
}

struct Builder {
    message: String,
    labels: Vec<(Span, String, Color)>,
    note: Option<String>,
    help: Option<String>,
}

impl Builder {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            labels: Vec::new(),
            note: None,
            help: None,
        }
    }

    fn primary(mut self, span: Span, text: impl Into<String>) -> Self {
        self.labels.insert(0, (span, text.into(), PRIMARY));
        self
    }

    fn secondary(mut self, span: Span, text: impl Into<String>) -> Self {
        self.labels.push((span, text.into(), Color::Primary));
        self
    }

    fn note(mut self, text: impl Into<String>) -> Self {
        self.note = Some(text.into());
        self
    }

    fn help(mut self, text: impl Into<String>) -> Self {
        self.help = Some(text.into());
        self
    }

    fn build(self) -> Diagnostic {
        let rendered = SOURCE.with_borrow(|(src, filename)| {
            let id = filename.as_str();

            let anchor = self.labels.first().map(|(s, _, _)| s.start.offset()).unwrap_or(0);

            let mut builder = Report::build(ReportKind::Error, (id, anchor..anchor))
                .with_config(Config::default().with_compact(false))
                .with_message(&self.message);

            for (span, text, color) in self.labels {
                let range: std::ops::Range<usize> = span.into();
                builder = builder
                    .with_label(Label::new((id, range)).with_message(text).with_color(color));
            }

            if let Some(note) = &self.note {
                builder = builder.with_note(note);
            }
            if let Some(help) = &self.help {
                builder = builder.with_help(help);
            }

            let mut buf: Vec<u8> = Vec::new();
            builder.finish().write((id, Source::from(src.as_str())), &mut buf).ok();
            // SAFETY: ariadne only writes valid UTF-8
            unsafe { String::from_utf8_unchecked(buf) }
        });

        Diagnostic { rendered }
    }
}

pub trait Diagnosticable {
    fn into_diagnostic(self) -> Diagnostic;
}

impl Diagnosticable for LexError {
    fn into_diagnostic(self) -> Diagnostic {
        use crate::lexer::error::LexErrorKind as K;

        let b = match &self.kind {
            K::UnexpectedChar(c) => Builder::new(format!("unexpected character {}", c.fg(PRIMARY)))
                .primary(self.span, "not valid here"),

            K::UnterminatedString => Builder::new("unterminated string literal")
                .primary(self.span, "opened here, but never closed")
                .help(format!(
                    "add a closing {} at the end of the string",
                    "\"".fg(SECONDARY)
                )),

            K::UnterminatedComment => Builder::new("unterminated block comment")
                .primary(self.span, "block comment opened here, but never closed")
                .help(format!("add a closing {}", "*/".fg(SECONDARY))),

            K::InvalidEscape(c) => Builder::new(format!(
                "invalid escape sequence {}",
                format!("\\{c}").fg(PRIMARY)
            ))
            .primary(
                self.span,
                format!(
                    "{} is not a recognised escape",
                    format!("\\{c}").fg(PRIMARY)
                ),
            )
            .help(format!(
                "valid escapes: {}  {}  {}  {}  {}  {}",
                "\\\\".fg(SECONDARY),
                "\\\"".fg(SECONDARY),
                "\\n".fg(SECONDARY),
                "\\t".fg(SECONDARY),
                "\\r".fg(SECONDARY),
                "\\0".fg(SECONDARY),
            )),

            K::InvalidNumber(detail) => Builder::new(format!("invalid number literal: {detail}"))
                .primary(self.span, "could not parse this as a number"),
        };

        let b = match self.help {
            Some(ref h)
                if !matches!(
                    &self.kind,
                    K::UnterminatedString | K::UnterminatedComment | K::InvalidEscape(_)
                ) =>
            {
                b.help(h.clone())
            }
            _ => b,
        };

        b.build()
    }
}

impl<'i> Diagnosticable for ParserError<'i> {
    fn into_diagnostic(self) -> Diagnostic {
        use crate::parser::error::ParseErrorKind as K;

        match &self.kind {
            K::Lexical(lex) => lex.clone().into_diagnostic(),

            K::Expected { expected, found } => Builder::new(format!(
                "expected {}, found {}",
                expected.fg(SECONDARY),
                found.fg(PRIMARY)
            ))
            .primary(self.span, format!("expected {} here", expected.fg(SECONDARY)))
            .help(format!("add a {} token here", expected.fg(SECONDARY)))
            .build(),

            K::ExpectedIdentifier { found } => Builder::new(format!(
                "expected identifier, found {}",
                found.fg(PRIMARY)
            ))
            .primary(self.span, "an identifier was expected here")
            .build(),

            K::UnexpectedIdentifier => Builder::new("invalid assignment target")
                .primary(self.span, "only identifiers and field paths can be assigned to")
                .build(),

            K::InvalidBinaryOperator { found } => Builder::new(format!(
                "unexpected token {} in expression",
                found.fg(PRIMARY)
            ))
            .primary(
                self.span,
                format!("{} cannot be used as a binary operator here", found.fg(PRIMARY)),
            )
            .note(format!(
                "valid unary operators are {} (negation) and {} (logical not)",
                "-".fg(SECONDARY),
                "!".fg(SECONDARY)
            ))
            .build(),

            K::InvalidUnaryOperator { found } => Builder::new(format!(
                "unexpected token {} in unary expression",
                found.fg(PRIMARY)
            ))
            .primary(
                self.span,
                format!("{} cannot be used as a unary operator", found.fg(PRIMARY)),
            )
            .build(),

            K::ExpectedExpression { found } => Builder::new(format!(
                "expected expression, found {}",
                found.fg(PRIMARY)
            ))
            .primary(self.span, "an expression was expected here")
            .build(),

            K::ExpectedTypeIdentifier { found } => Builder::new(format!(
                "unknown type {}",
                found.fg(PRIMARY)
            ))
            .primary(self.span, format!("{} is not a known type", found.fg(PRIMARY)))
            .note(format!(
                "valid types: {}",
                "i8, u8, i16, u16, i32, u32, i64, u64, f32, f64, bool, char, &str, String, iptr, uptr".fg(HIGHLIGHT)
            ))
            .build(),

            K::UnexpectedEof => Builder::new("unexpected end of file")
                .primary(self.span, "the file ended here unexpectedly")
                .build(),
        }
    }
}

impl<'h> Diagnosticable for HirError<'h> {
    fn into_diagnostic(self) -> Diagnostic {
        use crate::hir::error::HirErrorKind as K;

        match &self.kind {
            K::Parser(p) => p.clone().into_diagnostic(),

            K::TopLevelNonFunction => {
                Builder::new("only function declarations are allowed at the top level")
                    .primary(self.span, "this is not a function declaration")
                    .help(format!(
                        "move this into a function body, or wrap it in {}",
                        "fn main()".fg(SECONDARY)
                    ))
                    .build()
            }

            K::DuplicateFunction { name } => {
                Builder::new(format!("duplicate function {}", hi(name)))
                    .primary(self.span, format!("{} is defined here again", hi(name)))
                    .help(format!("rename one of the {} functions", hi(name)))
                    .build()
            }

            K::DuplicateMethod { struct_name, name } => Builder::new(format!(
                "duplicate method {} for {}",
                hi(name),
                hi(struct_name)
            ))
            .primary(
                self.span,
                format!("{} is already defined for {}", hi(name), hi(struct_name)),
            )
            .help(format!("remove or rename one of the {} methods", hi(name)))
            .build(),

            K::UndeclaredIdentifier { name } => {
                Builder::new(format!("use of undeclared identifier {}", hi(name)))
                    .primary(
                        self.span,
                        format!("{} is not declared in this scope", hi(name)),
                    )
                    .help(format!(
                        "declare {} with {} before using it",
                        hi(name),
                        format!("let {name} = …").fg(SECONDARY)
                    ))
                    .build()
            }

            K::UnknownFunction { name } => {
                Builder::new(format!("call to unknown function {}", hi(name)))
                    .primary(self.span, format!("{} is not a known function", hi(name)))
                    .help(format!(
                        "declare {} before calling it",
                        format!("fn {name}(…)").fg(SECONDARY)
                    ))
                    .build()
            }

            K::UnknownMethod { struct_name, name } => Builder::new(format!(
                "call to unknown method {} on {}",
                hi(name),
                hi(struct_name)
            ))
            .primary(
                self.span,
                format!("{} has no method named {}", hi(struct_name), hi(name)),
            )
            .help(format!(
                "add {} to an impl block for {}",
                format!("fn {name}(&self)").fg(SECONDARY),
                hi(struct_name)
            ))
            .build(),

            K::UnknownType { name } => Builder::new(format!("unknown type {}", hi(name)))
                .primary(self.span, format!("{} is not a known type", hi(name)))
                .help(format!(
                    "declare {} before using it",
                    format!("struct {name} {{ … }}").fg(SECONDARY)
                ))
                .build(),

            K::DuplicateStruct { name } => Builder::new(format!("duplicate struct {}", hi(name)))
                .primary(self.span, format!("{} is defined here again", hi(name)))
                .help(format!("rename one of the {} structs", hi(name)))
                .build(),

            K::DuplicateField { name } => Builder::new(format!("duplicate field {}", hi(name)))
                .primary(self.span, format!("{} is already declared", hi(name)))
                .note("struct field names must be unique")
                .build(),

            K::UnknownField { struct_name, field } => Builder::new(format!(
                "unknown field {} on {}",
                hi(field),
                hi(struct_name)
            ))
            .primary(
                self.span,
                format!("{} has no field named {}", hi(struct_name), hi(field)),
            )
            .build(),

            K::MissingField { struct_name, field } => Builder::new(format!(
                "missing field {} in {} literal",
                hi(field),
                hi(struct_name)
            ))
            .primary(self.span, format!("{} must be initialised here", hi(field)))
            .help(format!(
                "all fields of {} must be provided in the struct literal",
                hi(struct_name)
            ))
            .build(),

            K::CircularStruct { name } => Builder::new(format!(
                "circular struct definition involving {}",
                hi(name)
            ))
            .primary(
                self.span,
                format!("{} is part of a by-value struct cycle", hi(name)),
            )
            .note("break the cycle; a pointer or box type will be needed for recursive structs")
            .help("Nyx does not support self-referential or circular structs yet")
            .build(),

            K::InvalidFieldAccess => Builder::new("invalid field access")
                .primary(
                    self.span,
                    "field access is only supported on local variable bindings",
                )
                .build(),

            K::InvalidAssignmentTarget => Builder::new("invalid assignment target")
                .primary(
                    self.span,
                    "the left-hand side must be an identifier or a field path",
                )
                .note(format!(
                    "use {} or {}",
                    "name = value".fg(SECONDARY),
                    "name.field = value".fg(SECONDARY)
                ))
                .build(),

            K::ArityMismatch {
                name,
                expected,
                found,
            } => {
                let arg_word = |n: usize| if n == 1 { "argument" } else { "arguments" };
                Builder::new(format!("wrong number of arguments to {}", hi(name)))
                    .primary(
                        self.span,
                        format!(
                            "{} {} provided, but {} expects {}",
                            found.fg(PRIMARY),
                            arg_word(*found),
                            hi(name),
                            expected.fg(SECONDARY),
                        ),
                    )
                    .build()
            }

            K::DuplicateBind { name } => Builder::new(format!("duplicate binding {}", hi(name)))
                .primary(
                    self.span,
                    format!("{} is already bound in this scope", hi(name)),
                )
                .note("re-declaring the same name in the same scope is not allowed")
                .help("use a different name, or shadow it in a nested block")
                .build(),

            K::MissingInitialiser { name } => {
                Builder::new(format!("missing initialiser for {}", hi(name)))
                    .primary(
                        self.span,
                        format!("{} has no value and no type annotation", hi(name)),
                    )
                    .note("Nyx cannot infer the type without an initial value to check against")
                    .help(format!(
                        "add a type annotation {} or provide an initial value",
                        format!("let {name}: <type>;").fg(SECONDARY)
                    ))
                    .build()
            }

            K::MissingReceiver { name } => {
                Builder::new(format!("method {} is missing a receiver", hi(name)))
                    .primary(
                        self.span,
                        format!(
                            "{} must declare {} or {}",
                            hi(name),
                            "&self".fg(SECONDARY),
                            "&mut self".fg(SECONDARY)
                        ),
                    )
                    .help(format!(
                        "write {}",
                        format!("fn {name}(&self, …)").fg(SECONDARY)
                    ))
                    .build()
            }

            K::ReceiverOutsideImpl => Builder::new("self receiver outside impl block")
                .primary(
                    self.span,
                    "receivers are only valid inside method definitions",
                )
                .help(format!(
                    "move this function into {}",
                    "impl Type { … }".fg(SECONDARY)
                ))
                .build(),

            K::TypeMismatch { expected, found } => Builder::new(format!(
                "type mismatch: expected {}, found {}",
                hi(expected),
                hi(found)
            ))
            .primary(self.span, format!("this is of type {}", hi(found)))
            .secondary(self.span, format!("expected {} here", hi(expected)))
            .build(),

            K::ImmutableBind { name } => {
                Builder::new(format!("cannot assign to immutable binding {}", hi(name)))
                    .primary(
                        self.span,
                        format!("{} is immutable and cannot be reassigned", hi(name)),
                    )
                    .note("bindings are immutable by default")
                    .help(format!(
                        "declare it as mutable: {}",
                        format!("let mut {name} = …").fg(SECONDARY)
                    ))
                    .build()
            }

            K::ConstFnViolation(ConstFnViolationKind::NonConstCall { name }) => {
                Builder::new(format!(
                    "cannot call non-const function {} in a const context",
                    hi(name)
                ))
                .primary(
                    self.span,
                    format!("{} is not declared {}", hi(name), hi("const")),
                )
                .note(format!(
                    "{} functions may only call other {} functions",
                    hi("const"),
                    hi("const")
                ))
                .help(format!(
                    "mark {} as {} if it qualifies",
                    format!("fn {name}").fg(SECONDARY),
                    format!("const fn {name}").fg(SECONDARY)
                ))
                .build()
            }

            K::DuplicateInterface { name } => {
                Builder::new(format!("duplicate interface {}", hi(name)))
                    .primary(self.span, format!("{} is defined here again", hi(name)))
                    .help(format!("rename one of the {} interfaces", hi(name)))
                    .build()
            }

            K::UnknownInterface { name } => Builder::new(format!("unknown interface {}", hi(name)))
                .primary(self.span, format!("{} is not a known interface", hi(name)))
                .help(format!(
                    "declare {} before using it",
                    format!("interface {name} {{ … }}").fg(SECONDARY)
                ))
                .build(),

            K::MissingInterfaceMethod {
                struct_name,
                interface_name,
                method_name,
            } => Builder::new(format!(
                "missing method {} required by interface {}",
                hi(method_name),
                hi(interface_name)
            ))
            .primary(
                self.span,
                format!("{} does not implement {}", hi(struct_name), hi(method_name)),
            )
            .note(format!(
                "{} requires {}",
                hi(interface_name),
                format!("fn {method_name}(…)").fg(SECONDARY)
            ))
            .help(format!(
                "add {} to {}",
                format!("fn {method_name}(…)").fg(SECONDARY),
                format!("impl {struct_name} with {interface_name}").fg(SECONDARY)
            ))
            .build(),

            K::MissingSuperinterfaceImpl {
                struct_name,
                interface_name,
                superinterface_name,
            } => Builder::new(format!(
                "missing {} implementation required by {}",
                hi(superinterface_name),
                hi(interface_name)
            ))
            .primary(
                self.span,
                format!(
                    "{} implements {} without {}",
                    hi(struct_name),
                    hi(interface_name),
                    hi(superinterface_name)
                ),
            )
            .note(format!(
                "{} extends {}",
                hi(interface_name),
                hi(superinterface_name)
            ))
            .help(format!(
                "add {}",
                format!("impl {struct_name} with {superinterface_name} {{ … }}").fg(SECONDARY)
            ))
            .build(),

            K::InterfaceSignatureMismatch {
                struct_name,
                interface_name,
                method_name,
                expected,
                found,
                impl_span,
            } => Builder::new(format!(
                "method {} does not match interface {}",
                hi(method_name),
                hi(interface_name)
            ))
            .primary(self.span, format!("found: {}", found.fg(PRIMARY)))
            .secondary(
                *impl_span,
                format!(
                    "{} requires: {}",
                    hi(interface_name),
                    expected.fg(SECONDARY)
                ),
            )
            .note(format!(
                "expected: {}\n  found: {}",
                expected.fg(SECONDARY),
                found.fg(PRIMARY)
            ))
            .help(format!(
                "update {} in {} to match the interface",
                hi(method_name),
                format!("impl {struct_name} with {interface_name}").fg(SECONDARY)
            ))
            .build(),
        }
    }
}

impl Diagnosticable for MirError {
    fn into_diagnostic(self) -> Diagnostic {
        match self.kind {
            MirErrorKind::Hir(e) => e.into_diagnostic(),
        }
    }
}

impl From<ModuleError> for Diagnostic {
    fn from(value: ModuleError) -> Self {
        match value {
            ModuleError::Diagnostic(d) => d,

            ModuleError::FileNotFound { path, span } => Builder::new(format!(
                "module file not found: {}",
                path.display().fg(PRIMARY)
            ))
            .primary(span.unwrap_or_default(), "imported here")
            .help(format!(
                "make sure the file {} exists",
                path.display().fg(HIGHLIGHT)
            ))
            .build(),

            ModuleError::CircularImport { path, span } => Builder::new(format!(
                "circular import: {} is already being loaded",
                path.display().fg(PRIMARY)
            ))
            .primary(span, "this import creates a cycle")
            .help("remove the circular dependency between modules")
            .build(),

            ModuleError::EmptyPath => Builder::new("empty import path")
                .primary(Span::default(), "this path has no segments")
                .help(format!(
                    "use paths like {}",
                    "use project::module;".fg(SECONDARY)
                ))
                .build(),

            ModuleError::UnknownRoot { name, span } => {
                Builder::new(format!("unknown module root {}", hi(&name)))
                    .primary(span, format!("{} is not a known module root", hi(&name)))
                    .help("the root segment must match your project name")
                    .build()
            }

            ModuleError::UnknownExport { path, name, span } => Builder::new(format!(
                "module {} has no exported symbol {}",
                path.display().fg(HIGHLIGHT),
                hi(&name)
            ))
            .primary(
                span,
                format!("{} is not exported from this module", hi(&name)),
            )
            .help(format!(
                "add {} to {} to export it",
                hi("pub"),
                format!("fn {name}").fg(SECONDARY)
            ))
            .build(),

            ModuleError::TopLevelNonFunction { path: _, span } => {
                Builder::new("only function declarations are allowed at the top level")
                    .primary(span, "this is not a function declaration")
                    .help(format!(
                        "move this into a function body, or wrap it in {}",
                        "fn main()".fg(SECONDARY)
                    ))
                    .build()
            }
        }
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

impl From<LexError> for Diagnostic {
    fn from(e: LexError) -> Self {
        e.into_diagnostic()
    }
}

impl<'i> From<ParserError<'i>> for Diagnostic {
    fn from(e: ParserError<'i>) -> Self {
        e.into_diagnostic()
    }
}

impl<'h> From<HirError<'h>> for Diagnostic {
    fn from(e: HirError<'h>) -> Self {
        e.into_diagnostic()
    }
}

impl From<MirError> for Diagnostic {
    fn from(e: MirError) -> Self {
        e.into_diagnostic()
    }
}

impl From<Diagnostic> for NyxError {
    fn from(d: Diagnostic) -> Self {
        Self::Compile(d)
    }
}

impl<'h> From<HirError<'h>> for NyxError {
    fn from(e: HirError<'h>) -> Self {
        Self::Compile(e.into())
    }
}

impl From<MirError> for NyxError {
    fn from(e: MirError) -> Self {
        Self::Compile(e.into())
    }
}

impl<'i> From<ParserError<'i>> for NyxError {
    fn from(e: ParserError<'i>) -> Self {
        Self::Compile(e.into())
    }
}

impl From<std::io::Error> for NyxError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<ModuleError> for NyxError {
    fn from(value: ModuleError) -> Self {
        Self::Compile(value.into())
    }
}

impl std::fmt::Display for NyxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compile(d) => writeln!(f, "{d}"),
            Self::Io(io) => writeln!(f, "i/o error: {io}"),
            Self::Assembler(code) => writeln!(f, "assembler failed with exit code: {code}"),
            Self::Linker(code) => writeln!(f, "linker failed with exit code: {code}"),
            Self::ToolNotFound(msg) => {
                writeln!(f, "tool not found — is binutils installed? ({msg})")
            }
        }
    }
}
