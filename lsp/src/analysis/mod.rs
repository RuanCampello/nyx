mod completion;
mod eval;
mod hover;
mod walker;

#[cfg(test)]
mod tests;

pub use completion::{Completion, CompletionKind, Completions, GenericSlots};
pub use hover::{HoverInfo, HoverTarget};
pub use walker::Binding;

use crate::feature;
use completion::{completions, importable_items};
use frontend::hir::module;
use frontend::hir::{
    self, AdtId, ArrayId, FunctionId, StaticId, SymbolTable, ids::IndexVec, visit::Visitor,
};
use frontend::{
    diagnostic::AsDiagnostic,
    lexer::token::Span,
    source_map::{FileId, SourceMap},
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walker::Walker;

pub struct Analysis {
    entry: PathBuf,
    overlays: HashMap<PathBuf, String>,
}

/// Hover and go-to-definition data extracted from a HIR pass
pub struct SemanticAnalysis {
    pub diagnostics: Vec<CheckError>,
    /// resolves the global spans below to concrete files and line/column
    pub source_map: SourceMap,
    /// whether the project analysed into a hir at all: syntax and type errors are
    /// recovered from and leave this set, with the feature data below still valid
    /// for whatever resolved
    pub ok: bool,
    arena: std::sync::Mutex<Option<HirArena>>,
}

self_cell::self_cell!(
    struct HirArena {
        owner: bumpalo::Bump,
        #[covariant]
        dependent: Snapshot,
    }
);

/// Everything derived from one successfully-lowered HIR
#[derive(Default)]
struct Snapshot<'hir> {
    symbols: SymbolTable,
    adts: SparseIndex<AdtId, hir::AdtDef<'hir>>,
    arrays: IndexVec<ArrayId, hir::ArrayType<'hir>>,
    interfaces: Vec<hir::InterfaceSignature<'hir>>,
    functions: IndexVec<FunctionId, hir::Function<'hir>>,
    constants: Vec<hir::Constant<'hir>>,
    statics: IndexVec<StaticId, hir::Static<'hir>>,
    /// rendered `///` documentation, keyed by the span of the name it sits above
    docs: HashMap<Span, Box<str>>,
    /// the `use`-path form of each file, for the container line above a hover
    modules: HashMap<FileId, String>,
    /// `(span, what it names)` sorted by `span.start.offset()` for binary search
    /// from a cursor position
    hover_types: Vec<(Span, HoverTarget<'hir>)>,
    /// identifier-use span to definition-site span
    goto_definitions: HashMap<Span, Span>,
    /// `(name_span, type, owning function)` for every binding, the hint appears
    /// immediately after the binding name
    inlay_hints: Vec<(Span, hir::Type<'hir>, u32)>,
    document_symbols: Vec<DocumentSymbol>,
    /// Everything the completion provider can offer, precomputed while the HIR
    /// is still alive
    completions: Completions<'hir>,
    /// `(body span, locals declared in it)` for every function, so a position
    /// inside a body can offer the names that body has in scope
    scopes: Vec<(Span, Vec<Completion<'hir>>)>,
}

/// A name-indexed lookup that need not cover every value of `I`
#[derive(Debug)]
pub(super) struct SparseIndex<I, T> {
    values: HashMap<I, T>,
}

/// A top-level declared symbol for the document outline
#[derive(Debug)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    /// the whole declaration, which is what an outline highlights
    pub span: Span,
    /// the declared name alone, which is where selecting the symbol lands
    pub name_span: Span,
}

#[derive(Debug, Clone, Copy)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Constant,
}

/// A single compile-time error in structured form so consumers can render it as richly as the CLI
type CheckError = frontend::diagnostic::RichDiagnostic;

impl Analysis {
    /// Create a new analysis builder starting at the given entry path.
    pub fn new(entry: impl Into<PathBuf>) -> Self {
        Self { entry: entry.into(), overlays: HashMap::new() }
    }

    /// Add a single in-memory file overlay (e.g. unsaved editor buffer).
    #[cfg(test)]
    pub fn with_overlay(mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        self.overlays.insert(path.into(), content.into());
        self
    }

    /// Set multiple in-memory file overlays at once.
    pub fn with_overlays(mut self, overlays: HashMap<PathBuf, String>) -> Self {
        self.overlays.extend(overlays);
        self
    }

    /// Execute the semantic analysis and return the results
    pub fn run(self) -> SemanticAnalysis {
        let root = match self.entry.parent().unwrap_or(Path::new(".")).canonicalize() {
            Ok(r) => r,
            Err(e) => {
                return SemanticAnalysis {
                    diagnostics: vec![CheckError::bare(e.to_string())],
                    ..Default::default()
                };
            },
        };

        let name = root.file_name().and_then(|n| n.to_str()).unwrap_or("project").to_string();
        let std_root = module::resolve_std_root();
        let std_root = std_root.canonicalize().unwrap_or(std_root);
        let entry = self.entry.clone();

        let mut source_map = SourceMap::default();
        let mut result_diagnostics = Vec::new();
        let mut ok = false;

        let cell = HirArena::try_new(bumpalo::Bump::new(), |arena| {
            let loader = module::ModuleLoader::with_file_system(
                name.clone(),
                root.clone(),
                std_root.clone(),
                module::OverlayFS { overlay: self.overlays },
                arena,
            )
            .editor();

            let result = loader.load(&entry);
            source_map = frontend::diagnostic::take_source_map();

            match result {
                // recovery keeps a (partial) HIR even with errors: surface every
                // recovered diagnostic while still serving features for what resolved
                Ok(mut hir) => {
                    let modules = module_paths(&source_map, &name, &root, &std_root);
                    result_diagnostics = std::mem::take(&mut hir.diagnostics);
                    ok = true;
                    Ok(build_snapshot(hir, &source_map, modules, arena))
                },
                Err((diagnostics, e)) => {
                    result_diagnostics = diagnostics;
                    Err(e)
                },
            }
        });

        match cell {
            Ok(cell) => SemanticAnalysis {
                diagnostics: result_diagnostics,
                source_map,
                ok,
                arena: std::sync::Mutex::new(Some(cell)),
            },
            Err(e) => {
                let span = e.span().unwrap_or_default();
                result_diagnostics.push(e.rich(span));
                SemanticAnalysis {
                    diagnostics: result_diagnostics,
                    source_map,
                    ok: false,
                    arena: std::sync::Mutex::new(None),
                }
            },
        }
    }
}

impl SemanticAnalysis {
    #[cfg(test)]
    pub(crate) fn synthetic(ok: bool, inlay_hints: usize) -> Self {
        let cell = HirArena::new(bumpalo::Bump::new(), |_| Snapshot {
            inlay_hints: (0..inlay_hints)
                .map(|_| (Span::default(), hir::Type::default(), 0))
                .collect(),
            ..Default::default()
        });

        Self {
            diagnostics: Vec::new(),
            source_map: SourceMap::default(),
            ok,
            arena: std::sync::Mutex::new(Some(cell)),
        }
    }

    #[cfg(test)]
    pub(crate) fn inlay_hint_count(&self) -> usize {
        self.with_snapshot(|snapshot| snapshot.map_or(0, |s| s.inlay_hints.len()))
    }

    fn with_snapshot<R>(&self, visit: impl FnOnce(Option<&Snapshot<'_>>) -> R) -> R {
        let guard = self.arena.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        visit(guard.as_ref().map(HirArena::borrow_dependent))
    }

    pub fn hover_at(
        &self,
        map: &SourceMap,
        file: FileId,
        pos: frontend::BytePos,
    ) -> Option<(Span, HoverInfo)> {
        self.with_snapshot(|snapshot| {
            let snapshot = snapshot?;
            let (span, target) = *snapshot
                .hover_types
                .iter()
                .filter(|(span, _)| {
                    map.span_data(*span).file == file && span.start <= pos && pos < span.end
                })
                .min_by_key(|(span, _)| span.end.0 - span.start.0)?;

            Some((span, snapshot.hover(target, map)?))
        })
    }

    /// the definition span of whichever recorded use covers `pos` in `file`
    pub fn goto_definition_at(
        &self,
        map: &SourceMap,
        file: FileId,
        pos: frontend::BytePos,
    ) -> Option<Span> {
        self.with_snapshot(|snapshot| {
            snapshot?
                .goto_definitions
                .iter()
                .filter(|(use_span, _)| {
                    map.span_data(**use_span).file == file
                        && use_span.start <= pos
                        && pos < use_span.end
                })
                .min_by_key(|(use_span, _)| use_span.end.0 - use_span.start.0)
                .map(|(_, def)| *def)
        })
    }

    /// every recorded inlay hint in `file`, pre-rendered as `(span, label)`
    pub fn inlay_hints_in(&self, map: &SourceMap, file: FileId) -> Vec<(Span, String)> {
        self.with_snapshot(|snapshot| {
            let Some(snapshot) = snapshot else {
                return Vec::new();
            };

            snapshot
                .inlay_hints
                .iter()
                .filter(|(span, ..)| map.span_data(*span).file == file)
                .map(|&(span, typ, function)| (span, snapshot.hint(typ, function)))
                .collect()
        })
    }

    pub fn document_symbols_in(
        &self,
        map: &SourceMap,
        file: FileId,
    ) -> Vec<(String, SymbolKind, Span, Span)> {
        self.with_snapshot(|snapshot| {
            snapshot
                .map(|s| s.document_symbols.as_slice())
                .unwrap_or(&[])
                .iter()
                .filter(|symbol| map.span_data(symbol.span).file == file)
                .map(|symbol| (symbol.name.clone(), symbol.kind, symbol.span, symbol.name_span))
                .collect()
        })
    }

    pub fn completion_candidates<T>(
        &self,
        context: &feature::completion::Context<'_>,
        position: Option<frontend::BytePos>,
        render: impl FnOnce(&feature::completion::Candidates<'_>) -> Vec<T>,
    ) -> Vec<T> {
        self.with_snapshot(|snapshot| {
            let Some(snapshot) = snapshot else {
                return Vec::new();
            };

            let scope = position.and_then(|position| {
                snapshot
                    .scopes
                    .iter()
                    .filter(|(body, _)| body.start <= position && position < body.end)
                    .min_by_key(|(body, _)| body.end.0 - body.start.0)
                    .map(|(_, locals)| locals.as_slice())
            });

            render(&feature::completion::candidates(&snapshot.completions, context, scope))
        })
    }
}

impl Default for SemanticAnalysis {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            source_map: SourceMap::default(),
            ok: false,
            arena: std::sync::Mutex::new(None),
        }
    }
}

impl<I, T> Default for SparseIndex<I, T> {
    fn default() -> Self {
        Self { values: HashMap::new() }
    }
}

impl<I: Copy + Eq + std::hash::Hash, T> SparseIndex<I, T> {
    fn insert(&mut self, id: I, value: T) {
        assert!(self.values.insert(id, value).is_none(), "duplicate sparse index");
    }

    fn get(&self, id: I) -> Option<&T> {
        self.values.get(&id)
    }

    fn iter(&self) -> impl Iterator<Item = &T> {
        self.values.values()
    }

    fn iter_enumerated(&self) -> impl Iterator<Item = (I, &T)> {
        self.values.iter().map(|(&id, value)| (id, value))
    }
}

impl<I: Eq + std::hash::Hash, T> std::ops::Index<I> for SparseIndex<I, T> {
    type Output = T;

    fn index(&self, id: I) -> &Self::Output {
        &self.values[&id]
    }
}

impl<'a, I, T> IntoIterator for &'a SparseIndex<I, T> {
    type Item = &'a T;
    type IntoIter = std::collections::hash_map::Values<'a, I, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.values()
    }
}

