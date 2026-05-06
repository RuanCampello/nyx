#![allow(dead_code)]

use crate::hir::Type;
use crate::lexer::token::Span;
use crate::parser::error::ParserError;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Clone)]
pub struct HirError<'h> {
    pub(crate) kind: HirErrorKind<'h>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HirErrorKind<'h> {
    Parser(ParserError<'h>),
    TopLevelNonFunction,
    DuplicateFunction {
        name: String,
    },
    UndeclaredIdentifier {
        name: String,
    },
    UnknownFunction {
        name: String,
    },
    ArityMismatch {
        name: String,
        expected: usize,
        found: usize,
    },
    DuplicateBind {
        name: String,
    },
    MissingInitialiser {
        name: String,
    },
    TypeMismatch {
        expected: Type,
        found: Type,
    },
    ImmutableBind {
        name: String,
    },
    ConstFnViolation(ConstFnViolationKind),
}

#[derive(Debug, PartialEq, Clone)]
pub enum ConstFnViolationKind {
    NonConstCall { name: String },
}

#[derive(Debug, PartialEq)]
pub enum ResolverError {
    EmptyPath,
    FileNotFound { path: PathBuf },
    UnknownExport { module_path: String, name: String },
    UnknownRoot { name: String },
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
