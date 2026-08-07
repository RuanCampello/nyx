//! Multi-file module system with path resolution, cycle detection, and symbol merging.

mod graph;
mod resolver;

use crate::{
    diagnostic::{AsDiagnostic, Diagnostic, RichDiagnostic},
    hir::{
        self, Declarations, FunctionId, Hir, SymbolTable, error::HirError, index_vec::IndexVec,
        mono, scope::Scope, structs,
    },
    lexer::token::Span,
    parser::error::ParserError,
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
    /// whether lowering collects diagnostics and recovers instead of failing fast
    recover: bool,
    /// whether generic template bodies are analysed for editor features
    analyse_templates: bool,
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
        code = "E045",
        message = "Statements are not allowed at the top level",
        primary = "this statement is outside any function",
        help = "Move it into a function body, or wrap it in {`fn main() {{ … }}`}"
    )]
    TopLevelNonFunction { path: PathBuf, span: Span },

    #[diagnostic(
        code = "E046",
        message = "Project contains no Nyx modules",
        primary = "no source modules were found",
        note = "No {`.nyx`} files were found directly inside {path.display()}",
        help = "Add a {`.nyx`} source file or pass one explicitly"
    )]
    NoModules { path: PathBuf },

    // boxed so the `Err` variant stays small: every loader function returns
    // `Result<_, ModuleError>` and pays for the variant size on the happy path
    #[diagnostic(transparent)]
    Check(Box<RichDiagnostic>),
}

