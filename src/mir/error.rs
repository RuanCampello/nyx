use crate::{diagnostic::Diagnostic, hir::error::HirError};

#[derive(Debug)]
pub struct MirError {
    pub kind: MirErrorKind,
}

#[derive(Debug)]
pub enum MirErrorKind {
    Hir(Diagnostic),
}

impl From<HirError<'_>> for MirError {
    fn from(value: HirError<'_>) -> Self {
        Self { kind: MirErrorKind::Hir(value.into()) }
    }
}
