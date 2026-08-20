use backend::optimisation::Level;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct Case<'o> {
    name: String,
    file: PathBuf,
    exit_code: Option<i32>,
    stdout: &'o str,
}

fn cases<'o>() -> Vec<Case<'o>> {
    let mut cases: Vec<_> = fs::read_dir("tests/single")
        .expect("integration fixture directory must exist")
        .map(|entry| entry.expect("integration fixture must be readable").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "nyx"))
        .map(|file| {
            let name = file
                .file_stem()
                .expect("fixture must have a stem")
                .to_string_lossy()
                .into_owned();
            let (exit_code, stdout) = expectation(&name);

            Case { name, file, exit_code, stdout }
        })
        .collect();

    cases.sort_unstable_by(|lhs, rhs| lhs.name.cmp(&rhs.name));
    cases
}

fn expectation<'s>(name: &str) -> (Option<i32>, &'s str) {
    let exit_code = match name {
        "add" | "max" => None,
        "fibonacci" => Some(55),
        "collatz" => Some(111),
        "const_eval"
        | "floats"
        | "exit"
        | "impl_methods"
        | "bitwise"
        | "regalloc_terminator"
        | "cast"
        | "cmp_overload"
        | "generic_methods"
        | "raw_pointers"
        | "expression_bodies"
        | "statics"
        | "mmap_alloc"
        | "heap_classes"
        | "page_align"
        | "heap_alloc"
        | "heap_reclaim"
        | "struct_shorthand" => Some(42),
        "factorial" | "const_factorial" => Some(120),
        "math" => Some(42),
        "nth_prime" => Some(229),
        "call_stack" => Some(49),
        "mixed_stack_args" => Some(42),
        "mandelbrot" => Some(232),
        "if_conversion" => Some(75),
        "binary_search" => Some(11),
        "target_dependent" => Some(77),
        "loops" => Some(165),
        "inlined_add" => Some(3),
        "inline_complex" => Some(38),
        "inline_methods" => Some(30),
        "overflow" | "mul_overflow" | "narrow_overflow" | "array_oob_panic" | "slice_oob_panic" => {
            Some(101)
        },
        "string_len" => Some(11),
        "array_features" => Some(36),
        "array_sorting" | "slice_mut" => Some(12),
        "slice_basics" => Some(22),
        "slice_std_methods" => Some(19),
        _ => Some(0),
    };

    let stdout = match name {
        "hello_world" => "hello, world!\nJohn Doe is 42 years old!",
        "modules" => "Initialising...\nDone.\n",
        _ => "",
    };
    (exit_code, stdout)
}

fn compile_and_assemble(path: &Path) -> Result<PathBuf, String> {
    compile_and_assemble_at(path, Level::Debug)
}

fn compile_and_assemble_at(path: &Path, level: Level) -> Result<PathBuf, String> {
    let project = path.file_stem().unwrap_or(path.as_os_str()).to_string_lossy().to_string();

    backend::optimisation::set(level);
    let compiled = backend::compile_project(path, &project);
    backend::optimisation::set(Level::Debug);

    let asm = compiled.map_err(|e| e.to_string())?;

    let temp_dir = std::env::temp_dir();
    let test_name = format!("{}-{level:?}", path.file_stem().unwrap().to_string_lossy());

    let asm_path = temp_dir.join(format!("{test_name}.s"));
    let obj_path = temp_dir.join(format!("{test_name}.o"));
    fs::write(&asm_path, &asm).map_err(|e| format!("failed to write assembly: {e}"))?;

    let as_status = Command::new("as")
        .args(["-o", obj_path.to_str().unwrap(), asm_path.to_str().unwrap()])
        .status()
        .map_err(|e| format!("`as` failed: {e}"))?;

    fs::remove_file(&asm_path).ok();

    if !as_status.success() {
        fs::remove_file(&obj_path).ok();
        return Err(format!("`as` exited with code {}", as_status.code().unwrap_or(-1)));
    }

    Ok(obj_path)
}

