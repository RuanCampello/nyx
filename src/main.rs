mod progress;

use backend::{NyxError, TargetArch, optimisation};
use clap::{Parser, Subcommand, ValueEnum};
use progress::BuildProgress;
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
    /// Compile a nyx source file or project
    ///
    /// Directory projects prefer `main.nyx` when present, otherwise every
    /// `.nyx` module in the directory is compiled without an entry point.
    Build {
        /// Path to the `.nyx` source file or the module entry point
        ///
        /// - Single file:  `nyx build file.nyx`
        /// - Project dir:  `nyx build ./my_project/`
        /// - Omitted:      builds the current directory
        path: Option<PathBuf>,

        /// Override the default entry filename inside a project directory.
        ///
        /// Defaults to `main.nyx` when that file exists. Ignored when `path` is
        /// a `.nyx` file. Without that file, all modules are compiled.
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

        /// Optimisation level
        #[arg(long, value_name = "LEVEL", default_value = "debug")]
        opt: optimisation::Level,
    },

    /// Compile a nyx source file or project and immediately run it
    ///
    /// A project without a `main` function is compiled but not launched.
    Run {
        /// Path to a `.nyx` source file or a project directory.
        ///
        /// - Single file:  `nyx run file.nyx`
        /// - Project dir:  `nyx run ./my_project/`
        /// - Omitted:      runs the current directory
        path: Option<PathBuf>,

        /// Override the default entry filename inside a project directory.
        ///
        /// Defaults to `main.nyx` when that file exists. Ignored when `path` is
        /// a `.nyx` file. Without that file, all modules are compiled.
        #[arg(long, value_name = "FILE", default_value = "main.nyx")]
        entry: String,

        /// Override the project name used in `use` path resolution.
        /// Defaults to the entry file's parent directory name.
        #[arg(long, value_name = "NAME")]
        project: Option<String>,

        /// Optimisation level
        #[arg(long, value_name = "LEVEL", default_value = "debug")]
        opt: optimisation::Level,
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

#[derive(Debug, PartialEq, Eq, Default, Clone, Copy, ValueEnum)]
enum OptimisationLevel {
    /// No optimisations, all runtime safety checks enabled
    #[default]
    Debug,
    /// Sensible production optimisations
    Sane,
    /// Aggressive optimisations
    Max,
}

struct BuildOutput {
    emitted: Vec<PathBuf>,
    linked: bool,
}

fn main() -> Result<(), NyxError> {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Build { path, entry, output, emit, project, target, opt } => {
            optimisation::set(opt);

            let entry = resolve_entry(path, &entry)?;
            let name = resolve_project_name(&entry, project);
            let arch = resolve_target(target)?;

            cmd_build(&entry, output.as_deref(), &emit, &name, arch)
        },

        Commands::Run { path, entry, project, opt } => {
            optimisation::set(opt);

            let entry = resolve_entry(path, &entry)?;
            let name = resolve_project_name(&entry, project);

            cmd_run(&entry, &name)
        },
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

    let output = build_emit(entry, &exe, &kinds, project, target)?;
    for path in output.emitted {
        println!("Emitted: {}", path.display());
    }
    if kinds.contains(&Emit::Link) && !output.linked {
        println!("No main function, linking skipped");
    }

    Ok(0)
}

fn cmd_run(entry: &Path, project: &str) -> Result<i32, NyxError> {
    let exe = temp_exe_path(entry);
    let target = TargetArch::host();

    let result = (|| -> Result<i32, NyxError> {
        let output = build_emit(entry, &exe, &HashSet::from([Emit::Link]), project, target)?;
        if !output.linked {
            return Ok(0);
        }

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
) -> Result<BuildOutput, NyxError> {
    let requested_link = kinds.contains(&Emit::Link);
    let requested_object = kinds.contains(&Emit::Obj);
    let total_phases =
        2 + usize::from(requested_object || requested_link) + usize::from(requested_link);
    let mut progress = BuildProgress::new(project, total_phases);

    progress.phase("Compiling");
    let asm = backend::compile_project_for(source, project, target)?;
    let link = requested_link && has_entry_point(&asm);
    let needs_object = requested_object || link;
    progress.set_total(2 + usize::from(needs_object) + usize::from(link));
    let mut emitted = Vec::new();

    progress.phase("Emitting assembly");
    let asm_path = stem.with_extension("s");
    let keep_asm = kinds.contains(&Emit::Asm);

    fs::write(&asm_path, &asm)?;

    if keep_asm {
        emitted.push(asm_path.clone());
    }

    if !needs_object {
        if !keep_asm {
            fs::remove_file(&asm_path).ok();
        }
        progress.finish();
        return Ok(BuildOutput { emitted, linked: false });
    }

    let obj_path = stem.with_extension("o");
    let keep_obj = kinds.contains(&Emit::Obj);

    progress.phase("Assembling");
    let assemble_result = backend::assemble_for(&asm_path, &obj_path, target);
    if !keep_asm {
        fs::remove_file(&asm_path).ok();
    }
    assemble_result?;

    if keep_obj {
        emitted.push(obj_path.clone());
    }

    if !link {
        progress.finish();
        return Ok(BuildOutput { emitted, linked: false });
    }

    progress.phase("Linking");
    let exe_path = stem.with_extension("");
    let link_result = backend::link_for(&obj_path, stem, &[], target);
    fs::remove_file(&obj_path).ok();
    link_result?;

    emitted.push(exe_path);
    progress.finish();

    Ok(BuildOutput { emitted, linked: true })
}

#[inline]
fn has_entry_point(assembly: &str) -> bool {
    assembly.lines().any(|line| line == "_start:")
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
    if entry.is_file() {
        return Ok(entry);
    }

    match root.is_dir() {
        true => Ok(root),
        false => Err(NyxError::Io(Error::new(
            ErrorKind::NotFound,
            format!("project path not found: {}", root.display()),
        ))),
    }
}

#[inline(always)]
fn resolve_project_name(entry: &Path, override_name: Option<String>) -> String {
    override_name.unwrap_or_else(|| {
        let root = match entry.is_dir() {
            true => entry,
            false => entry.parent().unwrap_or(entry),
        };

        root.file_name().and_then(|n| n.to_str()).unwrap_or("project").to_string()
    })
}

fn resolve_target(target: Option<String>) -> Result<TargetArch, NyxError> {
    use std::io::{Error, ErrorKind};

    match target {
        None => Ok(TargetArch::host()),
        Some(s) => TargetArch::parse_name(&s).ok_or_else(|| {
            NyxError::Io(Error::new(
                ErrorKind::InvalidInput,
                format!("unknown target architecture: `{s}` (expected: x86_64, aarch64)"),
            ))
        }),
    }
}

#[inline(always)]
fn temp_exe_path(source: &Path) -> PathBuf {
    let stem = source.file_stem().unwrap_or(source.as_os_str()).to_string_lossy();

    source.parent().unwrap_or(Path::new(".")).join(format!("{stem}.run.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_without_main_is_a_compilation_source() {
        let source = resolve_entry(Some(PathBuf::from("std")), "main.nyx").unwrap();

        assert_eq!(source, PathBuf::from("std"));
        assert_eq!(resolve_project_name(&source, None), "std");
    }

    #[test]
    fn generated_assembly_reports_whether_it_can_be_executed() {
        assert!(has_entry_point(".text\n.globl _start\n_start:\n"));
        assert!(!has_entry_point(".text\nnyx.library_function:\n"));
    }
}
