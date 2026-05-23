#![allow(dead_code)]

use crate::hir::Type;
use crate::lexer::token::Span;
use crate::parser::error::ParserError;
use nyx_macros::Diagnostic;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Clone)]
pub struct HirError<'h> {
    pub(crate) kind: HirErrorKind<'h>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, PartialEq, Diagnostic)]
#[rustfmt::skip]
pub enum HirErrorKind<'h> {
    #[diagnostic(custom)]
    Parser(ParserError<'h>),
    #[diagnostic(
        message = "only function declarations are allowed at the top level",
        primary = "this is not a function declaration",
        help = "move this into a function body, or wrap it in {`fn main()`}"
    )]
    TopLevelNonFunction,

    #[diagnostic(
        message = "duplicate function {name!}",
        primary = "{name!} is defined here again",
        help = "rename one of the {name!} functions"
    )]
    DuplicateFunction { name: String },

    #[diagnostic(
        message = "duplicate method {name!} for {struct_name!}",
        primary = "{name!} is already defined for {struct_name!}",
        help = "remove or rename one of the {name!} methods"
    )]
    DuplicateMethod { struct_name: String, name: String },

    #[diagnostic(
        message = "use of undeclared identifier {name!}",
        primary = "{name!} is not declared in this scope",
        help = "declare {name!} with {`let {name} = …`} before using it"
    )]
    UndeclaredIdentifier { name: String },

    #[diagnostic(
        message = "call to unknown function {name!}",
        primary = "{name!} is not a known function",
        help = "declare {`fn {name}(…)`} before calling it"
    )]
    UnknownFunction { name: String },

     #[diagnostic(
        message = "call to unknown method {name!} on {struct_name!}",
        primary = "{struct_name!} has no method named {name!}",
        help = "add {`fn {name}(&self)`} to an impl block for {struct_name!}"
    )]
    UnknownMethod { struct_name: String, name: String },

     #[diagnostic(
        message = "unknown type {name!}",
        primary = "{name!} is not a known type",
        help = "declare {`struct {name} { … }`} before using it"
    )]
    UnknownType { name: String },

    #[diagnostic(custom)]
    OrphanImpl { name: String },

    #[diagnostic(
        message = "duplicate struct {name!}",
        primary = "{name!} is defined here again",
        help = "rename one of the {name!} structs"
    )]
    DuplicateStruct { name: String },

    #[diagnostic(
        message = "duplicate field {name!}",
        primary = "{name!} is already declared",
        note = "struct field names must be unique"
    )]
    DuplicateField { name: String },

    #[diagnostic(
        message = "invalid field access",
        primary = "field access is only supported on local variable bindings"
    )]
    InvalidFieldAccess,

    #[diagnostic(
        message = "invalid assignment target",
        primary = "the left-hand side must be an identifier or a field path",
        note = "use {`name = value`} or {`name.field = value`}"
    )]
    InvalidAssignmentTarget,

    #[diagnostic(
        message = "unknown field {field!} on {struct_name!}",
        primary = "{struct_name!} has no field named {field!}"
    )]
    UnknownField { struct_name: String, field: String },

    #[diagnostic(
        message = "missing field {field!} in {struct_name!} literal",
        primary = "{field!} must be initialised here",
        help = "all fields of {struct_name!} must be provided in the struct literal"
    )]
    MissingField { struct_name: String, field: String },

    #[diagnostic(
        message = "circular struct definition involving {name!}",
        primary = "{name!} is part of a by-value struct cycle",
        note = "break the cycle; a pointer or box type will be needed for recursive structs",
        help = "Nyx does not support self-referential or circular structs yet"
    )]
    CircularStruct { name: String },

    #[diagnostic(
        message = "wrong number of arguments to {name!}",
        primary = "{found} arguments provided, but {name!} expects {expected}"
    )]
    ArityMismatch { name: String, expected: usize, found: usize },

    #[diagnostic(
        message = "duplicate binding {name!}",
        primary = "{name!} is already bound in this scope",
        note = "re-declaring the same name in the same scope is not allowed",
        help = "use a different name, or shadow it in a nested block"
    )]
    DuplicateBind { name: String },

    #[diagnostic(
        message = "missing initialiser for {name!}",
        primary = "{name!} has no value and no type annotation",
        note = "Nyx cannot infer the type without an initial value to check against",
        help = "add a type annotation {`let {name}: <type>;`} or provide an initial value"
    )]
    MissingInitialiser { name: String },

    #[diagnostic(
        message = "method {name!} is missing a receiver",
        primary = "{name!} must declare {`&self`} or {`&mut self`}",
        help = "write {`fn {name}(&self, …)`}"
    )]
    MissingReceiver { name: String },

    #[diagnostic(
        message = "self receiver outside impl block",
        primary = "receivers are only valid inside method definitions",
        help = "move this function into {`impl Type { … }`}"
    )]
    ReceiverOutsideImpl,

    #[diagnostic(
        message = "type mismatch: expected {expected!}, found {found!}",
        primary = "this is of type {found!}",
        secondary(span_field = "span", label = "expected {expected!} here")
    )]
    TypeMismatch { expected: Type, found: Type },

    #[diagnostic(
        message = "cannot assign to immutable binding {name!}",
        primary = "{name!} is immutable and cannot be reassigned",
        note = "bindings are immutable by default",
        help = "declare it as mutable: {`let mut {name} = …`}"
    )]
    ImmutableBind { name: String },

    #[diagnostic(custom)]
    ConstFnViolation(ConstFnViolationKind),

    #[diagnostic(
        message = "invalid cast from {src!} to {target!}",
        primary = "cannot cast from type {src!} to {target!}",
        note = "casting is only supported between primitive integer, bool, and char types"
    )]
    InvalidCast { src: Type, target: Type },

    #[diagnostic(
        message = "duplicate interface {name!}",
        primary = "{name!} is defined here again",
        help = "rename one of the {name!} interfaces"
    )]
    DuplicateInterface { name: String },

    #[diagnostic(
        message = "unknown interface {name!}",
        primary = "{name!} is not a known interface",
        help = "declare {`interface {name} { … }`} before using it"
    )]
    UnknownInterface { name: String },

    #[diagnostic(
        message = "missing method {method_name!} required by interface {interface_name!}",
        primary = "{struct_name!} does not implement {method_name!}",
        note = "{interface_name!} requires {`fn {method_name}(…)`}",
        help = "add {`fn {method_name}(…)`} to {`impl {struct_name} with {interface_name}`}"
    )]
    MissingInterfaceMethod { struct_name: String, interface_name: String, method_name: String },

    #[diagnostic(
        message = "missing {superinterface_name!} implementation required by {interface_name!}",
        primary = "{struct_name!} implements {interface_name!} without {superinterface_name!}",
        note = "{interface_name!} extends {superinterface_name!}",
        help = "add {`impl {struct_name} with {superinterface_name} { … }`}"
    )]
    MissingSuperinterfaceImpl {
        struct_name: String,
        interface_name: String,
        superinterface_name: String,
    },

    #[diagnostic(custom)]
    InterfaceSignatureMismatch {
        struct_name: String,
        interface_name: String,
        method_name: String,
        expected: String,
        found: String,
        impl_span: Span,
    },

    #[diagnostic(
        message = "circular dependency in constant {name!}",
        primary = "constant {name!} depends on itself"
    )]
    CircularConstant { name: String },

    #[diagnostic(
        message = "duplicate constant {name!}",
        primary = "{name!} is defined here again",
        help = "rename one of the {name!} constants"
    )]
    DuplicateConstant { name: String },
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

impl<'h> HirError<'h> {
    #[inline(always)]
    pub(in crate::hir) fn new(kind: HirErrorKind<'h>, span: Span) -> Self {
        Self { kind, span }
    }
}

macro_rules! hir_error {
    ($span:expr, $kind:ident $({ $($field:ident $(: $val:expr)?),* $(,)? })?) => {
        $crate::hir::error::HirError::new(
            $crate::hir::error::HirErrorKind::$kind $({ $($field $(: $val)?),* })?,
            $span,
        )
    };
    ($span:expr, $kind:ident($($arg:expr),*)) => {
        $crate::hir::error::HirError::new(
            $crate::hir::error::HirErrorKind::$kind($($arg),*),
            $span,
        )
    };
    ($span:expr, $kind:ident) => {
        $crate::hir::error::HirError::new(
            $crate::hir::error::HirErrorKind::$kind,
            $span,
        )
    };
}

pub(in crate::hir) use hir_error;

impl<'h> From<ParserError<'h>> for HirError<'h> {
    fn from(value: ParserError<'h>) -> Self {
        let span = value.span;

        Self { kind: HirErrorKind::Parser(value), span }
    }
}