fn build_snapshot<'hir>(
    hir: hir::Hir<'hir>,
    map: &SourceMap,
    modules: HashMap<FileId, String>,
    arena: &'hir bumpalo::Bump,
) -> Snapshot<'hir> {
    use SymbolKind::*;

    let hir::Hir {
        symbols,
        adts,
        arrays,
        functions,
        constants,
        statics,
        interfaces,
        docs,
        imports,
        type_refs,
        ..
    } = hir;

    let mut adt_index = SparseIndex::default();
    for (position, definition) in adts.into_iter().enumerate() {
        adt_index.insert(AdtId(position as u32), definition);
    }

    let snapshot = Snapshot {
        symbols,
        adts: adt_index,
        arrays: arrays.snapshot(),
        interfaces,
        statics,
        docs,
        modules,
        functions,
        constants,
        ..Default::default()
    };

    let mut hover_types = Vec::new();
    let mut goto_definitions = HashMap::new();
    let mut inlay_hints = Vec::new();

    macro_rules! push_hover {
        ($span:expr, $target:expr) => {
            if $span != Span::default() {
                hover_types.push(($span, $target));
            }
        };
    }

    let by_name: HashMap<_, _> = snapshot
        .constants
        .iter()
        .enumerate()
        .map(|(at, constant)| (constant.name, (at as u32, constant)))
        .collect();

    let templates = hover::template_names(&snapshot);
    let open = |func: &hir::Function<'_>| !hover::is_shadowed_instance(func, &snapshot, &templates);

    for (at, func) in snapshot.functions.iter().enumerate().filter(|(_, f)| open(f)) {
        let at = at as u32;
        push_hover!(func.name_span, HoverTarget::Function(at));

        let mut forms = HashMap::new();
        for param in &func.params {
            let local = &func.locals[param.id];
            forms.insert(param.id, Binding::Pattern);

            let target =
                HoverTarget::Local { function: at, local: param.id, form: Binding::Pattern };
            push_hover!(local.decl_span, target);
        }

        let mut walker = Walker {
            typeck: &func.typeck,
            locals: &func.locals,
            index: &snapshot,
            map,
            function: at,
            functions: &snapshot.functions,
            constants: &by_name,
            hover: &mut hover_types,
            defs: &mut goto_definitions,
            hints: &mut inlay_hints,
            forms,
        };

        for statement in func.body.statements {
            walker.visit_statement(statement);
        }
    }

    for (id, def) in snapshot.adts.iter_enumerated() {
        match def.is_struct() {
            true => {
                push_hover!(def.name_span, HoverTarget::Struct(id));

                for (at, field) in def.fields().iter().enumerate() {
                    let target = HoverTarget::Field { structure: id, field: at as u32 };
                    push_hover!(field.name_span, target);
                }
            },
            _ => {
                push_hover!(def.name_span, HoverTarget::Enum(id));
                for (at, variant) in def.variants().iter().enumerate() {
                    let target = HoverTarget::Variant { enumeration: id, variant: at as u32 };
                    push_hover!(variant.name_span, target);
                }
            },
        }
    }

    for (at, constant) in snapshot.constants.iter().enumerate() {
        push_hover!(constant.name_span, HoverTarget::Constant(at as u32));
    }

    for (at, interface) in snapshot.interfaces.iter().enumerate() {
        push_hover!(interface.name_span, HoverTarget::Interface(at as u32));
        for (method, signature) in interface.methods.iter().enumerate() {
            let target =
                HoverTarget::InterfaceMethod { interface: at as u32, method: method as u32 };
            push_hover!(signature.name_span, target);
        }

        for (constant, signature) in interface.constants.iter().enumerate() {
            let target =
                HoverTarget::InterfaceConstant { interface: at as u32, constant: constant as u32 };
            push_hover!(signature.name_span, target);
        }
    }

    for (span, typ) in &type_refs {
        let target = hover::nominal_name_span(*typ, &snapshot);
        push_hover!(*span, HoverTarget::Nominal { typ: *typ, function: None });

        if let Some(target) = target
            && target != Span::default()
        {
            goto_definitions.insert(*span, target);
        }
    }

    let imported_names: HashSet<_> =
        imports.iter().map(|(_, name)| snapshot.symbols.get(*name).to_owned()).collect();
    let importable = importable_items(&snapshot);
    for (span, name) in &imports {
        let Some(item) = importable.get(snapshot.symbols.get(*name)).cloned() else {
            continue;
        };

        let name_span = item.name_span(&snapshot);
        push_hover!(*span, item.hover_target(&snapshot));
        if name_span != Span::default() {
            goto_definitions.insert(*span, name_span);
        }
    }

    hover_types.sort_unstable_by_key(|(span, _)| span.start.offset());

    let scopes = snapshot
        .functions
        .iter()
        .filter(|func| func.decl_span != Span::default() && open(func))
        .map(|func| (func.decl_span, completion::scope_of(func, &snapshot, arena)))
        .collect();

    let mut symbols = Vec::new();
    macro_rules! collect {
        ($iter:expr, $kind:expr) => {
            symbols.extend($iter.filter_map(|item| {
                (item.decl_span != Span::default()).then(|| DocumentSymbol {
                    name: short_name(&snapshot.symbols.get(item.name)),
                    kind: $kind,
                    span: item.decl_span,
                    name_span: match item.name_span == Span::default() {
                        true => item.decl_span,
                        _ => item.name_span,
                    },
                })
            }));
        };
    }

    collect!(snapshot.functions.iter().filter(|f| open(f)), Function);
    collect!(snapshot.adts.iter().filter(|d| d.is_struct()), Struct);
    collect!(snapshot.adts.iter().filter(|d| d.is_enum()), Enum);
    collect!(snapshot.constants.iter(), Constant);

    symbols.sort_unstable_by_key(|s| s.span.start.offset());
    let candidates = completions(&snapshot, map, &imported_names, arena);

    Snapshot {
        hover_types,
        goto_definitions,
        inlay_hints,
        document_symbols: symbols,
        completions: candidates,
        scopes,
        ..snapshot
    }
}