fn compile_and_run(path: &Path) -> Result<(i32, String), String> {
    compile_and_run_at(path, Level::Debug)
}

fn compile_and_run_at(path: &Path, level: Level) -> Result<(i32, String), String> {
    let obj_path = compile_and_assemble_at(path, level)?;
    let test_name = format!("{}-{level:?}", path.file_stem().unwrap().to_string_lossy());
    let temp_dir = std::env::temp_dir();
    let exe_path = temp_dir.join(format!("{test_name}.test"));

    let ld_status = Command::new("ld")
        .args(["-o", exe_path.to_str().unwrap(), obj_path.to_str().unwrap()])
        .status()
        .map_err(|e| format!("`ld` failed: {e}"))?;

    fs::remove_file(&obj_path).ok();

    if !ld_status.success() {
        return Err(format!("`ld` exited with code {}", ld_status.code().unwrap_or(-1)));
    }

    let output = Command::new(&exe_path)
        .output()
        .map_err(|e| format!("failed to run executable: {e}"))?;

    fs::remove_file(&exe_path).ok();

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("program printed invalid UTF-8: {error}"))?;
    Ok((output.status.code().unwrap_or(-1), stdout))
}

#[test]
fn run_integration_tests() {
    let mut passed = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for test in cases() {
        match test.exit_code {
            Some(expected_code) => match compile_and_run(&test.file) {
                Ok((code, stdout)) if code == expected_code && stdout == test.stdout => {
                    passed += 1;
                    println!("{}: exit code {}", test.name, code);
                },

                Ok((code, stdout)) => {
                    failed += 1;

                    let msg = format!(
                        "{}: expected exit code {} and stdout {:?}, got {} and {:?}",
                        test.name, expected_code, test.stdout, code, stdout
                    );

                    eprintln!("{msg}");
                    errors.push(msg);
                },

                Err(err) => {
                    failed += 1;

                    let msg = format!("{}: {err}", test.name);
                    println!("{msg}");
                    errors.push(msg);
                },
            },

            None => match compile_and_assemble(&test.file) {
                Ok(obj_path) => {
                    passed += 1;
                    fs::remove_file(&obj_path).ok();

                    println!("{}: compiles", test.name);
                },

                Err(err) => {
                    failed += 1;
                    let msg = format!("{}: {err}", test.name);
                    eprintln!("{msg}");

                    errors.push(msg);
                },
            },
        }
    }

    println!("\n{} passed, {} failed", passed, failed);
    if !errors.is_empty() {
        panic!("\nIntegration test failures:\n{}", errors.join("\n"));
    }
}

/// `qemu-user` installs the emulator unsuffixed, `qemu-user-static` suffixes it
fn qemu() -> Option<&'static str> {
    ["qemu-aarch64", "qemu-aarch64-static"]
        .into_iter()
        .find(|binary| Command::new(binary).arg("--version").status().is_ok())
}

