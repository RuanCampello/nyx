use rstest::rstest;
use std::{fs, path::Path, process::Command};

fn compile_and_run(entry: &Path, project_name: &str) -> Result<i32, String> {
    let asm = backend::compile_project(entry, project_name).map_err(|e| e.to_string())?;

    let temp_dir = std::env::temp_dir();
    let test_name = project_name.replace("-", "_");

    let asm_path = temp_dir.join(format!("{test_name}_mod.s"));
    let obj_path = temp_dir.join(format!("{test_name}_mod.o"));
    let exe_path = temp_dir.join(format!("{test_name}_mod.test"));

    fs::write(&asm_path, &asm).map_err(|e| format!("failed to write assembly: {e}"))?;

    let assemble = backend::assemble(&asm_path, &obj_path).map_err(|e| e.to_string());
    fs::remove_file(&asm_path).ok();
    assemble?;

    let link = backend::link(&obj_path, &exe_path, &[]).map_err(|e| e.to_string());
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
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| format!("failed to infer project name for {}", entry.display()))
}

#[rstest]
#[case::simple("tests/module/simple/main.nyx", 42)]
#[case::geometry("tests/module/geometry/main.nyx", 0)]
#[case::composable_interfaces("tests/module/composable_interfaces/main.nyx", 0)]
fn project_compiles_and_runs(#[case] entry: &str, #[case] expected: i32) {
    let entry = Path::new(entry);
    let project = project_name(entry).unwrap_or_else(|err| panic!("{err}"));

    match compile_and_run(entry, &project) {
        Ok(code) => assert_eq!(code, expected, "exit code"),
        Err(err) => panic!("{err}"),
    }
}
