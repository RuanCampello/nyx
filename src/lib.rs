pub(crate) mod diagnostic;
pub(crate) mod hir;
pub(crate) mod lexer;
pub(crate) mod lir;
pub(crate) mod mir;
pub(crate) mod parser;

/// A compilation error with a human-readable message
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct NyxError {
    pub message: String,
}

impl NyxError {
    fn new(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

/// run the full nyx compilation pipeline on `src` and return gas assembly
pub fn compile(src: &str) -> Result<String, NyxError> {
    let statements = parser::Parser::new(src).parse().map_err(NyxError::new)?;
    let hir = hir::lower(statements).map_err(NyxError::new)?;
    let mir = mir::lower(hir).map_err(NyxError::new)?;
    let asm = lir::emit::<lir::target::X86_64>(&mir);

    Ok(asm)
}
