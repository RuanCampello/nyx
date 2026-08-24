pub use frontend::error_codes::ErrorCode;
pub use frontend::parser::statement::is_primitive;
pub use frontend::{
    BytePos, FileId, HasSpan, Label, Loc, RichDiagnostic, Severity, SourceMap, Span, SpanData,
    diagnostic, error_codes, hir, is_keyword, lexer, lints, parser, source_map,
};

use diagnostic::{AsDiagnostic, Diagnostic};
use hir::module;
use std::path::Path;

pub mod lir;
pub mod mir;

#[derive(Debug)]
pub enum NyxError {
    /// A compile-time error with a human-readable message
    Compile(Diagnostic),
    Io(std::io::Error),
    Assembler(i32),
    Linker(i32),
    // A required tool wasn't found on `PATH`
    ToolNotFound(String),
}

pub mod optimisation {
    use std::cell::Cell;

    /// Levels are ordered: every level enables everything the level below it does.
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Default, clap::ValueEnum)]
    pub enum Level {
        /// No optimisations, all runtime safety checks enabled
        #[default]
        Debug = 0,
        /// Sensible production optimisations
        ///
        /// - Overflow checks removed
        /// - Dead Code Elimination
        /// - Constant folding and propagation
        /// - Common subexpression elimination
        Sane = 1,
        /// Aggressive optimisations
        ///
        /// - Loop Unrolling
        Max = 2,
    }

    thread_local! {
        static LEVEL: Cell<Level> = const { Cell::new(Level::Debug) };
    }

    pub fn set(level: Level) {
        LEVEL.with(|slot| slot.set(level));
    }

    pub fn get() -> Level {
        LEVEL.with(Cell::get)
    }

    pub fn is_debug() -> bool {
        matches!(get(), Level::Debug)
    }
}

/// Target architecture for code generation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetArch {
    X86_64,
    AArch64,
}

/// Run the full single-file nyx compilation pipeline in a `src` and return `GAS` assembly
pub fn compile(src: &str) -> Result<String, NyxError> {
    compile_for(src, TargetArch::host())
}

/// Run the full single-file nyx compilation pipeline for a specific target
pub fn compile_for(src: &str, target: TargetArch) -> Result<String, NyxError> {
    diagnostic::reset();
    diagnostic::add_file("<source>", src);

    let arena = bumpalo::Bump::new();
    let parsed = parser::Parser::new(src).parse();

    let mut hir = hir::lower(parsed.statements, &arena);
    let mut diagnostics: Vec<_> = parsed
        .diagnostics
        .into_iter()
        .map(|error| error.rich(Span::default()))
        .collect();
    diagnostics.append(&mut hir.diagnostics);
    report(diagnostics)?;

    let mut mir = mir::lower(hir)?;
    report(mir::known_panics(&mir))?;
    mir::optimise(&mut mir, target);
    mir::eliminate_dead(&mut mir);

    let asm = match target {
        TargetArch::X86_64 => lir::emit::<lir::target::X86_64>(&mir),
        TargetArch::AArch64 => lir::emit::<lir::target::AArch64>(&mir),
    };

    Ok(asm)
}

/// Compile a multi-file `Nyx` project rooted at `source`.
///
/// A source file loads all modules reachable from it. A directory loads every
/// `.nyx` module directly inside it, so library projects do not need `main.nyx`.
pub fn compile_project(source: &Path, name: &str) -> Result<String, NyxError> {
    compile_project_for(source, name, TargetArch::host())
}

/// Compile a multi-file `Nyx` project for a specific target
pub fn compile_project_for(
    source: &Path,
    name: &str,
    target: TargetArch,
) -> Result<String, NyxError> {
    let root = match source.is_dir() {
        true => source,
        false => match source.parent() {
            Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
            Some(parent) => parent,
            None => Path::new("."),
        },
    }
    .canonicalize()?;
    let arena = bumpalo::Bump::new();

    let loader = module::ModuleLoader::new(name.to_string(), root, &arena);
    let loaded = match source.is_dir() {
        true => loader.load_directory(source),
        false => loader.load(source),
    };
    let mut hir = match loaded {
        Ok(hir) => hir,
        Err((diagnostics, err)) => {
            let mut rendered = diagnostic::render_batch(diagnostics).display();
            if !rendered.is_empty() {
                rendered.push('\n');
            }
            rendered.push_str(&Diagnostic::from(err).display());
            return Err(Diagnostic::from_rendered(rendered).into());
        },
    };
    report(std::mem::take(&mut hir.diagnostics))?;
    let mut mir = mir::lower(hir)?;
    report(mir::known_panics(&mir))?;
    mir::optimise(&mut mir, target);
    mir::eliminate_dead(&mut mir);

    let asm = match target {
        TargetArch::X86_64 => lir::emit::<lir::target::X86_64>(&mir),
        TargetArch::AArch64 => lir::emit::<lir::target::AArch64>(&mir),
    };

    Ok(asm)
}

/// Assemble a `.s` file into an `.o` object
pub fn assemble(assembly: &Path, output: &Path) -> Result<(), NyxError> {
    assemble_for(assembly, output, TargetArch::host())
}