#[test]
fn run_aarch64_integration_tests() {
    let Some(qemu) = qemu() else {
        println!("qemu-aarch64 not found, skipping aarch64 integration tests");
        return;
    };
    if Command::new("aarch64-linux-gnu-as").arg("--version").status().is_err() {
        println!("aarch64-linux-gnu-as not found, skipping aarch64 integration tests");
        return;
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for test in cases() {
        let src = &test.file;
        let project = src.file_stem().unwrap().to_string_lossy().to_string();

        let compile_res = (|| -> Result<(i32, String), String> {
            let asm = backend::compile_project_for(src, &project, backend::TargetArch::AArch64)
                .map_err(|e| e.to_string())?;

            let temp_dir = std::env::temp_dir();
            let asm_path = temp_dir.join(format!("{}_aarch64.s", test.name));
            let obj_path = temp_dir.join(format!("{}_aarch64.o", test.name));
            let exe_path = temp_dir.join(format!("{}_aarch64.test", test.name));

            fs::write(&asm_path, &asm).map_err(|e| format!("failed to write assembly: {e}"))?;

            let as_status = Command::new("aarch64-linux-gnu-as")
                .args(["-o", obj_path.to_str().unwrap(), asm_path.to_str().unwrap()])
                .status()
                .map_err(|e| format!("aarch64-linux-gnu-as failed: {e}"))?;

            fs::remove_file(&asm_path).ok();

            if !as_status.success() {
                return Err(format!(
                    "aarch64-linux-gnu-as exited with code {}",
                    as_status.code().unwrap_or(-1)
                ));
            }

            if test.exit_code.is_none() {
                fs::remove_file(&obj_path).ok();
                return Ok((0, String::new()));
            }

            let ld_status = Command::new("aarch64-linux-gnu-ld")
                .args(["-o", exe_path.to_str().unwrap(), obj_path.to_str().unwrap()])
                .status()
                .map_err(|e| format!("aarch64-linux-gnu-ld failed: {e}"))?;

            fs::remove_file(&obj_path).ok();

            if !ld_status.success() {
                return Err(format!(
                    "aarch64-linux-gnu-ld exited with code {}",
                    ld_status.code().unwrap_or(-1)
                ));
            }

            if let Some(expected_code) = test.exit_code {
                let output = Command::new(qemu)
                    .arg(&exe_path)
                    .output()
                    .map_err(|e| format!("{qemu} failed to run: {e}"))?;

                fs::remove_file(&exe_path).ok();

                let code = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8(output.stdout)
                    .map_err(|error| format!("program printed invalid UTF-8: {error}"))?;

                match code == expected_code && stdout == test.stdout {
                    true => Ok((code, stdout)),
                    _ => Err(format!(
                        "expected exit code {} and stdout {:?}, got {} and {:?}",
                        expected_code, test.stdout, code, stdout
                    )),
                }
            } else {
                fs::remove_file(&exe_path).ok();
                Ok((0, String::new()))
            }
        })();

        match compile_res {
            Ok((code, _)) => {
                passed += 1;
                println!("{}: passed (exit code {})", test.name, code);
            },
            Err(err) => {
                failed += 1;
                let msg = format!("{}: {}", test.name, err);
                eprintln!("{msg}");
                errors.push(msg);
            },
        }
    }

    println!("\nAArch64: {} passed, {} failed", passed, failed);
    if !errors.is_empty() {
        panic!("\nAArch64 Integration test failures:\n{}", errors.join("\n"));
    }
}

/// Fixtures whose result legitimately depends on the optimisation level, with what they
/// are expected to produce above `debug`
const LEVEL_DEPENDENT: &[(&str, i32)] =
    &[("overflow", 0), ("mul_overflow", 0), ("narrow_overflow", 0)];

#[test]
fn optimisation_levels_agree_with_debug() {
    let mut errors = Vec::new();
    let mut checked = 0;

    for test in cases() {
        let Some(baseline) = test.exit_code else {
            continue;
        };

        let expected = LEVEL_DEPENDENT
            .iter()
            .find(|(name, _)| *name == test.name)
            .map_or(baseline, |(_, code)| *code);

        for level in [Level::Sane, Level::Max] {
            match compile_and_run_at(&test.file, level) {
                Ok((code, stdout)) if code == expected && stdout == test.stdout => checked += 1,
                Ok((code, stdout)) => errors.push(
                    format!(
                        "{} at {level:?}: expected exit code {expected}, got {code}",
                        test.name
                    ) + &format!("; expected stdout {:?}, got {:?}", test.stdout, stdout),
                ),
                Err(err) => errors.push(format!("{} at {level:?}: {err}", test.name)),
            }
        }
    }

    println!("\noptimisation levels: {checked} runs agreed with debug");
    assert!(errors.is_empty(), "\noptimised runs diverged:\n{}", errors.join("\n"));
}
