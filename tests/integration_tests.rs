use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct Case<'c> {
    name: &'c str,
    file: &'c str,
    exit_code: Option<i32>,
}

const CASES: &[Case] = &[
    Case { name: "add", file: "tests/single/add.nyx", exit_code: None },
    Case {
        name: "inference",
        file: "tests/single/inference.nyx",
        exit_code: None,
    },
    Case {
        name: "fibonacci",
        file: "tests/single/fibonacci.nyx",
        exit_code: Some(55),
    },
    Case {
        name: "collatz",
        file: "tests/single/collatz.nyx",
        exit_code: Some(111),
    },
    Case {
        name: "factorial",
        file: "tests/single/factorial.nyx",
        exit_code: Some(120),
    },
    Case {
        name: "math",
        file: "tests/single/math.nyx",
        exit_code: Some(42),
    },
    Case {
        name: "nth_prime",
        file: "tests/single/nth_prime.nyx",
        exit_code: Some(229),
    },
    Case {
        name: "add_with_main",
        file: "tests/single/add_with_main.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "call_stack",
        file: "tests/single/call_stack.nyx",
        exit_code: Some(49),
    },
    Case {
        name: "mandelbrot",
        file: "tests/single/mandelbrot.nyx",
        exit_code: Some(232),
    },
    Case {
        name: "binary_search",
        file: "tests/single/binary_search.nyx",
        exit_code: Some(11),
    },
    Case {
        name: "floats",
        file: "tests/single/floats.nyx",
        exit_code: Some(42),
    },
    Case {
        name: "exit",
        file: "tests/single/exit.nyx",
        exit_code: Some(42),
    },
    Case {
        name: "hello_world",
        file: "tests/single/hello_world.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "target_dependent",
        file: "tests/single/target_dependent.nyx",
        exit_code: Some(77),
    },
    Case {
        name: "basic_struct",
        file: "tests/single/basic_struct.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "nested_structs",
        file: "tests/single/nested_structs.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "impl_methods",
        file: "tests/single/impl_methods.nyx",
        exit_code: Some(42),
    },
    Case {
        name: "inlined_add",
        file: "tests/single/inlined_add.nyx",
        exit_code: Some(3),
    },
    Case {
        name: "inline_complex",
        file: "tests/single/inline_complex.nyx",
        exit_code: Some(38),
    },
    Case {
        name: "inline_methods",
        file: "tests/single/inline_methods.nyx",
        exit_code: Some(30),
    },
    Case {
        name: "interfaces",
        file: "tests/single/interfaces.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "char_tests",
        file: "tests/single/char_tests.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "regalloc_terminator",
        file: "tests/single/regalloc_terminator.nyx",
        exit_code: Some(42),
    },
    Case {
        name: "div_sizes",
        file: "tests/single/div_sizes.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "bitwise",
        file: "tests/single/bitwise.nyx",
        exit_code: Some(42),
    },
    Case {
        name: "signum",
        file: "tests/single/signum.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "cast",
        file: "tests/single/cast.nyx",
        exit_code: Some(42),
    },
    Case {
        name: "char_ext_tests",
        file: "tests/single/char_ext_tests.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "iptr_uptr_tests",
        file: "tests/single/iptr_uptr_tests.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "std_interfaces",
        file: "tests/single/std_interfaces.nyx",
        exit_code: Some(0),
    },
    Case {
        name: "overflow",
        file: "tests/single/overflow.nyx",
        exit_code: Some(1),
    },
];

fn compile_and_assemble(path: &Path) -> Result<PathBuf, String> {
    let project = path
        .file_stem()
        .unwrap_or_else(|| path.as_os_str())
        .to_string_lossy()
        .to_string();
    let asm = nyx::compile_project(path, &project).map_err(|e| e.to_string())?;

    let temp_dir = std::env::temp_dir();
    let test_name = path.file_stem().unwrap().to_string_lossy().to_string();

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

fn compile_and_run(path: &Path) -> Result<i32, String> {
    let obj_path = compile_and_assemble(path)?;
    let test_name = path.file_stem().unwrap().to_string_lossy().to_string();
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

    let run_status = Command::new(&exe_path)
        .status()
        .map_err(|e| format!("failed to run executable: {e}"))?;

    fs::remove_file(&exe_path).ok();

    Ok(run_status.code().unwrap_or(-1))
}

#[test]
fn run_integration_tests() {
    let mut passed = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for test in CASES {
        let src = PathBuf::from(test.file);

        match test.exit_code {
            Some(expected_code) => match compile_and_run(&src) {
                Ok(code) if code == expected_code => {
                    passed += 1;
                    println!("{}: exit code {}", test.name, code);
                },

                Ok(code) => {
                    failed += 1;

                    let msg = format!(
                        "{}: expected exit code {}, got {}",
                        test.name, expected_code, code
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

            None => match compile_and_assemble(&src) {
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

#[test]
fn run_aarch64_integration_tests() {
    if Command::new("qemu-aarch64").arg("--version").status().is_err() {
        println!("qemu-aarch64 not found, skipping aarch64 integration tests");
        return;
    }
    if Command::new("aarch64-linux-gnu-as").arg("--version").status().is_err() {
        println!("aarch64-linux-gnu-as not found, skipping aarch64 integration tests");
        return;
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for test in CASES {
        let src = PathBuf::from(test.file);
        let project = src.file_stem().unwrap().to_string_lossy().to_string();

        let compile_res = (|| -> Result<i32, String> {
            let asm = nyx::compile_project_for(&src, &project, nyx::TargetArch::AArch64)
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
                let run_status = Command::new("qemu-aarch64")
                    .arg(&exe_path)
                    .status()
                    .map_err(|e| format!("qemu-aarch64 failed to run: {e}"))?;

                fs::remove_file(&exe_path).ok();

                let code = run_status.code().unwrap_or(-1);
                match code == expected_code {
                    true => Ok(code),
                    _ => Err(format!("expected exit code {}, got {}", expected_code, code)),
                }
            } else {
                fs::remove_file(&exe_path).ok();
                Ok(0)
            }
        })();

        match compile_res {
            Ok(code) => {
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
