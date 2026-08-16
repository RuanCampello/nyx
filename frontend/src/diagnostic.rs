use crate::hir::error::HirError;
use crate::lexer::error::LexError;
use crate::lexer::token::{BytePos, Span};
use crate::parser::error::ParserError;
use crate::source_map::{FileId, SourceMap};
use crate::{error_codes, lints};
use ariadne::{Cache, Color, Config, Label as AriadneLabel, Report, ReportKind, Source};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

/// A diagnostic in structured, plain-text form, the same information the CLI
/// renders through `ariadne`, but consumable across the crate boundary
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RichDiagnostic {
    pub severity: Severity,
    pub code: Option<error_codes::ErrorCode>,
    pub lint: Option<lints::Lint>,
    pub message: String,
    pub primary: Option<Label>,
    pub secondary: Vec<Label>,
    pub note: Option<String>,
    pub help: Option<String>,
    pub rendered: Option<String>,
}

/// A single labelled span within a [`RichDiagnostic`], carrying plain (no ANSI)
/// text so consumers like the LSP can present it however they wish
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Debug)]
pub struct Diagnostic {
    pub(crate) rendered: String,
}

pub struct Builder {
    severity: Severity,
    code: Option<error_codes::ErrorCode>,
    lint: Option<lints::Lint>,
    message: String,
    labels: Vec<(Span, String, Color)>,
    note: Option<String>,
    help: Option<String>,
}

#[derive(Default)]
struct TypeNames {
    adts: Vec<String>,
    arrays: Vec<String>,
}

/// An [ariadne::Cache] over the per-thread [SourceMap], building one
/// [Source] per file on first use so a single report can span many files
struct MapCache {
    sources: HashMap<FileId, Source<String>>,
    names: HashMap<FileId, String>,
}

/// Severity of a [`RichDiagnostic`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
}

pub trait AsDiagnostic {
    fn into_diagnostic(self, span: Span) -> Diagnostic;
    fn rich(self, span: Span) -> RichDiagnostic;
    fn message(self) -> String;
}

/// The faulty thing: offending names, found types, primary labels
pub(crate) const ERROR: Color = Color::Rgb(243, 139, 168);
/// The expected side and contextual labels
pub(crate) const CONTEXT: Color = Color::Rgb(249, 226, 175);
/// Interface and bound names
pub(crate) const BOUND: Color = Color::Rgb(148, 226, 213);
/// Suggested code in notes and helps
pub(crate) const SUGGEST: Color = Color::Rgb(137, 180, 250);

thread_local! {
    static SOURCE_MAP: RefCell<SourceMap> = RefCell::new(SourceMap::default());
    static TYPE_NAMES: RefCell<TypeNames> = RefCell::new(TypeNames::default());
}

/// Clear the per-thread source map and type-name registry
/// Call once at the start of a compilation or analysis run before registering files
pub fn reset() {
    SOURCE_MAP.with_borrow_mut(|map| *map = SourceMap::default());
    TYPE_NAMES.with_borrow_mut(|names| *names = TypeNames::default());
}

#[derive(Clone, Copy)]
enum TypeNameKind {
    Adt,
    Array,
}

pub fn add_file(name: impl Into<std::path::PathBuf>, src: impl Into<String>) -> (FileId, BytePos) {
    SOURCE_MAP.with_borrow_mut(|map| map.add_file(name, src))
}

pub fn take_source_map() -> SourceMap {
    SOURCE_MAP.with_borrow_mut(std::mem::take)
}

pub(crate) fn register_adt_name(id: u32, name: &str) {
    TYPE_NAMES.with_borrow_mut(|names| names.register(TypeNameKind::Adt, id, name));
}

pub(crate) fn register_array_name(id: u32, rendered: &str) {
    TYPE_NAMES.with_borrow_mut(|names| names.register(TypeNameKind::Array, id, rendered));
}

pub(crate) fn write_adt_name(f: &mut fmt::Formatter<'_>, id: u32) -> fmt::Result {
    TYPE_NAMES.with_borrow(|names| names.write(f, TypeNameKind::Adt, id))
}

pub(crate) fn write_array_name(f: &mut fmt::Formatter<'_>, id: u32) -> fmt::Result {
    TYPE_NAMES.with_borrow(|names| names.write(f, TypeNameKind::Array, id))
}

