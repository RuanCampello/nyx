use nyx::compile_project;
use std::{fs, path::PathBuf, process::Command};

fn compile_and_run(entry: &str, project_name: &str) -> Result<i32, String> {
    let entry_path = PathBuf::from(entry);
    let asm = compile_project(&entry_path, project_name).map_err(|e| e.to_string())?;

    let temp_dir = std::env::temp_dir();
    let test_name = entry_path.file_stem().unwrap().to_string_lossy().to_string();

    let asm_path = temp_dir.join(format!("{test_name}_mod.s"));
    let obj_path = temp_dir.join(format!("{test_name}_mod.o"));
    let exe_path = temp_dir.join(format!("{test_name}_mod.test"));

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
