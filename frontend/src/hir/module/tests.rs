use super::*;
use crate::hir::{ExpressionKind, Function, Hir, Statement, Type, TypeKind};
use std::collections::HashMap;
use std::io;

#[derive(Default)]
struct VirtualFS {
    files: HashMap<PathBuf, String>,
}

const APP: &str = "my_app";
const PROJECT: &str = "/project";
const STD: &str = "/std";

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

fn local_typ<'hir>(main: &Function<'hir>, hir: &Hir<'hir>, name: &str) -> Type<'hir> {
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

    let hir = vloader(fs, &arena).load(Path::new("/project/main.nyx")).unwrap();
    assert!(hir.diagnostics.iter().any(|error| error.message == "Circular import"));
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
    let hir = vloader(fs, &arena).load(Path::new("/project/main.nyx")).unwrap();
    assert!(hir.diagnostics.iter().any(|error| error.message.contains("is not exported")));
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
    let add_count = hir.functions.iter().filter(|f| hir.symbols.get(f.name) == "nyx::add").count();
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

    let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
    assert!(
        hir.diagnostics
            .iter()
            .any(|error| error.message.contains("Wrong number of arguments"))
    );
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

    let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
    assert!(
        hir.diagnostics
            .iter()
            .any(|error| error.message.contains("cannot be declared multiple times"))
    );
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
                && main.typeck.type_of(arg.id) == hir.types.common.i32
        }
    });

    assert!(has_exit_call);

    let exit = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::exit").unwrap();
    let emits_exit_syscall = exit.body.statements.iter().any(|stmt| {
        let hir::Statement::Expr(id) = stmt else {
            return false;
        };
        matches!(&id.kind, hir::ExpressionKind::Call { args, .. } if args.len() == 1)
            && matches!(
                exit.typeck.type_dependent_def(id.id),
                Some(hir::Res::Syscall(hir::Syscall::Exit))
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

    let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
    assert!(hir.diagnostics.iter().any(|error| error.message.contains("syscall")));
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
    assert!(matches!(&id.kind, hir::ExpressionKind::Call { args, .. } if args.len() == 1));
    assert_eq!(
        main.typeck.type_dependent_def(id.id),
        Some(hir::Res::Intrinsic(hir::Intrinsic::PrintLn))
    );
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
    assert_eq!(hir.adts.iter().filter(|adt| adt.is_struct()).count(), 1);
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

    let hir = vloader(fs, &arena).load("/project/main.nyx").unwrap();
    assert!(
        hir.diagnostics
            .iter()
            .any(|error| error.message.contains("Cannot implement methods on"))
    );
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
    let main_fn = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

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
    let main_fn = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

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
    let main_fn = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

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
    assert!(
        hir.adts
            .iter()
            .any(|adt| adt.is_enum() && hir.symbols.get(adt.name) == "Status")
    );
    let main_fn = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();

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
    assert_eq!(
        hir.adts
            .iter()
            .filter(|adt| hir.symbols.get(adt.name).ends_with("Status"))
            .count(),
        1,
        "structural generic use must not clone enum definitions",
    );
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
    let main_fn = hir.functions.iter().find(|f| hir.symbols.get(f.name) == "nyx::main").unwrap();
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
