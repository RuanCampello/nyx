//! High-level IR (HIR) produced by semantic analysis.
//!
//! HIR is a tree-structured, fully resolved and typed.
//! Identifiers are lowered to stable numeric IDs.

use crate::{
    diagnostic,
    hir::{
        declarations::Declarations,
        error::HirError,
        index_vec::{Idx, IndexVec},
        scope::{FunctionSignature, Scope},
    },
    lexer::token::Span,
    parser::{
        expression::{BinaryOperator, TypeIntrinsicKind, UnaryOperator},
        statement::{self, StructRepr},
    },
};
use lasso::{Key, Spur};
use std::str::FromStr;
use std::{collections::HashMap, ops::Index};

pub(crate) use scope::SLICE_IMPL_NAME;
pub(crate) use structs::struct_field;
pub use structs::type_layout;
pub use symbols::SymbolTable;
pub use types::*;

mod collector;
mod constants;
mod declarations;
pub mod diagnostics;
pub mod error;
pub mod index_vec;
mod infer;
mod interfaces;
mod lower;
pub mod module;
mod mono;
mod scope;
mod structs;
mod symbols;
mod type_resolver;
pub mod types;

#[derive(Debug, Clone, PartialEq)]
pub struct Hir<'hir> {
    pub symbols: SymbolTable,
    pub structs: IndexVec<StructId, Struct>,
    pub enums: IndexVec<EnumId, Enum>,
    /// Interned fixed-size array types, keyed by [`ArrayId`]
    pub arrays: IndexVec<ArrayId, ArrayType>,
    pub functions: IndexVec<FunctionId, Function<'hir>>,
    pub constants: Vec<Constant<'hir>>,
    /// Rendered `///` documentation per item, keyed by its `decl_span`
    pub docs: HashMap<Span, Box<str>>,
    /// Recoverable lowering diagnostics
    /// Empty unless recovery mode was on
    pub diagnostics: Vec<diagnostic::RichDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Struct {
    id: StructId,
    pub name: SymbolId,
    pub decl_span: Span,
    /// Fields in source declaration order
    pub fields: Vec<StructField>,
    pub(in crate::hir) repr: StructRepr,
    /// Cached byte layout, filled once every nominal type is collected
    pub layout: Layout,
    /// Declared generic parameter names, indexed by [`TypeKind::GenericParam`]
    /// Populated only on open (identity) template instances, for display
    pub generics: Vec<SymbolId>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StructField {
    pub name: SymbolId,
    pub typ: Type,
    /// byte offset within the owning struct, filled by layout computation
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Enum {
    pub id: EnumId,
    pub name: SymbolId,
    pub decl_span: Span,
    pub variants: Vec<EnumVariant>,
    pub repr: EnumRepr,
    /// cached byte layout, filled once every nominal type is collected
    pub layout: Layout,
    /// byte offset of a variant payload past the discriminant tag
    pub payload_offset: u32,
    pub generics: Vec<SymbolId>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnumVariant {
    pub name: SymbolId,
    pub value: i64,
    pub payload: Option<Type>,
}

/// A fixed-size array type `[element; len]`, interned in the [Hir] array table
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayType {
    pub element: Type,
    pub len: u32,
}

/// Fully resolved aggregate size and alignment, cached on each nominal type by
/// [structs::compute_layouts] once all types are collected
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    size: u32,
    align: u32,
    contains_float: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Statement<'hir> {
    LetInit {
        id: LocalId,
        init: &'hir Expression<'hir>,
    },
    LetUninit {
        id: LocalId,
    },
    Expr(&'hir Expression<'hir>),
    Return(Option<&'hir Expression<'hir>>),
    If {
        condition: &'hir Expression<'hir>,
        then_block: Block<'hir>,
        else_block: Option<Block<'hir>>,
    },
    Loop {
        kind: LoopKind<'hir>,
        body: Block<'hir>,
    },
    Break,
    Continue,
    Block(Block<'hir>),
}

/// resolved loop [header](crate::parser::statement::LoopHeader)
/// with the respective indexes
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoopKind<'hir> {
    Infinite,
    Range {
        binding: Option<LocalId>,
        start: &'hir Expression<'hir>,
        end: &'hir Expression<'hir>,
        inclusive: bool,
    },
    Iterable {
        binding: LocalId,
        iterable: &'hir Expression<'hir>,
    },
}

/// An expression node in the read-only HIR database
///
/// The node carries no type, types live in [`TypeckResults`]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expression<'hir> {
    pub id: ExprId,
    pub kind: ExpressionKind<'hir>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Res {
    /// A free function, method, or operator-overload target
    Function(FunctionId),
    /// An enum variant constructor (e.g. `Optional::Some(x)`)
    Variant { id: EnumId, index: usize },
}

/// Type-checking results for a body, keyed by [`ExprId`]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TypeckResults {
    node_types: IndexVec<ExprId, Type>,
    /// What each call/method expression resolved to, keyed by the call expression's id
    type_dependent_defs: HashMap<ExprId, Res>,
    /// Generic arguments applied at a node
    node_args: HashMap<ExprId, Vec<Type>>,
    /// Constant uses spliced into this body: root expression id -> constant name
    const_uses: HashMap<ExprId, SymbolId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function<'hir> {
    pub id: FunctionId,
    pub name: SymbolId,
    pub decl_span: Span,
    pub kind: FunctionKind,
    pub params: Vec<Parameter>,
    pub locals: IndexVec<LocalId, Local>,
    pub return_type: Type,
    pub is_const: bool,
    pub is_pub: bool,
    pub inline: bool,
    pub typeck: TypeckResults,
    pub body: Block<'hir>,
    /// Declared generic parameter names, indexed by [TypeKind::GenericParam]
    /// Populated only on open (identity) template instances, for display
    pub generics: Vec<SymbolId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Constant<'hir> {
    pub name: SymbolId,
    pub typ: Type,
    pub value: &'hir Expression<'hir>,
    pub typeck: TypeckResults,
    pub is_pub: bool,
    pub decl_span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Method {
    pub receiver: Type,
    pub(in crate::hir) name: SymbolId,
    pub(in crate::hir) mutable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    pub id: LocalId,
    name: SymbolId,
    mutable: bool,
    pub typ: Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub id: LocalId,
    pub name: SymbolId,
    pub typ: Type,
    pub decl_span: Span,
    mutable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Block<'hir> {
    pub statements: &'hir [Statement<'hir>],
    span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arm<'hir> {
    pub pattern: &'hir Pattern<'hir>,
    pub guard: Option<&'hir Expression<'hir>>,
    pub body: &'hir Expression<'hir>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pattern<'hir> {
    pub kind: PatternKind<'hir>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PatternKind<'hir> {
    /// Wildcard `_`, matches anything, binds nothing
    Wildcard,
    /// Binds the matched value to a local (e.g. `x` in `match v { x => ... }`)
    Binding(LocalId),
    /// Enum variant pattern (e.g. `Some(x)`)
    Variant { id: EnumId, variant_idx: usize, sub: Option<&'hir Pattern<'hir>> },
    /// Or-pattern `A | B | C`
    Or(&'hir [Pattern<'hir>]),
    /// Literal value
    Literal(Literal),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExpressionKind<'hir> {
    /// An inline literal value
    Literal(Literal),
    Local(LocalId),
    /// A unary operation (e.g. `!x`)
    Unary {
        operator: UnaryOperator,
        expr: &'hir Expression<'hir>,
    },
    /// A binary operation (e.g. `a * b`)
    Binary {
        operator: BinaryOperator,
        left: &'hir Expression<'hir>,
        right: &'hir Expression<'hir>,
    },
    /// An access of a named field on a struct
    Field {
        base: &'hir Expression<'hir>,
        field: SymbolId,
    },
    /// An assignment (e.g. `a = f()`)
    Assign {
        target: &'hir Expression<'hir>,
        value: &'hir Expression<'hir>,
    },
    /// A struct literal (e.g. `A { x: 1, y: 2 }`)
    Struct {
        id: StructId,
        fields: &'hir [(SymbolId, &'hir Expression<'hir>)],
    },
    /// An array literal (e.g. `[1, 2, 3]`)
    Array {
        elements: &'hir [&'hir Expression<'hir>],
    },
    /// An array repeat literal (e.g. `[0; 3]`)
    ArrayRepeat {
        value: &'hir Expression<'hir>,
        count: u32,
    },
    /// An index access (e.g. `a[i]`) into an array or slice
    Index {
        base: &'hir Expression<'hir>,
        index: &'hir Expression<'hir>,
    },
    /// A path referencing an item, e.g. the name of a called function
    ///
    /// Carries only the structural name, the resolved [`FunctionId`] lives in
    /// [`TypeckResults::type_dependent_defs`], keyed by the enclosing call's id
    Path(SymbolId),
    /// A use of a named constant
    ///
    /// The referenced value tree lives in the constant's own [`ExprId`] space,
    /// MIR swaps to its [`TypeckResults`] when lowering through this node
    Const(&'hir Constant<'hir>),
    /// A function call
    ///
    /// The `callee` is a structural [`ExpressionKind::Path`],
    /// the resolved target is looked up from the side-tables in [`TypeckResults`]
    Call {
        callee: &'hir Expression<'hir>,
        args: &'hir [&'hir Expression<'hir>],
    },
    /// A method call (e.g. `x.foo(a, b)`)
    ///
    /// The resolved target is looked up from [`TypeckResults::type_dependent_defs`],
    /// keyed by this expression's id
    MethodCall {
        name: SymbolId,
        receiver: &'hir Expression<'hir>,
        args: &'hir [&'hir Expression<'hir>],
    },
    Syscall {
        code: SyscallCode,
        args: &'hir [&'hir Expression<'hir>],
    },
    IntrinsicCall {
        intrinsic: Intrinsic,
        args: &'hir [&'hir Expression<'hir>],
    },
    TypeIntrinsic {
        kind: TypeIntrinsicKind,
        typ: Type,
    },
    /// A cast (e.g. `x as i64`)
    Cast {
        from: &'hir Expression<'hir>,
        to: Type,
    },
    /// A `match` block
    Match {
        scrutinee: &'hir Expression<'hir>,
        arms: &'hir [Arm<'hir>],
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Literal {
    /// Unit value `()`, the zero-sized type
    #[allow(unused)]
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(SymbolId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    Free,
    Method(Method),
    Intrinsic(Intrinsic),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    PrintLn,
    Print,
    Syscall,
    Len,
    WrappingAdd,
    WrappingSub,
    WrappingMul,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallCode {
    Write,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub Spur);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// Index of an [`Expression`] within typechecking results
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExprId(pub u32);

/// Lowers the program AST to a HIR program.
pub fn lower<'hir>(
    mut statements: Vec<statement::Statement<'hir>>,
    arena: &'hir bumpalo::Bump,
) -> Result<Hir<'hir>, HirError<'hir>> {
    let interfaces: std::collections::HashMap<_, _> = statements
        .iter()
        .filter_map(|stmt| match stmt {
            statement::Statement::Item(statement::Item {
                kind: statement::ItemKind::Interface(i),
                ..
            }) => Some((i.name, i.clone())),
            _ => None,
        })
        .collect();

    let declarations = Declarations::partition(&mut statements, |name| interfaces.get(name))?;

    let mut scope = Scope::new(arena);
    scope.extend(&declarations, arena)?;

    let functions = scope.lower_matching_functions(&declarations, |_| true, arena)?;
    let functions = mono::monomorphise(functions, &mut scope, arena)?;

    let arrays = scope.arrays.snapshot();
    structs::compute_layouts(&mut scope.structs, &mut scope.enums, &arrays);

    Ok(Hir {
        symbols: scope.symbols,
        structs: scope.structs,
        enums: scope.enums,
        arrays,
        functions,
        constants: scope.constants.into_values().cloned().collect(),
        docs: scope.docs,
        diagnostics: scope.diagnostics.take_errors(),
    })
}

pub fn join_docs(lines: &[&str]) -> Option<Box<str>> {
    if lines.is_empty() {
        return None;
    }

    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(line.strip_prefix(' ').unwrap_or(line));
    }

    Some(out.into_boxed_str())
}

/// Walk a place expression (`Local`/`Field`) to its base local, if any
pub(crate) fn place_base_local(expr: &Expression<'_>) -> Option<LocalId> {
    match &expr.kind {
        ExpressionKind::Local(local) => Some(*local),
        ExpressionKind::Field { base, .. } => place_base_local(base),
        ExpressionKind::Index { base, .. } => place_base_local(base),
        _ => None,
    }
}

impl Layout {
    pub(crate) const fn new(size: u32, align: u32, contains_float: bool) -> Self {
        Self { size, align, contains_float }
    }

    pub(crate) const fn contains_float(self) -> bool {
        self.contains_float
    }
}

impl Intrinsic {
    #[inline]
    pub const fn is_wrapping(self) -> bool {
        self.binary_operator().is_some()
    }

    /// the arithmetic operation a wrapping intrinsic lowers to
    #[inline]
    pub const fn binary_operator(self) -> Option<BinaryOperator> {
        match self {
            Self::WrappingAdd => Some(BinaryOperator::Add),
            Self::WrappingSub => Some(BinaryOperator::Sub),
            Self::WrappingMul => Some(BinaryOperator::Mul),
            _ => None,
        }
    }
}

impl FunctionKind {
    pub fn intrinsic(&self) -> Option<Intrinsic> {
        match self {
            Self::Intrinsic(i) => Some(*i),
            _ => None,
        }
    }
}

impl TypeckResults {
    #[inline(always)]
    pub fn type_of(&self, id: ExprId) -> Type {
        self.node_types[id]
    }

    #[inline(always)]
    pub fn type_dependent_def(&self, id: ExprId) -> Option<Res> {
        self.type_dependent_defs.get(&id).copied()
    }

    #[inline(always)]
    pub fn const_use(&self, id: ExprId) -> Option<SymbolId> {
        self.const_uses.get(&id).copied()
    }
}

impl Res {
    #[inline(always)]
    pub fn function(self) -> Option<FunctionId> {
        match self {
            Res::Function(id) => Some(id),
            Res::Variant { .. } => None,
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::new(0, 1, false)
    }
}

impl Idx for FunctionId {
    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl Idx for ExprId {
    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl Idx for LocalId {
    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl Idx for StructId {
    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl Idx for ArrayId {
    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl Idx for EnumId {
    fn to_usize(self) -> usize {
        self.id() as usize
    }
}

impl<'hir> Index<FunctionId> for Hir<'hir> {
    type Output = Function<'hir>;
    fn index(&self, id: FunctionId) -> &Function<'hir> {
        &self.functions[id]
    }
}

impl<'hir> Index<StructId> for Hir<'hir> {
    type Output = Struct;
    fn index(&self, id: StructId) -> &Struct {
        &self.structs[id]
    }
}

impl<'hir> Index<EnumId> for Hir<'hir> {
    type Output = Enum;
    fn index(&self, id: EnumId) -> &Enum {
        &self.enums[id]
    }
}

impl<'hir> Index<LocalId> for Function<'hir> {
    type Output = Local;
    fn index(&self, id: LocalId) -> &Local {
        &self.locals[id]
    }
}

impl<'hir> From<&Function<'hir>> for FunctionSignature {
    fn from(value: &Function<'hir>) -> Self {
        Self {
            params: value.params.iter().map(|param| param.typ).collect(),
            return_type: value.return_type,
            name: value.name,
            kind: value.kind,
            is_const: value.is_const,
        }
    }
}

impl From<i64> for ExpressionKind<'_> {
    fn from(value: i64) -> Self {
        Self::Literal(Literal::Int(value))
    }
}

impl From<f64> for ExpressionKind<'_> {
    fn from(value: f64) -> Self {
        Self::Literal(Literal::Float(value))
    }
}

impl From<char> for ExpressionKind<'_> {
    fn from(value: char) -> Self {
        Self::Literal(Literal::Char(value))
    }
}

impl From<bool> for ExpressionKind<'_> {
    fn from(value: bool) -> Self {
        Self::Literal(Literal::Bool(value))
    }
}

impl From<Layout> for (u32, u32) {
    fn from(value: Layout) -> Self {
        (value.size, value.align)
    }
}

impl FromStr for Intrinsic {
    type Err = ();

    fn from_str(str: &str) -> Result<Self, Self::Err> {
        Ok(match str {
            "println" => Self::PrintLn,
            "print" => Self::Print,
            "syscall" => Self::Syscall,
            "len" => Self::Len,

            _ => return Err(()),
        })
    }
}

impl FromStr for SyscallCode {
    type Err = ();

    fn from_str(str: &str) -> Result<Self, Self::Err> {
        Ok(match str {
            "SYS_WRITE" => Self::Write,
            "SYS_EXIT" => Self::Exit,

            _ => return Err(()),
        })
    }
}

impl std::fmt::Debug for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SymbolId({})", self.0.into_usize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hir::error::HirErrorKind, parser::Parser};

    #[test]
    fn unknown_identifier() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new("fn main() { x + 1; }").parse().unwrap();
        let err = super::lower(statements, &arena).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::UndeclaredIdentifier { name: "x" })
    }

    #[test]
    fn missing_return() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new("fn answer(): i32 {}").parse().unwrap();
        let err = super::lower(statements, &arena).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::MissingReturn { name: "nyx::answer", expected: TypeKind::I32.into() }
        );

        let statements = Parser::new(
            r#"
            fn sign(x: i32): i32 {
                if x < 0 {
                    return -1;
                }
            }
        "#,
        )
        .parse()
        .unwrap();
        let err = super::lower(statements, &arena).unwrap_err();
        assert!(matches!(err.kind, HirErrorKind::MissingReturn { .. }));

        let statements = Parser::new(
            r#"
            fn sign(x: i32): i32 {
                if x < 0 {
                    return -1;
                }

                1
            }
        "#,
        )
        .parse()
        .unwrap();
        assert!(super::lower(statements, &arena).is_ok());
    }

    #[test]
    fn mutability() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(
            r#"
            fn main() {
                let x: i32 = 1;
                x = 2;
            }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements, &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "x" });

        let statements = Parser::new(
            r#"
            fn main() {
                let mut x: i32 = 1;
                x = 2;
            }
        "#,
        )
        .parse()
        .unwrap();

        assert!(super::lower(statements, &arena).is_ok());
    }

    #[test]
    fn range_endpoints_must_be_integers() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(
            r#"
            fn main() {
                loop 1.0..2.0 { }
            }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements, &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::InvalidRangeType { typ: TypeKind::F64.into() })
    }

    #[test]
    fn loop_over_inferred_array() {
        let arena = bumpalo::Bump::new();

        let statements = Parser::new(
            r#"
            fn main(): i32 {
                let values = [2, 3, 5];
                let mut total = 0;
                loop value in values {
                    total = total + value;
                }
                total
            }
        "#,
        )
        .parse()
        .unwrap();
        let hir = super::lower(statements, &arena).unwrap();
        assert_eq!(hir.arrays[0].element, TypeKind::I32.into());

        for (annotation, expected) in [
            ("u8", TypeKind::U8),
            ("i16", TypeKind::I16),
            ("u32", TypeKind::U32),
            ("i64", TypeKind::I64),
        ] {
            let source = format!(
                r#"
                fn main() {{
                    let values = [1, 2, 3];
                    let mut total: {annotation} = 0;
                    loop value in values {{
                        total = total + value;
                    }}
                }}
            "#
            );
            let statements = Parser::new(&source).parse().unwrap();
            let hir = super::lower(statements, &arena).unwrap();
            assert_eq!(hir.arrays[0].element, expected.into(), "element should be {annotation}");
        }
    }

    #[test]
    fn loop_control_requires_a_loop() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new("fn main() { break; }").parse().unwrap();
        let err = super::lower(statements, &arena).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::LoopControlOutsideLoop { kind: "break" });
    }

    #[test]
    fn bitwise_and_shifts_typechecking() {
        let arena = bumpalo::Bump::new();
        let source_ok = r#"
            fn main() {
                let a: i32 = 1;
                let b: i32 = 2;
                let c: i32 = a & b;
                let d: i32 = a | b;
                let e: i32 = a ^ b;
                let f: i32 = !a;
                let g: i32 = a << b;
                let h: i32 = a >> b;

                let x: bool = true;
                let y: bool = false;
                let z: bool = x & y;
                let w: bool = x | y;
                let v: bool = x ^ y;
                let u: bool = !x;
            }
        "#;
        let statements = Parser::new(source_ok).parse().unwrap();
        assert!(super::lower(statements, &arena).is_ok());

        let source_err_shift = r#"
            fn main() {
                let a: bool = true;
                let b: i32 = 2;
                let c: bool = a << b;
            }
        "#;
        let statements = Parser::new(source_err_shift).parse().unwrap();
        let err = super::lower(statements, &arena).unwrap_err();
        assert!(matches!(err.kind, HirErrorKind::TypeMismatch { .. }));

        let source_err_not = r#"
            fn main() {
                let a: f64 = 1.0;
                let b: f64 = !a;
            }
        "#;
        let statements = Parser::new(source_err_not).parse().unwrap();
        let err = super::lower(statements, &arena).unwrap_err();
        assert!(matches!(err.kind, HirErrorKind::TypeMismatch { .. }));
    }

    #[test]
    fn if_condition_must_be_bool() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(
            r#"
            fn main() {
                let x: i64 = 1;
                if x { }
            }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements, &arena).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: TypeKind::Bool.into(),
                found: TypeKind::I64.into()
            }
        )
    }

    #[test]
    fn duplicated_function() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(
            r#"
            fn foo(): i32 { 1 }
            fn foo(): i32 { 2 }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements, &arena).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::DuplicateFunction { name: "foo" });
    }

    #[test]
    fn arity_mismatch_too_many() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(
            r#"
            fn add(a: i32, b: i32): i32 { a + b }
            fn main() { add(1, 2, 3); }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements, &arena).unwrap_err();

        assert_eq!(
            err.kind,
            HirErrorKind::ArityMismatch { name: "nyx::add", expected: 2, found: 3 }
        );
    }

    #[test]
    fn unknown_function() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new("fn main() { foo(); }").parse().unwrap();

        let err = super::lower(statements, &arena).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::UnknownFunction { name: "foo" });
    }

    #[test]
    fn type_mismatch_in_let() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(
            r#"
            fn add(a: i32, b: i32): i32 { a + b }
            fn main() {
                let x: i32 = add(1, 2);
                let y: bool = add(1, 2);
            }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements, &arena).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: TypeKind::Bool.into(),
                found: TypeKind::I32.into()
            }
        )
    }

    #[test]
    fn type_inference_from_expr() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new("fn main() { let x = 1 + 2; }").parse().unwrap();
        let hir = super::lower(statements, &arena).unwrap();

        let main = &hir.functions[0];
        assert_eq!(main.locals[0].typ, TypeKind::I32.into());
    }

    #[test]
    fn top_level_non_function() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new("let x: i64 = 1;").parse().unwrap();
        let err = super::lower(statements, &arena).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::TopLevelNonFunction)
    }

    #[test]
    fn integer_literal_as_function_arg_typed_i64() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(
            r#"
            fn foo(x: i64): i64 { x }
            fn main() { foo(1); }
        "#,
        )
        .parse()
        .unwrap();

        let hir = super::lower(statements, &arena).unwrap();

        assert_eq!(hir.functions.len(), 2);
        let foo = &hir.functions[0];
        assert_eq!(foo.return_type, TypeKind::I64.into());
        assert_eq!(foo.params.len(), 1);
        assert_eq!(foo.params[0].typ, TypeKind::I64.into());

        let main = &hir.functions[1];
        let call_id = match &main.body.statements[0] {
            Statement::Expr(expr) => *expr,
            other => panic!("expected Expr statement, got {other:?}"),
        };
        assert_eq!(main.typeck.type_of(call_id.id), TypeKind::I64.into());
        let arg = match &call_id.kind {
            ExpressionKind::Call { args, .. } => {
                assert_eq!(args.len(), 1);
                args[0]
            },
            other => panic!("expected Call expression, got {other:?}"),
        };
        assert_eq!(main.typeck.type_of(arg.id), TypeKind::I64.into());
        assert_eq!(arg.kind, 1.into());
    }

    #[test]
    fn float_literal_defaults_to_f64() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new("fn main() { let x = 3.14; }").parse().unwrap();
        let hir = super::lower(statements, &arena).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, TypeKind::F64.into());
    }

    #[test]
    fn integer_literal_defaults_to_i32_in_binary_expr() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new("fn main() { let x = 1 + 2; }").parse().unwrap();
        let hir = super::lower(statements, &arena).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, TypeKind::I32.into());

        let stmt = &func.body.statements[0];
        assert!(matches!(stmt, Statement::LetInit { id: LocalId(0), .. }));
    }

    #[test]
    fn float_literal_widens_to_f32() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new("fn main() { let x: f32 = 3.14; }").parse().unwrap();
        let hir = super::lower(statements, &arena).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, TypeKind::F32.into());
    }

    #[test]
    fn mutable_assign_widens_literal() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(
            r#"
            fn main() {
                let mut x: i64 = 0;
                x = 99;
            }
        "#,
        )
        .parse()
        .unwrap();
        let hir = super::lower(statements, &arena).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, TypeKind::I64.into());
        assert!(func.locals[0].mutable);

        let assign_id = match &func.body.statements[1] {
            Statement::Expr(expr) => *expr,
            other => panic!("expected Expr statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(assign_id.id), TypeKind::I64.into());
        let (target_id, value) = match &assign_id.kind {
            ExpressionKind::Assign { target, value } => match &target.kind {
                ExpressionKind::Local(id) => (*id, *value),
                _ => panic!("expected local assignment target"),
            },
            other => panic!("expected Assign expression, got {other:?}"),
        };

        assert_eq!(target_id, LocalId(0));
        assert_eq!(func.typeck.type_of(value.id), TypeKind::I64.into());
        assert_eq!(value.kind, 99.into());
    }

    #[test]
    fn integer_literal_widens_in_binary_with_i64_local() {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(
            r#"
            fn main() {
                let x: i64 = 10;
                let y = x + 1;
            }
        "#,
        )
        .parse()
        .unwrap();
        let hir = super::lower(statements, &arena).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 2);
        assert_eq!(func.locals[0].typ, TypeKind::I64.into());
        assert_eq!(func.locals[1].typ, TypeKind::I64.into());

        let y_stmt = &func.body.statements[1];
        assert!(matches!(y_stmt, Statement::LetInit { id: LocalId(1), .. }));
    }

    #[test]
    fn new_integer_types_accepted() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn bytes(a: i8, b: u8, c: i16, d: u16): i32 {
                0
            }
        "#;

        assert!(super::lower(Parser::new(src).parse().unwrap(), &arena).is_ok());
    }

    #[test]
    fn integer_literal_widens() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn main() {
                let x: i16 = 100;
                let y: u8 = 42;
            }
        "#;

        assert!(super::lower(Parser::new(src).parse().unwrap(), &arena).is_ok());
    }

    #[test]
    fn uptr_iptr_type_resolution() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn main() {
                let a: uptr = 10;
                let b: iptr = 20;
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.locals[0].typ, TypeKind::Uptr.into());
        assert_eq!(func.locals[1].typ, TypeKind::Iptr.into());
    }

    #[test]
    fn uptr_iptr_literal_widening() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn main() {
                let a: uptr = 100;
                let b: iptr = 200;
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];

        let init_a = match &func.body.statements[0] {
            Statement::LetInit { init: e, .. } => *e,
            other => panic!("expected Let with init, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(init_a.id), TypeKind::Uptr.into());
        assert_eq!(init_a.kind, 100.into());

        let init_b = match &func.body.statements[1] {
            Statement::LetInit { init: e, .. } => *e,
            other => panic!("expected Let with init, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(init_b.id), TypeKind::Iptr.into());
        assert_eq!(init_b.kind, 200.into());
    }

    #[test]
    fn uptr_arithmetic() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn add(a: uptr, b: uptr): uptr { a + b }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.return_type, TypeKind::Uptr.into());
        assert_eq!(func.params[0].typ, TypeKind::Uptr.into());
        assert_eq!(func.params[1].typ, TypeKind::Uptr.into());
    }

    #[test]
    fn iptr_arithmetic() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn scale(base: iptr, factor: iptr): iptr { base * factor }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.return_type, TypeKind::Iptr.into());
        assert_eq!(func.params[0].typ, TypeKind::Iptr.into());
        assert_eq!(func.params[1].typ, TypeKind::Iptr.into());
    }

    #[test]
    fn uptr_range() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn triangle(limit: uptr): uptr {
                let mut acc: uptr = 0;
                loop i in 1..=limit {
                    acc = acc + i;
                }
                acc
            }
        "#;

        assert!(super::lower(Parser::new(src).parse().unwrap(), &arena).is_ok());
    }

    #[test]
    fn uptr_iptr_mixed_type_mismatch() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn main() {
                let a: uptr = 1;
                let b: iptr = a;
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: TypeKind::Iptr.into(),
                found: TypeKind::Uptr.into()
            }
        );
    }

    #[test]
    fn bare_int_literal_defaults_to_i32() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn main() {
                let x = 0;
                let f = 1.0;
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.locals[0].typ, TypeKind::I32.into(), "unconstrained integer falls back");
        assert_eq!(func.locals[1].typ, TypeKind::F64.into(), "float literal unchanged");
    }

    #[test]
    fn int_binding_back_propagates_from_later_annotation() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn main() {
                let x = 0;
                let y: i64 = x;
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.locals[0].typ, TypeKind::I64.into(), "use against i64 pins the literal");
        assert_eq!(func.locals[1].typ, TypeKind::I64.into());
    }

    #[test]
    fn int_binding_conflicting_uses_report_mismatch() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn main() {
                let x = 0;
                let y: u8 = x;
                let z: u32 = x;
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: TypeKind::U32.into(),
                found: TypeKind::U8.into()
            }
        );
    }

    #[test]
    fn mixed_width_arithmetic_still_errors() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn main() {
                let a: i32 = 1;
                let b: uptr = 2;
                let c = a + b;
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: TypeKind::I32.into(),
                found: TypeKind::Uptr.into()
            }
        );
    }

    #[test]
    fn struct_fields_remain_in_source_order() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct Packed {
                a: i8,
                b: i64,
                c: i32,
            }

            fn main() {
                let value: Packed = Packed { a: 1, b: 2, c: 3 };
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();

        assert_eq!(hir.structs.len(), 1);
        let field_names: Vec<_> =
            hir.structs[0].fields.iter().map(|field| hir.symbols.get(field.name)).collect();
        assert_eq!(field_names, vec!["a", "b", "c"]);

        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, Type::structure(StructId(0)));
    }

    #[test]
    fn nested_struct_fields_are_resolved() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct Inner {
                n: i32,
            }

            struct Outer {
                inner: Inner,
                flag: bool,
            }

            fn main() {
                let value = Outer {
                    inner: Inner { n: 1 },
                    flag: true,
                };
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        assert_eq!(hir.structs.len(), 2);

        let outer_inner = hir.structs[1]
            .fields
            .iter()
            .find(|field| hir.symbols.get(field.name) == "inner")
            .unwrap();
        assert_eq!(outer_inner.typ, Type::structure(StructId(0)));
    }

    #[test]
    fn enum_payload_can_reference_a_later_struct() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            enum Msg {
                ChangeColour(Colour),
            }

            struct Colour {
                r: u8,
                g: u8,
                b: u8,
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        assert_eq!(hir.enums[0].variants[0].payload, Some(Type::structure(StructId(0))));
    }

    #[test]
    fn circular_structs_are_rejected() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct A {
                b: B,
            }

            struct B {
                a: A,
            }

            fn main() { }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::CircularStruct { name: "A" });
    }

    #[test]
    fn struct_literal_requires_all_fields() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct Point {
                x: i32,
                y: i32,
            }

            fn main() {
                let point = Point { x: 1 };
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::MissingField { struct_name: "Point", field: "y" });
    }

    #[test]
    fn struct_literal_rejects_unknown_field_with_span() {
        let arena = bumpalo::Bump::new();
        let src = "struct Point{x:i32}\nfn main(){let p=Point{z:1};}";

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::UnknownField { struct_name: "Point", field: "z" });
        let mut map = crate::source_map::SourceMap::default();
        map.add_file("t", src);
        assert_eq!(map.loc(err.span.start).col_utf8, 22);
        assert_eq!(map.loc(err.span.end).col_utf8, 25);
    }

    #[test]
    fn struct_literal_rejects_duplicate_field_with_span() {
        let arena = bumpalo::Bump::new();
        let src = "struct Point{x:i32}\nfn main(){let p=Point{x:1,x:2};}";

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::DuplicateField { name: "x" });
        let mut map = crate::source_map::SourceMap::default();
        map.add_file("t", src);
        assert_eq!(map.loc(err.span.start).col_utf8, 26);
        assert_eq!(map.loc(err.span.end).col_utf8, 29);
    }

    #[test]
    fn immutable_field_assignment_reports_assignment_span() {
        let arena = bumpalo::Bump::new();
        let src = "struct Point{x:i32}\nfn main(){let p=Point{x:1};p.x=2;}";

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "p" });
        let mut map = crate::source_map::SourceMap::default();
        map.add_file("t", src);
        assert_eq!(map.loc(err.span.start).col_utf8, 27);
        assert_eq!(map.loc(err.span.end).col_utf8, 30);
    }

    #[test]
    fn chained_field_access() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct Point { x: i64, y: i64 }
            struct Rect { top_left: Point, bottom_right: Point }
 
            fn main(): i64 {
                let p1 = Point { x: 0, y: 10 };
                let p2 = Point { x: 10, y: 0 };
                let r = Rect { top_left: p1, bottom_right: p2 };
                r.bottom_right.x
            }
        "#;

        assert!(super::lower(Parser::new(src).parse().unwrap(), &arena).is_ok());
    }

    #[test]
    fn impl_blocks_collect_methods_for_same_struct() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn value(&self): i32 { self.value }
            }

            impl Counter {
                fn add(&mut self, delta: i32) {
                    self.value = self.value + delta;
                }
            }

            fn main(): i32 {
                let mut counter = Counter { value: 40 };
                counter.add(2);
                counter.value()
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        assert_eq!(hir.functions.len(), 3);
        assert!(hir.functions.iter().any(|f| matches!(f.kind, FunctionKind::Method(_))));
    }

    #[test]
    fn duplicate_methods_across_impl_blocks_are_rejected() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn value(&self): i32 { self.value }
            }

            impl Counter {
                fn value(&self): i32 { self.value }
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::DuplicateMethod { struct_name: "Counter", name: "value" }
        );
        let mut map = crate::source_map::SourceMap::default();
        map.add_file("t", src);
        assert_eq!(map.loc(err.span.start).col_utf8, 16);
    }

    #[test]
    fn mut_self_method_requires_mutable_receiver() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn add(&mut self, delta: i32) {
                    self.value = self.value + delta;
                }
            }

            fn main() {
                let counter = Counter { value: 40 };
                counter.add(2);
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "counter" });
    }

    #[test]
    fn shared_self_cannot_assign_fields() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn set(&self, value: i32) {
                    self.value = value;
                }
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "self" });
    }

    #[test]
    fn wrong_interface_parameters_impl() {
        let arena = bumpalo::Bump::new();
        let src = r#"
        interface StorageEngine {
            fn flush(&self): bool;
            fn read_page(&self): i64;
        }

        struct BTreeStorage {
            page_size: i64,
        }

        impl BTreeStorage with StorageEngine {
            fn flush(&self): bool { true }
            fn read_page(&self, page_id: i64): i64 { self.page_size }
        }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).expect_err("known bug");
        assert!(matches!(
            err.kind,
            HirErrorKind::InterfaceSignatureMismatch {
                struct_name,
                interface_name,
                method_name,
                ..
            } if struct_name == "BTreeStorage"
                && interface_name == "StorageEngine"
                && method_name == "read_page"
        ));
    }

    #[test]
    fn primitive_orphan_rule_is_enforced() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            impl i64 {
                fn val(&self): i64 { *self }
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::OrphanImpl { name: "i64" });
    }

    fn const_value<'hir>(expr: &Expression<'hir>) -> &'hir Expression<'hir> {
        match expr.kind {
            ExpressionKind::Const(constant) => constant.value,
            ref other => panic!("expected Const node, got {other:?}"),
        }
    }

    #[test]
    fn const_top_level() {
        let src = r#"
            const ANSWER: i32 = 42;
            fn main(): i32 {
                ANSWER
            }
        "#;
        let arena = bumpalo::Bump::new();
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        assert_eq!(const_value(ret_expr).kind, 42.into());
    }

    #[test]
    fn const_scoped_and_qualified() {
        let src = r#"
            struct Dummy {}
            impl Dummy {
                pub const VALUE: uptr = 127;
            }
            fn main(): uptr {
                Dummy::VALUE
            }
        "#;
        let arena = bumpalo::Bump::new();
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::Uptr.into());
        assert_eq!(const_value(ret_expr).kind, 127.into());
    }

    #[test]
    fn const_primitive_scoped_in_std() {
        let src = r#"
            impl i8 {
                pub const MAX: uptr = 127;
            }
            fn main(): uptr {
                i8::MAX
            }
        "#;
        let arena = bumpalo::Bump::new();
        let mut statements = Parser::new(src).parse().unwrap();
        let declarations = Declarations::partition(&mut statements, |_| None).unwrap();
        let mut scope = Scope::new(&arena);
        scope.in_std = true;
        scope.extend(&declarations, &arena).unwrap();
        let functions = scope.lower_matching_functions(&declarations, |_| true, &arena).unwrap();
        let main_func = &functions[0];
        let ret_expr = match &main_func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(main_func.typeck.type_of(ret_expr.id), TypeKind::Uptr.into());
        assert_eq!(const_value(ret_expr).kind, 127.into());
    }

    #[test]
    fn const_nested_evaluation() {
        let src = r#"
            const A: i32 = 10;
            const B: i32 = A + 2;
            fn main(): i32 {
                B
            }
        "#;
        let arena = bumpalo::Bump::new();
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        match &const_value(ret_expr).kind {
            ExpressionKind::Binary { left, operator, right } => {
                assert_eq!(*operator, BinaryOperator::Add);
                assert_eq!(const_value(left).kind, 10.into());
                assert_eq!(right.kind, 2.into());
            },
            other => panic!("expected Binary expression, got {other:?}"),
        };
    }

    #[test]
    fn const_circular_dependency() {
        let src = r#"
            const A: i32 = B;
            const B: i32 = A;
            fn main() {}
        "#;
        let arena = bumpalo::Bump::new();
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert!(matches!(
            err.kind,
            HirErrorKind::CircularConstant { name } if name == "A" || name == "B"
        ));
    }

    #[test]
    fn const_duplicate_declaration() {
        let src = r#"
            const X: i32 = 1;
            const X: i32 = 2;
            fn main() {}
        "#;
        let arena = bumpalo::Bump::new();
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::DuplicateConstant { name: "X" });
    }

    #[test]
    fn const_scoped_duplicate_declaration() {
        let src = r#"
            struct Dummy {}
            impl Dummy {
                pub const VALUE: i32 = 1;
                pub const VALUE: i32 = 2;
            }
            fn main() {}
        "#;
        let arena = bumpalo::Bump::new();
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::DuplicateConstant { name: "Dummy::VALUE" });
    }

    #[test]
    fn const_undefined_reference() {
        let src = r#"
            const A: i32 = UNDEFINED;
            fn main() {}
        "#;
        let arena = bumpalo::Bump::new();
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::UndeclaredIdentifier { name: "UNDEFINED" });
    }

    #[test]
    fn const_shadowing() {
        let src = r#"
            const ANSWER: i32 = 42;
            fn main(): i32 {
                let ANSWER: i32 = 100;
                ANSWER
            }
        "#;
        let arena = bumpalo::Bump::new();
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[1] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        assert!(matches!(ret_expr.kind, ExpressionKind::Local(_)));
    }

    #[test]
    fn const_in_function_body_is_a_const_use_not_a_local() {
        let src = r#"
            fn main(): i32 {
                const ANSWER: i32 = 42;
                ANSWER
            }
        "#;
        let arena = bumpalo::Bump::new();
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.body.statements.len(), 1);
        assert!(func.locals.is_empty());

        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        assert_eq!(const_value(ret_expr).kind, 42.into());
    }

    #[test]
    fn const_in_function_body_folds_binary_use() {
        let src = r#"
            fn main(): i32 {
                const BASE: i32 = 10;
                BASE + 2
            }
        "#;
        let arena = bumpalo::Bump::new();
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];

        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        match &ret_expr.kind {
            ExpressionKind::Binary { left, right, .. } => {
                assert_eq!(const_value(left).kind, 10.into());
                assert_eq!(right.kind, 2.into());
            },
            other => panic!("expected Binary expression, got {other:?}"),
        }
    }

    #[test]
    fn const_in_function_body_references_earlier_const() {
        let src = r#"
            fn main(): i32 {
                const A: i32 = 10;
                const B: i32 = A + 5;
                B
            }
        "#;
        let arena = bumpalo::Bump::new();
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];

        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(func.typeck.type_of(ret_expr.id), TypeKind::I32.into());
        // B's value is `A + 5`, with A itself a nested constant reference
        match &const_value(ret_expr).kind {
            ExpressionKind::Binary { left, right, .. } => {
                assert_eq!(const_value(left).kind, 10.into());
                assert_eq!(right.kind, 5.into());
            },
            other => panic!("expected Binary expression, got {other:?}"),
        }
    }

    #[test]
    fn const_in_function_body_cannot_capture_local() {
        let src = r#"
            fn main(): i32 {
                let x: i32 = 5;
                const BAD: i32 = x;
                0
            }
        "#;
        let arena = bumpalo::Bump::new();
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::NonConstValue { name: "x" });
    }

    #[test]
    fn const_in_function_body_rejects_duplicate() {
        let src = r#"
            fn main(): i32 {
                const N: i32 = 1;
                const N: i32 = 2;
                N
            }
        "#;
        let arena = bumpalo::Bump::new();
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::DuplicateConstant { name: "N" });
    }

    #[test]
    fn nested_non_const_item_is_rejected() {
        let src = r#"
            fn main() {
                struct Inner {}
            }
        "#;
        let arena = bumpalo::Bump::new();
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::NestedItem { kind: "struct" });
    }

    #[test]
    fn literal_pattern_integer() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn classify(x: i32): i32 {
                match x {
                    0 -> 10,
                    1 -> 20,
                    _ -> 30,
                }
            }
        "#;
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];
        let match_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        let arms = match &match_expr.kind {
            ExpressionKind::Match { arms, .. } => *arms,
            other => panic!("expected Match, got {other:?}"),
        };
        assert_eq!(arms.len(), 3);
        assert!(matches!(arms[0].pattern.kind, PatternKind::Literal(Literal::Int(0))));
        assert!(matches!(arms[1].pattern.kind, PatternKind::Literal(Literal::Int(1))));
        assert!(matches!(arms[2].pattern.kind, PatternKind::Wildcard));
    }

    #[test]
    fn literal_pattern_bool() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn negate(b: bool): bool {
                match b {
                    true -> false,
                    false -> true,
                }
            }
        "#;
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];
        let match_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        let arms = match &match_expr.kind {
            ExpressionKind::Match { arms, .. } => *arms,
            other => panic!("expected Match, got {other:?}"),
        };
        assert!(matches!(arms[0].pattern.kind, PatternKind::Literal(Literal::Bool(true))));
        assert!(matches!(arms[1].pattern.kind, PatternKind::Literal(Literal::Bool(false))));
    }

    #[test]
    fn or_pattern_folds_into_single_or_node() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            enum Dir { N = 0, S = 1, E = 2, W = 3 } as u8
            fn is_horizontal(d: Dir): bool {
                match d {
                    Dir::E | Dir::W -> true,
                    _ -> false,
                }
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];
        let match_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        let arms = match &match_expr.kind {
            ExpressionKind::Match { arms, .. } => *arms,
            other => panic!("expected Match, got {other:?}"),
        };
        assert_eq!(arms.len(), 2);
        assert!(matches!(arms[0].pattern.kind, PatternKind::Or(pats) if pats.len() == 2));
        assert!(matches!(arms[1].pattern.kind, PatternKind::Wildcard));
    }

    #[test]
    fn match_arm_guard_attached() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn sign(x: i32): i32 {
                match x {
                    n if n > 0 -> 1,
                    _ -> 0,
                }
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let func = &hir.functions[0];
        let match_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        let arms = match &match_expr.kind {
            ExpressionKind::Match { arms, .. } => *arms,
            other => panic!("expected Match, got {other:?}"),
        };
        assert_eq!(arms.len(), 2);
        assert!(arms[0].guard.is_some(), "first arm must have a guard");
        assert!(arms[1].guard.is_none(), "wildcard arm must have no guard");
    }

    #[test]
    fn generic_free_function_is_monomorphised() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn pick<T>(a: T, b: T): T { a }
            fn main(): i32 { pick(7, 9) }
        "#;
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();

        let name = |f: &Function| hir.symbols.get(f.name).to_owned();

        let pick = hir
            .functions
            .iter()
            .find(|f| name(f).contains("pick$i32"))
            .expect("specialised pick$i32 instance");
        assert_eq!(pick.params[0].typ, TypeKind::I32.into());
        assert_eq!(pick.return_type, TypeKind::I32.into());
        assert!(
            hir.functions.iter().all(|f| !name(f).ends_with("pick")),
            "the open template body must not be emitted"
        );

        let main = hir.functions.iter().find(|f| name(f) == "nyx::main").unwrap();
        let call = match &main.body.statements[0] {
            Statement::Return(Some(expr)) => *expr,
            other => panic!("expected return, got {other:?}"),
        };
        assert!(matches!(call.kind, ExpressionKind::Call { .. }));
        assert_eq!(main.typeck.type_dependent_def(call.id), Some(Res::Function(pick.id)));
    }

    #[test]
    fn generic_turbofish_selects_instance() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            fn id<T>(x: T): T { x }
            fn main(): i64 { id::<i64>(5) }
        "#;
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let name = |f: &Function| hir.symbols.get(f.name).to_owned();

        let instance = hir
            .functions
            .iter()
            .find(|f| name(f).contains("id$i64"))
            .expect("specialised id$i64 instance");
        assert_eq!(instance.params[0].typ, TypeKind::I64.into());
    }

    #[test]
    fn generic_free_fn_resolves_generic_method() {
        let arena = bumpalo::Bump::new();
        let src = r#"
            struct Box<T> { val: T }
            impl Box<T> { fn get(&self): T { self.val } }
            fn unwrap<T>(b: &Box<T>): T { b.get() }
            fn main(): i64 { unwrap::<i64>(&Box::<i64> { val: 7 }) }
        "#;
        let hir = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap();
        let name = |f: &Function| hir.symbols.get(f.name).to_owned();

        assert!(
            hir.functions
                .iter()
                .any(|f| name(f).contains("Box$i64") && name(f).contains("get")),
            "expected a specialised get method on Box$i64"
        );
    }

    fn span_text(src: &str, span: Span) -> &str {
        &src[span.start.0 as usize..span.end.0 as usize]
    }

    #[test]
    fn array_constant_index_out_of_bounds() {
        let arena = bumpalo::Bump::new();
        let src = "fn main(){let a:[i32;3]=[1,2,3];a[5];}";
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::IndexOutOfBounds { index: 5, len: 3 });
        assert_eq!(span_text(src, err.span), "a[5]");
    }

    #[test]
    fn indexing_a_non_indexable_type_is_rejected() {
        let arena = bumpalo::Bump::new();
        let src = "fn main(){let x:i32=1;x[0];}";
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert!(matches!(err.kind, HirErrorKind::NotIndexable { .. }));
        assert_eq!(span_text(src, err.span), "x[0]");
    }

    #[test]
    fn a_non_integer_index_is_rejected() {
        let arena = bumpalo::Bump::new();
        let src = "fn main(){let a:[i32;2]=[1,2];let b:bool=true;a[b];}";
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert!(matches!(err.kind, HirErrorKind::TypeMismatch { .. }));
    }

    #[test]
    fn writing_an_immutable_array_element_is_rejected() {
        let arena = bumpalo::Bump::new();
        let src = "fn main(){let a:[i32;2]=[1,2];a[0]=9;}";
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "a" });
        assert_eq!(span_text(src, err.span), "a[0]");
    }

    #[test]
    fn writing_through_a_shared_slice_is_rejected() {
        let arena = bumpalo::Bump::new();
        let src = "fn set(s:&[i32]){s[0]=9;}";
        let err = super::lower(Parser::new(src).parse().unwrap(), &arena).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::AssignBehindSharedRef);
        assert_eq!(span_text(src, err.span), "s[0]");
    }
}