impl RichDiagnostic {
    pub fn bare(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: None,
            lint: None,
            message: message.into(),
            primary: None,
            secondary: Vec::new(),
            note: None,
            help: None,
            rendered: None,
        }
    }
}

/// Render a batch of diagnostics into one displayable [Diagnostic]
pub fn render_batch(diagnostics: impl IntoIterator<Item = RichDiagnostic>) -> Diagnostic {
    let rendered = diagnostics
        .into_iter()
        .map(|mut d| match d.rendered.take() {
            Some(rendered) => rendered,
            None => d.into_diagnostic(Span::default()).rendered,
        })
        .collect::<Vec<_>>()
        .join("\n");
    Diagnostic { rendered }
}

impl AsDiagnostic for RichDiagnostic {
    fn into_diagnostic(self, _span: Span) -> Diagnostic {
        let mut builder = Builder::new(self.message).severity(self.severity);
        if let Some(code) = self.code {
            builder = builder.code(code);
        }
        if let Some(lint) = self.lint {
            builder = builder.lint(lint);
        }
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
    pub fn from_rendered(rendered: String) -> Self {
        Self { rendered }
    }

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

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered)
    }
}

impl Builder {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: None,
            lint: None,
            message: message.into(),
            labels: Vec::new(),
            note: None,
            help: None,
        }
    }

    pub fn severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    pub fn lint(mut self, lint: lints::Lint) -> Self {
        self.severity = lint.default_level().severity();
        self.lint = Some(lint);
        self
    }

    pub fn code(mut self, code: error_codes::ErrorCode) -> Self {
        self.code = Some(code);
        self
    }

    pub fn primary(mut self, span: Span, text: impl Into<String>) -> Self {
        self.labels.insert(0, (span, text.into(), ERROR));
        self
    }

    pub fn secondary(mut self, span: Span, text: impl Into<String>) -> Self {
        self.labels.push((span, text.into(), CONTEXT));
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

        // anchor at the earliest label in the primary file so every label in
        // that file renders inside one snippet rather than split groups
        let (anchor_file, primary_range) = map.local_range(self.labels[0].0);
        let anchor = self
            .labels
            .iter()
            .map(|(span, _, _)| map.local_range(*span))
            .filter(|(file, _)| *file == anchor_file)
            .map(|(_, range)| range.start)
            .min()
            .unwrap_or(primary_range.start);
        let cache = MapCache::new(map, self.labels.iter().map(|(s, _, _)| map.span_data(*s).file));

        let kind = match self.severity {
            Severity::Error => ReportKind::Error,
            Severity::Warning => ReportKind::Warning,
        };

        let mut builder = Report::build(kind, (anchor_file, anchor..anchor))
            .with_config(Config::default().with_compact(false))
            .with_message(&self.message);

        match (self.code, self.lint) {
            (Some(code), _) => builder = builder.with_code(code),
            (None, Some(lint)) => builder = builder.with_code(lint),
            (None, None) => {},
        }

        let mut labels: Vec<_> = self
            .labels
            .into_iter()
            .map(|(span, text, color)| {
                let (file, range) = map.local_range(span);
                (file, range, text, color)
            })
            .collect();
        labels.sort_by_key(|(file, range, _, _)| (*file != anchor_file, *file, range.start));

        for (order, (file, range, text, color)) in labels.into_iter().enumerate() {
            builder = builder.with_label(
                AriadneLabel::new((file, range))
                    .with_message(text)
                    .with_order(order as i32)
                    .with_color(color),
            );
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

impl TypeNames {
    fn register(&mut self, kind: TypeNameKind, id: u32, name: &str) {
        let slot = match kind {
            TypeNameKind::Adt => &mut self.adts,
            TypeNameKind::Array => &mut self.arrays,
        };
        let id = id as usize;
        if slot.len() <= id {
            slot.resize(id + 1, String::new());
        }
        slot[id] = name.to_string();
    }

    fn write(&self, f: &mut fmt::Formatter<'_>, kind: TypeNameKind, id: u32) -> fmt::Result {
        let (slot, fallback) = match kind {
            TypeNameKind::Adt => (&self.adts, "adt"),
            TypeNameKind::Array => (&self.arrays, "array"),
        };
        match slot.get(id as usize) {
            Some(name) if !name.is_empty() => f.write_str(name),
            _ => write!(f, "{fallback}#{id}"),
        }
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
