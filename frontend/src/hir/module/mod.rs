//! Multi-file module system with path resolution, cycle detection, and symbol merging.

mod graph;
mod resolver;

use crate::{
    diagnostic::{AsDiagnostic, Diagnostic, RichDiagnostic},
    hir::{
        self, Declarations, FunctionId, Hir, SymbolTable,
        collect::{ItemCollector, ItemTable},
        ids::IndexVec,
        mono, structs,
    },
    lexer::token::Span,
};
use graph::ModuleGraph;
use macros::Diagnostic;
use resolver::ModuleResolver;
use std::path::{Path, PathBuf};

/// Orchestrates module loading, path resolution, and HIR construction.
///
/// Maintains a cache of loaded modules and the shared symbol table
/// to ensure symbol IDs remain unique across the entire compilation.
pub struct ModuleLoader<'hir, F: FileSystem = FS> {
    resolver: ModuleResolver,
    /// shared symbols interner for all modules
    symbols: SymbolTable,
    fs: F,
    arena: &'hir bumpalo::Bump,
    /// whether open generic bodies are retained in the editor-facing HIR
    retain_templates: bool,
}

pub struct FS;

pub struct OverlayFS {
    pub overlay: std::collections::HashMap<PathBuf, String>,
}

#[derive(Debug, Diagnostic)]
#[rustfmt::skip]
pub enum ModuleError {
    #[diagnostic(
        code = "E040",
        message = "Cannot find the imported module",
        primary = "imported here",
        help = "Create {path.display()} or fix the import path"
    )]
    FileNotFound { path: PathBuf, span: Option<Span> },

    #[diagnostic(
        code = "E041",
        message = "Circular import",
        primary = "this import completes a cycle",
        note = "{path.display()^} is already being loaded",
        help = "Break the dependency cycle between the modules"
    )]
    CircularImport { path: PathBuf, span: Span },

    #[diagnostic(
        code = "E042",
        message = "Empty import path",
        primary = "no module named here",
        help = "Import as {`use project::module;`}"
    )]
    EmptyPath,

    #[diagnostic(
        code = "E043",
        message = "Unknown module root {name!}",
        primary = "not a known root",
        note = "The first path segment must be your project name or {`std`}"
    )]
    UnknownRoot { name: String, span: Span },

    #[diagnostic(
        code = "E044",
        message = "Symbol {name!} is not exported",
        primary = "not exported by this module",
        help = "Add {`pub`} to {`fn {name}`} to export it"
    )]
    UnknownExport {
        path: PathBuf,
        name: String,
        span: Span,
    },

    #[diagnostic(
        code = "E046",
        message = "Project contains no Nyx modules",
        primary = "no source modules were found",
        note = "No {`.nyx`} files were found directly inside {path.display()}",
        help = "Add a {`.nyx`} source file or pass one explicitly"
    )]
    NoModules { path: PathBuf },

}

impl ModuleError {
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::FileNotFound { span, .. } => *span,
            Self::CircularImport { span, .. }
            | Self::UnknownRoot { span, .. }
            | Self::UnknownExport { span, .. } => Some(*span),
            Self::EmptyPath | Self::NoModules { .. } => None,
        }
    }
}

pub trait FileSystem {
    fn read(&self, path: &Path) -> Result<String, std::io::Error>;
    fn canonicalise(&self, path: &Path) -> Result<PathBuf, std::io::Error>;

    /// every `.nyx` module sitting directly in `dir`, in a stable order
    fn modules_in(&self, dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };

        let mut modules: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "nyx"))
            .collect();
        modules.sort_unstable();

        modules
    }
}

impl<'hir> ModuleLoader<'hir, FS> {
    pub fn new(name: String, root: PathBuf, arena: &'hir bumpalo::Bump) -> Self {
        Self::with_file_system(name, root, resolve_std_root(), FS, arena)
    }
}

impl<'hir, F: FileSystem> ModuleLoader<'hir, F> {
    pub fn with_file_system(
        name: String,
        root: PathBuf,
        std: PathBuf,
        fs: F,
        arena: &'hir bumpalo::Bump,
    ) -> Self {
        let canonical_root = fs.canonicalise(&root).unwrap_or_else(|_| root.clone());
        let canonical_std = fs.canonicalise(&std).unwrap_or_else(|_| std.clone());
        Self {
            resolver: ModuleResolver::new(name, canonical_root, canonical_std),
            fs,
            symbols: SymbolTable::new(),
            arena,
            retain_templates: false,
        }
    }

    /// Analyse generic template bodies and retain navigation side tables for an editor.
    #[inline]
    pub fn editor(mut self) -> Self {
        self.retain_templates = true;
        self
    }

