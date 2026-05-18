use crate::hir::error::ConstFnViolationKind;
use crate::hir::module::ModuleError;
use crate::lexer::HasSpan;
use crate::lexer::error::LexError;
use crate::lexer::token::Span;
use crate::mir::error::{MirError, MirErrorKind};
use crate::parser::error::ParserError;
use crate::{NyxError, hir::error::HirError};
use ariadne::{Color as Colour, Fmt, Label, Report, ReportKind, Source};
use std::cell::RefCell;

#[derive(Debug)]
pub struct Diagnostic {
    pub(crate) rendered: String,
}

pub struct Info {
    message: String,

    primary: LabelInfo,
    secondary: Option<LabelInfo>,

    note: Option<String>,
    help: Option<String>,
}

struct LabelInfo {
    text: String,
    colour: Colour,
    span: Span,
}

pub trait Diagnosticable {
    fn info(&self) -> Info;
}

thread_local! {
    static SOURCE: RefCell<(String, String)> = RefCell::new((String::new(), String::new()));
}

const RED: Colour = Colour::Fixed(210);
const PEACH: Colour = Colour::Fixed(216);
const GREEN: Colour = Colour::Fixed(114);
const BLUE: Colour = Colour::Fixed(111);
const MAUVE: Colour = Colour::Fixed(183);
const YELLOW: Colour = Colour::Fixed(229);
const TEAL: Colour = Colour::Fixed(116);

pub fn initialise(src: &str, filename: &str) {
    SOURCE.with_borrow_mut(|s| *s = (src.to_string(), filename.to_string()));
}

impl Diagnostic {
    pub fn display(self) -> String {
        self.rendered
    }

    fn from_info(info: Info) -> Self {
        let span: std::ops::Range<usize> = info.primary.span.into();

        let rendered = SOURCE.with_borrow(|(src, filename)| {
            let id = filename.as_str();

            let mut builder = Report::build(ReportKind::Error, (id, span.start..span.start))
                .with_message(&info.message)
                .with_label(
                    Label::new((id, span))
                        .with_message(&info.primary.text)
                        .with_color(info.primary.colour),
                );

            if let Some(secondary) = info.secondary {
                let range: std::ops::Range<usize> = secondary.span.into();

                builder = builder.with_label(
                    Label::new((id, range))
                        .with_message(&secondary.text)
                        .with_color(secondary.colour),
                );
            }

            if let Some(note) = &info.note {
                builder = builder.with_note(note);
            }

            if let Some(help) = &info.help {
                builder = builder.with_help(help);
            }

            render(src, filename, builder.finish())
        });

        Self { rendered }
    }
}

impl Info {
    fn primary(message: impl Into<String>, label: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            primary: LabelInfo {
                span,
                text: label.into(),
                colour: RED,
            },
            secondary: None,
            note: None,
            help: None,
        }
    }

    fn with_secondary(mut self, label: impl Into<String>, colour: Colour, span: Span) -> Self {
        self.secondary = Some(LabelInfo {
            text: label.into(),
            span,
            colour,
        });

        self
    }

    fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

#[inline(always)]
fn render<'s>(
    src: &'s str,
    filename: &str,
    report: Report<'_, (&str, std::ops::Range<usize>)>,
) -> String {
    let mut buf = Vec::with_capacity(src.len());
    report.write((filename, Source::from(src)), &mut buf).ok();

    // SAFETY: we know that the string is a valid utf8 cause it's from another valid str buffer
    unsafe { String::from_utf8_unchecked(buf) }
}

impl Diagnosticable for LexError {
    fn info(&self) -> Info {
        use crate::lexer::error::LexErrorKind as Kind;

        let info = match &self.kind {
            Kind::UnexpectedChar(c) => Info::primary(
                format!("unexpected character `{c}`"),
                "not valid here",
                self.span,
            ),
            Kind::UnterminatedString => Info::primary(
                "unterminated string literal",
                "string opened here but never closed",
                self.span,
            )
            .with_help("add a closing '\"' at the end of the string"),
            Kind::UnterminatedComment => Info::primary(
                "unterminated block comment",
                "block comment opened here but never closed",
                self.span,
            )
            .with_help("add a closing '*/'"),
            Kind::InvalidEscape(c) => Info::primary(
                format!("invalid escape sequence `\\{c}`"),
                format!("`\\{c}` is not a recognised escape"),
                self.span,
            )
            .with_help("valid escapes: `\\\\`, `\\\"`, `\\n`, `\\t`, `\\r`, `\\0`"),
            Kind::InvalidNumber(detail) => Info::primary(
                format!("invalid number literal: {detail}"),
                "could not parse this as a number",
                self.span,
            ),
        };

        match self.help {
            Some(ref help) if info.help.is_none() => info.with_help(help),
            _ => info,
        }
    }
}