impl ModuleError {
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::FileNotFound { span, .. } => *span,
            Self::CircularImport { span, .. }
            | Self::UnknownRoot { span, .. }
            | Self::UnknownExport { span, .. }
            | Self::TopLevelNonFunction { span, .. } => Some(*span),
            Self::EmptyPath | Self::NoModules { .. } | Self::Check(_) => None,
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
            recover: false,
            analyse_templates: false,
        }
    }

    /// full editor mode: recover from errors and analyse generic template bodies
    #[inline]
    pub fn recovering(mut self) -> Self {
        self.recover = true;
        self.analyse_templates = true;
        self
    }

    /// batch mode: recover so every error is collected, but skip the
    /// editor-only template analysis whose instances must not reach codegen
    #[inline]
    pub fn collecting(mut self) -> Self {
        self.recover = true;
        self
    }

    #[inline]
    const fn stops_after_syntax(&self) -> bool {
        self.recover && !self.analyse_templates
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
        let stops_after_syntax = self.stops_after_syntax();

        // parsing and graph construction do not touch the HIR, the lowering
        // scope is only introduced once the graph is in hand
        let mut graph = graph::build_graph(
            &entries,
            &self.resolver,
            &self.fs,
            arena,
            self.recover,
            self.analyse_templates,
        )
        .map_err(|err| (Vec::new(), err))?;

        let mut scope = Scope::new(arena);
        scope.recover = self.recover;
        scope.index_refs = self.analyse_templates;
        scope.symbols = self.symbols;

        for diagnostic in std::mem::take(&mut graph.diagnostics) {
            scope.diagnostics.emit(diagnostic);
        }

        let order = graph.all_nodes_order();
        let (declarations, item_diagnostics) = graph
            .collect_declarations(self.recover)
            .map_err(|err| (scope.diagnostics.take_errors(), err))?;

        for diagnostic in item_diagnostics {
            scope.diagnostics.emit(diagnostic);
        }

        if stops_after_syntax && scope.diagnostics.has_errors() {
            return Ok(Hir::broken(scope.diagnostics.take_errors(), scope.symbols));
        }

        for &idx in &order {
            scope.in_std = graph.nodes[idx].in_std;
            if let Err(err) = scope.extend_types(&declarations[idx]) {
                scope.soft(err).map_err(|err| (scope.diagnostics.take_errors(), err.into()))?;
            }
        }
        for &idx in &order {
            scope.in_std = graph.nodes[idx].in_std;
            if let Err(err) = scope.extend_items(&declarations[idx], arena) {
                scope.soft(err).map_err(|err| (scope.diagnostics.take_errors(), err.into()))?;
            }
        }

        let functions = lower_all(&graph, &declarations, &order, &mut scope, arena)
            .map_err(|err| (scope.diagnostics.take_errors(), err))?;
        let mut functions = mono::monomorphise(functions, &mut scope, arena)
            .map_err(|err| (scope.diagnostics.take_errors(), err.into()))?;

        let diagnostics = scope.diagnostics.take_errors();

        // editors need features inside generic template bodies too, lower one
        // identity instance of each and discard the diagnostics, which are
        // noise (bounds cannot be solved without concrete types)
        if self.analyse_templates {
            for function in mono::analyse_templates(&mut scope, arena) {
                functions.push(function);
            }
            scope.diagnostics.take_errors();
        }

        let arrays = scope.arrays.snapshot();
        structs::compute_layouts(&mut scope.structs, &mut scope.enums, &arrays);

        let statics = scope.statics_ordered();

        Ok(Hir {
            functions,
            structs: scope.structs,
            enums: scope.enums,
            arrays,
            statics,
            constants: scope.constants.into_values().cloned().collect(),
            interfaces: scope.interfaces.into_values().collect(),
            docs: scope.docs,
            imports: scope.imports,
            type_refs: scope.type_refs,
            symbols: scope.symbols,
            diagnostics,
        })
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

impl<'h> From<HirError<'h>> for ModuleError {
    fn from(e: HirError<'h>) -> Self {
        Self::Check(Box::new(e.kind.rich(e.span)))
    }
}

impl<'i> From<ParserError<'i>> for ModuleError {
    fn from(e: ParserError<'i>) -> Self {
        Self::Check(Box::new(e.kind.rich(e.span)))
    }
}

impl From<ModuleError> for Diagnostic {
    fn from(value: ModuleError) -> Diagnostic {
        match value {
            ModuleError::Check(rich) => rich.into_diagnostic(Span::default()),
            other => {
                let span = match &other {
                    ModuleError::FileNotFound { span, .. } => span.unwrap_or_default(),
                    ModuleError::CircularImport { span, .. }
                    | ModuleError::UnknownRoot { span, .. }
                    | ModuleError::UnknownExport { span, .. }
                    | ModuleError::TopLevelNonFunction { span, .. } => *span,
                    ModuleError::EmptyPath | ModuleError::NoModules { .. } => Span::default(),
                    ModuleError::Check(_) => unsafe { std::hint::unreachable_unchecked() },
                };
                AsDiagnostic::into_diagnostic(other, span)
            },
        }
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

/// Resolves the path to the compiler's built-in `std/` directory
fn lower_all<'hir, 'src>(
    graph: &ModuleGraph<'src>,
    declarations: &[Declarations<'_, 'src>],
    order: &[usize],
    scope: &mut Scope<'hir>,
    arena: &'hir bumpalo::Bump,
) -> Result<IndexVec<FunctionId, hir::Function<'hir>>, ModuleError>
where
    'src: 'hir,
{
    let mut functions = IndexVec::new();

    for &idx in order {
        scope.in_std = graph.nodes[idx].in_std;

        for function in scope.lower_matching_functions(&declarations[idx], |_| true, arena)? {
            functions.push(function);
        }
    }

    Ok(functions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{ExpressionKind, Function, Hir, Statement, Type, TypeKind};
    use std::collections::HashMap;
    use std::io;

    #[derive(Default)]
    struct VirtualFS {
        files: HashMap<PathBuf, String>,
    }

    impl FileSystem for VirtualFS {
        fn read(&self, path: &Path) -> Result<String, io::Error> {
            if let Some(content) = self.files.get(path) {
                return Ok(content.clone());
            }

            if path.starts_with(STD) {
                let filename = path.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, path.display().to_string())
                })?;
                let real_path = resolve_std_root().join(filename);
                if let Ok(content) = std::fs::read_to_string(&real_path) {
                    return Ok(content);
                }
            }

            Err(io::Error::new(io::ErrorKind::NotFound, path.display().to_string()))
        }

        fn canonicalise(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
            if self.files.contains_key(path) {
                return Ok(path.to_path_buf());
            }

            if path.starts_with(STD) {
                let filename = path.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, path.display().to_string())
                })?;
                let real_path = resolve_std_root().join(filename);
                if real_path.exists() {
                    return Ok(path.to_path_buf());
                }
            }

            Err(io::Error::new(io::ErrorKind::NotFound, path.display().to_string()))
        }

        fn modules_in(&self, dir: &Path) -> Vec<PathBuf> {
            let mut modules: Vec<_> = self
                .files
                .keys()
                .filter(|path| {
                    path.parent() == Some(dir)
                        && path.extension().is_some_and(|extension| extension == "nyx")
                })
                .cloned()
                .collect();
            modules.sort_unstable();
            modules
        }
    }

    impl VirtualFS {
        fn add(mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
            self.files.insert(path.into(), content.into());
            self
        }
    }

    fn vloader<'hir>(fs: VirtualFS, arena: &'hir bumpalo::Bump) -> ModuleLoader<'hir, VirtualFS> {
        ModuleLoader::with_file_system(APP.into(), PROJECT.into(), STD.into(), fs, arena)
    }

    const APP: &str = "my_app";
    const PROJECT: &str = "/project";
    const STD: &str = "/std";

    fn local_typ<'a>(main: &'a Function<'_>, hir: &Hir<'_>, name: &str) -> Type {
        main.locals
            .iter()
            .find(|l| hir.symbols.get(l.name) == name)
            .unwrap_or_else(|| panic!("missing local {name}"))
            .typ
    }

    #[test]
    fn resolve_simple_path() {
        let arena = bumpalo::Bump::new();
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT), &arena);
        let path = loader.resolve_path(&[APP, "math"], Span::default()).unwrap();

        assert_eq!(path, PathBuf::from("/project/math.nyx"));
    }

    #[test]
    fn directory_loads_every_module_without_main() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add("/project/one.nyx", "pub fn one(): i32 { 1 }")
            .add("/project/two.nyx", "pub fn two(): i32 { 2 }");
        let hir = vloader(fs, &arena).load_directory(PROJECT).unwrap();
        let names: Vec<_> =
            hir.functions.iter().map(|function| hir.symbols.get(function.name)).collect();

        assert!(names.iter().any(|name| name.ends_with("::one")));
        assert!(names.iter().any(|name| name.ends_with("::two")));
    }

    #[test]
    fn empty_directory_reports_no_modules() {
        let arena = bumpalo::Bump::new();
        let (_, error) = vloader(VirtualFS::default(), &arena).load_directory(PROJECT).unwrap_err();

        assert!(matches!(error, ModuleError::NoModules { .. }));
    }

    #[test]
    fn resolve_nested_path() {
        let arena = bumpalo::Bump::new();
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT), &arena);
        let path = loader.resolve_path(&[APP, "utils", "io", "file"], Span::default()).unwrap();

        assert_eq!(path, PathBuf::from("/project/utils/io/file.nyx"));
    }

    #[test]
    fn reject_unknown_root() {
        let arena = bumpalo::Bump::new();
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT), &arena);
        let err = loader.resolve_path(&["other", "foo"], Span::default()).unwrap_err();

        match err {
            ModuleError::UnknownRoot { name, .. } => assert_eq!(name, "other"),
            _ => panic!("expected unknownroot error"),
        }
    }

    #[test]
    fn reject_empty_path() {
        let arena = bumpalo::Bump::new();
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT), &arena);
        let err = loader.resolve_path(&[], Span::default()).unwrap_err();

        assert!(matches!(err, ModuleError::EmptyPath));
    }

    #[test]
    fn reject_root_only() {
        let arena = bumpalo::Bump::new();
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT), &arena);
        let err = loader.resolve_path(&[APP], Span::default()).unwrap_err();

        assert!(matches!(err, ModuleError::EmptyPath));
    }

    #[test]
    fn len_result_and_literal_defaults_infer() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
            fn main() {
                let arr = [0; 3];
                let l = arr.len();
                let x = 232;
                let y: bool = true;
            }
            "#,
        );
        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        let main = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

        assert_eq!(local_typ(main, &hir, "l"), TypeKind::Uptr.into(), "len() result");
        assert_eq!(local_typ(main, &hir, "x"), TypeKind::I32.into(), "bare literal defaults i32");
        assert_eq!(local_typ(main, &hir, "y"), TypeKind::Bool.into());
    }

    #[test]
    fn direct_qualified_calls_load_std_and_project_modules() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add("/project/math.nyx", "pub fn add(left: i32, right: i32): i32 { left + right }")
            .add(
                "/project/main.nyx",
                r#"
                    fn main(): i32 {
                        std::io::println("ok");
                        my_app::math::add(40, 2)
                    }
                "#,
            );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        let main = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

        assert!(matches!(main.body.statements[0], Statement::Expr(_)));
        assert_eq!(main.return_type, TypeKind::I32.into());
    }

    #[test]
    fn counters_infer_uptr_from_len_and_indexing() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
            fn sort(s: &mut [i32]) {
                let mut i = 0;
                let mut j = 0;
                loop {
                    if !(j + i < s.len()) { break; }
                    s[j] = 0;
                    j = j + 1;
                }
            }
            fn main() {
                let mut data: [i32; 4] = [0; 4];
                sort(&mut data);
            }
            "#,
        );
        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        let sort = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::sort").unwrap();

        assert_eq!(local_typ(sort, &hir, "i"), TypeKind::Uptr.into());
        assert_eq!(local_typ(sort, &hir, "j"), TypeKind::Uptr.into());
    }

    fn declares(hir: &Hir<'_>, names: &[&str]) -> bool {
        names
            .iter()
            .all(|name| hir.functions.iter().any(|f| hir.symbols.get(f.name) == *name))
    }

    #[test]
    fn single_file_function() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add("/project/main.nyx", "fn main(): i32 { 42 }");
        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();

        assert!(declares(&hir, &["nyx::main"]));
    }

    #[test]
    fn import_and_call() {
        let _fs = VirtualFS::default()
            .add("/project/math.nyx", "pub fn add(a: i32, b: i32): i32 { a + b }")
            .add(
                "/project/main.nyx",
                r#"
                use my_app::math;
                fn main(): i32 { math::add(1, 2) }
                "#,
            );
    }

    #[test]
    fn file_not_found() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default();
        let (_, err) = vloader(fs, &arena).load(Path::new("/project/missing.nyx")).unwrap_err();

        assert!(matches!(err, ModuleError::FileNotFound { .. }));
    }

    #[test]
    fn circular_import() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add(
                "/project/a.nyx",
                r#"
            use my_app::b::{foo};
            pub fn bar(): i32 { foo() }
            "#,
            )
            .add(
                "/project/b.nyx",
                r#"
            use my_app::a::{bar};
            pub fn foo(): i32 { bar() }
            "#,
            )
            .add(
                "/project/main.nyx",
                r#"
            use my_app::a::{bar};
            fn main(): i32 { bar() }
            "#,
            );

        let (_, err) = vloader(fs, &arena).load(Path::new("/project/main.nyx")).unwrap_err();

        assert!(matches!(err, ModuleError::CircularImport { .. }));
    }

    #[test]
    fn non_pub_return_unknown_function() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add("/project/math.nyx", "fn secret(a: i32): i32 { a + 1 }")
            .add(
                "/project/main.nyx",
                r#"
            use my_app::math::{secret};
            fn main(): i32 { secret(1) }
            "#,
            );

        // 'secret' is not exported so shouldn't be included in the symbols
        let (_, err) = vloader(fs, &arena).load(Path::new("/project/main.nyx")).unwrap_err();

        assert!(matches!(err, ModuleError::UnknownExport { .. }));
    }

    #[test]
    fn transitive_dependency() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add("/project/base.nyx", "pub fn one(): i32 { 1 }")
            .add(
                "/project/mid.nyx",
                r#"
            use my_app::base::{one};
            pub fn two(): i32 { one() + one() }
            "#,
            )
            .add(
                "/project/main.nyx",
                r#"
            use my_app::mid::{two};
            fn main(): i32 { two() }
            "#,
            );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        assert!(declares(&hir, &["nyx::one", "nyx::two", "nyx::main"]));
    }

    #[test]
    fn same_dependency_imported_twice_was_not_duplicated() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add("/project/math.nyx", "pub fn add(a: i32, b: i32): i32 { a + b }")
            .add(
                "/project/util.nyx",
                r#"
            use my_app::math::{add};
            pub fn add_one(x: i32): i32 { add(x, 1) }
            "#,
            )
            .add(
                "/project/main.nyx",
                r#"
            use my_app::math::{add};
            use my_app::util::{add_one};
            fn main(): i32 { add(add_one(1), 1) }
            "#,
            );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        let add_count =
            hir.functions.iter().filter(|f| hir.symbols.get(f.name) == "nyx::add").count();
        assert_eq!(add_count, 1, "add should appear exactly once in the merged HIR");
    }

    #[test]
    fn arity_mismatch_across_modules() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add("/project/math.nyx", "pub fn add(a: i32, b: i32): i32 { a + b }")
            .add(
                "/project/main.nyx",
                r#"
            use my_app::math::{add};
            fn main(): i32 { add(1) }
            "#,
            );

        let (_, err) = vloader(fs, &arena).load("/project/main.nyx").unwrap_err();
        assert!(matches!(err, ModuleError::Check(_)));
    }

    #[test]
    fn nested_path() {
        let arena = bumpalo::Bump::new();
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT), &arena);
        let path = loader
            .resolve_path(&[APP, "std", "collections", "map"], Span::default())
            .unwrap();

        assert_eq!(path, PathBuf::from("/project/std/collections/map.nyx"));
    }

    #[test]
    fn empty_module_is_valid() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add("/project/empty.nyx", "").add(
            "/project/main.nyx",
            r#"
            use my_app::empty;
            fn main(): i32 { 42 }
            "#,
        );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        assert!(declares(&hir, &["nyx::main"]));
    }

    #[test]
    fn duplicate_function_across_modules_rejected() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add("/project/math.nyx", "pub fn add(a: i32, b: i32): i32 { a + b }")
            .add(
                "/project/main.nyx",
                r#"
            use my_app::math::{add};
            fn add(a: i32, b: i32): i32 { a - b }
            fn main(): i32 { add(1, 2) }
            "#,
            );

        let (_, err) = vloader(fs, &arena).load("/project/main.nyx").unwrap_err();
        assert!(matches!(err, ModuleError::Check(_)));
    }

    #[test]
    fn namespace_import_with_qualified_call() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add(
                "/project/main.nyx",
                r#"
            use my_app::math::{add};
            fn main(): i32 {
                add(1, 2)
            }
            "#,
            )
            .add("/project/math.nyx", "pub fn add(a: i32, b: i32): i32 { a + b }");

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        assert!(declares(&hir, &["nyx::main"]));
    }

    #[test]
    fn qualified_import() {
        let arena = bumpalo::Bump::new();
        use crate::hir;

        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
            use std::process;
            fn main() {
                process::exit(0);
            }
            "#,
        );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        assert!(declares(&hir, &["nyx::main"]));

        let main = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();
        let has_exit_call = main.body.statements.iter().any(|stmt| {
            let hir::Statement::Expr(id) = stmt else {
                return false;
            };
            let hir::ExpressionKind::Call { args, .. } = &id.kind else {
                return false;
            };

            args.len() == 1 && {
                let arg = args[0];
                matches!(arg.kind, hir::ExpressionKind::Literal(hir::Literal::Int(0)))
                    && main.typeck.type_of(arg.id) == hir::Type::new(hir::TypeKind::I32)
            }
        });

        assert!(has_exit_call);

        let exit = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::exit").unwrap();
        let emits_exit_syscall = exit.body.statements.iter().any(|stmt| {
            let hir::Statement::Expr(id) = stmt else {
                return false;
            };
            matches!(
                &id.kind,
                hir::ExpressionKind::Syscall { code: hir::Syscall::Exit, args }
                if args.len() == 1
            )
        });

        assert!(emits_exit_syscall);
    }

    #[test]
    fn syscall_primitive_is_std_only() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
            use std::process;
            fn main() {
                syscall(SYS_EXIT, 0);
            }
            "#,
        );

        let (_, err) = vloader(fs, &arena).load("/project/main.nyx").unwrap_err();
        assert!(matches!(err, ModuleError::Check(_)));
    }

    #[test]
    fn qualified_std_intrinsics_keep_call_arguments() {
        let arena = bumpalo::Bump::new();
        use crate::hir;

        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
            use std::io;
            fn main() {
                io::println("ok");
            }
            "#,
        );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        let main = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

        let hir::Statement::Expr(id) = &main.body.statements[0] else {
            panic!("expected an expression statement");
        };
        assert!(matches!(
            &id.kind,
            hir::ExpressionKind::IntrinsicCall { intrinsic: hir::Intrinsic::PrintLn, args }
            if args.len() == 1
        ));
    }

    #[test]
    fn struct_in_single_mod() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
            struct Point {
                x: i64,
                y: i64,
            }

            fn make(x: i64, y: i64): Point {
                Point { x: x, y: y }
            }

            fn main(): i64 {
                let point = make(3, 4);
                point.x
            }
            "#,
        );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        assert!(declares(&hir, &["nyx::make", "nyx::main"]));
        assert_eq!(hir.structs.len(), 1);
    }

    #[test]
    fn struct_orphan_rule_rejected() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add("/project/types.nyx", "pub struct Point { x: i32, y: i32 }")
            .add(
                "/project/main.nyx",
                r#"
            use my_app::types::{Point};
            impl Point {
                fn sum(&self): i32 { self.x + self.y }
            }
            fn main(): i32 { 0 }
            "#,
            );

        let (_, err) = vloader(fs, &arena).load("/project/main.nyx").unwrap_err();
        assert!(matches!(err, ModuleError::Check(_)));
    }

    #[test]
    fn test_size_of_and_align_of_primitives() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
                use std::mem;
                fn main(): uptr {
                    let a = mem::size_of(i32);
                    let b = mem::align_of(i64);
                    a + b
                }
                "#,
        );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        let main_fn =
            hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

        let a_init = match &main_fn.body.statements[0] {
            Statement::LetInit { init, .. } => *init,
            _ => panic!("expected let statement"),
        };
        assert!(
            matches!(a_init.kind, ExpressionKind::TypeIntrinsic { .. }),
            "size_of should remain a HIR type intrinsic"
        );
        assert_eq!(main_fn.typeck.type_of(a_init.id), TypeKind::Uptr.into());

        let b_init = match &main_fn.body.statements[1] {
            Statement::LetInit { init, .. } => *init,
            _ => panic!("expected let statement"),
        };
        assert!(
            matches!(b_init.kind, ExpressionKind::TypeIntrinsic { .. }),
            "align_of should remain a HIR type intrinsic"
        );
        assert_eq!(main_fn.typeck.type_of(b_init.id), TypeKind::Uptr.into());
    }

    #[test]
    fn test_size_of_and_align_of_structs() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
                use std::mem;
                struct Foo {
                    a: i8,
                    b: i64,
                    c: i32,
                }
                fn main(): uptr {
                    mem::size_of(Foo)
                }
                "#,
        );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        let main_fn =
            hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => *expr,
            Statement::Return(Some(expr)) => *expr,
            _ => panic!("expected expression or return statement"),
        };
        assert!(
            matches!(body_expr.kind, ExpressionKind::TypeIntrinsic { .. }),
            "size_of should remain a HIR type intrinsic"
        );
        assert_eq!(main_fn.typeck.type_of(body_expr.id), TypeKind::Uptr.into());
    }

    #[test]
    fn test_size_of_and_align_of_struct_representations() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
                use std::mem;

                struct DefaultLayout { a: i8, b: i64, c: i32 }
                struct ExternLayout { a: i8, b: i64, c: i32 } as extern
                struct PackedLayout { a: i8, b: i64, c: i32 } as packed, align(4)
                struct RustLikePacked8 { a: i8, b: i64, c: i32 } as packed, align(8)
                struct RustLikePacked32 { a: i8, b: i64, c: i32 } as packed, align(32)

                fn main(): uptr {
                    mem::size_of(DefaultLayout)
                    + mem::size_of(ExternLayout)
                    + mem::size_of(PackedLayout)
                    + mem::align_of(PackedLayout)
                }
                "#,
        );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        let main_fn =
            hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => *expr,
            Statement::Return(Some(expr)) => *expr,
            _ => panic!("expected expression or return statement"),
        };

        assert_eq!(main_fn.typeck.type_of(body_expr.id), TypeKind::Uptr.into());
    }

    #[test]
    fn test_size_of_and_align_of_enums() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
                use std::mem;

                enum Status { Ok, Err = 7 } as u16

                fn main(): uptr {
                    mem::size_of(Status) + mem::align_of(Status)
                }
                "#,
        );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        assert!(hir.enums.iter().any(|e| hir.symbols.get(e.name) == "Status"));
        let main_fn =
            hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => *expr,
            Statement::Return(Some(expr)) => *expr,
            _ => panic!("expected expression or return statement"),
        };
        assert_eq!(main_fn.typeck.type_of(body_expr.id), TypeKind::Uptr.into());
    }

    #[test]
    fn exported_enum_supports_impl_and_self_interface_methods() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default()
            .add(
                "/project/status.nyx",
                r#"
                use std::default::{Default};
                use std::cmp::{PartialEq};

                pub enum Status { Ready = 1, Done = 2 } as u8

                impl Status {
                    fn code(&self): u8 { 7 }
                    fn touch(&mut self): u8 { 9 }
                }

                impl Status with Default {
                    fn default(): Self { Status::Ready }
                }

                impl Status with PartialEq {
                    fn eq(&self, other: &Self): bool { true }
                }
                "#,
            )
            .add(
                "/project/main.nyx",
                r#"
                use my_app::status::{Status};
                use std::mem;

                fn main(): u8 {
                    let mut status = Status::default();

                    if mem::size_of(Status) != 1 return 1;
                    if mem::align_of(Status) != 1 return 2;
                    if !status.eq(&Status::Done) return 3;
                    status.touch()
                }
                "#,
            );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        assert_eq!(hir.enums.len(), 6);
        assert!(hir.functions.iter().any(|f| hir.symbols.get(f.name).contains("Status")));
    }

    #[test]
    fn test_size_of_without_qualifier() {
        let arena = bumpalo::Bump::new();
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
                use std::mem::{size_of};
                fn main(): uptr {
                    size_of(i8)
                }
                "#,
        );

        let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
        let main_fn =
            hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();
        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => *expr,
            Statement::Return(Some(expr)) => *expr,
            _ => panic!("expected expression or return statement"),
        };
        assert!(
            matches!(body_expr.kind, ExpressionKind::TypeIntrinsic { .. }),
            "size_of should remain a HIR type intrinsic"
        );
    }
}