#[inline]
pub(super) fn short_name(qualified: &str) -> String {
    pretty_args(qualified.rsplit("::").next().unwrap_or(qualified))
}

#[inline]
pub(super) fn base_name(qualified: &str) -> String {
    let tail = qualified.rsplit("::").next().unwrap_or(qualified);
    tail.split('$').next().unwrap_or(tail).to_owned()
}

#[inline]
fn pretty_args(name: &str) -> String {
    match name.split_once('$') {
        Some((base, args)) => format!("{base}<{}>", args.replace('$', ", ")),
        None => name.to_owned(),
    }
}

fn module_paths(
    map: &SourceMap,
    project: &str,
    root: &Path,
    std_root: &Path,
) -> HashMap<FileId, String> {
    map.files()
        .map(|file| {
            let module = match file.name.strip_prefix(std_root) {
                Ok(relative) => module_path("std", relative),
                _ => match file.name.strip_prefix(root) {
                    Ok(relative) => module_path(project, relative),
                    _ => file
                        .name
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| project.to_owned()),
                },
            };

            (file.id, module)
        })
        .collect()
}

fn module_path(root: &str, relative: &Path) -> String {
    let mut segments = vec![root.to_owned()];
    let relative = relative.with_extension("");
    segments.extend(relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()));

    // the entry file is its directory's module
    if segments.len() > 1 && segments.last().is_some_and(|segment| segment == "main") {
        segments.pop();
    }

    segments.join("::")
}
