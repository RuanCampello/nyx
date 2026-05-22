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

pub fn current_source() -> (String, String) {
    SOURCE.with_borrow(|s| s.clone())
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
    source: (String, String),
    message: String,
    labels: Vec<(Span, String, Color)>,
    note: Option<String>,
    help: Option<String>,
}

impl Builder {
    fn new(message: impl Into<String>) -> Self {
        Self {
            source: current_source(),
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
        let (src, filename) = &self.source;
        let id = filename.as_str();

        let anchor = self.labels.first().map(|(s, _, _)| s.start.offset()).unwrap_or(0);

        let mut builder = Report::build(ReportKind::Error, (id, anchor..anchor))
            .with_config(Config::default().with_compact(false))
            .with_message(&self.message);

        for (span, text, color) in self.labels {
            let range: std::ops::Range<usize> = span.into();
            builder =
                builder.with_label(Label::new((id, range)).with_message(text).with_color(color));
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
        let rendered = unsafe { String::from_utf8_unchecked(buf) };

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

            K::UnterminatedChar => Builder::new("unterminated character literal")
                .primary(self.span, "opened here, but never closed")
                .help(format!(
                    "add a closing {} at the end of the character literal",
                    "'".fg(SECONDARY)
                )),

            K::EmptyChar => Builder::new("empty character literal")
                .primary(self.span, "character literals cannot be empty")
                .help("provide a character inside the single quotes"),

            K::OverlongChar => Builder::new("character literal is too long")
                .primary(
                    self.span,
                    "character literals must contain exactly one character",
                )
                .help("use double quotes for string literals instead"),
        };

        let b = match self.help {
            Some(ref h)
                if !matches!(
                    &self.kind,
                    K::UnterminatedString
                        | K::UnterminatedComment
                        | K::InvalidEscape(_)
                        | K::UnterminatedChar
                        | K::EmptyChar
                        | K::OverlongChar
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

            K::OrphanImpl { name } => {
                #[rustfmt::skip]
                let msg = match ["i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "f32", "f64", "bool", "char", "uptr", "iptr", "str", "string", "unit"].contains(&name.as_str()) {
                    true => format!("cannot define impl block for primitive type {} outside of std", hi(name)),
                    _ => format!("cannot define impl block for struct {} defined in another module", hi(name)),
                };
                Builder::new(msg)
                    .primary(
                        self.span,
                        "impl blocks can only be defined in the type's defining module",
                    )
                    .build()
            }

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

            K::CircularConstant { name } => {
                Builder::new(format!("circular dependency in constant {}", hi(name)))
                    .primary(self.span, format!("constant {} depends on itself", hi(name)))
                    .build()
            }

            K::DuplicateConstant { name } => {
                Builder::new(format!("duplicate constant {}", hi(name)))
                    .primary(self.span, format!("{} is defined here again", hi(name)))
                    .help(format!("rename one of the {} constants", hi(name)))
                    .build()
            }
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

#[cfg(test)]
mod tests {
    use crate::diagnostic;
    use crate::hir::{
        self, Type,
        error::{ConstFnViolationKind, HirErrorKind},
    };
    use crate::lexer::{self, Lexer, error::LexErrorKind};
    use crate::parser::{self, Parser, error::ParseErrorKind};

    fn hir_err(src: &str) -> hir::error::HirError<'static> {
        diagnostic::initialise(src, "<test>");
        let statements = Parser::new(src).parse().expect("parse must succeed for hir tests");
        hir::lower(statements)
            .map_err(|e| unsafe {
                std::mem::transmute::<hir::error::HirError<'_>, hir::error::HirError<'static>>(e)
            })
            .unwrap_err()
    }

    fn parse_err(src: &str) -> parser::error::ParserError<'static> {
        diagnostic::initialise(src, "<test>");
        let result = Parser::new(src).parse();
        let err = result.unwrap_err();
        unsafe {
            std::mem::transmute::<parser::error::ParserError<'_>, parser::error::ParserError<'static>>(
                err,
            )
        }
    }

    fn lex_err(src: &str) -> lexer::error::LexError {
        diagnostic::initialise(src, "<test>");
        Lexer::new(src).collect::<Result<Vec<_>, _>>().unwrap_err()
    }

    macro_rules! hir_check {
        ($src:expr) => {{
            let err = hir_err($src);
            let diag = diagnostic::Diagnostic::from(err.clone());
            println!("{}", diag);
            err.kind
        }};
    }

    macro_rules! parse_check {
        ($src:expr) => {{
            let err = parse_err($src);
            let diag = diagnostic::Diagnostic::from(err.clone());
            println!("{}", diag);
            err.kind
        }};
    }

    macro_rules! lex_check {
        ($src:expr) => {{
            let err = lex_err($src);
            let diag = diagnostic::Diagnostic::from(err.clone());
            println!("{}", diag);
            err.kind
        }};
    }

    #[test]
    fn lex_unexpected_char() {
        let kind = lex_check!("let x = @;");
        assert_eq!(kind, LexErrorKind::UnexpectedChar('@'));
    }

    #[test]
    fn lex_unterminated_string() {
        let kind = lex_check!(r#"let x = "hello;"#);
        assert_eq!(kind, LexErrorKind::UnterminatedString);
    }

    #[test]
    fn lex_unterminated_string_newline() {
        let kind = lex_check!("let x = \"hello\nworld\";");
        assert_eq!(kind, LexErrorKind::UnterminatedString);
    }

    #[test]
    fn lex_unterminated_block_comment() {
        let kind = lex_check!("let x = 1; /* never closed");
        assert_eq!(kind, LexErrorKind::UnterminatedComment);
    }

    #[test]
    fn lex_invalid_escape() {
        let kind = lex_check!(r#"let x = "hello\qworld";"#);
        assert_eq!(kind, LexErrorKind::InvalidEscape('q'));
    }

    #[test]
    fn lex_invalid_escape_various() {
        for (src, ch) in [(r#""\p""#, 'p'), (r#""\x""#, 'x'), (r#""\z""#, 'z')] {
            let kind = lex_check!(src);
            assert_eq!(kind, LexErrorKind::InvalidEscape(ch), "source: {src}");
        }
    }

    #[test]
    fn parse_expected_token() {
        let kind = parse_check!("let x = 1");
        assert!(
            matches!(kind, ParseErrorKind::Expected { .. }),
            "got {kind:?}"
        );
    }

    #[test]
    fn parse_expected_identifier() {
        let kind = parse_check!("let 42: i32 = 1;");
        assert!(
            matches!(kind, ParseErrorKind::ExpectedIdentifier { .. }),
            "got {kind:?}"
        );
    }

    #[test]
    fn parse_unexpected_identifier_bad_assignment() {
        let kind = parse_check!("fn main() { (a + b) = 1; }");
        assert!(
            matches!(kind, ParseErrorKind::UnexpectedIdentifier),
            "got {kind:?}"
        );
    }

    #[test]
    fn parse_invalid_binary_operator() {
        let kind = lex_check!("fn main() { let x = 1 % 2; }");
        assert_eq!(kind, LexErrorKind::UnexpectedChar('%'));
    }

    #[test]
    fn parse_invalid_unary_operator() {
        // `+` has no prefix parse rule; the parser raises ExpectedExpression
        let kind = parse_check!("fn main() { let x = +1; }");
        assert!(
            matches!(kind, ParseErrorKind::ExpectedExpression { .. }),
            "got {kind:?}"
        );
    }

    #[test]
    fn parse_expected_expression() {
        let kind = parse_check!("fn main() { let x = ; }");
        assert!(
            matches!(kind, ParseErrorKind::ExpectedExpression { .. }),
            "got {kind:?}"
        );
    }

    #[test]
    fn parse_expected_type_identifier() {
        let kind = parse_check!("fn main() { let x: &unknown = 1; }");
        assert!(
            matches!(kind, ParseErrorKind::ExpectedTypeIdentifier { .. }),
            "got {kind:?}"
        );
    }

    #[test]
    fn parse_unexpected_eof() {
        let kind = parse_check!("fn main() {");
        assert!(
            matches!(kind, ParseErrorKind::UnexpectedEof),
            "got {kind:?}"
        );
    }

    #[test]
    fn parse_lexical_error_surfaced() {
        // A lex error that surfaces through the parser path
        let kind = parse_check!(r#"fn main() { let x = "\q"; }"#);
        assert!(matches!(kind, ParseErrorKind::Lexical(_)), "got {kind:?}");
    }

    #[test]
    fn hir_top_level_non_function() {
        let kind = hir_check!("let x: i32 = 1;");
        assert_eq!(kind, HirErrorKind::TopLevelNonFunction);
    }

    #[test]
    fn hir_duplicate_function() {
        let kind = hir_check!(
            "fn foo(): i32 { 1 }
         fn foo(): i32 { 2 }"
        );
        assert_eq!(kind, HirErrorKind::DuplicateFunction { name: "foo".into() });
    }

    #[test]
    fn hir_duplicate_method() {
        let kind = hir_check!(
            "struct Counter { value: i32 }
         impl Counter { fn get(&self): i32 { self.value } }
         impl Counter { fn get(&self): i32 { self.value } }"
        );
        assert_eq!(
            kind,
            HirErrorKind::DuplicateMethod {
                struct_name: "Counter".into(),
                name: "get".into(),
            }
        );
    }

    #[test]
    fn hir_undeclared_identifier() {
        let kind = hir_check!("fn main() { x + 1; }");
        assert_eq!(
            kind,
            HirErrorKind::UndeclaredIdentifier { name: "x".into() }
        );
    }

    #[test]
    fn hir_unknown_function() {
        let kind = hir_check!("fn main() { foo(); }");
        assert_eq!(kind, HirErrorKind::UnknownFunction { name: "foo".into() });
    }

    #[test]
    fn hir_unknown_method() {
        let kind = hir_check!(
            "struct Point { x: i32 }
         fn main() { let p = Point { x: 1 }; p.frobnicate(); }"
        );
        assert_eq!(
            kind,
            HirErrorKind::UnknownMethod {
                struct_name: "Point".into(),
                name: "frobnicate".into(),
            }
        );
    }

    #[test]
    fn hir_unknown_type_in_let() {
        let kind = hir_check!("fn main() { let x: Phantom = 1; }");
        assert_eq!(
            kind,
            HirErrorKind::UnknownType {
                name: "Phantom".into()
            }
        );
    }

    #[test]
    fn hir_unknown_type_in_param() {
        let kind = hir_check!("fn foo(x: Ghost): i32 { 0 }");
        assert_eq!(
            kind,
            HirErrorKind::UnknownType {
                name: "Ghost".into()
            }
        );
    }

    #[test]
    fn hir_duplicate_struct() {
        let kind = hir_check!(
            "struct Foo { x: i32 }
         struct Foo { y: i32 }"
        );
        assert_eq!(kind, HirErrorKind::DuplicateStruct { name: "Foo".into() });
    }

    #[test]
    fn hir_duplicate_field_in_struct() {
        let kind = hir_check!("struct Bad { x: i32, x: i64 }");
        assert_eq!(kind, HirErrorKind::DuplicateField { name: "x".into() });
    }

    #[test]
    fn hir_duplicate_field_in_literal() {
        let kind = hir_check!(
            "struct Point { x: i32, y: i32 }
         fn main() { let p = Point { x: 1, x: 2 }; }"
        );
        assert_eq!(kind, HirErrorKind::DuplicateField { name: "x".into() });
    }

    #[test]
    fn hir_invalid_field_access() {
        let kind = hir_check!(
            "struct Point { x: i32 }
         fn make(): Point { Point { x: 1 } }
         fn main(): i32 { make().x }"
        );
        assert_eq!(kind, HirErrorKind::InvalidFieldAccess);
    }

    #[test]
    fn hir_invalid_assignment_target() {
        let kind = parse_check!("fn main() { (a + b) = 1; }");
        assert!(
            matches!(kind, ParseErrorKind::UnexpectedIdentifier),
            "got {kind:?}"
        );
    }

    #[test]
    fn hir_unknown_field() {
        let kind = hir_check!(
            "struct Point { x: i32 }
         fn main() { let p = Point { x: 1 }; let _ = p.z; }"
        );
        assert_eq!(
            kind,
            HirErrorKind::UnknownField {
                struct_name: "Point".into(),
                field: "z".into(),
            }
        );
    }

    #[test]
    fn hir_missing_field_in_literal() {
        let kind = hir_check!(
            "struct Point { x: i32, y: i32 }
         fn main() { let p = Point { x: 1 }; }"
        );
        assert_eq!(
            kind,
            HirErrorKind::MissingField {
                struct_name: "Point".into(),
                field: "y".into(),
            }
        );
    }

    #[test]
    fn hir_circular_struct() {
        let kind = hir_check!(
            "struct A { b: B }
         struct B { a: A }"
        );
        assert_eq!(kind, HirErrorKind::CircularStruct { name: "A".into() });
    }

    #[test]
    fn hir_arity_mismatch_too_many() {
        let kind = hir_check!(
            "fn add(a: i32, b: i32): i32 { a + b }
         fn main() { add(1, 2, 3); }"
        );
        assert_eq!(
            kind,
            HirErrorKind::ArityMismatch {
                name: "add".into(),
                expected: 2,
                found: 3,
            }
        );
    }

    #[test]
    fn hir_arity_mismatch_too_few() {
        let kind = hir_check!(
            "fn add(a: i32, b: i32): i32 { a + b }
         fn main() { add(1); }"
        );
        assert_eq!(
            kind,
            HirErrorKind::ArityMismatch {
                name: "add".into(),
                expected: 2,
                found: 1,
            }
        );
    }

    #[test]
    fn hir_arity_mismatch_method() {
        let kind = hir_check!(
            "struct Counter { value: i32 }
         impl Counter { fn add(&mut self, delta: i32) { self.value = self.value + delta; } }
         fn main() { let mut c = Counter { value: 0 }; c.add(1, 2); }"
        );
        assert_eq!(
            kind,
            HirErrorKind::ArityMismatch {
                name: "add".into(),
                expected: 1,
                found: 2,
            }
        );
    }

    #[test]
    fn hir_duplicate_bind() {
        let kind = hir_check!(
            "fn main() {
             let x: i32 = 1;
             let x: i32 = 2;
         }"
        );
        assert_eq!(kind, HirErrorKind::DuplicateBind { name: "x".into() });
    }

    #[test]
    fn hir_missing_initialiser() {
        let kind = hir_check!("fn main() { let x; }");
        assert_eq!(kind, HirErrorKind::MissingInitialiser { name: "x".into() });
    }

    #[test]
    fn hir_receiver_outside_impl() {
        let kind = hir_check!("fn foo(&self): i32 { 0 }");
        assert_eq!(kind, HirErrorKind::ReceiverOutsideImpl);
    }

    #[test]
    fn hir_type_mismatch_let_annotation() {
        let kind = hir_check!(
            "fn main() {
             let x: i32 = true;
         }"
        );
        assert_eq!(
            kind,
            HirErrorKind::TypeMismatch {
                expected: Type::I32,
                found: Type::Bool,
            }
        );
    }

    #[test]
    fn hir_type_mismatch_return_type() {
        let kind = hir_check!("fn foo(): i32 { true }");
        assert_eq!(
            kind,
            HirErrorKind::TypeMismatch {
                expected: Type::I32,
                found: Type::Bool,
            }
        );
    }

    #[test]
    fn hir_type_mismatch_if_condition() {
        let kind = hir_check!(
            "fn main() {
             if 42 { }
         }"
        );
        assert_eq!(
            kind,
            HirErrorKind::TypeMismatch {
                expected: Type::Bool,
                found: Type::I32,
            }
        );
    }

    #[test]
    fn hir_type_mismatch_while_condition() {
        let kind = hir_check!(
            "fn main() {
             while 1 { }
         }"
        );
        assert_eq!(
            kind,
            HirErrorKind::TypeMismatch {
                expected: Type::Bool,
                found: Type::I32,
            }
        );
    }

    #[test]
    fn hir_immutable_bind_variable() {
        let kind = hir_check!(
            "fn main() {
             let x: i32 = 1;
             x = 2;
         }"
        );
        assert_eq!(kind, HirErrorKind::ImmutableBind { name: "x".into() });
    }

    #[test]
    fn hir_immutable_bind_field_via_shared_self() {
        let kind = hir_check!(
            "struct Counter { value: i32 }
         impl Counter {
             fn bad(&self) { self.value = 1; }
         }"
        );
        assert_eq!(
            kind,
            HirErrorKind::ImmutableBind {
                name: "self".into()
            }
        );
    }

    #[test]
    fn hir_immutable_bind_mut_method_on_immutable_var() {
        let kind = hir_check!(
            "struct Counter { value: i32 }
         impl Counter {
             fn inc(&mut self) { self.value = self.value + 1; }
         }
         fn main() {
             let c = Counter { value: 0 };
             c.inc();
         }"
        );
        assert_eq!(kind, HirErrorKind::ImmutableBind { name: "c".into() });
    }

    #[test]
    fn hir_const_fn_violation() {
        let kind = hir_check!(
            "fn helper(): i32 { 42 }
         const fn bad(): i32 { helper() }"
        );
        assert!(
            matches!(
                kind,
                HirErrorKind::ConstFnViolation(ConstFnViolationKind::NonConstCall { .. })
            ),
            "got {kind:?}"
        );
        if let HirErrorKind::ConstFnViolation(ConstFnViolationKind::NonConstCall { name }) = kind {
            assert_eq!(name, "helper");
        }
    }

    #[test]
    fn hir_duplicate_interface() {
        let kind = hir_check!(
            "interface Greet { fn hello(&self); }
         interface Greet { fn bye(&self); }"
        );
        assert_eq!(
            kind,
            HirErrorKind::DuplicateInterface {
                name: "Greet".into()
            }
        );
    }

    #[test]
    fn hir_unknown_interface_in_impl() {
        let kind = hir_check!(
            "struct Foo { x: i32 }
         impl Foo with Ghost { fn hello(&self) { } }"
        );
        assert_eq!(
            kind,
            HirErrorKind::UnknownInterface {
                name: "Ghost".into()
            }
        );
    }

    #[test]
    fn hir_unknown_interface_in_superinterface() {
        let kind = hir_check!("interface Child: NonExistent { fn method(&self); }");
        assert_eq!(
            kind,
            HirErrorKind::UnknownInterface {
                name: "NonExistent".into()
            }
        );
    }

    #[test]
    fn hir_missing_interface_method() {
        let kind = hir_check!(
            "interface Greet {
             fn hello(&self): i32;
             fn bye(&self): i32;
         }
         struct Foo { x: i32 }
         impl Foo with Greet {
             fn hello(&self): i32 { 1 }
         }"
        );
        assert_eq!(
            kind,
            HirErrorKind::MissingInterfaceMethod {
                struct_name: "Foo".into(),
                interface_name: "Greet".into(),
                method_name: "bye".into(),
            }
        );
    }

    #[test]
    fn hir_missing_superinterface_impl() {
        let kind = hir_check!(
            "interface Base { fn base(&self): i32; }
         interface Derived: Base { fn derived(&self): i32; }
         struct Foo { x: i32 }
         impl Foo with Derived {
             fn derived(&self): i32 { 1 }
         }"
        );
        assert_eq!(
            kind,
            HirErrorKind::MissingSuperinterfaceImpl {
                struct_name: "Foo".into(),
                interface_name: "Derived".into(),
                superinterface_name: "Base".into(),
            }
        );
    }

    #[test]
    fn hir_interface_signature_mismatch_return_type() {
        let kind = hir_check!(
            "interface Shape { fn area(&self): i64; }
         struct Rect { w: i32, h: i32 }
         impl Rect with Shape {
             fn area(&self): i32 { self.w * self.h }
         }"
        );
        assert!(
            matches!(kind, HirErrorKind::InterfaceSignatureMismatch { .. }),
            "got {kind:?}"
        );
        if let HirErrorKind::InterfaceSignatureMismatch {
            struct_name,
            interface_name,
            method_name,
            ..
        } = &kind
        {
            assert_eq!(struct_name, "Rect");
            assert_eq!(interface_name, "Shape");
            assert_eq!(method_name, "area");
        }
    }

    #[test]
    fn hir_interface_signature_mismatch_extra_param() {
        let kind = hir_check!(
            "interface Namer { fn name(&self): i32; }
         struct Foo { x: i32 }
         impl Foo with Namer {
             fn name(&self, extra: i32): i32 { extra }
         }"
        );
        assert!(
            matches!(kind, HirErrorKind::InterfaceSignatureMismatch { .. }),
            "got {kind:?}"
        );
        if let HirErrorKind::InterfaceSignatureMismatch { method_name, .. } = &kind {
            assert_eq!(method_name, "name");
        }
    }

    #[test]
    fn hir_interface_signature_mismatch_wrong_receiver_mutability() {
        let kind = hir_check!(
            "interface Mutator { fn mutate(&mut self); }
         struct Foo { x: i32 }
         impl Foo with Mutator {
             fn mutate(&self) { }
         }"
        );
        assert!(
            matches!(kind, HirErrorKind::InterfaceSignatureMismatch { .. }),
            "got {kind:?}"
        );
    }
}
