use nyx;
use std::{fs, path::Path, process::Command};

struct Case<'c> {
    name: &'c str,
    entry: &'c str,
    exit_code: i32,
}

const CASES: &[Case] = &[Case {
    name: "simple",
    entry: "tests/module/simple/main.nyx",
    exit_code: 42,
}];

fn compile_and_run(entry: &Path, project_name: &str) -> Result<i32, String> {
    let asm = nyx::compile_project(entry, project_name).map_err(|e| e.to_string())?;

    let temp_dir = std::env::temp_dir();
    let test_name = project_name.replace("-", "_");

    let asm_path = temp_dir.join(format!("{test_name}_mod.s"));
    let obj_path = temp_dir.join(format!("{test_name}_mod.o"));
    let exe_path = temp_dir.join(format!("{test_name}_mod.test"));

    fs::write(&asm_path, &asm).map_err(|e| format!("failed to write assembly: {e}"))?;

    let assemble = nyx::assemble(&asm_path, &obj_path).map_err(|e| e.to_string());
    fs::remove_file(&asm_path).ok();
    assemble?;

    let link = nyx::link(&obj_path, &exe_path, &[]).map_err(|e| e.to_string());
    fs::remove_file(&obj_path).ok();
    link?;

    let run_status = Command::new(&exe_path)
        .status()
        .map_err(|e| format!("failed to run executable: {e}"))?;

    fs::remove_file(&exe_path).ok();

    Ok(run_status.code().unwrap_or(-1))
}

fn project_name(entry: &Path) -> Result<String, String> {
    entry
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .ok_or_else(|| format!("failed to infer project name for {}", entry.display()))
}

#[test]
fn run_module_tests() {
    let mut passed = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for test in CASES {
        let entry = Path::new(test.entry);

        let project = match project_name(&entry) {
            Ok(name) => name,
            Err(err) => {
                failed += 1;
                errors.push(format!("{}: {}", test.name, err));

                continue;
            }
        };

        match compile_and_run(&entry, &project) {
            Ok(code) if code == test.exit_code => {
                passed += 1;
                println!("{}: exit code {}", test.name, code);
            }

            Ok(code) => {
                failed += 1;
                let msg = format!(
                    "{}: expected exit code {} but got {}",
                    test.name, test.exit_code, code
                );
                errors.push(msg);
            }

            Err(err) => {
                failed += 1;
                let msg = format!("{}: {}", test.name, err);
                eprintln!("{msg}");
                errors.push(msg);
            }
        }
    }

    println!("\n{passed} passed, {failed} failed");
    if !errors.is_empty() {
        panic!("Module test failures:\n{}", errors.join("\n"))
    }
}