impl Diagnosticable for ParserError<'_> {
    fn info(&self) -> Info {
        use crate::parser::error::ParseErrorKind as Kind;

        match &self.kind {
            Kind::Lexical(lex) => return lex.info(),

            Kind::Expected { expected, found } => Info::primary(
                format!("expected `{expected}`, found `{found}`"),
                format!("expected `{expected}` here"),
                self.span,
            )
            .with_help(format!("add a `{expected}` token here")),

            Kind::ExpectedIdentifier { found } => Info::primary(
                format!("expected identifier, found `{found}`"),
                "an identifier was expected here",
                self.span,
            ),

            Kind::UnexpectedIdentifier => Info::primary(
                "invalid assignment target",
                "only identifiers can be assigned to",
                self.span,
            ),

            Kind::InvalidBinaryOperator { found } => Info::primary(
                format!("unexpected token `{found}` in expression"),
                format!("`{found}` cannot be used as a binary operator here"),
                self.span,
            )
            .with_note("valid unary operators are `-` (negation) and `!` (logical not)"),

            Kind::InvalidUnaryOperator { found } => Info::primary(
                format!("unexpected token `{found}` in unary expression"),
                format!("`{found}` cannot be used as a unary operator"),
                self.span,
            ),

            Kind::ExpectedExpression { found } => Info::primary(
                format!("expected expression, found `{found}`"),
                "an expression was expected here",
                self.span,
            ),

            Kind::ExpectedTypeIdentifier { found } => Info::primary(
                format!("unknown type `{found}`"),
                format!("`{found}` is not a known type"),
                self.span,
            )
            .with_note(
                "valid types: i8, u8, i16, u16, i32, u32, i64, u64, \
                 f32, f64, bool, char, &str, String, iptr, uptr",
            ),

            Kind::UnexpectedEof => Info::primary(
                "unexpected end of file",
                "the file ended here unexpectedly",
                self.span,
            ),
        }
    }
}

