use backend::optimisation::Level;
use rstest::rstest;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

/// fixtures whose result legitimately depends on the optimisation level, with
/// what they are expected to produce above `debug`
const LEVEL_DEPENDENT: &[(&str, i32)] =
    &[("overflow", 0), ("mul_overflow", 0), ("narrow_overflow", 0)];

/// what a fixture is expected to produce, keyed by its file stem
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
        | "struct_shorthand"
        | "compound_assign"
        | "match_control_flow"
        | "try_operator"
        | "expression_blocks"
        | "interpolation"
        | "unsigned_division"
        | "remainder"
        | "compact_forms" => Some(42),
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
        "interpolation" => {
            "hello ruan with 43\n\
             floor=-9223372036854775808 ceiling=18446744073709551615\n\
             call 40 index 9 unsigned 12345678901234\n\
             bool true char x negative -7\n\
             literal braces { } stay put\n\
             nested 1\n\
             no trailing newline\n"
        },
        _ => "",
    };
    (exit_code, stdout)
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .expect("fixture must have a stem")
        .to_string_lossy()
        .into_owned()
}

fn compile_and_assemble_at(path: &Path, level: Level) -> Result<PathBuf, String> {
    let project = stem(path);

    backend::optimisation::set(level);
    let compiled = backend::compile_project(path, &project);
    backend::optimisation::set(Level::Debug);

    let asm = compiled.map_err(|e| e.to_string())?;

    let temp_dir = std::env::temp_dir();
    let test_name = format!("{project}-{level:?}");

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

fn compile_and_run_at(path: &Path, level: Level) -> Result<(i32, String), String> {
    let obj_path = compile_and_assemble_at(path, level)?;
    let test_name = format!("{}-{level:?}", stem(path));
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

#[rstest]
fn fixture_runs_on_host(#[files("tests/single/*.nyx")] file: PathBuf) {
    let (exit_code, expected_stdout) = expectation(&stem(&file));

    let Some(expected_code) = exit_code else {
        let object = compile_and_assemble_at(&file, Level::Debug)
            .unwrap_or_else(|err| panic!("compile-only fixture failed: {err}"));
        fs::remove_file(&object).ok();

        return;
    };

    match compile_and_run_at(&file, Level::Debug) {
        Ok((code, stdout)) => {
            assert_eq!(code, expected_code, "exit code");
            assert_eq!(stdout, expected_stdout, "stdout");
        },
        Err(err) => panic!("{err}"),
    }
}

#[rstest]
fn optimisation_level_agrees_with_debug(
    #[files("tests/single/*.nyx")] file: PathBuf,
    #[values(Level::Sane, Level::Max)] level: Level,
) {
    let name = stem(&file);
    let (exit_code, expected_stdout) = expectation(&name);

    let Some(baseline) = exit_code else {
        return;
    };

    let expected = LEVEL_DEPENDENT
        .iter()
        .find(|(fixture, _)| *fixture == name)
        .map_or(baseline, |(_, code)| *code);

    match compile_and_run_at(&file, level) {
        Ok((code, stdout)) => {
            assert_eq!(code, expected, "exit code at {level:?}");
            assert_eq!(stdout, expected_stdout, "stdout at {level:?}");
        },
        Err(err) => panic!("{err}"),
    }
}

/// `qemu-user` installs the emulator unsuffixed, `qemu-user-static` suffixes it
fn qemu() -> Option<&'static str> {
    static FOUND: OnceLock<Option<&'static str>> = OnceLock::new();

    *FOUND.get_or_init(|| {
        ["qemu-aarch64", "qemu-aarch64-static"]
            .into_iter()
            .find(|binary| Command::new(binary).arg("--version").status().is_ok())
    })
}

fn cross_assembler() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();

    *FOUND.get_or_init(|| Command::new("aarch64-linux-gnu-as").arg("--version").status().is_ok())
}

#[rstest]
fn fixture_runs_on_aarch64(#[files("tests/single/*.nyx")] file: PathBuf) {
    let (Some(qemu), true) = (qemu(), cross_assembler()) else {
        eprintln!("aarch64 cross toolchain not found, skipping");
        return;
    };

    let name = stem(&file);
    let (exit_code, expected_stdout) = expectation(&name);

    let cross_compile = || -> Result<(i32, String), String> {
        let asm = backend::compile_project_for(&file, &name, backend::TargetArch::AArch64)
            .map_err(|e| e.to_string())?;

        let temp_dir = std::env::temp_dir();
        let asm_path = temp_dir.join(format!("{name}_aarch64.s"));
        let obj_path = temp_dir.join(format!("{name}_aarch64.o"));
        let exe_path = temp_dir.join(format!("{name}_aarch64.test"));

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

        if exit_code.is_none() {
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

        let output = Command::new(qemu)
            .arg(&exe_path)
            .output()
            .map_err(|e| format!("{qemu} failed to run: {e}"))?;

        fs::remove_file(&exe_path).ok();

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("program printed invalid UTF-8: {error}"))?;

        Ok((output.status.code().unwrap_or(-1), stdout))
    };

    match (cross_compile(), exit_code) {
        (Ok((code, stdout)), Some(expected)) => {
            assert_eq!(code, expected, "exit code");
            assert_eq!(stdout, expected_stdout, "stdout");
        },
        (Err(err), _) => panic!("{err}"),
        _ => {},
    }
}
