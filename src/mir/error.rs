use crate::hir::error::HirError;

#[derive(Debug, Clone, PartialEq)]
pub struct MirError {
    pub kind: MirErrorKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MirErrorKind {
    Hir(HirError<'static>),
}

impl From<HirError<'static>> for MirError {
    fn from(value: HirError<'static>) -> Self {
        Self { kind: MirErrorKind::Hir(value) }
    }
}
