use std::path::PathBuf;

use crate::hir::Type;
use crate::lexer::token::Span;
use crate::parser::error::ParserError;

#[derive(Debug, PartialEq, Clone, thiserror::Error)]
#[error("{kind}")]
pub struct HirError<'h> {
    pub(crate) kind: HirErrorKind<'h>,
    pub(crate) span: Span,
}

// FIXME: duplication of thiserror and string error creation in diagnostic
// we do this currently to use .to_string in mir and some other errros
// this should be centralised, comrade

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum HirErrorKind<'h> {
    #[error(transparent)]
    Parser(ParserError<'h>),

    #[error("only functions declarations are allowed at top level")]
    TopLevelNonFunction,

    #[error("duplicate function: `{name}`")]
    DuplicateFunction { name: String },

    #[error("use of undeclared identifier: `{name}`")]
    UndeclaredIdentifier { name: String },

    #[error("unknown function: `{name}`")]
    UnknownFunction { name: String },

    #[error("call to `{name}` expects {expected} arguments but {found} where provided")]
    ArityMismatch {
        name: String,
        expected: usize,
        found: usize,
    },

    #[error("duplicate binding `{name}` in the same scope")]
    DuplicateBind { name: String },

    #[error("missing initialiser from `{name}` and type cannot be inferred")]
    MissingInitialiser { name: String },

    #[error("type mismatch: expected `{expected}`, found `{found}`")]
    TypeMismatch { expected: Type, found: Type },

    #[error("re-assignment of immutable bind: `{name}`")]
    ImmutableBind { name: String },

    #[error(transparent)]
    ConstFnViolation(ConstFnViolationKind),
}

#[derive(Debug, PartialEq, Clone, thiserror::Error)]
pub enum ConstFnViolationKind {
    #[error("cannot call non-const function `{name}` in constant functions")]
    NonConstCall { name: String },
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum ResolverError {
    #[error("module path has not segments")]
    EmptyPath,

    #[error("module file not found: {}", path.display())]
    FileNotFound { path: PathBuf },

    #[error("module `{module_path}` has no exported symbol `{name}`")]
    UnknownExport { module_path: String, name: String },

    #[error("unknown module root `{name}` — expected project name")]
    UnknownRoot { name: String },

    #[error("circular import involving: {}", path.display())]
    CircularImport { path: PathBuf },
}

impl<'h> From<ParserError<'h>> for HirError<'h> {
    fn from(value: ParserError<'h>) -> Self {
        let span = value.span;

        Self {
            kind: HirErrorKind::Parser(value),
            span,
        }
    }
}