impl Diagnosticable for HirError<'_> {
    fn info(&self) -> Info {
        use crate::hir::error::HirErrorKind as Kind;

        match &self.kind {
            Kind::Parser(_) => unreachable!(),

            Kind::TopLevelNonFunction => Info::primary(
                "only function declarations are allowed at the top level",
                "this is not a function declaration",
                self.span,
            )
            .with_help("move this into a function body, or wrap it in `fn main()`"),

            Kind::DuplicateFunction { name } => Info::primary(
                format!("duplicate function `{name}`"),
                format!("`{name}` is defined here again"),
                self.span,
            )
            .with_help(format!("rename one of the `{name}` functions")),

            Kind::DuplicateMethod { struct_name, name } => Info::primary(
                format!("duplicate method `{name}` for `{struct_name}`"),
                format!("`{name}` is already defined for `{struct_name}`"),
                self.span,
            )
            .with_help(format!("remove or rename one of the `{name}` methods")),

            Kind::UndeclaredIdentifier { name } => Info::primary(
                format!("use of undeclared identifier `{name}`"),
                format!("`{name}` is not declared in this scope"),
                self.span,
            )
            .with_help(format!(
                "declare `{name}` with `let {name} = ...` before using it"
            )),

            Kind::UnknownFunction { name } => Info::primary(
                format!("call to unknown function `{name}`"),
                format!("`{name}` is not a known function"),
                self.span,
            )
            .with_help(format!("declare `fn {name}(...)` before calling it")),

            Kind::UnknownMethod { struct_name, name } => Info::primary(
                format!("call to unknown method `{name}` on `{struct_name}`"),
                format!("`{struct_name}` has no method named `{name}`"),
                self.span,
            )
            .with_help("declare `impl {struct_name} {{ fn {name}(&self) {{ ... }} }}`"),

            Kind::UnknownType { name } => Info::primary(
                format!("unknown type `{name}`"),
                format!("`{name}` is not a known type"),
                self.span,
            )
            .with_help(format!("declare `struct {name} {{ ... }}` before using it")),

            Kind::DuplicateStruct { name } => Info::primary(
                format!("duplicate struct `{name}`"),
                format!("`{name}` is defined here again"),
                self.span,
            )
            .with_help(format!("rename one of the `{name}` structs")),

            Kind::DuplicateField { name } => Info::primary(
                format!("duplicate field `{name}`"),
                format!("`{name}` is already declared"),
                self.span,
            )
            .with_note("struct field names must be unique"),

            Kind::UnknownField { struct_name, field } => Info::primary(
                format!("unknown field `{field}` for struct `{struct_name}`"),
                format!("`{struct_name}` has no field named `{field}`"),
                self.span,
            ),

            Kind::MissingField { struct_name, field } => Info::primary(
                format!("missing field `{field}` for struct `{struct_name}`"),
                format!("`{field}` must be initialised"),
                self.span,
            )
            .with_help(format!("all fields of `{struct_name}` must be provided")),

            Kind::CircularStruct { name } => Info::primary(
                format!("circular struct definition involving `{name}`"),
                format!("`{name}` is part of a by-value struct cycle"),
                self.span,
            )
            .with_help("Nyx does not support self-referential or circular structs yet")
            .with_note("break the cycle; an eventual pointer/box type will be needed for this"),

            Kind::InvalidFieldAccess => Info::primary(
                "invalid field access",
                "field access is only supported on local bindings",
                self.span,
            ),

            Kind::InvalidAssignmentTarget => Info::primary(
                "invalid assignment target",
                "the left-hand side of an assignment must be an identifier or a field access",
                self.span,
            )
            .with_note("use `name = value` or `name.field = value`"),

            Kind::ArityMismatch {
                name,
                expected,
                found,
            } => Info::primary(
                format!("wrong number of arguments to `{name}`"),
                format!(
                    "{found} argument{} provided, but `{name}` expects {expected}",
                    if *found == 1 { "" } else { "s" },
                ),
                self.span,
            ),

            Kind::DuplicateBind { name } => Info::primary(
                format!("duplicate binding `{name}`"),
                format!("`{name}` is already bound in this scope"),
                self.span,
            )
            .with_note("re-declaring the same name in the same scope is not allowed")
            .with_help("use a different name, or shadow it in a nested block"),

            Kind::MissingInitialiser { name } => Info::primary(
                format!("missing initialiser for `{name}`"),
                format!("`{name}` has no initialiser and no type annotation"),
                self.span,
            )
            .with_note("Nyx cannot infer the type without a value to check against")
            .with_help("add a type annotation: `let {name}: <type>;` or provide an initial value"),

            Kind::MissingReceiver { name } => Info::primary(
                format!("method `{name}` is missing a receiver"),
                format!("`{name}` must declare `&self` or `&mut self`"),
                self.span,
            )
            .with_help(format!("write `fn {name}(&self, ...)`")),

            Kind::ReceiverOutsideImpl => Info::primary(
                "`self` receiver outside `impl` block",
                "receivers are only valid in methods",
                self.span,
            )
            .with_help("move this function into an `impl Type { ... }` block"),

            Kind::TypeMismatch { expected, found } => Info::primary(
                format!("type mismatch: expected `{expected}`, found `{found}`"),
                format!("this has type `{found}`"),
                self.span,
            )
            .with_help("expected `{expected}` here"),

            Kind::ImmutableBind { name } => Info::primary(
                format!("cannot assign to immutable binding `{name}`"),
                format!("`{name}` is immutable and cannot be reassigned"),
                self.span,
            )
            .with_note("bindings are immutable by default")
            .with_help(format!("declare it as mutable: `let mut {name} = ...`")),

            Kind::ConstFnViolation(ConstFnViolationKind::NonConstCall { name }) => Info::primary(
                format!("cannot call non-const function `{name}` in a const context"),
                format!("`{name}` is not declared `const`"),
                self.span,
            )
            .with_note("const functions may only call other const functions")
            .with_help(format!(
                "mark `fn {name}` as `const fn {name}` if it qualifies"
            )),

            Kind::UnknownInterface { name } => Info::primary(
                format!("unknown interface `{name}`"),
                format!("`{name}` is not a known interface"),
                self.span,
            )
            .with_help(format!(
                "declare `interface {name} {{ ... }}` before using it"
            )),

            Kind::MissingInterfaceMethod {
                struct_name,
                interface_name,
                method_name: method,
            } => Info::primary(
                format!("missing method `{method}` required by interface `{interface_name}`"),
                format!("`{struct_name}` does not implement `{method}`"),
                self.span,
            )
            .with_note(format!("`{interface_name}` requires `fn {method}(...)`"))
            .with_help(format!(
                "add `fn {method}(...)` to `impl {struct_name} with {interface_name}`"
            )),

            Kind::MissingSuperinterfaceImpl {
                struct_name,
                interface_name,
                superinterface_name,
            } => Info::primary(
                format!(
                    "missing`{superinterface_name}` implementation required by `{interface_name}`",
                ),
                format!(
                    "`{struct_name}` implements `{interface_name}` without `{superinterface_name}`"
                ),
                self.span,
            )
            .with_note(format!(
                "`{interface_name}` extends `{superinterface_name}`"
            ))
            .with_help(format!(
                "add `impl {struct_name} with {superinterface_name} {{ ... }}`"
            )),

            Kind::InterfaceSignatureMismatch {
                struct_name,
                interface_name,
                method_name: method,
                expected,
                found,
            } => Info::primary(
                format!(
                    "method `{}` does not match interface `{interface_name}`",
                    method.fg(Colour::Fixed(115))
                ),
                format!("`{struct_name}` implements `{method}` with an incompatible signature"),
                        self.span).with_note(format!("expected: {expected}\nfound: {found}")).with_help(format!(
                    "update `{method}` in `impl {struct_name} with {interface_name}` to match the interface"
                )),

            Kind::DuplicateInterface { name } => Info::primary(format!("duplicate interface `{name}`"), format!("`{name}` is defined here again"), self.span).with_help(format!("rename one of the `{name}` interfaces")),
        }
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
                message: format!(
                    "circular import: `{}` is already being loaded",
                    path.display()
                ),
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
                message: format!(
                    "module `{}` has no exported symbol `{name}`",
                    path.display()
                ),
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

impl Diagnosticable for MirError {
    fn info(&self) -> Info {
        match &self.kind {
            MirErrorKind::Hir(error) => error.info(),
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
            Self::ToolNotFound(msg) => {
                writeln!(f, "tool not found — is binutils installed? ({msg})")
            }
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

impl From<std::io::Error> for NyxError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