    pub fn load(
        self,
        entry: impl AsRef<Path>,
    ) -> Result<Hir<'hir>, (Vec<RichDiagnostic>, ModuleError)> {
        self.load_entries(vec![entry.as_ref().to_path_buf()])
    }

    pub fn load_directory(
        self,
        directory: impl AsRef<Path>,
    ) -> Result<Hir<'hir>, (Vec<RichDiagnostic>, ModuleError)> {
        let directory = directory.as_ref();
        let entries = self.fs.modules_in(directory);
        if entries.is_empty() {
            return Err((Vec::new(), ModuleError::NoModules { path: directory.into() }));
        }

        self.load_entries(entries)
    }

    fn load_entries(
        self,
        entries: Vec<PathBuf>,
    ) -> Result<Hir<'hir>, (Vec<RichDiagnostic>, ModuleError)> {
        crate::diagnostic::reset();

        let arena = self.arena;
        // parsing and graph construction do not touch the HIR, the lowering
        // scope is only introduced once the graph is in hand
        let mut graph =
            graph::build_graph(&entries, &self.resolver, &self.fs, arena, self.retain_templates)
                .map_err(|err| (Vec::new(), err))?;

        let mut collector = ItemCollector::new(arena);
        collector.symbols = self.symbols;

        collector.diagnostics.get_mut().extend(std::mem::take(&mut graph.diagnostics));

        let order = graph.all_nodes_order();
        let (declarations, item_diagnostics) = graph.collect_declarations();
        collector.diagnostics.get_mut().extend(item_diagnostics);

        for &idx in &order {
            collector.in_std.set(graph.nodes[idx].in_std);
            collector.extend_types(&declarations[idx]);
        }
        for &idx in &order {
            collector.in_std.set(graph.nodes[idx].in_std);
            collector.extend_items(&declarations[idx], arena);
        }

        let declaration_arrays = collector.arrays.snapshot();
        structs::compute_layouts(&mut collector.adts.defs, &declaration_arrays);

        let mut scope = collector.freeze();
        let functions =
            lower_all(&graph, &declarations, &order, &scope, arena, self.retain_templates);
        let templates = scope.lower_generic_templates(&functions, arena, self.retain_templates);
        let functions = mono::monomorphise(functions, &templates, &scope);
        let functions = with_editor_templates(functions, templates, self.retain_templates);
        let functions = hir::freeze_function_ids(functions);
        hir::const_check::check(&scope, &functions);

        let diagnostics = scope.diagnostics.get_mut().take_errors();

        Ok(scope.into_hir(functions, diagnostics))
    }

    #[cfg(test)]
    fn resolve_path(&self, segments: &[&str], span: Span) -> Result<PathBuf, ModuleError> {
        self.resolver.resolve_path(segments, span)
    }
}

impl FileSystem for FS {
    fn read(&self, path: &Path) -> Result<String, std::io::Error> {
        std::fs::read_to_string(path)
    }

    fn canonicalise(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
        path.canonicalize()
    }
}

impl FileSystem for OverlayFS {
    fn read(&self, path: &Path) -> Result<String, std::io::Error> {
        if let Some(content) = self.overlay.get(path) {
            return Ok(content.clone());
        }
        std::fs::read_to_string(path)
    }

    fn canonicalise(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
        path.canonicalize()
    }
}

impl From<ModuleError> for Diagnostic {
    fn from(value: ModuleError) -> Diagnostic {
        let span = match &value {
            ModuleError::FileNotFound { span, .. } => span.unwrap_or_default(),
            ModuleError::CircularImport { span, .. }
            | ModuleError::UnknownRoot { span, .. }
            | ModuleError::UnknownExport { span, .. } => *span,
            ModuleError::EmptyPath | ModuleError::NoModules { .. } => Span::default(),
        };
        AsDiagnostic::into_diagnostic(value, span)
    }
}

/// order:
/// 1. `NYX_STD_PATH` environment variable
/// 2. `<binary_dir>/std/`
/// 3. `std/` relative to CWD
pub fn resolve_std_root() -> PathBuf {
    if let Ok(env) = std::env::var("NYX_STD_PATH") {
        return PathBuf::from(env);
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("std");
        if candidate.is_dir() {
            return candidate;
        }
    }

    let candidate = PathBuf::from("std");
    if candidate.is_dir() {
        return candidate;
    }

    // development fallback: the std shipped in this checkout, regardless of
    // which workspace crate the process was started from
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../std"))
}

/// In editor mode, interleaves each retained generic template ahead of the
/// instances monomorphisation produced from it, so navigation can still find
/// the generic definition itself; otherwise `templates` is discarded.
fn with_editor_templates<'hir>(
    functions: IndexVec<FunctionId, hir::Function<'hir>>,
    templates: std::collections::HashMap<FunctionId, hir::Function<'hir>>,
    retain: bool,
) -> IndexVec<FunctionId, hir::Function<'hir>> {
    if !retain {
        return functions;
    }

    let mut indexed = IndexVec::new();
    for function in templates.into_values() {
        indexed.push(function);
    }
    for function in functions {
        indexed.push(function);
    }
    indexed
}

/// Resolves the path to the compiler's built-in `std/` directory
fn lower_all<'hir, 'src>(
    graph: &ModuleGraph<'src>,
    declarations: &[Declarations<'_, 'src>],
    order: &[usize],
    scope: &ItemTable<'hir>,
    arena: &'hir bumpalo::Bump,
    retain_intrinsics: bool,
) -> IndexVec<FunctionId, hir::Function<'hir>>
where
    'src: 'hir,
{
    let mut functions = IndexVec::new();

    for &idx in order {
        scope.in_std.set(graph.nodes[idx].in_std);

        for function in
            scope.lower_matching_functions(&declarations[idx], |_| true, retain_intrinsics, arena)
        {
            functions.push(function);
        }
    }

    functions
}

#[cfg(test)]
mod tests;
