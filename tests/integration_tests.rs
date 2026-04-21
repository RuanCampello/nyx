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
    Case {
        name: "add",
        file: "tests/add.nyx",
        exit_code: None,
    },
    Case {
        name: "inference",
        file: "tests/inference.nyx",
        exit_code: None,
    },
    Case {
        name: "fibonacci",
        file: "tests/fibonacci.nyx",
        exit_code: Some(55),
    },
    Case {
        name: "collatz",
        file: "tests/collatz.nyx",
        exit_code: Some(111),
    },
    Case {
        name: "factorial",
        file: "tests/factorial.nyx",
        exit_code: Some(120),
    },
    Case {
        name: "math",
        file: "tests/math.nyx",
        exit_code: Some(42),
    },
    Case {
        name: "binary_search",
        file: "tests/binary_search.nyx",
        exit_code: Some(11),
    },
];

fn compile_and_assemble(path: &Path) -> Result<PathBuf, String> {
    let src = fs::read_to_string(path).map_err(|e| format!("failed to read source: {e}"))?;
    let asm = nyx::compile(&src).map_err(|e| e.message)?;

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
        return Err(format!(
            "`as` exited with code {}",
            as_status.code().unwrap_or(-1)
        ));
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
        return Err(format!(
            "`ld` exited with code {}",
            ld_status.code().unwrap_or(-1)
        ));
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
                }

                Ok(code) => {
                    failed += 1;

                    let msg = format!(
                        "{}: expected exit code {}, got {}",
                        test.name, expected_code, code
                    );

                    eprintln!("{msg}");
                    errors.push(msg);
                }

                Err(err) => {
                    failed += 1;

                    let msg = format!("{}: {err}", test.name);
                    println!("{msg}");
                    errors.push(msg);
                }
            },

            None => match compile_and_assemble(&src) {
                Ok(obj_path) => {
                    passed += 1;
                    fs::remove_file(&obj_path).ok();

                    println!("{}: compiles", test.name);
                }

                Err(err) => {
                    failed += 1;
                    let msg = format!("{}: {err}", test.name);
                    eprintln!("{msg}");

                    errors.push(msg);
                }
            },
        }
    }

    println!("\n{} passed, {} failed", passed, failed);
    if !errors.is_empty() {
        panic!("\nIntegration test failures:\n{}", errors.join("\n"));
    }
}
