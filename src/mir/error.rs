use crate::hir::error::HirError;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{kind}")]
pub struct MirError {
    pub kind: MirErrorKind,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum MirErrorKind {
    #[error(transparent)]
    Hir(#[from] HirError<'static>),
}
