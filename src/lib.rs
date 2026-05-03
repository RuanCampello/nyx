#![allow(unused)]

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

/// Run the full single-file nyx compilation pipeline in a `src` and return `GAS` assembly
pub fn compile(src: &str) -> Result<String, NyxError> {
    let statements = parser::Parser::new(src).parse()?;
    let hir = hir::lower(statements)?;
    let mir = mir::lower(hir)?;
    let asm = lir::emit::<lir::target::X86_64>(&mir);

    Ok(asm)
}

/// Compile a multi-file `Nyx` project root at `entry`
///
/// The entry file is typically `main.nyx`. All `use` imports reachable from it
/// are discovered, type-checked, and merged into a single assembly output
pub fn compile_project(entry: &Path, name: &str) -> Result<String, NyxError> {
    let root = entry.parent().unwrap_or(Path::new(".")).canonicalize()?;

    let mut loader = module::ModuleLoader::new(name.to_string(), root);
    let hir = loader.load(entry)?;
    let mir = mir::lower(hir)?;
    let asm = lir::emit::<lir::target::X86_64>(&mir);

    Ok(asm)
}

/// Assemble a `.s` file into an `.o` object
pub fn assemble(assembly: &Path, output: &Path) -> Result<(), NyxError> {
    use std::process::Command;

    let as_status = Command::new("as")
        .args(["-o", output.to_str().unwrap(), assembly.to_str().unwrap()])
        .status()
        .map_err(|e| NyxError::ToolNotFound(e.to_string()))?;

    if !as_status.success() {
        std::fs::remove_file(output).ok();

        return Err(NyxError::Assembler(as_status.code().unwrap_or(-1)));
    }

    Ok(())
}

/// Links an object file with an optional extra `ld` arguments
pub fn link(object: &Path, output: &Path, args: &[&str]) -> Result<(), NyxError> {
    use std::process::Command;

    let ld_status = Command::new("ld")
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
