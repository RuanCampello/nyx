use crate::lexer::token::Span;
use crate::parser::error::ParserError;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum HirErrorKind<'h> {
    #[error(transparent)]
    Parser(ParserError<'h>),

    #[error("only functions declarations are allowed at top level")]
    TopLevelNonFunction { span: Span },

    #[error("duplicate function: `{name}`")]
    DuplicateFunction { name: String },

    #[error("use of undeclared identifier: `{name}`")]
    UndeclaredIdentifier { name: String },

    #[error("re-assignment of immutable bind: `{name}`")]
    ImmutableBind { name: String },
}
