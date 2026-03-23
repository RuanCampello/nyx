use crate::hir::Type;
use crate::lexer::token::Span;
use crate::parser::error::ParserError;

#[derive(Debug, PartialEq, Clone, thiserror::Error)]
#[error("{kind}")]
pub struct HirError<'h> {
    pub(in crate::hir) kind: HirErrorKind<'h>,
}

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

    #[error("duplicate binding `{name}` in the same scope")]
    DuplicateBind { name: String },

    #[error("missing initialiser from `{name}` and type cannot be inferred")]
    MissingInitialiser { name: String },

    #[error("type mismatch: expected `{expected}`, found `{found}`")]
    TypeMismatch { expected: Type, found: Type },

    #[error("re-assignment of immutable bind: `{name}`")]
    ImmutableBind { name: String },
}

impl<'h> From<ParserError<'h>> for HirError<'h> {
    fn from(value: ParserError<'h>) -> Self {
        Self {
            kind: HirErrorKind::Parser(value),
        }
    }
}