/// Assemble a `.s` file into an `.o` object for a specific target
pub fn assemble_for(assembly: &Path, output: &Path, target: TargetArch) -> Result<(), NyxError> {
    use std::process::Command;

    let as_status = Command::new(target.assembler())
        .args(["-o", output.to_str().unwrap(), assembly.to_str().unwrap()])
        .status()
        .map_err(|e| NyxError::ToolNotFound(e.to_string()))?;

    if !as_status.success() {
        std::fs::remove_file(output).ok();
        return Err(NyxError::Assembler(as_status.code().unwrap_or(-1)));
    }

    Ok(())
}

/// Links an object file with optional extra `ld` arguments
pub fn link(object: &Path, output: &Path, args: &[&str]) -> Result<(), NyxError> {
    link_for(object, output, args, TargetArch::host())
}

/// Links an object file for a specific target
pub fn link_for(
    object: &Path,
    output: &Path,
    args: &[&str],
    target: TargetArch,
) -> Result<(), NyxError> {
    use std::process::Command;

    let ld_status = Command::new(target.linker())
        .args(args)
        .args(["-o", output.to_str().unwrap(), object.to_str().unwrap()])
        .status()
        .map_err(|e| NyxError::ToolNotFound(e.to_string()))?;

    if !ld_status.success() {
        std::fs::remove_file(output).ok();
        return Err(NyxError::Assembler(ld_status.code().unwrap_or(-1)));
    }

    Ok(())
}

fn report(diagnostics: Vec<diagnostic::RichDiagnostic>) -> Result<(), NyxError> {
    let (errors, warnings): (Vec<_>, Vec<_>) =
        diagnostics.into_iter().partition(|d| d.severity == diagnostic::Severity::Error);

    if !warnings.is_empty() {
        eprintln!("{}", diagnostic::render_batch(warnings).display());
    }

    if !errors.is_empty() {
        return Err(diagnostic::render_batch(errors).into());
    }

    Ok(())
}

impl TargetArch {
    #[inline(always)]
    pub const fn host() -> Self {
        #[cfg(target_arch = "aarch64")]
        return Self::AArch64;
        #[cfg(target_arch = "x86_64")]
        return Self::X86_64;

        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        unreachable!("this target is not yet implemented")
    }

    #[inline(always)]
    pub const fn as_str<'s>(&self) -> &'s str {
        match self {
            Self::X86_64 => "x86_64",
            Self::AArch64 => "aarch64",
        }
    }

    #[inline(always)]
    pub fn parse_name(s: &str) -> Option<Self> {
        match s {
            "x86_64" | "x86-64" => Some(Self::X86_64),
            "aarch64" | "arm64" => Some(Self::AArch64),
            _ => None,
        }
    }

    #[inline(always)]
    pub const fn assembler<'s>(&self) -> &'s str {
        match self {
            Self::X86_64 => match cfg!(target_arch = "aarch64") {
                true => "x86_64-linux-gnu-as",
                _ => "as",
            },
            Self::AArch64 => match cfg!(target_arch = "aarch64") {
                true => "as",
                _ => "aarch64-linux-gnu-as",
            },
        }
    }

    #[inline(always)]
    pub const fn linker<'s>(&self) -> &'s str {
        match self {
            Self::X86_64 => match cfg!(target_arch = "aarch64") {
                true => "x86_64-linux-gnu-ld",
                _ => "ld",
            },
            Self::AArch64 => match cfg!(target_arch = "aarch64") {
                true => "ld",
                _ => "aarch64-linux-gnu-ld",
            },
        }
    }
}

impl From<Diagnostic> for NyxError {
    fn from(diagnostic: Diagnostic) -> Self {
        Self::Compile(diagnostic)
    }
}

impl From<mir::error::MirError> for Diagnostic {
    fn from(error: mir::error::MirError) -> Self {
        match error.kind {
            mir::error::MirErrorKind::Hir(diagnostic) => diagnostic,
        }
    }
}

impl From<mir::error::MirError> for NyxError {
    fn from(error: mir::error::MirError) -> Self {
        Self::Compile(error.into())
    }
}

impl<'h> From<hir::error::HirError<'h>> for NyxError {
    fn from(error: hir::error::HirError<'h>) -> Self {
        Self::Compile(error.into())
    }
}

impl<'i> From<parser::error::ParserError<'i>> for NyxError {
    fn from(error: parser::error::ParserError<'i>) -> Self {
        Self::Compile(error.into())
    }
}

impl From<std::io::Error> for NyxError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<hir::module::ModuleError> for NyxError {
    fn from(error: hir::module::ModuleError) -> Self {
        Self::Compile(error.into())
    }
}

impl std::fmt::Display for NyxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compile(diagnostic) => diagnostic.fmt(f),
            Self::Io(error) => error.fmt(f),
            Self::Assembler(code) => write!(f, "assembler exited with status {code}"),
            Self::Linker(code) => write!(f, "linker exited with status {code}"),
            Self::ToolNotFound(tool) => write!(f, "required tool not found: {tool}"),
        }
    }
}

impl std::error::Error for NyxError {}
