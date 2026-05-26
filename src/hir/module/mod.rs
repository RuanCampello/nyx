//! Multi-file module system with path resolution, cycle detection, and symbol merging.

mod arena;
mod demand;
mod graph;
mod resolver;
mod signatures;

use crate::{
    diagnostic::Diagnostic,
    hir::{Hir, SymbolTable, scope::Scope},
    lexer::token::Span,
};
use nyx_macros::Diagnostic;
use resolver::ModuleResolver;
use std::path::{Path, PathBuf};

/// Orchestrates module loading, path resolution, and HIR construction.
///
/// Maintains a cache of loaded modules and the shared symbol table
/// to ensure symbol IDs remain unique across the entire compilation.
pub(crate) struct ModuleLoader<F: FileSystem = FS> {
    resolver: ModuleResolver,
    scope: Scope<'static>,
    /// shared symbols interner for all modules
    symbols: SymbolTable,
    fs: F,
}

#[derive(Debug, Diagnostic)]
#[rustfmt::skip]
pub enum ModuleError {
    #[diagnostic(
        message = "module file not found: {path.display()!}",
        primary = "imported here",
        help = "make sure the file {path.display()!} exists"
    )]
    FileNotFound { path: PathBuf, span: Option<Span> },

    #[diagnostic(
        message = "circular import: {path.display()!} is already being loaded",
        primary = "this import creates a cycle",
        help = "remove the circular dependency between modules"
    )]
    CircularImport { path: PathBuf, span: Span },

    #[diagnostic(
        message = "empty import path",
        primary = "this path has no segments",
        help = "use paths like {`use project::module;`}"
    )]
    EmptyPath,

    #[diagnostic(
        message = "unknown module root {name!}",
        primary = "{name!} is not a known module root",
        help = "the root segment must match your project name"
    )]
    UnknownRoot { name: String, span: Span },

    #[diagnostic(
        message = "module {path.display()!} has no exported symbol {name!}",
        primary = "{name!} is not exported from this module",
        help = "add {`pub`} to {`fn {name}`} to export it"
    )]
    UnknownExport {
        path: PathBuf,
        name: String,
        span: Span,
    },

    #[diagnostic(
        message = "only function declarations are allowed at the top level",
        primary = "this is not a function declaration",
        help = "move this into a function body, or wrap it in {`fn main()`}"
    )]
    TopLevelNonFunction { path: PathBuf, span: Span },

    #[diagnostic(transparent)]
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
            resolver: ModuleResolver::new(name, canonical_root, canonical_std),
            fs,
            scope: Scope::new(),
            symbols: SymbolTable::new(),
        }
    }

    /// Load all modules reacheable from the `entry` point and produce a merged `HIR`
    ///
    /// Modules are merged in dependency-first order. The entry module is always last,
    /// ensuring that `main` gets an id that the `_start` can call
    #[rustfmt::skip]
    pub fn load(&mut self, entry: impl AsRef<Path>) -> Result<Hir, ModuleError> {
        let arena = arena::SourceArena::new();
        let mut graph = graph::build_graph(entry.as_ref(), &self.resolver, &self.fs, &arena)?;
        let order = graph.all_nodes_order();
        let interfaces = signatures::build_signatures(&mut graph, &order, &mut self.scope, &mut self.symbols)?;
        let functions = demand::lower_reachable(&mut graph, &order, &interfaces, &self.scope, &mut self.symbols)?;

        Ok(Hir {
            functions,
            structs: self.scope.structs.clone(),
            enums: self.scope.enums.clone(),
            symbols: self.symbols.clone().into_symbols(),
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

impl From<Diagnostic> for ModuleError {
    fn from(value: Diagnostic) -> Self {
        Self::Diagnostic(value)
    }
}

impl From<ModuleError> for Diagnostic {
    fn from(value: ModuleError) -> Diagnostic {
        match value {
            ModuleError::Diagnostic(d) => d,
            other => {
                let span = match &other {
                    ModuleError::FileNotFound { span, .. } => span.unwrap_or_default(),
                    ModuleError::CircularImport { span, .. } => *span,
                    ModuleError::EmptyPath => Span::default(),
                    ModuleError::UnknownRoot { span, .. } => *span,
                    ModuleError::UnknownExport { span, .. } => *span,
                    ModuleError::TopLevelNonFunction { span, .. } => *span,
                    ModuleError::Diagnostic(_) => unreachable!(),
                };
                crate::diagnostic::AsDiagnostic::as_diagnostic(other, span)
            },
        }
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
    use crate::hir::{ExpressionKind, Statement, Type, TypeKind};
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
        let main = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "nyx::main")
            .unwrap();
        assert_eq!(main.return_type, Type::new(TypeKind::I32));
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        let add_count = hir
            .functions
            .iter()
            .filter(|f| {
                hir.symbols.get(f.name.0.into_usize()).map(|s| s == "nyx::add").unwrap_or(false)
            })
            .count();
        assert_eq!(add_count, 1, "add should appear exactly once in the merged HIR");
    }

    #[test]
    fn arity_mismatch_across_modules() {
        let fs = VirtualFS::default()
            .add("/project/math.nyx", "pub fn add(a: i32, b: i32): i32 { a + b }")
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
            .add("/project/math.nyx", "pub fn add(a: i32, b: i32): i32 { a + b }")
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
            .add("/project/math.nyx", "pub fn add(a: i32, b: i32): i32 { a + b }");

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        assert_eq!(hir.functions.len(), 2);
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
        assert_eq!(hir.functions.len(), 2);

        let main = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "nyx::main")
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
                        typ,
                        ..
                    } if *typ == hir::Type::new(hir::TypeKind::I32)
                )
            )
        });

        assert!(has_exit_call);

        let exit = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "nyx::exit")
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
            .find(|f| hir.symbols[f.name.0.into_usize()] == "nyx::main")
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
        assert_eq!(hir.functions.len(), 2);
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
            .find(|f| hir.symbols[f.name.0.into_usize()] == "nyx::main")
            .unwrap();

        let a_init = match &main_fn.body.statements[0] {
            Statement::Let { init: Some(expr), .. } => expr,
            _ => panic!("expected let statement"),
        };
        assert_eq!(a_init.kind, ExpressionKind::Integer(4));
        assert_eq!(a_init.typ, Type::new(TypeKind::Uptr));

        let b_init = match &main_fn.body.statements[1] {
            Statement::Let { init: Some(expr), .. } => expr,
            _ => panic!("expected let statement"),
        };
        assert_eq!(b_init.kind, ExpressionKind::Integer(8));
        assert_eq!(b_init.typ, Type::new(TypeKind::Uptr));
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
            .find(|f| hir.symbols[f.name.0.into_usize()] == "nyx::main")
            .unwrap();

        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => expr,
            Statement::Return(Some(expr)) => expr,
            _ => panic!("expected expression or return statement"),
        };
        assert_eq!(body_expr.kind, ExpressionKind::Integer(16));
        assert_eq!(body_expr.typ, Type::new(TypeKind::Uptr));
    }

    #[test]
    fn test_size_of_and_align_of_struct_representations() {
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        assert_eq!(hir.structs[0].size, 16);
        assert_eq!(hir.structs[0].align, 8);
        assert_eq!(hir.structs[1].size, 24);
        assert_eq!(hir.structs[1].align, 8);
        assert_eq!(hir.structs[2].size, 16);
        assert_eq!(hir.structs[2].align, 4);
        assert_eq!(hir.structs[3].size, 24);
        assert_eq!(hir.structs[3].align, 8);
        assert_eq!(hir.structs[4].size, 24);
        assert_eq!(hir.structs[4].align, 8);
        let main_fn = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "nyx::main")
            .unwrap();

        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => expr,
            Statement::Return(Some(expr)) => expr,
            _ => panic!("expected expression or return statement"),
        };

        assert_eq!(body_expr.typ, Type::new(TypeKind::Uptr));
    }

    #[test]
    fn test_size_of_and_align_of_enums() {
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        assert_eq!(hir.enums.len(), 1);
        let main_fn = hir
            .functions
            .iter()
            .find(|f| hir.symbols[f.name.0.into_usize()] == "nyx::main")
            .unwrap();

        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => expr,
            Statement::Return(Some(expr)) => expr,
            _ => panic!("expected expression or return statement"),
        };
        assert_eq!(body_expr.typ, Type::new(TypeKind::Uptr));
    }

    #[test]
    fn exported_enum_supports_impl_and_self_interface_methods() {
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

        let hir = vloader(fs).load("/project/main.nyx").unwrap();
        assert_eq!(hir.enums.len(), 1);
        assert!(
            hir.functions
                .iter()
                .any(|f| hir.symbols[f.name.0.into_usize()].contains("Status"))
        );
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
            .find(|f| hir.symbols[f.name.0.into_usize()] == "nyx::main")
            .unwrap();
        let body_expr = match &main_fn.body.statements[0] {
            Statement::Expr(expr) => expr,
            Statement::Return(Some(expr)) => expr,
            _ => panic!("expected expression or return statement"),
        };
        assert_eq!(body_expr.kind, ExpressionKind::Integer(1));
    }
}
