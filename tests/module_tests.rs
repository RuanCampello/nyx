use nyx::compile_project;
use std::{fs, path::PathBuf, process::Command};

fn compile_and_run(entry: &str, project_name: &str) -> Result<i32, String> {
    let entry_path = PathBuf::from(entry);
    let asm = compile_project(&entry_path, project_name).map_err(|e| e.to_string())?;

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
