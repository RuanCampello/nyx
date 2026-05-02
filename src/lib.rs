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
