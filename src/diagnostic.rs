use crate::hir::module::ModuleError;
use crate::lexer::HasSpan;
use crate::lexer::error::LexError;
use crate::lexer::token::{BytePos, Span};
use crate::mir::error::{MirError, MirErrorKind};
use crate::parser::error::ParserError;
use crate::source_map::{FileId, SourceMap};
use crate::{NyxError, hir::error::HirError};
use ariadne::{Cache, Color, Config, Fmt, Label as AriadneLabel, Report, ReportKind, Source};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

/// A diagnostic in structured, plain-text form, the same information the CLI
/// renders through `ariadne`, but consumable across the crate boundary
#[derive(Debug, Clone, PartialEq)]
pub struct RichDiagnostic {
    pub severity: Severity,
    pub code: Option<&'static str>,
    pub message: String,
    pub primary: Option<Label>,
    pub secondary: Vec<Label>,
    pub note: Option<String>,
    pub help: Option<String>,
}

/// A single labelled span within a [`RichDiagnostic`], carrying plain (no ANSI)
/// text so consumers like the LSP can present it however they wish
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Debug)]
pub struct Diagnostic {
    pub(crate) rendered: String,
}

pub struct Builder {
    message: String,
    labels: Vec<(Span, String, Color)>,
    note: Option<String>,
    help: Option<String>,
}

/// An [`ariadne::Cache`] over the per-thread [`SourceMap`], building one
/// [`Source`] per file on first use so a single report can span many files
struct MapCache {
    sources: HashMap<FileId, Source<String>>,
    names: HashMap<FileId, String>,
}

/// Severity of a [`RichDiagnostic`]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

pub trait AsDiagnostic {
    fn into_diagnostic(self, span: Span) -> Diagnostic;
    fn rich(self, span: Span) -> RichDiagnostic;
    fn message(self) -> String;
}

pub(crate) const PRIMARY: Color = Color::Rgb(243, 139, 168);
pub(crate) const SECONDARY: Color = Color::Rgb(180, 190, 254);
pub(crate) const HIGHLIGHT: Color = Color::Rgb(137, 180, 250);

thread_local! {
    static SOURCE_MAP: RefCell<SourceMap> = RefCell::new(SourceMap::default());
}

#[inline(always)]
pub(crate) fn hi(s: impl std::fmt::Display) -> impl std::fmt::Display {
    s.fg(HIGHLIGHT)
}

/// Clear the per-thread source map
/// Call once at the start of a compilation or analysis run before registering files
pub fn reset() {
    SOURCE_MAP.with_borrow_mut(|map| *map = SourceMap::default());
}

/// Register a file in the per-thread source map and return its id and the base
/// offset its spans must be relative to
pub fn add_file(name: impl Into<std::path::PathBuf>, src: impl Into<String>) -> (FileId, BytePos) {
    SOURCE_MAP.with_borrow_mut(|map| map.add_file(name, src))
}

/// Move the per-thread source map out, leaving an empty one behind
/// The caller (e.g. the LSP) takes ownership to resolve spans after a run completes
pub fn take_source_map() -> SourceMap {
    SOURCE_MAP.with_borrow_mut(std::mem::take)
}

impl RichDiagnostic {
    pub fn bare(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: None,
            message: message.into(),
            primary: None,
            secondary: Vec::new(),
            note: None,
            help: None,
        }
    }
}

impl AsDiagnostic for RichDiagnostic {
    fn into_diagnostic(self, _span: Span) -> Diagnostic {
        let mut builder = Builder::new(self.message);
        if let Some(primary) = self.primary {
            builder = builder.primary(primary.span, primary.message);
        }
        for label in self.secondary {
            builder = builder.secondary(label.span, label.message);
        }
        if let Some(note) = self.note {
            builder = builder.note(note);
        }
        if let Some(help) = self.help {
            builder = builder.help(help);
        }
        builder.build()
    }

    fn rich(self, _span: Span) -> RichDiagnostic {
        self
    }

    fn message(self) -> String {
        self.message
    }
}

