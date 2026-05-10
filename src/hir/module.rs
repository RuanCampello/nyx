//! Multi-file module system with path resolution, cycle detection, and symbol merging.

use crate::{
    diagnostic::{self, Diagnostic},
    hir::{
        Function, FunctionBuilder, FunctionId, Hir, Offset, Struct, SymbolTable,
        functions::{collect_function_signatures, collect_structs, signatures_from_hir},
    },
    lexer::token::Span,
    parser::{
        Parser,
        statement::{Statement, UseItems},
    },
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Orchestrates module loading, path resolution, and HIR construction.
///
/// Maintains a cache of loaded modules and the shared symbol table
/// to ensure symbol IDs remain unique across the entire compilation.
pub(crate) struct ModuleLoader<F: FileSystem = FS> {
    name: String,
    root: PathBuf,
    /// standard library root
    std: PathBuf,
    cache: HashMap<PathBuf, Arc<Module>>,
    /// modules currently being loaded
    /// used for cycle detection
    in_flight: HashSet<PathBuf>,
    /// shared symbols interner for all modules
    symbols: SymbolTable,
    fs: F,
}

/// A fully-parsed and validated module
#[derive(Debug, Clone)]
struct Module {
    #[allow(dead_code)]
    exports: HashMap<String, usize>,
    /// all struct definitions in declaration order
    structs: Vec<Struct>,
    /// all functions in declaration order
    functions: Vec<Function>,
}

#[derive(Debug)]
pub enum ModuleError {
    FileNotFound {
        path: PathBuf,
        span: Option<Span>,
    },
    CircularImport {
        path: PathBuf,
        span: Span,
    },
    EmptyPath,
    UnknownRoot {
        name: String,
        span: Span,
    },
    UnknownExport {
        path: PathBuf,
        name: String,
        span: Span,
    },
    TopLevelNonFunction {
        path: PathBuf,
        span: Span,
    },
    Diagnostic(Diagnostic),
}

pub(crate) trait FileSystem {
    fn read(&self, path: &Path) -> Result<String, std::io::Error>;
    fn canonicalise(&self, path: &Path) -> Result<PathBuf, std::io::Error>;
}

pub(crate) struct FS;

impl ModuleLoader<FS> {
    pub fn new(name: String, root: PathBuf) -> Self {
        Self::with_file_system(name, root, resolve_std_root(), FS)
    }
}

impl<F: FileSystem> ModuleLoader<F> {
    pub fn with_file_system(name: String, root: PathBuf, std: PathBuf, fs: F) -> Self {
        Self {
            name,
            root,
            fs,
            std,
            cache: HashMap::new(),
            in_flight: HashSet::new(),
            symbols: SymbolTable::new(),
        }
    }

    /// Load all modules reacheable from the `entry` point and produce a merged `HIR`
    ///
    /// Modules are merged in dependency-first order. The entry module is always last,
    /// ensuring that `main` gets an id that the `_start` can call
    pub fn load(&mut self, entry: impl AsRef<Path>) -> Result<Hir, ModuleError> {
        let canonical =
            self.fs.canonicalise(entry.as_ref()).map_err(|_| ModuleError::FileNotFound {
                path: entry.as_ref().into(),
                span: None,
            })?;

        self.discover(&canonical, None)?;

        let mut dependencies: Vec<_> = self.cache.keys().cloned().collect();
        dependencies.sort_unstable();

        if let Some(position) = dependencies.iter().position(|pos| pos == &canonical) {
            let entry = dependencies.remove(position);

            dependencies.push(entry);
        }

        let mut functions = Vec::with_capacity(1 << 8);
        let mut structs = Vec::with_capacity(1 << 8);

        let mut offset = 0;
        for path in &dependencies {
            let module = Arc::clone(&self.cache[path]);

            for mut struct_def in module.structs.iter().cloned() {
                struct_def.offset(offset);
                structs.push(struct_def);
            }

            for mut function in module.functions.iter().cloned() {
                function.offset(offset);
                functions.push(function);
            }

            offset += module.structs.len() as u32;
        }

        Ok(Hir {
            functions,
            structs,
            symbols: self.symbols.clone().into_symbols(),
        })
    }

    /// Recursevly load a module and all its dependencies
    fn discover(
        &mut self,
        canonical: &Path,
        triggered_by: Option<Span>,
    ) -> Result<(), ModuleError> {
        if self.cache.contains_key(canonical) {
            return Ok(());
        }

        if self.in_flight.contains(canonical) {
            return Err(ModuleError::CircularImport {
                path: canonical.into(),
                span: triggered_by.unwrap_or_default(),
            });
        }

        self.in_flight.insert(canonical.to_path_buf());
        let source = self.fs.read(canonical).map_err(|_| ModuleError::FileNotFound {
            path: canonical.into(),
            span: triggered_by,
        })?;

        diagnostic::initialise(&source, canonical.to_str().unwrap_or("<unknown>"));

        let statements = Parser::new(&source).parse().map_err(|e| Diagnostic::from(e))?;

        for statement in &statements {
            let Statement::Use(declaration) = statement else {
                continue;
            };

            let import = self.resolve_path(&declaration.path.segments, declaration.span)?;
            if !self.fs.read(&import).is_ok() {
                continue;
            }

            self.discover(&import, Some(declaration.span))?;

            // validate named imports against the module's exports
            if let UseItems::Named(ref items) = declaration.items {
                let module = &self.cache[&import];

                for item in items {
                    if !module.exports.contains_key(item.name) {
                        return Err(ModuleError::UnknownExport {
                            path: import.into(),
                            name: item.name.into(),
                            span: item.span,
                        });
                    }
                }
            }
        }

        self.in_flight.remove(canonical);

        let module = self.analyse(canonical, statements)?;
        self.cache.insert(canonical.to_path_buf(), Arc::new(module));

        Ok(())
    }

    fn analyse(&mut self, path: &Path, statements: Vec<Statement>) -> Result<Module, ModuleError> {
        for statement in &statements {
            match statement {
                Statement::Fn(_) | Statement::Use(_) | Statement::Struct(_) => {}
                _ => {
                    return Err(ModuleError::TopLevelNonFunction {
                        path: path.into(),
                        span: statement.span(),
                    });
                }
            }
        }

        let (structs, struct_map) =
            collect_structs(&statements, &mut self.symbols).map_err(|e| Diagnostic::from(e))?;

        // build a combined signature + id table that includes all already-lowered dependencies
        let mut dependencies: Vec<_> = self.cache.keys().collect();
        dependencies.sort_unstable();

        let mut functions = Vec::new();
        for dependency in dependencies {
            let module = &self.cache[dependency];
            functions.extend(module.functions.iter().cloned());
        }

        let (mut signatures, mut map) = signatures_from_hir(&functions);

        let local_offset = signatures.len() as u32;
        let (local_signatures, local_map) =
            collect_function_signatures(&statements, &mut self.symbols, &struct_map)
                .map_err(|e| Diagnostic::from(e))?;

        // merge local signatures into combined table
        for (&symbol, &local_id) in &local_map {
            // check for duplicated function names between local and imported
            if map.contains_key(&symbol) {
                use crate::hir::error::{HirError, HirErrorKind};

                let name = self.symbols.get(symbol).to_string();

                let span = statements
                    .iter()
                    .find_map(|stmt| match stmt {
                        Statement::Fn(func) if func.name == name.as_str() => Some(func.span),
                        _ => None,
                    })
                    .unwrap_or_default();

                return Err(ModuleError::Diagnostic(
                    HirError {
                        kind: HirErrorKind::DuplicateFunction { name },
                        span,
                    }
                    .into(),
                ));
            }

            map.insert(symbol, FunctionId(local_offset + local_id.0));
        }

        signatures.extend(local_signatures);

        let mut functions = Vec::new();
        let mut exports = HashMap::new();

        for statement in statements {
            let function = match statement {
                Statement::Fn(f) => f,
                _ => continue,
            };

            let builder = FunctionBuilder::new(
                &signatures,
                &map,
                &structs,
                &struct_map,
                &mut self.symbols,
                function,
            );
            let mut hir = builder.lower().map_err(|e| Diagnostic::from(e))?;

            hir.id = FunctionId(local_offset + functions.len() as u32);

            if hir.is_pub {
                exports.insert(self.symbols.get(hir.name).to_string(), functions.len());
            }

            functions.push(hir);
        }

        Ok(Module {
            functions,
            structs,
            exports,
        })
    }

    fn resolve_path(&self, segments: &[&str], span: Span) -> Result<PathBuf, ModuleError> {
        let (&root, rest) = segments.split_first().ok_or(ModuleError::EmptyPath)?;

        if rest.is_empty() {
            return Err(ModuleError::EmptyPath);
        }

        let base = match root {
            root if root == self.name => &self.root,
            "std" => &self.std,
            other => {
                return Err(ModuleError::UnknownRoot {
                    name: other.to_string(),
                    span,
                });
            }
        };

        let (dirs, file_segment) = rest.split_at(rest.len() - 1);
        let mut path = base.to_path_buf();

        path.extend(dirs);
        path.push(format!("{}.nyx", file_segment[0]));

        Ok(path)
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

impl From<Diagnostic> for ModuleError {
    fn from(value: Diagnostic) -> Self {
        Self::Diagnostic(value)
    }
}

/// Resolves the path to the compiler's built-in `std/` directory
///
/// order:
/// 1. `NYX_STD_PATH` environment variable
/// 2. `<binary_dir>/std/`
/// 3. `std/` relative to CWD
fn resolve_std_root() -> PathBuf {
    if let Ok(env) = std::env::var("NYX_STD_PATH") {
        return PathBuf::from(env);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("std");
            if candidate.is_dir() {
                return candidate;
            }
        }
    }

    PathBuf::from("std")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::Type;
    use lasso::Key;
    use std::collections::HashMap;
    use std::io;

    #[derive(Default)]
    struct VirtualFS {
        files: HashMap<PathBuf, String>,
    }

    impl FileSystem for VirtualFS {
        fn read(&self, path: &Path) -> Result<String, io::Error> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.display().to_string()))
        }

        fn canonicalise(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
            match self.files.contains_key(path) {
                true => Ok(path.to_path_buf()),
                _ => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    path.display().to_string(),
                )),
            }
        }
    }

    impl VirtualFS {
        fn add(mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
            self.files.insert(path.into(), content.into());
            self
        }
    }

    fn vloader(fs: VirtualFS) -> ModuleLoader<VirtualFS> {
        ModuleLoader::with_file_system(APP.into(), PROJECT.into(), STD.into(), fs)
    }

    const APP: &str = "my_app";
    const PROJECT: &str = "/project";
    const STD: &str = "/std";

    #[test]
    fn resolve_simple_path() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let path = loader.resolve_path(&[APP, "math"], Span::default()).unwrap();

        assert_eq!(path, PathBuf::from("/project/math.nyx"));
    }

    #[test]
    fn resolve_nested_path() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let path = loader.resolve_path(&[APP, "utils", "io", "file"], Span::default()).unwrap();

        assert_eq!(path, PathBuf::from("/project/utils/io/file.nyx"));
    }

    #[test]
    fn reject_unknown_root() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let err = loader.resolve_path(&["other", "foo"], Span::default()).unwrap_err();

        match err {
            ModuleError::UnknownRoot { name, .. } => assert_eq!(name, "other"),
            _ => panic!("expected unknownroot error"),
        }
    }

    #[test]
    fn reject_empty_path() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let err = loader.resolve_path(&[], Span::default()).unwrap_err();

        assert!(matches!(err, ModuleError::EmptyPath));
    }

    #[test]
    fn reject_root_only() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let err = loader.resolve_path(&[APP], Span::default()).unwrap_err();

        assert!(matches!(err, ModuleError::EmptyPath));
    }

    #[test]
    fn single_file_function() {
        let fs = VirtualFS::default().add("/project/main.nyx", "fn main(): i32 { 42 }");
        let hir = vloader(fs).load("/project/main.nyx").unwrap();

        assert_eq!(hir.functions.len(), 1);
        assert_eq!(hir.functions[0].return_type, Type::I32);
    }

    #[test]
    fn import_and_call() {
        let _fs = VirtualFS::default()
            .add(
                "/project/math.nyx",
                "pub fn add(a: i32, b: i32): i32 { a + b }",
            )
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
        let fs = VirtualFS::default();
        let err = vloader(fs).load(Path::new("/project/missing.nyx")).unwrap_err();

        assert!(matches!(err, ModuleError::FileNotFound { .. }));
    }

    #[test]
    fn circular_import() {
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

        let err = vloader(fs).load(Path::new("/project/main.nyx")).unwrap_err();

        assert!(matches!(err, ModuleError::CircularImport { .. }));
    }

    #[test]
    fn non_pub_return_unknown_function() {
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
        let err = vloader(fs).load(Path::new("/project/main.nyx")).unwrap_err();

        assert!(matches!(err, ModuleError::UnknownExport { .. }));
    }

    #[test]
    fn transitive_dependency() {
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        assert_eq!(hir.functions.len(), 3);
    }

    #[test]
    fn same_dependency_imported_twice_was_not_duplicated() {
        let fs = VirtualFS::default()
            .add(
                "/project/math.nyx",
                "pub fn add(a: i32, b: i32): i32 { a + b }",
            )
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        let add_count = hir
            .functions
            .iter()
            .filter(|f| hir.symbols.get(f.name.0.into_usize()).map(|s| s == "add").unwrap_or(false))
            .count();
        assert_eq!(
            add_count, 1,
            "add should appear exactly once in the merged HIR"
        );
    }

    #[test]
    fn arity_mismatch_across_modules() {
        let fs = VirtualFS::default()
            .add(
                "/project/math.nyx",
                "pub fn add(a: i32, b: i32): i32 { a + b }",
            )
            .add(
                "/project/main.nyx",
                r#"
            use my_app::math::{add};
            fn main(): i32 { add(1) }
            "#,
            );

        let err = vloader(fs).load("/project/main.nyx").unwrap_err();
        assert!(matches!(err, ModuleError::Diagnostic(_)));
    }

    #[test]
    fn nested_path() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let path = loader
            .resolve_path(&[APP, "std", "collections", "map"], Span::default())
            .unwrap();

        assert_eq!(path, PathBuf::from("/project/std/collections/map.nyx"));
    }

    #[test]
    fn empty_module_is_valid() {
        let fs = VirtualFS::default().add("/project/empty.nyx", "").add(
            "/project/main.nyx",
            r#"
            use my_app::empty;
            fn main(): i32 { 42 }
            "#,
        );

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        assert_eq!(hir.functions.len(), 1);
    }

    #[test]
    fn duplicate_function_across_modules_rejected() {
        let fs = VirtualFS::default()
            .add(
                "/project/math.nyx",
                "pub fn add(a: i32, b: i32): i32 { a + b }",
            )
            .add(
                "/project/main.nyx",
                r#"
            use my_app::math::{add};
            fn add(a: i32, b: i32): i32 { a - b }
            fn main(): i32 { add(1, 2) }
            "#,
            );

        let err = vloader(fs).load("/project/main.nyx").unwrap_err();
        assert!(matches!(err, ModuleError::Diagnostic(_)));
    }

    #[test]
    fn namespace_import_with_qualified_call() {
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
            .add(
                "/project/math.nyx",
                "pub fn add(a: i32, b: i32): i32 { a + b }",
            );

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        assert_eq!(hir.functions.len(), 2);
    }

    #[test]
    fn qualified_import() {
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
            use std::process;
            fn main() {
                process::exit(0);
            }
            "#,
        );

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        println!("{hir:#?}");
    }

    #[test]
    fn struct_in_single_mod() {
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        assert_eq!(hir.functions.len(), 2);
        assert_eq!(hir.structs.len(), 1);
    }
}
