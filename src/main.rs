use clap::{Parser, Subcommand};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

/// the nyx compiler
#[derive(Parser)]
#[command(
    name = "nyx",
    version,
    about = "A modern, strict, compiled programming language",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// compile a nyx source file to a native executable
    Build {
        /// path to the `.nyx` source file
        file: PathBuf,

        /// output executable path (defaults to the source file stem)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// compile a nyx source file and immediately run it
    Run {
        /// path to the `.nyx` source file.
        file: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    let exit_code = match cli.command {
        Commands::Build { file, output } => cmd_build(&file, output.as_deref()),
        Commands::Run { file } => cmd_run(&file),
    };

    process::exit(exit_code);
}

fn cmd_build(source: &Path, output: Option<&Path>) -> i32 {
    let exe = output
        .map(PathBuf::from)
        .unwrap_or_else(|| default_output(source));

    match build_to(source, &exe) {
        Ok(()) => {
            eprintln!("Built {}", exe.display());
            0
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}

fn cmd_run(source: &Path) -> i32 {
    let exe = temp_exe_path(source);

    let result = (|| -> Result<i32, String> {
        build_to(source, &exe)?;

        let status = Command::new(&exe)
            .status()
            .map_err(|e| format!("failed to run `{}`: {e}", exe.display()))?;

        Ok(status.code().unwrap_or(1))
    })();

    let _ = fs::remove_file(&exe);

    match result {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}

/// full compile → assemble → link pipeline
/// produces a native ELF executable at `output`
fn build_to(source: &Path, output: &Path) -> Result<(), String> {
    // read source
    let src = fs::read_to_string(source)
        .map_err(|e| format!("cannot read `{}`: {e}", source.display()))?;

    // compile source → GAS assembly
    let asm = nyx::compile(&src).map_err(|e| e.message)?;

    // write assembly to a temp `.s` file
    let asm_path = output.with_extension("s");
    fs::write(&asm_path, &asm)
        .map_err(|e| format!("cannot write `{}`: {e}", asm_path.display()))?;

    // assemble: as -o <obj> <asm>
    let obj_path = output.with_extension("o");
    let as_status = Command::new("as")
        .args(["-o", obj_path.to_str().unwrap(), asm_path.to_str().unwrap()])
        .status()
        .map_err(|e| format!("`as` not found — is binutils installed? ({e})"))?;

    fs::remove_file(&asm_path).ok();

    if !as_status.success() {
        fs::remove_file(&obj_path).ok();
        return Err(format!(
            "`as` exited with code {}",
            as_status.code().unwrap_or(-1)
        ));
    }

    // link: ld -o <exe> <obj>
    let ld_status = Command::new("ld")
        .args(["-o", output.to_str().unwrap(), obj_path.to_str().unwrap()])
        .status()
        .map_err(|e| format!("`ld` not found — is binutils installed? ({e})"))?;

    fs::remove_file(&obj_path).ok();

    if !ld_status.success() {
        return Err(format!(
            "`ld` exited with code {}",
            ld_status.code().unwrap_or(-1)
        ));
    }

    Ok(())
}

#[inline(always)]
fn default_output(source: &Path) -> PathBuf {
    source.with_extension("")
}

#[inline(always)]
fn temp_exe_path(source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .unwrap_or_else(|| source.as_os_str())
        .to_string_lossy();

    source
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{stem}.run.tmp"))
}
