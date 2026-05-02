use crate::diagnostic::Diagnostic;

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

/// run the full nyx compilation pipeline on `src` and return gas assembly
pub fn compile(src: &str) -> Result<String, NyxError> {
    let statements = parser::Parser::new(src).parse()?;
    let hir = hir::lower(statements)?;
    let mir = mir::lower(hir)?;
    let asm = lir::emit::<lir::target::X86_64>(&mir);

    Ok(asm)
}
