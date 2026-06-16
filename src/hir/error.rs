use crate::diagnostic;
use crate::hir::Type;
use crate::lexer::token::Span;
use nyx_macros::Diagnostic;

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct HirError<'h> {
    pub(crate) kind: HirErrorKind<'h>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Diagnostic)]
#[rustfmt::skip]
pub enum HirErrorKind<'h> {
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
    DuplicateFunction { name: &'h str },

    #[diagnostic(
        message = "duplicate method {name!} for {struct_name!}",
        primary = "{name!} is already defined for {struct_name!}",
        help = "remove or rename one of the {name!} methods"
    )]
    DuplicateMethod { struct_name: &'h str, name: &'h str },

    #[diagnostic(
        message = "use of undeclared identifier {name!}",
        primary = "{name!} is not declared in this scope",
        help = "declare {name!} with {`let {name} = …`} before using it"
    )]
    UndeclaredIdentifier { name: &'h str },

    #[diagnostic(
        message = "call to unknown function {name!}",
        primary = "{name!} is not a known function",
        help = "declare {`fn {name}(…)`} before calling it"
    )]
    UnknownFunction { name: &'h str },

     #[diagnostic(
        message = "call to unknown method {name!} on {struct_name!}",
        primary = "{struct_name!} has no method named {name!}",
        help = "add {`fn {name}(&self)`} to an impl block for {struct_name!}"
    )]
    UnknownMethod { struct_name: &'h str, name: &'h str },

     #[diagnostic(
        message = "unknown type {name!}",
        primary = "{name!} is not a known type",
        help = "declare {`struct {name} { … }`} before using it"
    )]
    UnknownType { name: &'h str },

    #[diagnostic(
        message = "cannot implement methods on {name!}",
        primary = "{name!} is not a struct declared in this module",
        help = "methods can only be defined on structs in the same module"
    )]
    OrphanImpl { name: &'h str },

    #[diagnostic(
        message = "duplicate struct {name!}",
        primary = "{name!} is defined here again",
        help = "rename one of the {name!} structs"
    )]
    DuplicateStruct { name: &'h str },

    #[diagnostic(
        message = "duplicate enum {name!}",
        primary = "{name!} is defined here again",
        help = "rename one of the {name!} enums"
    )]
    DuplicateEnum { name: &'h str },

    #[diagnostic(
        message = "duplicate field {name!}",
        primary = "{name!} is already declared",
        note = "struct field names must be unique"
    )]
    DuplicateField { name: &'h str },

    #[diagnostic(
        message = "duplicate enum variant {name!}",
        primary = "{name!} is already declared",
        note = "enum variant names must be unique"
    )]
    DuplicateVariant { name: &'h str },

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
    UnknownField { struct_name: &'h str, field: &'h str },

    #[diagnostic(
        message = "missing field {field!} in {struct_name!} literal",
        primary = "{field!} must be initialised here",
        help = "all fields of {struct_name!} must be provided in the struct literal"
    )]
    MissingField { struct_name: &'h str, field: &'h str },

    #[diagnostic(
        message = "circular struct definition involving {name!}",
        primary = "{name!} is part of a by-value struct cycle",
        note = "break the cycle; a pointer or box type will be needed for recursive structs",
        help = "Nyx does not support self-referential or circular structs yet"
    )]
    CircularStruct { name: &'h str },

    #[diagnostic(
        message = "wrong number of arguments to {name!}",
        primary = "{found} arguments provided, but {name!} expects {expected}"
    )]
    ArityMismatch { name: &'h str, expected: usize, found: usize },

    #[diagnostic(
        message = "duplicate binding {name!}",
        primary = "{name!} is already bound in this scope",
        note = "re-declaring the same name in the same scope is not allowed",
        help = "use a different name, or shadow it in a nested block"
    )]
    DuplicateBind { name: &'h str },

    #[diagnostic(
        message = "missing initialiser for {name!}",
        primary = "{name!} has no value and no type annotation",
        note = "Nyx cannot infer the type without an initial value to check against",
        help = "add a type annotation {`let {name}: <type>;`} or provide an initial value"
    )]
    MissingInitialiser { name: &'h str },

    #[diagnostic(
        message = "self receiver outside impl block",
        primary = "receivers are only valid inside method definitions",
        help = "move this function into {`impl Type { … }`}"
    )]
    ReceiverOutsideImpl,

    #[diagnostic(
        message = "type mismatch: expected {expected!}, found {found!}",
        primary = "this is of type {found!}",
        secondary(label = "expected {expected!} here")
    )]
    TypeMismatch { expected: Type, found: Type },

    #[diagnostic(
        message = "cannot assign to immutable binding {name!}",
        primary = "{name!} is immutable and cannot be reassigned",
        note = "bindings are immutable by default",
        help = "declare it as mutable: {`let mut {name} = …`}"
    )]
    ImmutableBind { name: &'h str },

    #[diagnostic(transparent)]
    ConstFnViolation(ConstFnViolationKind<'h>),

    #[diagnostic(
        message = "invalid cast from {src!} to {target!}",
        primary = "cannot cast from type {src!} to {target!}",
        note = "casting is only supported between primitive integer, bool, and char types"
    )]
    InvalidCast { src: Type, target: Type },

    #[diagnostic(
        message = "cannot index a value of type {typ!}",
        primary = "{typ!} cannot be indexed",
        help = "indexing is only supported on arrays {`[T; N]`} and slices {`&[T]`}"
    )]
    NotIndexable { typ: Type },

    #[diagnostic(
        message = "index out of bounds: the length is {len} but the index is {index}",
        primary = "index {index} is out of bounds for an array of length {len}"
    )]
    IndexOutOfBounds { index: u64, len: u32 },

    // TODO: suggest help based on the real user input code

    #[diagnostic(
        message = "cannot infer the element type of an empty array",
        primary = "the element type is unknown here",
        help = "add a type annotation, e.g. {`let a: [i32; 0] = [];`}"
    )]
    EmptyArrayType,

    #[diagnostic(
        message = "cannot assign to a value behind a shared {`&`} reference",
        primary = "the referent is read-only through a shared reference",
        help = "take a mutable reference {`&mut`} to write through it"
    )]
    AssignBehindSharedRef,

    #[diagnostic(
        message = "duplicate interface {name!}",
        primary = "{name!} is defined here again",
        help = "rename one of the {name!} interfaces"
    )]
    DuplicateInterface { name: &'h str },

    #[diagnostic(
        message = "unknown interface {name!}",
        primary = "{name!} is not a known interface",
        help = "declare {`interface {name} { … }`} before using it"
    )]
    UnknownInterface { name: &'h str },

    #[diagnostic(
        message = "missing method {method_name!} required by interface {interface_name!}",
        primary = "{struct_name!} does not implement {method_name!}",
        note = "{interface_name!} requires {`fn {method_name}(…)`}",
        help = "add {`fn {method_name}(…)`} to {`impl {struct_name} with {interface_name}`}"
    )]
    MissingInterfaceMethod { struct_name: &'h str, interface_name: &'h str, method_name: &'h str },

    #[diagnostic(
        message = "missing {superinterface_name!} implementation required by {interface_name!}",
        primary = "{struct_name!} implements {interface_name!} without {superinterface_name!}",
        note = "{interface_name!} extends {superinterface_name!}",
        help = "add {`impl {struct_name} with {superinterface_name} { … }`}"
    )]
    MissingSuperinterfaceImpl {
        struct_name: &'h str,
        interface_name: &'h str,
        superinterface_name: &'h str,
    },

    #[diagnostic(
        message = "method {method_name!} does not match interface {interface_name!}",
        primary = "found: {found~}",
        secondary(span_field = "impl_span", label = "{interface_name!} requires: {expected^}"),
        note = "expected: {expected^}\n  found: {found~}",
        help = "update {method_name!} in {`impl {struct_name} with {interface_name}`} to match the interface"
    )]
    InterfaceSignatureMismatch {
        struct_name: &'h str,
        interface_name: &'h str,
        method_name: &'h str,
        expected: &'h str,
        found: &'h str,
        impl_span: Span,
    },

    #[diagnostic(
        message = "circular dependency in constant {name!}",
        primary = "constant {name!} depends on itself"
    )]
    CircularConstant { name: &'h str },

    #[diagnostic(
        message = "duplicate constant {name!}",
        primary = "{name!} is defined here again",
        help = "rename one of the {name!} constants"
    )]
    DuplicateConstant { name: &'h str },

    #[diagnostic(
        message = "type {type_name!} does not satisfy bound {bound_name!}",
        primary = "{type_name!} is used here as {bound_name!}",
        help = "add {`impl {type_name} with {bound_name} {{ … }}`}"
    )]
    UnsatisfiedBound { type_name: Type, bound_name: &'h str },

    #[diagnostic(
        message = "operator `{op!}` requires `{interface_name!}`",
        primary = "`{type_name!}` does not implement `{interface_name!}`",
        help = "add {`impl {type_name} with {interface_name} {{ … }}`}"
    )]
    OperatorRequiresInterface { op: &'h str, type_name: &'h str, interface_name: CmpInterface },
}

#[derive(Debug, PartialEq, Clone, Copy, Diagnostic)]
pub enum ConstFnViolationKind<'h> {
    #[diagnostic(
        message = "cannot call non-const function {name!} from a const fn",
        primary = "{name!} is not a const function",
        help = "add {`const`} to {`fn {name}`}"
    )]
    NonConstCall { name: &'h str },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CmpInterface {
    Equality,
    Ordering,
}

impl<'h> HirError<'h> {
    #[inline(always)]
    pub(in crate::hir) fn new(kind: HirErrorKind<'h>, span: Span) -> Self {
        Self { kind, span }
    }
}

impl<'h> From<HirError<'h>> for diagnostic::RichDiagnostic {
    fn from(value: HirError<'h>) -> Self {
        use diagnostic::AsDiagnostic;

        value.kind.rich(value.span)
    }
}

impl std::fmt::Display for CmpInterface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CmpInterface::Equality => "PartialEq",
            CmpInterface::Ordering => "PartialOrd",
        })
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