impl AsDiagnostic for Box<RichDiagnostic> {
    fn into_diagnostic(self, span: Span) -> Diagnostic {
        (*self).into_diagnostic(span)
    }

    fn rich(self, span: Span) -> RichDiagnostic {
        (*self).rich(span)
    }

    fn message(self) -> String {
        (*self).message()
    }
}

impl std::fmt::Display for RichDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;

        if let Some(primary) = &self.primary
            && !primary.message.is_empty()
        {
            write!(f, "\n{}", primary.message)?;
        }
        if let Some(note) = &self.note {
            write!(f, "\nnote: {note}")?;
        }
        if let Some(help) = &self.help {
            write!(f, "\nhelp: {help}")?;
        }

        Ok(())
    }
}

impl Diagnostic {
    pub fn display(self) -> String {
        self.rendered
    }

    pub fn from_builder(builder: Builder) -> Self {
        builder.build()
    }
}

impl AsDiagnostic for Diagnostic {
    fn into_diagnostic(self, _span: Span) -> Diagnostic {
        self
    }

    fn rich(self, _span: Span) -> RichDiagnostic {
        RichDiagnostic::bare(self.rendered)
    }

    fn message(self) -> String {
        self.rendered
    }
}

impl<'src> From<LexError<'src>> for Diagnostic {
    fn from(e: LexError<'src>) -> Self {
        e.into_diagnostic(Span::default())
    }
}

impl<'i> From<ParserError<'i>> for Diagnostic {
    fn from(e: ParserError<'i>) -> Self {
        e.into_diagnostic(Span::default())
    }
}

impl<'h> From<HirError<'h>> for Diagnostic {
    fn from(e: HirError<'h>) -> Self {
        e.kind.into_diagnostic(e.span)
    }
}

impl From<MirError> for Diagnostic {
    fn from(e: MirError) -> Self {
        match e.kind {
            MirErrorKind::Hir(diagnostic) => diagnostic,
        }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered)
    }
}

impl Builder {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            labels: Vec::new(),
            note: None,
            help: None,
        }
    }

    pub fn primary(mut self, span: Span, text: impl Into<String>) -> Self {
        self.labels.insert(0, (span, text.into(), PRIMARY));
        self
    }

    pub fn secondary(mut self, span: Span, text: impl Into<String>) -> Self {
        self.labels.push((span, text.into(), Color::Primary));
        self
    }

    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.note = Some(text.into());
        self
    }

    pub fn help(mut self, text: impl Into<String>) -> Self {
        self.help = Some(text.into());
        self
    }

    pub fn build(self) -> Diagnostic {
        SOURCE_MAP.with_borrow(|map| self.render(map))
    }

    fn render(self, map: &SourceMap) -> Diagnostic {
        if self.labels.is_empty() || map.is_empty() {
            return Diagnostic { rendered: self.message };
        }

        let (anchor_file, anchor) = map.local_range(self.labels[0].0);
        let cache = MapCache::new(map, self.labels.iter().map(|(s, _, _)| map.span_data(*s).file));

        let mut builder =
            Report::build(ReportKind::Error, (anchor_file, anchor.start..anchor.start))
                .with_config(Config::default().with_compact(false))
                .with_message(&self.message);

        for (span, text, color) in self.labels {
            let (file, range) = map.local_range(span);
            builder = builder
                .with_label(AriadneLabel::new((file, range)).with_message(text).with_color(color));
        }

        if let Some(note) = &self.note {
            builder = builder.with_note(note);
        }
        if let Some(help) = &self.help {
            builder = builder.with_help(help);
        }

        let mut buf = Vec::new();
        builder.finish().write(cache, &mut buf).ok();
        // SAFETY: ariadne only writes valid UTF-8
        let rendered = unsafe { String::from_utf8_unchecked(buf) };
        Diagnostic { rendered }
    }
}

impl MapCache {
    fn new(map: &SourceMap, files: impl IntoIterator<Item = FileId>) -> Self {
        let mut sources = HashMap::new();
        let mut names = HashMap::new();
        for id in files {
            sources.entry(id).or_insert_with(|| Source::from(map.source(id).to_owned()));
            names.entry(id).or_insert_with(|| map.path(id).display().to_string());
        }
        Self { sources, names }
    }
}

