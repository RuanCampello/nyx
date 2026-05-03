use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const PROGRAMS: &[(&str, &str)] = &[
    ("mandelbrot", include_str!("../tests/single/mandelbrot.nyx")),
    ("nth_prime", include_str!("../tests/single/nth_prime.nyx")),
];

fn build(name: &str, asm: &str) -> PathBuf {
    let temp_dir = std::env::temp_dir();
    let asm_path = temp_dir.join(format!("{name}_bench.s"));
    let obj_path = temp_dir.join(format!("{name}_bench.o"));
    let lib_path = temp_dir.join(format!("lib{name}_bench.so"));

    fs::write(&asm_path, asm).unwrap_or_else(|e| panic!("Failed to write asm for {name}: {e}"));

    Command::new("as")
        .args(["-o", obj_path.to_str().unwrap(), asm_path.to_str().unwrap()])
        .status()
        .expect("`as` failed to execute");

    Command::new("ld")
        .args([
            "-shared",
            "-z",
            "noexecstack",
            "-o",
            lib_path.to_str().unwrap(),
            obj_path.to_str().unwrap(),
        ])
        .status()
        .expect("linking failed");

    fs::remove_file(&asm_path).ok();
    fs::remove_file(&obj_path).ok();

    lib_path
}

fn compilation(c: &mut Criterion) {
    let mut group = c.benchmark_group("compilation");
    group.warm_up_time(Duration::from_millis(500));

    for (name, src) in PROGRAMS {
        group.bench_function(*name, |b| {
            b.iter(|| {
                let asm = nyx::compile(src).expect("program must be compilable");
                black_box(asm);
            })
        });
    }

    group.finish();
}

fn execution(c: &mut Criterion) {
    use libloading::{Library, Symbol};

    let mut group = c.benchmark_group("execution");
    group.warm_up_time(Duration::from_secs(4));

    for (name, src) in PROGRAMS {
        let asm = nyx::compile(src).expect("program must be compilable");
        let path = build(name, &asm);
        let lib = unsafe { Library::new(&path).expect("couldn't load library") };
        let main: Symbol<unsafe extern "C" fn() -> i32> =
            unsafe { lib.get(b"nyx_main\0").expect("failed to solve main") };

        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(unsafe { main() });
            })
        });

        fs::remove_file(path).expect("executable couldn't be removed");
    }

    group.finish();
}

criterion_group!(benches, compilation, execution);
criterion_main!(benches);
