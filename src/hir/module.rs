//! Multi-file module system with path resolution, cycle detection, and symbol merging.

use crate::{
    diagnostic::{self, Diagnostic},
    hir::{Declarations, Function, Hir, Struct, SymbolTable, scope::Scope},
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
    scope: Scope,
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
        let canonical_root = fs.canonicalise(&root).unwrap_or_else(|_| root.clone());
        let canonical_std = fs.canonicalise(&std).unwrap_or_else(|_| std.clone());
        Self {
            name,
            root: canonical_root,
            fs,
            std: canonical_std,
            scope: Scope::new(),
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

        // automatically discover standard library prelude
        for name in &["int.nyx", "float.nyx", "char.nyx"] {
            let path = self.std.join(name);
            if let Ok(canon) = self.fs.canonicalise(&path) {
                if self.fs.read(&canon).is_ok() {
                    self.discover(&canon, None)?;
                }
            }
        }

        self.discover(&canonical, None)?;

        let mut dependencies: Vec<_> = self.cache.keys().cloned().collect();
        dependencies.sort_unstable();

        if let Some(position) = dependencies.iter().position(|pos| pos == &canonical) {
            let entry = dependencies.remove(position);

            dependencies.push(entry);
        }

        let mut functions = Vec::with_capacity(1 << 8);
        let mut structs = Vec::with_capacity(1 << 8);

        for path in &dependencies {
            let module = Arc::clone(&self.cache[path]);

            for struct_def in module.structs.iter().cloned() {
                structs.push(struct_def);
            }

            for function in module.functions.iter().cloned() {
                functions.push(function);
            }
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

        let module = self.analyse(canonical, &source, statements)?;
        self.cache.insert(canonical.to_path_buf(), Arc::new(module));

        Ok(())
    }

    fn analyse(
        &mut self,
        path: &Path,
        source: &str,
        statements: Vec<Statement>,
    ) -> Result<Module, ModuleError> {
        let decls = Declarations::partition(&statements).map_err(|e| Diagnostic::from(e))?;
        let struct_offset = self.scope.structs.len();
        let in_std = path.starts_with(&self.std);
        self.scope
            .extend(&decls, &mut self.symbols, in_std)
            .map_err(|e| Diagnostic::from(e))?;

        diagnostic::initialise(source, path.to_str().unwrap_or("<unknown>"));

        let functions = self
            .scope
            .lower_functions(&decls, &mut self.symbols, in_std)
            .map_err(|e| Diagnostic::from(e))?;

        let structs = self.scope.structs[struct_offset..].to_vec();

        let mut exports = HashMap::new();
        for s in &decls.structs {
            if s.is_pub {
                exports.insert(s.name.to_string(), 0);
            }
        }
        for i in &decls.interfaces {
            if i.is_pub {
                exports.insert(i.name.to_string(), 0);
            }
        }
        for f in &functions {
            if f.is_pub {
                exports.insert(self.symbols.get(f.name).to_string(), 0);
            }
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
    use crate::hir::{ExpressionKind, Statement, Type};
    use lasso::Key;
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

            Err(io::Error::new(
                io::ErrorKind::NotFound,
                path.display().to_string(),
            ))
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

            Err(io::Error::new(
                io::ErrorKind::NotFound,
                path.display().to_string(),
            ))
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

        assert_eq!(hir.functions.len(), 16);
        let main = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "main")
            .unwrap();
        assert_eq!(main.return_type, Type::I32);
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
        assert_eq!(hir.functions.len(), 18);
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
        assert_eq!(hir.functions.len(), 16);
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
        assert_eq!(hir.functions.len(), 17);
    }

    #[test]
    fn qualified_import() {
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        assert_eq!(hir.functions.len(), 17); // main + exit (syscall is a synthetic scope entry, not a Module fn)

        let main = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "main")
            .unwrap();
        let has_exit_call = main.body.statements.iter().any(|stmt| {
            matches!(
                stmt,
                hir::Statement::Expr(hir::Expression {
                    kind: hir::ExpressionKind::Call {
                        args,
                        ..
                    },
                    ..
                })
                if args.len() == 1 && matches!(
                    &args[0],
                    hir::Expression {
                        kind: hir::ExpressionKind::Integer(0),
                        typ: hir::Type::I32,
                        ..
                    }
                )
            )
        });

        assert!(has_exit_call);

        let exit = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "exit")
            .unwrap();
        let emits_exit_syscall = exit.body.statements.iter().any(|stmt| {
            matches!(
                stmt,
                hir::Statement::Expr(hir::Expression {
                    kind: hir::ExpressionKind::Syscall {
                        code: hir::SyscallCode::Exit,
                        args,
                    },
                    ..
                })
                if args.len() == 1
            )
        });

        assert!(emits_exit_syscall);
    }

    #[test]
    fn syscall_primitive_is_std_only() {
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
            use std::process;
            fn main() {
                syscall(SYS_EXIT, 0);
            }
            "#,
        );

        let err = vloader(fs).load("/project/main.nyx").unwrap_err();
        assert!(matches!(err, ModuleError::Diagnostic(_)));
    }

    #[test]
    fn qualified_std_intrinsics_keep_call_arguments() {
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        let main = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "main")
            .unwrap();

        assert!(matches!(
            &main.body.statements[0],
            hir::Statement::Expr(hir::Expression {
                kind: hir::ExpressionKind::IntrinsicCall {
                    intrinsic: hir::Intrinsic::PrintLn,
                    args,
                },
                ..
            }) if args.len() == 1
        ));
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
        assert_eq!(hir.functions.len(), 17);
        assert_eq!(hir.structs.len(), 1);
    }

    #[test]
    fn struct_orphan_rule_rejected() {
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

        let err = vloader(fs).load("/project/main.nyx").unwrap_err();
        assert!(matches!(err, ModuleError::Diagnostic(_)));
    }

    #[test]
    fn test_size_of_and_align_of_primitives() {
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        let main_fn = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "main")
            .unwrap();

        let a_init = match &main_fn.body.statements[0] {
            Statement::Let {
                init: Some(expr), ..
            } => expr,
            _ => panic!("expected let statement"),
        };
        assert_eq!(a_init.kind, ExpressionKind::Integer(4));
        assert_eq!(a_init.typ, Type::Uptr);

        let b_init = match &main_fn.body.statements[1] {
            Statement::Let {
                init: Some(expr), ..
            } => expr,
            _ => panic!("expected let statement"),
        };
        assert_eq!(b_init.kind, ExpressionKind::Integer(8));
        assert_eq!(b_init.typ, Type::Uptr);
    }

    #[test]
    fn test_size_of_and_align_of_structs() {
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        let main_fn = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "main")
            .unwrap();

        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => expr,
            Statement::Return(Some(expr)) => expr,
            _ => panic!("expected expression or return statement"),
        };
        assert_eq!(body_expr.kind, ExpressionKind::Integer(16));
        assert_eq!(body_expr.typ, Type::Uptr);
    }

    #[test]
    fn test_size_of_without_qualifier() {
        let fs = VirtualFS::default().add(
            "/project/main.nyx",
            r#"
                use std::mem::{size_of};
                fn main(): uptr {
                    size_of(i8)
                }
                "#,
        );

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        let main_fn = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "main")
            .unwrap();
        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => expr,
            Statement::Return(Some(expr)) => expr,
            _ => panic!("expected expression or return statement"),
        };
        assert_eq!(body_expr.kind, ExpressionKind::Integer(1));
    }
}
