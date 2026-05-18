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
#[rustfmt::skip]
pub enum HirErrorKind<'h> {
    Parser(ParserError<'h>),
    TopLevelNonFunction,
    DuplicateFunction { name: String },
    DuplicateMethod { struct_name: String, name: String },
    UndeclaredIdentifier { name: String },
    UnknownFunction { name: String },
    UnknownMethod { struct_name: String, name: String },
    UnknownType { name: String },
    DuplicateStruct { name: String },
    DuplicateField { name: String },
    InvalidFieldAccess,
    InvalidAssignmentTarget,
    UnknownField {
        struct_name: String,
        field: String,
    },
    MissingField {
        struct_name: String,
        field: String,
    },
    CircularStruct { name: String },
    ArityMismatch {
        name: String,
        expected: usize,
        found: usize,
    },
    DuplicateBind { name: String },
    MissingInitialiser { name: String },
    MissingReceiver { name: String },
    ReceiverOutsideImpl,
    TypeMismatch {
        expected: Type,
        found: Type,
    },
    ImmutableBind { name: String },
    ConstFnViolation(ConstFnViolationKind),

    DuplicateInterface { name: String },
    UnknownInterface { name: String },
    MissingInterfaceMethod { 
        struct_name: String,
        interface_name: String,
        method_name: String,
    },
    MissingSuperinterfaceImpl {
        struct_name: String,
        interface_name: String,
        superinterface_name: String,
    },
    InterfaceSignatureMismatch {
        struct_name: String,
        interface_name: String,
        method_name: String,
        expected: String,
        found: String,
    },
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
