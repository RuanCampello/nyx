use crate::diagnostic;
use crate::hir::Type;
use crate::lexer::token::Span;
use macros::Diagnostic;

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct HirError<'h> {
    pub(crate) kind: HirErrorKind<'h>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Diagnostic)]
#[rustfmt::skip]
pub enum HirErrorKind<'h> {
    #[diagnostic(
        code = "E100",
        message = "Statements are not allowed at the top level",
        primary = "this statement is outside any function",
        help = "Move it into a function body, or wrap it in {`fn main() {{ ... }}`}"
    )]
    TopLevelNonFunction,

    #[diagnostic(
        code = "E101",
        message = "Function {name!} cannot be declared multiple times",
        primary = "conflicting declaration",
        secondary(span_field = "previous", optional, label = "previous declaration of {name^}"),
        help = "Rename one of the {name!} functions"
    )]
    DuplicateFunction { name: &'h str, previous: Option<Span> },

    #[diagnostic(
        code = "E102",
        message = "Method {name!} is already defined for {struct_name^}",
        primary = "conflicting declaration",
        secondary(span_field = "previous", optional, label = "previous declaration of {name^}"),
        help = "Remove or rename one of the {name!} methods"
    )]
    DuplicateMethod { struct_name: &'h str, name: &'h str, previous: Option<Span> },

    #[diagnostic(
        code = "E103",
        message = "Cannot find {name!} in this scope",
        primary = "not found in this scope",
        help = "Declare it before use: {`let {name} = ...`}"
    )]
    UndeclaredIdentifier { name: &'h str },

    #[diagnostic(
        code = "E104",
        message = "Cannot find function {name!}",
        primary = "not a known function",
        help = "Declare {`fn {name}(…)`} before calling it"
    )]
    UnknownFunction { name: &'h str },

    #[diagnostic(
        code = "E105",
        message = "Type {struct_name^} has no method named {name!}",
        primary = "unknown method",
        help = "Add {`fn {name}(&self)`} to an {`impl {struct_name}`} block"
    )]
    UnknownMethod { struct_name: &'h str, name: &'h str },

    #[diagnostic(
        code = "E155",
        message = "Cannot find associated type {name!}",
        primary = "not an associated type of this implementation",
        help = "Declare {`type {name} = ...;`} in the implementation"
    )]
    UnknownAssociatedType { name: &'h str },

    #[diagnostic(
        code = "E156",
        message = "Implementation does not bind the associated type {name!}",
        primary = "{name~} is left unbound here",
        note = "{interface_name~} declares {name~} as an associated type, so every implementation supplies one",
        help = "Add {`type {name} = ...;`} to the implementation"
    )]
    UnboundAssociatedType { name: &'h str, interface_name: &'h str },

    #[diagnostic(
        code = "E106",
        message = "Cannot find type {name!}",
        primary = "not a known type",
        help = "Declare {`struct {name} {{ ... }}`} before using it"
    )]
    UnknownType { name: &'h str },

    #[diagnostic(
        code = "E107",
        message = "Cannot implement methods on {name!}",
        primary = "{name~} is not declared in this module",
        note = "Methods can only be defined on types declared in the same module"
    )]
    OrphanImpl { name: &'h str },

    #[diagnostic(
        code = "E108",
        message = "Struct {name!} cannot be declared multiple times",
        primary = "conflicting declaration",
        secondary(span_field = "previous", optional, label = "previous declaration of {name^}"),
        help = "Rename one of the {name!} structs"
    )]
    DuplicateStruct { name: &'h str, previous: Option<Span> },

    #[diagnostic(
        code = "E109",
        message = "Enum {name!} cannot be declared multiple times",
        primary = "conflicting declaration",
        secondary(span_field = "previous", optional, label = "previous declaration of {name^}"),
        help = "Rename one of the {name!} enums"
    )]
    DuplicateEnum { name: &'h str, previous: Option<Span> },

    #[diagnostic(
        code = "E110",
        message = "Field {name!} is declared twice",
        primary = "duplicate field",
        note = "Struct field names must be unique"
    )]
    DuplicateField { name: &'h str },

    #[diagnostic(
        code = "E111",
        message = "Variant {name!} is declared twice",
        primary = "duplicate variant",
        note = "Enum variant names must be unique"
    )]
    DuplicateVariant { name: &'h str },

    #[diagnostic(
        code = "E112",
        message = "Field access is not supported on this expression",
        primary = "only local variables and their fields can be accessed",
        help = "Bind the value first: {`let value = ...;`} then access {`value.field`}"
    )]
    InvalidFieldAccess,

    #[diagnostic(
        code = "E113",
        message = "Invalid assignment target",
        primary = "cannot assign to this expression",
        note = "Only {`name = value`} and {`name.field = value`} are assignable"
    )]
    InvalidAssignmentTarget,

    #[diagnostic(
        code = "E114",
        message = "Type {struct_name^} has no field named {field!}",
        primary = "unknown field"
    )]
    UnknownField { struct_name: &'h str, field: &'h str },

    #[diagnostic(
        code = "E115",
        message = "Field {field!} is missing from this {struct_name^} literal",
        primary = "{field~} must be initialised",
        note = "Every field of {struct_name^} must be given a value"
    )]
    MissingField { struct_name: &'h str, field: &'h str },

    #[diagnostic(
        code = "E116",
        message = "Struct {name!} contains itself by value",
        primary = "part of a by-value cycle",
        note = "A struct stored by value cannot contain itself, a cycle would have infinite size",
        help = "Nyx does not support recursive structs yet"
    )]
    CircularStruct { name: &'h str },

    #[diagnostic(
        code = "E117",
        message = "Wrong number of arguments to {name!}",
        primary = "called with {found~} argument(s), but {name!} expects {expected^}",
        secondary(span_field = "decl", optional, label = "{name^} is declared here with {expected^} parameter(s)")
    )]
    ArityMismatch { name: &'h str, expected: usize, found: usize, decl: Option<Span> },

    #[diagnostic(
        code = "E118",
        message = "The name {name!} is already bound in this scope",
        primary = "rebound here",
        secondary(span_field = "previous", optional, label = "{name^} first bound here"),
        help = "Use a different name, or shadow it in a nested block"
    )]
    DuplicateBind { name: &'h str, previous: Option<Span> },

    #[diagnostic(
        code = "E119",
        message = "Binding {name!} has no type and no value",
        primary = "cannot infer a type",
        note = "A binding needs a type annotation or an initial value to infer from",
        help = "Write {`let {name}: <type>;`} or {`let {name} = <value>;`}"
    )]
    MissingInitialiser { name: &'h str },

    #[diagnostic(
        code = "E120",
        message = "A {`self`} receiver is only valid inside an {`impl`} block",
        primary = "receiver declared here",
        help = "Move this function into {`impl Type {{ ... }}`}"
    )]
    ReceiverOutsideImpl,

    #[diagnostic(
        code = "E121",
        message = "Type mismatch: expected {expected^}, found {found!}",
        primary = "this is of type {found~}"
    )]
    TypeMismatch { expected: Type<'h>, found: Type<'h> },

    #[diagnostic(
        code = "E146",
        message = "Type {found!} does not match the declared type {expected^}",
        primary = "this is of type {found~}",
        secondary(span_field = "annotation", label = "{expected^} declared here")
    )]
    TypeAnnotationMismatch { expected: Type<'h>, found: Type<'h>, annotation: Span },

    #[diagnostic(
        code = "E122",
        message = "Function {name!} must return {expected^}, but can complete without returning",
        primary = "{expected^} declared here",
        help = "Return a value from every path, or end the body with an expression of type {expected^}"
    )]
    MissingReturn { name: &'h str, expected: Type<'h> },

    #[diagnostic(
        code = "E147",
        message = "Cannot call unsafe function {name!} from a safe one",
        primary = "{name~} is marked @unsafe",
        secondary(span_field = "decl", optional, label = "{name^} is declared here"),
        help = "Mark the caller @unsafe, or wrap the call in a function that upholds the invariants"
    )]
    UnsafeCall { name: &'h str, decl: Option<Span> },

    #[diagnostic(
        code = "E150",
        message = "No compiler implementation for intrinsic {name!}",
        primary = "marked @intrinsic, but the compiler implements nothing under this name",
        help = "Remove the marker and give {name~} a body, or implement it in the compiler"
    )]
    UnknownIntrinsic { name: &'h str },

    #[diagnostic(
        lint = "unused_unsafe",
        message = "Unnecessary @unsafe block",
        primary = "nothing here needs an unsafe context",
        help = "Remove the block, or narrow it to the operation that needs it"
    )]
    UnusedUnsafe,

    #[diagnostic(
        code = "E148",
        message = "Cannot dereference a raw pointer in a safe function",
        primary = "{found~} is a raw pointer",
        help = "Mark the enclosing function @unsafe to take responsibility for the pointer"
    )]
    UnsafeDeref { found: Type<'h> },

    #[diagnostic(
        code = "E157",
        message = "Cannot dereference {found}",
        primary = "{found~} is not a reference or a raw pointer",
        help = "Only {`&T`}, {`&mut T`}, {`*T`} and {`*mut T`} can be dereferenced"
    )]
    InvalidDeref { found: Type<'h> },

    #[diagnostic(
        code = "E123",
        message = "Cannot mutate immutable binding {name!}",
        primary = "{name~} cannot be mutated",
        secondary(span_field = "decl", optional, label = "{name^} is declared immutable here"),
        note = "Bindings are immutable by default",
        help = "Declare it mutable: {`let mut {name} = ...`}"
    )]
    ImmutableBind { name: &'h str, decl: Option<Span> },

    #[diagnostic(transparent)]
    ConstFnViolation(ConstFnViolationKind<'h>),

    #[diagnostic(
        code = "E125",
        message = "Cannot cast {src!} to {target^}",
        primary = "invalid cast",
        note = "Casts are only supported between primitive integer, bool, and char types"
    )]
    InvalidCast { src: Type<'h>, target: Type<'h> },

    #[diagnostic(
        code = "E126",
        message = "Type {typ!} cannot be indexed",
        primary = "not an array, slice, or implementation of {`Index`}",
        help = "Implement {`Index<Idx>`} for {typ!} to use {`value[index]`}"
    )]
    NotIndexable { typ: Type<'h> },

    #[diagnostic(
        code = "E159",
        message = "Type {typ!} cannot be indexed mutably",
        primary = "{`IndexMutable`} is not implemented",
        help = "Implement {`IndexMutable<Idx>`} for {typ!} to mutate {`value[index]`}"
    )]
    NotMutablyIndexable { typ: Type<'h> },

    #[diagnostic(
        code = "E127",
        message = "Index {index!} is out of bounds for an array of length {len^}",
        primary = "out of bounds"
    )]
    IndexOutOfBounds { index: u64, len: u32 },

    #[diagnostic(
        code = "E128",
        message = "Type {typ!} cannot be used as a range endpoint",
        primary = "not an integer",
        help = "Use an integer type such as {`i32`} or {`uptr`}"
    )]
    InvalidRangeType { typ: Type<'h> },

    #[diagnostic(
        code = "E129",
        message = "This range matches no values",
        primary = "empty range",
        help = "Make the lower bound less than or equal to the upper bound"
    )]
    EmptyRange,


    // TODO: this should be more generic, because the loop/range
    // should just require the copy interface as any other function that requires a generic
    // interface thing, not a special case for loop + copy
    #[diagnostic(
        code = "E130",
        message = "Loop item type {typ!} does not implement {`Copy`}",
        primary = "each element is copied into the loop binding",
        help = "Implement {`Copy`} for {typ}, or iterate by reference once that is supported"
    )]
    NonCopyLoopItem { typ: Type<'h> },

    #[diagnostic(
        code = "E131",
        message = "Type {typ!} is not iterable",
        primary = "not an array or slice",
        note = "Loops currently iterate over fixed arrays, slices, and integer ranges"
    )]
    NotIterable { typ: Type<'h> },

    #[diagnostic(
        code = "E132",
        message = "{kind!} outside a loop",
        primary = "no enclosing loop",
        note = "{`break`} and {`continue`} are only valid inside a loop body"
    )]
    LoopControlOutsideLoop { kind: &'static str },

    // TODO: suggest help based on the real user input code

    #[diagnostic(
        code = "E133",
        message = "Cannot infer the element type of an empty array",
        primary = "the element type is unknown here",
        help = "Annotate the binding, e.g. {`let a: [i32; 0] = [];`}"
    )]
    EmptyArrayType,

    #[diagnostic(
        code = "E134",
        message = "Cannot mutate through a shared {`&`} reference",
        primary = "the referent is read-only through this reference",
        help = "Use a mutable {`&mut`} reference to change or mutably borrow the referent"
    )]
    AssignBehindSharedRef,

    #[diagnostic(
        code = "E135",
        message = "Interface {name!} cannot be declared multiple times",
        primary = "conflicting declaration",
        secondary(span_field = "previous", optional, label = "previous declaration of {name^}"),
        help = "Rename one of the {name!} interfaces"
    )]
    DuplicateInterface { name: &'h str, previous: Option<Span> },

    #[diagnostic(
        code = "E136",
        message = "Cannot find interface {name!}",
        primary = "not a known interface",
        help = "Declare {`interface {name} {{ … }}`} before using it"
    )]
    UnknownInterface { name: &'h str },

    #[diagnostic(
        code = "E158",
        message = "{method_name!} must be {`const`} to implement {interface_name*}",
        primary = "this implementation is not {`const`}",
        secondary(span_field = "decl", optional, label = "{interface_name*} declares it {`const`} here"),
        note = "A {`const`} requirement binds every implementation, an implementation may be {`const`} on its own without the interface asking",
        help = "Declare it {`const fn {method_name}(…)`}"
    )]
    NonConstInterfaceMethod {
        struct_name: &'h str,
        interface_name: &'h str,
        method_name: &'h str,
        decl: Option<Span>,
    },

    #[diagnostic(
        code = "E137",
        message = "{struct_name!} is missing {method_name!} required by {interface_name*}",
        primary = "{method_name~} is not implemented in this block",
        secondary(span_field = "decl", optional, label = "{interface_name*} requires it here"),
        help = "Add {`fn {method_name}(…)`} to this {`impl`} block"
    )]
    MissingInterfaceMethod {
        struct_name: &'h str,
        interface_name: &'h str,
        method_name: &'h str,
        decl: Option<Span>,
    },

    #[diagnostic(
        code = "E138",
        message = "{interface_name*} requires {superinterface_name*}, which {struct_name!} does not implement",
        primary = "{struct_name~} implements {interface_name} without {superinterface_name}",
        note = "{interface_name*} extends {superinterface_name*}, so members must implement both",
        help = "Add {`impl {struct_name} with {superinterface_name} {{ ... }}`}"
    )]
    MissingSuperinterfaceImpl {
        struct_name: &'h str,
        interface_name: &'h str,
        superinterface_name: &'h str,
    },

    #[diagnostic(
        code = "E139",
        message = "Method {method_name!} does not match its declaration in {interface_name*}",
        primary = "found {found~}",
        secondary(span_field = "decl", optional, label = "{interface_name*} declares {expected^}"),
        help = "Update {method_name!} in {`impl {struct_name} with {interface_name}`} to match"
    )]
    InterfaceSignatureMismatch {
        struct_name: &'h str,
        interface_name: &'h str,
        method_name: &'h str,
        expected: &'h str,
        found: &'h str,
        decl: Option<Span>,
    },

    #[diagnostic(
        code = "E140",
        message = "Constant {name!} depends on itself",
        primary = "cyclic definition"
    )]
    CircularConstant { name: &'h str },

    #[diagnostic(
        code = "E141",
        message = "Constant {name!} cannot be declared multiple times",
        primary = "conflicting declaration",
        secondary(span_field = "previous", optional, label = "previous declaration of {name^}"),
        help = "Rename one of the {name!} constants"
    )]
    DuplicateConstant { name: &'h str, previous: Option<Span> },

    #[diagnostic(
        code = "E142",
        message = "Type {type_name!} does not satisfy the bound {bound_name*}",
        primary = "{type_name~} is used here as {bound_name*}",
        help = "Add {`impl {type_name} with {bound_name} {{ ... }}`}"
    )]
    UnsatisfiedBound { type_name: Type<'h>, bound_name: &'h str },

    #[diagnostic(
        code = "E143",
        message = "Operator {op!} requires {interface_name*}",
        primary = "{type_name~} does not implement {interface_name*}",
        help = "Add {`impl {type_name} with {interface_name} {{ ... }}`}"
    )]
    OperatorRequiresInterface { op: &'h str, type_name: &'h str, interface_name: CmpInterface },

    #[diagnostic(
        code = "E144",
        message = "{kind!} declarations are not allowed inside a function body",
        primary = "declared inside a function",
        help = "Move this {kind} to the module level; only {`const`} may be declared in a body"
    )]
    NestedItem { kind: &'h str },

    #[diagnostic(
        code = "E145",
        message = "Constants cannot refer to runtime values",
        primary = "{name!} is a local variable",
        note = "A constant is evaluated independently of the function it appears in",
        help = "Use a literal or another {`const`}"
    )]
    NonConstValue { name: &'h str },

    #[diagnostic(
        code = "E151",
        message = "{struct_name!} is missing {constant_name!} required by {interface_name*}",
        primary = "{constant_name~} is not defined in this block",
        secondary(span_field = "decl", optional, label = "{interface_name*} requires it here"),
        help = "Add {`const {constant_name}: … = …;`} to this {`impl`} block"
    )]
    MissingInterfaceConstant {
        struct_name: &'h str,
        interface_name: &'h str,
        constant_name: &'h str,
        decl: Option<Span>,
    },

    #[diagnostic(
        code = "E154",
        message = "Reading or writing {name!} requires an unsafe context",
        primary = "this touches a mutable global",
        note = "Nothing stops two pieces of code reaching a {`static mut`} at once, so the compiler cannot vouch for it",
        help = "Wrap the access in {`@unsafe { … }`} or mark the enclosing function {`@unsafe`}"
    )]
    UnsafeStatic { name: &'h str },

    #[diagnostic(
        code = "E153",
        message = "The initialiser of static {name!} is not known at compile time",
        primary = "this cannot be evaluated before the program runs",
        note = "A static is storage laid out in the executable, so it must start at a value the compiler can write there",
        help = "Use a literal, a negated literal, or a {`const`}"
    )]
    NonConstStaticInit { name: &'h str },

    #[diagnostic(
        code = "E152",
        message = "Constant {constant_name!} does not match its declaration in {interface_name*}",
        primary = "found type {found~}",
        secondary(span_field = "decl", optional, label = "{interface_name*} declares type {expected^}"),
        help = "Update {constant_name!} in {`impl {struct_name} with {interface_name}`} to match"
    )]
    InterfaceConstantTypeMismatch {
        struct_name: &'h str,
        interface_name: &'h str,
        constant_name: &'h str,
        expected: Type<'h>,
        found: Type<'h>,
        decl: Option<Span>,
    },
}

#[derive(Debug, PartialEq, Clone, Copy, Diagnostic)]
pub enum ConstFnViolationKind<'h> {
    #[diagnostic(
        code = "E124",
        message = "Cannot call non-const function {name!} from a {`const fn`}",
        primary = "{name~} is not a const function",
        help = "Add {`const`} to {`fn {name}`}"
    )]
    NonConstCall { name: &'h str },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CmpInterface {
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

        // capture the full-colour CLI rendering while the borrowed error data
        // is still alive, the structured fields stay plain for the editor
        let mut rich = value.kind.rich(value.span);
        rich.rendered = Some(value.kind.into_diagnostic(value.span).display());
        rich
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