impl Cache<FileId> for MapCache {
    type Storage = String;

    fn fetch(&mut self, id: &FileId) -> Result<&Source<String>, impl fmt::Debug> {
        self.sources.get(id).ok_or_else(|| format!("unregistered file {id:?}"))
    }

    fn display<'a>(&self, id: &'a FileId) -> Option<impl fmt::Display + 'a> {
        self.names.get(id).cloned()
    }
}

impl<'src> AsDiagnostic for LexError<'src> {
    fn into_diagnostic(self, _span: Span) -> Diagnostic {
        self.kind.into_diagnostic(self.span)
    }

    fn rich(self, _span: Span) -> RichDiagnostic {
        self.kind.rich(self.span)
    }

    fn message(self) -> String {
        self.kind.message()
    }
}

impl<'i> AsDiagnostic for ParserError<'i> {
    fn into_diagnostic(self, _span: Span) -> Diagnostic {
        self.kind.into_diagnostic(self.span)
    }

    fn rich(self, _span: Span) -> RichDiagnostic {
        self.kind.rich(self.span)
    }

    fn message(self) -> String {
        self.kind.message()
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
            },
        }
    }
}

impl From<Span> for std::ops::Range<usize> {
    fn from(value: Span) -> Self {
        Self { start: value.start.offset(), end: value.end.offset() }
    }
}

impl<'src> HasSpan for LexError<'src> {
    fn span(&self) -> Option<Span> {
        Some(self.span)
    }
}

impl<'p> HasSpan for ParserError<'p> {
    fn span(&self) -> Option<Span> {
        Some(self.span)
    }
}

impl<'h> HasSpan for HirError<'h> {
    fn span(&self) -> Option<Span> {
        Some(self.span)
    }
}

impl HasSpan for ModuleError {
    fn span(&self) -> Option<Span> {
        match self {
            Self::CircularImport { span, .. } => Some(*span),
            Self::UnknownRoot { span, .. } => Some(*span),
            Self::UnknownExport { span, .. } => Some(*span),
            Self::TopLevelNonFunction { span, .. } => Some(*span),
            Self::FileNotFound { span: Some(s), .. } => Some(*s),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::diagnostic;
    use crate::hir::{
        self, TypeKind,
        error::{ConstFnViolationKind, HirErrorKind},
    };
    use crate::lexer::{self, Lexer, error::LexErrorKind};
    use crate::parser::{self, Parser, error::ParseErrorKind};

    fn hir_err(src: &str) -> hir::error::HirError<'static> {
        diagnostic::reset();
        diagnostic::add_file("<test>", src);
        // Leak the source and arena so the borrowed error data lives for the whole
        // (short-lived) test process, letting us return a genuinely `'static` error.
        let src: &'static str = Box::leak(src.to_string().into_boxed_str());
        let arena: &'static bumpalo::Bump = Box::leak(Box::new(bumpalo::Bump::new()));
        let statements = Parser::new(src).parse().expect("parse must succeed for hir tests");
        hir::lower(statements, arena).unwrap_err()
    }

    fn parse_err(src: &str) -> parser::error::ParserError<'static> {
        diagnostic::reset();
        diagnostic::add_file("<test>", src);
        let result = Parser::new(src).parse();
        let err = result.unwrap_err();
        unsafe {
            std::mem::transmute::<parser::error::ParserError<'_>, parser::error::ParserError<'static>>(
                err,
            )
        }
    }

