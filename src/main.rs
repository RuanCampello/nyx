use clap::{Parser, Subcommand, ValueEnum};
use nyx::{NyxError, TargetArch};
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
    /// Compile a nyx source file or project to a native executable
    ///
    /// When no file is given, looks for `main.nyx` in the current directory
    /// and compiles the whole project
    Build {
        /// Path to the `.nyx` source file or the module entry point
        ///
        /// - Single file:  `nyx build file.nyx`
        /// - Project dir:  `nyx build ./my_project/`  (looks for `main.nyx` inside)
        /// - Omitted:      builds the current directory (looks for `main.nyx` here)
        path: Option<PathBuf>,

        /// Override the default entry filename inside a project directory.
        ///
        /// Defaults to `main.nyx`. Ignored when `path` is a `.nyx` file.
        #[arg(long, value_name = "FILE", default_value = "main.nyx")]
        entry: String,

        /// Output executable path (defaults to the source file stem)
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

        /// Override the project name used in `use` path resolution
        ///
        /// Defaults to the entry file's parent directory name.
        #[arg(long, value_name = "NAME")]
        project: Option<String>,

        /// Target architecture for code generation
        ///
        /// Defaults to the host architecture.
        /// Values: x86_64, aarch64 (aliases: x86-64, arm64)
        #[arg(long, value_name = "ARCH")]
        target: Option<String>,
    },

    /// Compile a nyx source file or project and immediately run it
    ///
    /// When no file is given, looks for `main.nyx` in the current directory.
    Run {
        /// Path to a `.nyx` source file or a project directory.
        ///
        /// - Single file:  `nyx run file.nyx`
        /// - Project dir:  `nyx run ./my_project/`
        /// - Omitted:      runs the current directory        
        path: Option<PathBuf>,

        /// Override the default entry filename inside a project directory.
        ///
        /// Defaults to `main.nyx`. Ignored when `path` is a `.nyx` file.
        #[arg(long, value_name = "FILE", default_value = "main.nyx")]
        entry: String,

        /// Override the project name used in `use` path resolution.
        /// Defaults to the entry file's parent directory name.
        #[arg(long, value_name = "NAME")]
        project: Option<String>,
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

    let result = match cli.command {
        Commands::Build { path, entry, output, emit, project, target } => {
            let entry = resolve_entry(path, &entry)?;
            let name = resolve_project_name(&entry, project);
            let arch = resolve_target(target)?;

            cmd_build(&entry, output.as_deref(), &emit, &name, arch)
        }

        Commands::Run { path, entry, project } => {
            let entry = resolve_entry(path, &entry)?;
            let name = resolve_project_name(&entry, project);

            cmd_run(&entry, &name)
        }
    };

    match result {
        Ok(exit_code) => process::exit(exit_code),
        Err(err) => eprintln!("{err}"),
    };

    process::exit(1)
}

fn cmd_build(
    entry: &Path,
    output: Option<&Path>,
    emit: &[Emit],
    project: &str,
    target: TargetArch,
) -> Result<i32, NyxError> {
    let exe = output.map(PathBuf::from).unwrap_or_else(|| entry.with_extension(""));
    let kinds = match emit.is_empty() {
        true => HashSet::from([Emit::Link]),
        _ => emit.iter().copied().collect(),
    };

    let emitted = build_emit(entry, &exe, &kinds, project, target)?;
    for path in emitted {
        println!("Emmitted: {}", path.display());
    }

    Ok(0)
}

fn cmd_run(entry: &Path, project: &str) -> Result<i32, NyxError> {
    let exe = temp_exe_path(entry);
    let target = TargetArch::host();

    let result = (|| -> Result<i32, NyxError> {
        build_emit(entry, &exe, &HashSet::from([Emit::Link]), project, target)?;

        let status =
            Command::new(&exe).status().map_err(|e| NyxError::ToolNotFound(e.to_string()))?;

        Ok(status.code().unwrap_or(1))
    })();

    let _ = fs::remove_file(&exe);
    result
}

/// Emits whichever outputs [kinds](self::Emit) requests.
fn build_emit(
    source: &Path,
    stem: &Path,
    kinds: &HashSet<Emit>,
    project: &str,
    target: TargetArch,
) -> Result<Vec<PathBuf>, NyxError> {
    let asm = nyx::compile_project_for(&source, &project, target)?;
    let mut emitted = Vec::new();

    // write assembly to a temp `.s` file
    let asm_path = stem.with_extension("s");
    let keep_asm = kinds.contains(&Emit::Asm);

    fs::write(&asm_path, &asm)?;

    if keep_asm {
        emitted.push(asm_path.clone());
    }

    let obj_path = stem.with_extension("o");
    let keep_obj = kinds.contains(&Emit::Obj);

    let assemble_result = nyx::assemble_for(&asm_path, &obj_path, target);
    if !keep_asm {
        fs::remove_file(&asm_path).ok();
    }
    assemble_result?;

    if keep_obj {
        emitted.push(obj_path.clone());
    }

    if !kinds.contains(&Emit::Link) {
        return Ok(emitted);
    }

    let exe_path = stem.with_extension("");
    let link_result = nyx::link_for(&obj_path, stem, &[], target);
    fs::remove_file(&obj_path).ok();
    link_result?;

    emitted.push(exe_path);
    Ok(emitted)
}

#[inline(always)]
fn resolve_entry(path: Option<PathBuf>, entry_filename: &str) -> Result<PathBuf, NyxError> {
    use std::io::{Error, ErrorKind};

    let root = path.unwrap_or_else(|| PathBuf::from("."));

    // single file
    if root.extension().and_then(|e| e.to_str()) == Some("nyx") {
        if root.exists() {
            return Ok(root);
        }

        return Err(NyxError::Io(Error::new(
            ErrorKind::NotFound,
            format!("source file not found: {}", root.display()),
        )));
    }

    let entry = root.join(entry_filename);
    if entry.exists() {
        return Ok(entry);
    }

    Err(NyxError::Io(Error::new(
        ErrorKind::NotFound,
        format!("entry file `{}` not found in `{}`", entry_filename, root.display()),
    )))
}

#[inline(always)]
fn resolve_project_name(entry: &Path, override_name: Option<String>) -> String {
    override_name.unwrap_or_else(|| {
        entry
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string()
    })
}

fn resolve_target(target: Option<String>) -> Result<TargetArch, NyxError> {
    use std::io::{Error, ErrorKind};

    match target {
        None => Ok(TargetArch::host()),
        Some(s) => TargetArch::from_str(&s).ok_or_else(|| {
            NyxError::Io(Error::new(
                ErrorKind::InvalidInput,
                format!("unknown target architecture: `{s}` (expected: x86_64, aarch64)"),
            ))
        }),
    }
}

#[inline(always)]
fn temp_exe_path(source: &Path) -> PathBuf {
    let stem = source.file_stem().unwrap_or_else(|| source.as_os_str()).to_string_lossy();

    source.parent().unwrap_or(Path::new(".")).join(format!("{stem}.run.tmp"))
}
