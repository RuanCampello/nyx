use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::fs;
use std::path::Path;
use std::time::Duration;

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

criterion_group!(benches, std_compilation, tests_compilation);
criterion_main!(benches);