    fn lex_err<'a>(src: &'a str) -> lexer::error::LexError<'a> {
        diagnostic::reset();
        diagnostic::add_file("<test>", src);
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
        assert!(matches!(kind, ParseErrorKind::Expected { .. }), "got {kind:?}");
    }

    #[test]
    fn parse_expected_identifier() {
        let kind = parse_check!("let 42: i32 = 1;");
        assert!(matches!(kind, ParseErrorKind::ExpectedIdentifier { .. }), "got {kind:?}");
    }

    #[test]
    fn parse_unexpected_identifier_bad_assignment() {
        let kind = parse_check!("fn main() { (a + b) = 1; }");
        assert!(matches!(kind, ParseErrorKind::UnexpectedIdentifier), "got {kind:?}");
    }

    #[test]
    fn parse_invalid_binary_operator() {
        let kind = lex_check!("fn main() { let x = 1 % 2; }");
        assert_eq!(kind, LexErrorKind::UnexpectedChar('%'));
    }

    #[test]
    fn parse_invalid_unary_operator() {
        let kind = parse_check!("fn main() { let x = +1; }");
        assert!(matches!(kind, ParseErrorKind::ExpectedExpression { .. }), "got {kind:?}");
    }

    #[test]
    fn parse_expected_expression() {
        let kind = parse_check!("fn main() { let x = ; }");
        assert!(matches!(kind, ParseErrorKind::ExpectedExpression { .. }), "got {kind:?}");
    }

    #[test]
    fn parse_unexpected_eof() {
        let kind = parse_check!("fn main() {");
        assert!(matches!(kind, ParseErrorKind::UnexpectedEof), "got {kind:?}");
    }

    #[test]
    fn parse_lexical_error_surfaced() {
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
        assert_eq!(kind, HirErrorKind::DuplicateFunction { name: "foo" });
    }

    #[test]
    fn hir_duplicate_method() {
        let kind = hir_check!(
            "struct Counter { value: i32 }
         impl Counter { fn get(&self): i32 { self.value } }
         impl Counter { fn get(&self): i32 { self.value } }"
        );
        assert_eq!(kind, HirErrorKind::DuplicateMethod { struct_name: "Counter", name: "get" });
    }

    #[test]
    fn hir_undeclared_identifier() {
        let kind = hir_check!("fn main() { x + 1; }");
        assert_eq!(kind, HirErrorKind::UndeclaredIdentifier { name: "x" });
    }

    #[test]
    fn hir_unknown_function() {
        let kind = hir_check!("fn main() { foo(); }");
        assert_eq!(kind, HirErrorKind::UnknownFunction { name: "foo" });
    }

    #[test]
    fn rich_diagnostic_carries_plain_structure() {
        use crate::diagnostic::{AsDiagnostic, Severity};
        let err = hir_err("fn main() { foo(); }");
        let span = err.span;
        let rich = err.kind.rich(span);

        assert_eq!(rich.severity, Severity::Error);
        // plain text, no ANSI escape sequences
        assert!(!rich.message.contains('\u{1b}'));
        assert!(rich.message.contains("foo"));
        let primary = rich.primary.expect("primary label");
        assert_eq!(primary.span, span);
        assert!(rich.help.is_some());
    }

    #[test]
    fn hir_unknown_method() {
        let kind = hir_check!(
            "struct Point { x: i32 }
         fn main() { let p = Point { x: 1 }; p.frobnicate(); }"
        );
        assert_eq!(kind, HirErrorKind::UnknownMethod { struct_name: "Point", name: "frobnicate" });
    }

    #[test]
    fn hir_unknown_type_in_let() {
        let kind = hir_check!("fn main() { let x: Phantom = 1; }");
        assert_eq!(kind, HirErrorKind::UnknownType { name: "Phantom" });
    }

    #[test]
    fn hir_unknown_type_in_param() {
        let kind = hir_check!("fn foo(x: Ghost): i32 { 0 }");
        assert_eq!(kind, HirErrorKind::UnknownType { name: "Ghost" });
    }

    #[test]
    fn hir_duplicate_struct() {
        let kind = hir_check!(
            "struct Foo { x: i32 }
         struct Foo { y: i32 }"
        );
        assert_eq!(kind, HirErrorKind::DuplicateStruct { name: "Foo" });
    }

    #[test]
    fn hir_duplicate_field_in_struct() {
        let kind = hir_check!("struct Bad { x: i32, x: i64 }");
        assert_eq!(kind, HirErrorKind::DuplicateField { name: "x" });
    }

    #[test]
    fn hir_duplicate_field_in_literal() {
        let kind = hir_check!(
            "struct Point { x: i32, y: i32 }
         fn main() { let p = Point { x: 1, x: 2 }; }"
        );
        assert_eq!(kind, HirErrorKind::DuplicateField { name: "x" });
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
        assert!(matches!(kind, ParseErrorKind::UnexpectedIdentifier), "got {kind:?}");
    }

    #[test]
    fn hir_unknown_field() {
        let kind = hir_check!(
            "struct Point { x: i32 }
         fn main() { let p = Point { x: 1 }; let _ = p.z; }"
        );
        assert_eq!(kind, HirErrorKind::UnknownField { struct_name: "Point", field: "z" });
    }

    #[test]
    fn hir_missing_field_in_literal() {
        let kind = hir_check!(
            "struct Point { x: i32, y: i32 }
         fn main() { let p = Point { x: 1 }; }"
        );
        assert_eq!(kind, HirErrorKind::MissingField { struct_name: "Point", field: "y" });
    }

    #[test]
    fn hir_circular_struct() {
        let kind = hir_check!(
            "struct A { b: B }
         struct B { a: A }"
        );
        assert_eq!(kind, HirErrorKind::CircularStruct { name: "A" });
    }

    #[test]
    fn hir_arity_mismatch_too_many() {
        let kind = hir_check!(
            "fn add(a: i32, b: i32): i32 { a + b }
         fn main() { add(1, 2, 3); }"
        );
        assert_eq!(kind, HirErrorKind::ArityMismatch { name: "nyx::add", expected: 2, found: 3 });
    }

    #[test]
    fn hir_arity_mismatch_too_few() {
        let kind = hir_check!(
            "fn add(a: i32, b: i32): i32 { a + b }
         fn main() { add(1); }"
        );
        assert_eq!(kind, HirErrorKind::ArityMismatch { name: "nyx::add", expected: 2, found: 1 });
    }

    #[test]
    fn hir_arity_mismatch_method() {
        let kind = hir_check!(
            "struct Counter { value: i32 }
         impl Counter { fn add(&mut self, delta: i32) { self.value = self.value + delta; } }
         fn main() { let mut c = Counter { value: 0 }; c.add(1, 2); }"
        );
        assert_eq!(kind, HirErrorKind::ArityMismatch { name: "add", expected: 1, found: 2 });
    }

    #[test]
    fn hir_duplicate_bind() {
        let kind = hir_check!(
            "fn main() {
             let x: i32 = 1;
             let x: i32 = 2;
         }"
        );
        assert_eq!(kind, HirErrorKind::DuplicateBind { name: "x" });
    }

    #[test]
    fn hir_missing_initialiser() {
        let kind = hir_check!("fn main() { let x; }");
        assert_eq!(kind, HirErrorKind::MissingInitialiser { name: "x" });
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
                expected: TypeKind::I32.into(),
                found: TypeKind::Bool.into()
            }
        );
    }

    #[test]
    fn hir_type_mismatch_return_type() {
        let kind = hir_check!("fn foo(): i32 { true }");
        assert_eq!(
            kind,
            HirErrorKind::TypeMismatch {
                expected: TypeKind::I32.into(),
                found: TypeKind::Bool.into()
            }
        );
    }

    #[test]
    fn hir_type_mismatch_if_condition() {
        let kind = hir_check!("fn main() { if 42 { } }");
        assert_eq!(
            kind,
            HirErrorKind::TypeMismatch {
                expected: TypeKind::Bool.into(),
                found: TypeKind::I32.into()
            }
        );
    }

    #[test]
    fn hir_type_mismatch_while_condition() {
        let kind = hir_check!("fn main() { while 1 { } }");
        assert_eq!(
            kind,
            HirErrorKind::TypeMismatch {
                expected: TypeKind::Bool.into(),
                found: TypeKind::I32.into()
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
        assert_eq!(kind, HirErrorKind::ImmutableBind { name: "x" });
    }

    #[test]
    fn hir_immutable_bind_field_via_shared_self() {
        let kind = hir_check!(
            "struct Counter { value: i32 }
         impl Counter { fn bad(&self) { self.value = 1; } }"
        );
        assert_eq!(kind, HirErrorKind::ImmutableBind { name: "self" });
    }

    #[test]
    fn hir_immutable_bind_mut_method_on_immutable_var() {
        let kind = hir_check!(
            "struct Counter { value: i32 }
         impl Counter { fn inc(&mut self) { self.value = self.value + 1; } }
         fn main() {
             let c = Counter { value: 0 };
             c.inc();
         }"
        );
        assert_eq!(kind, HirErrorKind::ImmutableBind { name: "c" });
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
            assert_eq!(name, "nyx::helper");
        }
    }

    #[test]
    fn hir_duplicate_interface() {
        let kind = hir_check!(
            "interface Greet { fn hello(&self); }
         interface Greet { fn bye(&self); }"
        );
        assert_eq!(kind, HirErrorKind::DuplicateInterface { name: "Greet" });
    }

    #[test]
    fn hir_unknown_interface_in_impl() {
        let kind = hir_check!(
            "struct Foo { x: i32 }
         impl Foo with Ghost { fn hello(&self) { } }"
        );
        assert_eq!(kind, HirErrorKind::UnknownInterface { name: "Ghost" });
    }

    #[test]
    fn hir_unknown_interface_in_superinterface() {
        let kind = hir_check!("interface Child: NonExistent { fn method(&self); }");
        assert_eq!(kind, HirErrorKind::UnknownInterface { name: "NonExistent" });
    }

    #[test]
    fn hir_missing_interface_method() {
        let kind = hir_check!(
            "interface Greet {
             fn hello(&self): i32;
             fn bye(&self): i32;
         }
         struct Foo { x: i32 }
         impl Foo with Greet { fn hello(&self): i32 { 1 } }"
        );
        assert_eq!(
            kind,
            HirErrorKind::MissingInterfaceMethod {
                struct_name: "Foo",
                interface_name: "Greet",
                method_name: "bye",
            }
        );
    }

    #[test]
    fn hir_missing_superinterface_impl() {
        let kind = hir_check!(
            "interface Base { fn base(&self): i32; }
         interface Derived: Base { fn derived(&self): i32; }
         struct Foo { x: i32 }
         impl Foo with Derived { fn derived(&self): i32 { 1 } }"
        );
        assert_eq!(
            kind,
            HirErrorKind::MissingSuperinterfaceImpl {
                struct_name: "Foo",
                interface_name: "Derived",
                superinterface_name: "Base",
            }
        );
    }

    #[test]
    fn hir_interface_signature_mismatch_return_type() {
        let kind = hir_check!(
            "interface Shape { fn area(&self): i64; }
         struct Rect { w: i32, h: i32 }
         impl Rect with Shape { fn area(&self): i32 { self.w * self.h } }"
        );
        assert!(matches!(kind, HirErrorKind::InterfaceSignatureMismatch { .. }), "got {kind:?}");
        if let HirErrorKind::InterfaceSignatureMismatch {
            struct_name,
            interface_name,
            method_name,
            ..
        } = kind
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
         impl Foo with Namer { fn name(&self, extra: i32): i32 { extra } }"
        );
        assert!(matches!(kind, HirErrorKind::InterfaceSignatureMismatch { .. }), "got {kind:?}");
        if let HirErrorKind::InterfaceSignatureMismatch { method_name, .. } = kind {
            assert_eq!(method_name, "name");
        }
    }

    #[test]
    fn hir_interface_signature_mismatch_wrong_receiver_mutability() {
        let kind = hir_check!(
            "interface Mutator { fn mutate(&mut self); }
         struct Foo { x: i32 }
         impl Foo with Mutator { fn mutate(&self) { } }"
        );
        assert!(matches!(kind, HirErrorKind::InterfaceSignatureMismatch { .. }), "got {kind:?}");
    }
}
