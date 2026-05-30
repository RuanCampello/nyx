use crate::{diagnostic::Diagnostic, hir::module};
use std::path::Path;

pub mod diagnostic;
pub(crate) mod hir;
pub(crate) mod lexer;
pub(crate) mod lir;
pub(crate) mod mir;
pub(crate) mod parser;

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
    use std::sync::OnceLock;

    // Optimisation level for code generation
    #[derive(Debug, PartialEq, Eq, Clone, Copy, Default, clap::ValueEnum)]
    pub enum Level {
        /// No optimisations, all runtime safety checks enabled
        #[default]
        Debug,
        /// Sensible production optimisations
        ///
        /// - Overflow checks removed
        /// - Dead code elimination
        /// - Constant folding and propagation
        /// - Common subexpression elimination
        Sane,
        /// Aggressive optimisations
        ///
        /// - Loop unrolling
        Max,
    }

    static LEVEL: OnceLock<Level> = OnceLock::new();

    pub fn set(level: Level) {
        LEVEL.set(level).expect("optimisation level set should never fail");
    }

    pub fn get() -> Level {
        *LEVEL.get().unwrap_or(&Level::Debug)
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
    let arena = bumpalo::Bump::new();
    let statements = parser::Parser::new(src).parse()?;
    let hir = hir::lower(statements, &arena)?;
    let mir = mir::lower(hir)?;

    let asm = match target {
        TargetArch::X86_64 => lir::emit::<lir::target::X86_64>(&mir),
        TargetArch::AArch64 => lir::emit::<lir::target::AArch64>(&mir),
    };

    Ok(asm)
}

/// Compile a multi-file `Nyx` project root at `entry`
///
/// The entry file is typically `main.nyx`. All `use` imports reachable from it
/// are discovered, type-checked, and merged into a single assembly output
pub fn compile_project(entry: &Path, name: &str) -> Result<String, NyxError> {
    compile_project_for(entry, name, TargetArch::host())
}

/// Compile a multi-file `Nyx` project for a specific target
pub fn compile_project_for(
    entry: &Path,
    name: &str,
    target: TargetArch,
) -> Result<String, NyxError> {
    let root = entry.parent().unwrap_or(Path::new(".")).canonicalize()?;
    let arena = bumpalo::Bump::new();

    let mut loader = module::ModuleLoader::new(name.to_string(), root, &arena);
    let hir = loader.load(entry)?;
    let mir = mir::lower(hir)?;

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
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::AArch64 => "aarch64",
        }
    }

    #[inline(always)]
    pub fn from_str(s: &str) -> Option<Self> {
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
