use bumpalo::Bump;
use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use nyx::lexer::Lexer;
use nyx::lir::target;
use nyx::parser::{Parser, statement::Statement};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const PROGRAMS: &[(&str, &str)] = &[
    ("mandelbrot", include_str!("../tests/single/mandelbrot.nyx")),
    ("nth_prime", include_str!("../tests/single/nth_prime.nyx")),
    ("binary_search", include_str!("../tests/single/binary_search.nyx")),
    ("nested_structs", include_str!("../tests/single/nested_structs.nyx")),
];

fn parsed(src: &str) -> Vec<Statement<'_>> {
    Parser::new(src).parse().expect("fixture must parse")
}

fn register_source(src: &str) {
    nyx::diagnostic::reset();
    nyx::diagnostic::add_file("<bench>", src);
}

fn lex(c: &mut Criterion) {
    let mut group = c.benchmark_group("lex");

    for (name, src) in PROGRAMS {
        group.bench_function(*name, |b| {
            b.iter(|| {
                for token in Lexer::new(black_box(*src)) {
                    black_box(token.expect("fixture must lex"));
                }
            })
        });
    }

    group.finish();
}

fn parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");

    for (name, src) in PROGRAMS {
        group.bench_function(*name, |b| b.iter(|| black_box(parsed(black_box(src)))));
    }

    group.finish();
}

fn hir(c: &mut Criterion) {
    let mut group = c.benchmark_group("hir");

    for (name, src) in PROGRAMS {
        register_source(src);
        group.bench_function(*name, |b| {
            b.iter_batched(
                || parsed(src),
                |statements| {
                    let arena = Bump::new();
                    black_box(nyx::hir::lower(statements, &arena).expect("fixture must lower"));
                },
                BatchSize::SmallInput,
            )
        });
    }

    group.finish();
}

fn mir(c: &mut Criterion) {
    let mut group = c.benchmark_group("mir");

    for (name, src) in PROGRAMS {
        register_source(src);
        group.bench_function(*name, |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let arena = Bump::new();
                    let hir = nyx::hir::lower(parsed(src), &arena).expect("fixture must lower");

                    let start = Instant::now();
                    let mir = nyx::mir::lower(hir).expect("fixture must lower to MIR");
                    total += start.elapsed();

                    black_box(mir);
                }
                total
            })
        });
    }

    group.finish();
}

fn lir_target<T: target::Lowerable>(c: &mut Criterion, group_name: &str)
where
    nyx::lir::Function<T>: target::Emittable<T>,
{
    let mut group = c.benchmark_group(group_name);

    for (name, src) in PROGRAMS {
        register_source(src);
        let arena = Bump::new();
        let hir = nyx::hir::lower(parsed(src), &arena).expect("fixture must lower");
        let mir = nyx::mir::lower(hir).expect("fixture must lower to MIR");

        group.bench_function(*name, |b| b.iter(|| black_box(nyx::lir::emit::<T>(&mir))));
    }

    group.finish();
}

fn lir(c: &mut Criterion) {
    lir_target::<target::X86_64>(c, "lir-x86_64");
    lir_target::<target::AArch64>(c, "lir-aarch64");
}

fn std_compilation(c: &mut Criterion) {
    let mut group = c.benchmark_group("std_compilation");
    group.warm_up_time(Duration::from_millis(500));

    let temp_dir = std::env::temp_dir();
    let entry_path = temp_dir.join("main.nyx");
    fs::write(
        &entry_path,
        r#"
        use std::int;
        use std::float;
        use std::char;
        use std::default;
        use std::cmp;
        use std::mem;
        use std::process;
        use std::io;

        fn main() {}
        "#,
    )
    .expect("failed to write temp main.nyx");

    group.bench_function("compile_std", |b| {
        b.iter(|| {
            let asm = nyx::compile_project(black_box(&entry_path), black_box("my_app"))
                .expect("failed to compile std project");
            black_box(asm);
        })
    });

    fs::remove_file(&entry_path).ok();
    group.finish();
}

fn tests_compilation(c: &mut Criterion) {
    let mut group = c.benchmark_group("tests_compilation");
    group.warm_up_time(Duration::from_millis(500));

    let test_dir = Path::new("tests/single");
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(test_dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "nyx") {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let project = path.file_stem().unwrap().to_string_lossy().to_string();
                if nyx::compile_project(&path, &project).is_ok() {
                    files.push((name, path));
                }
            }
        }
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, path) in &files {
        let project = path.file_stem().unwrap().to_string_lossy().to_string();
        group.bench_function(name, |b| {
            b.iter(|| {
                let asm = nyx::compile_project(black_box(path), black_box(&project))
                    .expect("compilation failed");
                black_box(asm);
            })
        });
    }

    group.finish();
}

criterion_group!(benches, lex, parse, hir, mir, lir, std_compilation, tests_compilation);
criterion_main!(benches);
