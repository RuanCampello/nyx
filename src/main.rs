use clap::{Parser, Subcommand, ValueEnum};
use nyx::NyxError;
use std::{
    collections::HashSet,
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
    /// Compile a nyx source file to a native executable
    Build {
        /// path to the `.nyx` source file
        file: PathBuf,

        /// output executable path (defaults to the source file stem)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Comma-separated list of outputs to emit
        ///
        /// Values: asm, obj, link (default: link)
        ///
        /// Examples:
        ///   --emit asm          — write AT&T assembly (.s)
        ///   --emit obj          — write object file (.o)
        ///   --emit asm,obj,link — write all three
        #[arg(long, value_delimiter = ',', value_name = "TYPES")]
        emit: Vec<Emit>,
    },

    /// Compile a nyx source file and immediately run it
    Run {
        /// path to the `.nyx` source file.
        file: PathBuf,
    },
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, ValueEnum)]
enum Emit {
    /// AT&T assembly source (.s)
    Asm,
    /// ELF object file (.o)
    Obj,
    /// Linked native executable (default)
    Link,
}

fn main() -> Result<(), NyxError> {
    let cli = Cli::parse();

    let exit_code = match cli.command {
        Commands::Build { file, output, emit } => cmd_build(&file, output.as_deref(), &emit)?,
        Commands::Run { file } => cmd_run(&file)?,
    };

    process::exit(exit_code);
}

fn cmd_build(source: &Path, output: Option<&Path>, emit: &[Emit]) -> Result<i32, NyxError> {
    let exe = output.map(PathBuf::from).unwrap_or_else(|| source.with_extension(""));
    let kinds = match emit.is_empty() {
        true => HashSet::from([Emit::Link]),
        _ => emit.iter().copied().collect(),
    };

    let emitted = build_emit(source, &exe, &kinds)?;
    for path in emitted {
        println!("Emmitted: {}", path.display());
    }

    Ok(0)
}

fn cmd_run(source: &Path) -> Result<i32, NyxError> {
    let exe = temp_exe_path(source);

    let result = (|| -> Result<i32, NyxError> {
        build_emit(source, &exe, &HashSet::from([Emit::Link]))?;

        let status = Command::new(&exe).status().map_err(|e| NyxError::ToolNotFound(e.to_string()))?;

        Ok(status.code().unwrap_or(1))
    })();

    let _ = fs::remove_file(&exe);
    result
}

/// Emits whichever outputs [kinds](self::Emit) requests.
fn build_emit(source: &Path, stem: &Path, kinds: &HashSet<Emit>) -> Result<Vec<PathBuf>, NyxError> {
    // read and compile source
    let src = fs::read_to_string(source)?;
    let asm = nyx::compile(&src)?;

    let mut emitted = Vec::new();

    // write assembly to a temp `.s` file
    let asm_path = stem.with_extension("s");
    let keep_asm = kinds.contains(&Emit::Asm);

    fs::write(&asm_path, &asm)?;

    if keep_asm {
        emitted.push(asm_path.clone());
    }

    // assemble: as -o <obj> <asm>
    let obj_path = stem.with_extension("o");
    let keep_obj = kinds.contains(&Emit::Obj);

    let as_status = Command::new("as")
        .args(["-o", obj_path.to_str().unwrap(), asm_path.to_str().unwrap()])
        .status()
        .map_err(|e| NyxError::ToolNotFound(e.to_string()))?;

    if !keep_asm {
        fs::remove_file(&asm_path).ok();
    }

    if !as_status.success() {
        fs::remove_file(&obj_path).ok();
        return Err(NyxError::Assembler(as_status.code().unwrap_or(-1)));
    }

    if keep_obj {
        emitted.push(obj_path.clone());
    }

    if !kinds.contains(&Emit::Link) {
        return Ok(emitted);
    }

    let exe_path = stem.with_extension("");

    // link: ld -o <exe> <obj>
    let ld_status = Command::new("ld")
        .args(["-o", stem.to_str().unwrap(), obj_path.to_str().unwrap()])
        .status()
        .map_err(|e| NyxError::ToolNotFound(e.to_string()))?;

    fs::remove_file(&obj_path).ok();

    if !ld_status.success() {
        return Err(NyxError::Linker(ld_status.code().unwrap_or(-1)));
    }

    emitted.push(exe_path);
    Ok(emitted)
}

#[inline(always)]
fn temp_exe_path(source: &Path) -> PathBuf {
    let stem = source.file_stem().unwrap_or_else(|| source.as_os_str()).to_string_lossy();

    source.parent().unwrap_or(Path::new(".")).join(format!("{stem}.run.tmp"))
}
