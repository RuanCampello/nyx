//! High-level IR (HIR) produced by semantic analysis.
//!
//! HIR is a tree-structured, fully resolved and typed.
//! Identifiers are lowered to stable numeric IDs.

use self::def::FnDef;
#[cfg(test)]
use crate::hir::error::HirError;
use crate::{
    diagnostic,
    hir::{collect::ItemTable, declarations::Declarations},
    lexer::token::Span,
    parser::{
        expression::{BinaryOperator, TypeIntrinsicKind, UnaryOperator},
        statement::{self},
    },
};
use lasso::{Key, Spur};
use std::{collections::HashMap, ops::Index};

pub(crate) use collect::SLICE_IMPL_NAME;
pub use collect::{
    ArrayTable, InterfaceConstSignature, InterfaceMethodSignature, InterfaceSignature,
};
pub use def::*;
pub use ids::*;
pub use structs::{enum_payload_offset, struct_field, type_contains_float, type_layout};
pub use symbols::SymbolTable;
pub use ty::*;

mod collect;
mod const_check;
mod constants;
mod declarations;
mod def;
pub mod diagnostics;
pub mod error;
mod exhaustive;
pub mod ids;
mod infer;
mod interfaces;
pub mod lang;
mod lower;
pub mod module;
mod mono;
mod overload;
mod statics;
mod structs;
mod symbols;
pub mod ty;
mod type_resolver;
pub mod visit;

#[derive(Debug, PartialEq)]
pub struct Hir<'hir> {
    pub types: TyInterner<'hir>,
    pub symbols: SymbolTable,
    pub adts: IndexVec<AdtId, AdtDef<'hir>>,
    /// interned fixed-size array types, keyed by [ArrayId]
    pub arrays: ArrayTable<'hir>,
    pub functions: IndexVec<FunctionId, Function<'hir>>,
    pub constants: Vec<Constant<'hir>>,
    pub statics: IndexVec<StaticId, Static<'hir>>,
    pub interfaces: Vec<InterfaceSignature<'hir>>,
    /// Rendered `///` documentation per item, keyed by its `decl_span`
    pub docs: HashMap<Span, Box<str>>,
    /// `(span, item name)` for every item named in a `use` declaration, so an
    /// import can be resolved back to the declaration it names
    pub imports: Vec<(Span, SymbolId)>,
    /// The type every named type annotation resolved to, keyed by the span of
    /// the annotation
    pub type_refs: HashMap<Span, Type<'hir>>,
    /// Diagnostics accumulated while lowering poisoned nodes
    pub diagnostics: Vec<diagnostic::RichDiagnostic>,
    #[cfg(test)]
    reported_errors: Vec<HirError<'hir>>,
}

/// A fixed-size array type `[element; len]`, interned in the [Hir] array table
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayType<'hir> {
    pub element: Type<'hir>,
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
    LetInit { id: LocalId, init: &'hir Expression<'hir> },
    LetUninit { id: LocalId },
    Expr(&'hir Expression<'hir>),
    Return(Option<&'hir Expression<'hir>>),
    Loop { kind: LoopKind<'hir>, body: Block<'hir> },
    Break,
    Continue,
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
    /// A compiler-provided operation using ordinary call syntax.
    Intrinsic(Intrinsic),
    /// A platform syscall selected from the first argument to `syscall`.
    Syscall(Syscall),
    /// An enum variant constructor (e.g. `Optional::Some(x)`)
    Variant { id: AdtId, index: usize },
    /// Method selected through a generic interface bound. The concrete target
    /// is filled by structural monomorphisation.
    ParamMethod { param: u8, interface: SymbolId, name: SymbolId },
    /// Associated function selected through a generic interface bound.
    /// Structural monomorphisation replaces it with the concrete implementation.
    ParamFunction { param: u8, interface: SymbolId, name: SymbolId },
}

/// Type-checking results for a body, keyed by [`ExprId`]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TypeckResults<'hir> {
    node_types: IndexVec<ExprId, Type<'hir>>,
    /// What each call/method expression resolved to, keyed by the call expression's id
    type_dependent_defs: HashMap<ExprId, Res>,
    /// Generic arguments applied at a node
    node_args: HashMap<ExprId, Vec<Type<'hir>>>,
    /// Constant uses spliced into this body: root expression id -> constant name
    const_uses: HashMap<ExprId, SymbolId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function<'hir> {
    pub id: FunctionId,
    pub name: SymbolId,
    pub decl_span: Span,
    /// The declared name alone, where goto-definition lands
    pub name_span: Span,
    pub kind: FunctionKind<'hir>,
    pub owner: Owner<'hir>,
    pub params: Vec<Parameter<'hir>>,
    pub locals: IndexVec<LocalId, Local<'hir>>,
    pub return_type: Type<'hir>,
    pub is_const: bool,
    pub is_pub: bool,
    pub inline: bool,
    pub is_unsafe: bool,
    pub typeck: TypeckResults<'hir>,
    pub body: Block<'hir>,
    /// Declared generic parameter names, indexed by [TypeKind::GenericParam]
    /// Populated only on open (identity) template instances, for display
    pub generics: Vec<SymbolId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Constant<'hir> {
    pub name: SymbolId,
    pub typ: Type<'hir>,
    pub owner: Owner<'hir>,
    pub value: &'hir Expression<'hir>,
    pub typeck: TypeckResults<'hir>,
    pub is_pub: bool,
    pub decl_span: Span,
    pub name_span: Span,
}

/// A module-level global occupying one address for the whole program
///
/// Where a [Constant] is spliced into each use, a static is storage, which is
/// what lets `static mut` carry state between calls
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Static<'hir> {
    pub name: SymbolId,
    pub typ: Type<'hir>,
    pub is_mut: bool,
    pub is_pub: bool,
    /// the compile-time value the storage is born holding
    pub init: Literal,
    pub decl_span: Span,
    pub name_span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Method<'hir> {
    pub receiver: Type<'hir>,
    pub(in crate::hir) name: SymbolId,
    pub(in crate::hir) mutable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter<'hir> {
    pub id: LocalId,
    name: SymbolId,
    mutable: bool,
    pub typ: Type<'hir>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local<'hir> {
    pub id: LocalId,
    pub name: SymbolId,
    pub typ: Type<'hir>,
    pub decl_span: Span,
    pub mutable: bool,
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
    pub body: ArmBody<'hir>,
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
    /// `name @ sub`, binds the matched value while testing `sub`
    Bind { local: LocalId, sub: &'hir Pattern<'hir> },
    /// Enum variant pattern (e.g. `Some(x)`)
    Variant { id: AdtId, variant_idx: usize, sub: Option<&'hir Pattern<'hir>> },
    /// Struct destructuring (e.g. `Foo { bar, baz: 0 }`), unnamed fields are unchecked
    Struct { id: AdtId, fields: &'hir [(SymbolId, &'hir Pattern<'hir>)] },
    /// Or-pattern `A | B | C`
    Or(&'hir [Pattern<'hir>]),
    /// Literal value
    Literal(Literal),
    /// Range pattern `start..end` / `start..=end` over integer or char literals
    Range { start: Literal, end: Literal, inclusive: bool },
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
    /// `target <op>= value`, where `target` is evaluated once
    CompoundAssign {
        target: &'hir Expression<'hir>,
        operator: BinaryOperator,
        value: &'hir Expression<'hir>,
    },
    /// A struct literal (e.g. `A { x: 1, y: 2 }`)
    Struct {
        id: AdtId,
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
    /// Carries only the structural name, the resolved [FunctionId] lives in
    /// [TypeckResults::type_dependent_defs], keyed by the enclosing call's id
    Path(SymbolId),
    /// A use of a named constant
    ///
    /// The referenced value tree lives in the constant's own [ExprId] space,
    /// MIR swaps to its [TypeckResults] when lowering through this node
    Const(&'hir Constant<'hir>),
    /// An associated constant selected through a generic bound. Structural
    /// monomorphisation resolves it once the parameter has a concrete type.
    ParamConst {
        param: u8,
        interface: SymbolId,
        name: SymbolId,
    },
    /// A read of a module-level global, which loads from its address
    Static(StaticId),
    /// A function call
    ///
    /// The `callee` is a structural [ExpressionKind::Path],
    /// the resolved target is looked up from the side-tables in [TypeckResults]
    Call {
        callee: &'hir Expression<'hir>,
        args: &'hir [&'hir Expression<'hir>],
    },
    /// A method call (e.g. `x.foo(a, b)`)
    ///
    /// The resolved target is looked up from [TypeckResults::type_dependent_defs],
    /// keyed by this expression's id
    MethodCall {
        name: SymbolId,
        receiver: &'hir Expression<'hir>,
        args: &'hir [&'hir Expression<'hir>],
    },
    TypeIntrinsic {
        kind: TypeIntrinsicKind,
        typ: Type<'hir>,
    },
    /// A cast (e.g. `x as i64`)
    Cast {
        from: &'hir Expression<'hir>,
        to: Type<'hir>,
    },
    /// A `match` block
    Match {
        scrutinee: &'hir Expression<'hir>,
        arms: &'hir [Arm<'hir>],
    },
    /// A block evaluated for the value of its tail expression
    /// Without a tail the block is unit, which is what a block statement lowers to
    Block {
        statements: &'hir [Statement<'hir>],
        tail: Option<&'hir Expression<'hir>>,
    },
    /// An `if` evaluated for a value
    /// Both branches are `Block` expressions, and a missing `else` makes the whole expression unit
    If {
        condition: &'hir Expression<'hir>,
        then_block: &'hir Expression<'hir>,
        else_block: Option<&'hir Expression<'hir>>,
    },
}

/// The block an item was declared in
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Owner<'hir> {
    /// declared at module level
    #[default]
    Free,
    /// declared in `impl T`
    Inherent(Type<'hir>),
    /// declared in `impl T with I`
    Interface { on: Type<'hir>, interface: SymbolId },
}

/// What an arm does once its pattern and guard have matched
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArmBody<'hir> {
    Expr(&'hir Expression<'hir>),
    Return(Option<&'hir Expression<'hir>>),
    Break,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Literal {
    /// Unit value `()`, the zero-sized type
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(SymbolId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind<'hir> {
    Free,
    Method(Method<'hir>),
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
pub enum Syscall {
    Write,
    Exit,
    Mmap,
    Munmap,
    Mremap,
    Madvise,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub Spur);

/// Lowers the program AST to HIR, retaining every recoverable source error in [Hir::diagnostics]
pub fn lower<'hir>(
    mut statements: Vec<statement::Statement<'hir>>,
    arena: &'hir bumpalo::Bump,
) -> Hir<'hir> {
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

    let mut scope = ItemTable::new(arena);
    statement::inject_default_methods(&mut statements, |name| interfaces.get(name));
    let (declarations, errors) = Declarations::collect_recovering(&statements);
    for error in errors {
        scope.soft(error);
    }

    scope.extend(&declarations, arena);
    scope.settle_bounds();
    let functions = scope.lower_matching_functions(&declarations, |_| true, false, arena);
    let templates = scope.lower_generic_templates(&functions, arena, false);
    let functions = mono::monomorphise(functions, &templates, &scope);
    let functions = freeze_function_ids(functions);
    const_check::check(&scope, &functions);

    let declaration_arrays = scope.arrays.snapshot();
    structs::compute_layouts(&mut scope.adts.defs, &declaration_arrays);

    let diagnostics = scope.diagnostics.get_mut().take_errors();
    scope.into_hir(functions, diagnostics)
}

impl<'hir> Hir<'hir> {
    pub fn unwrap(self) -> Self {
        self.expect("HIR contains errors")
    }

    pub fn expect(self, message: &str) -> Self {
        assert!(self.is_ok(), "{message}: {:?}", self.diagnostics);
        self
    }

    pub fn is_ok(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == crate::diagnostic::Severity::Error)
    }
}

impl<'hir> Owner<'hir> {
    /// rewrites the owning type through `f`, leaving `Free` untouched
    pub fn map_type(self, f: impl FnOnce(Type<'hir>) -> Type<'hir>) -> Self {
        match self {
            Self::Inherent(on) => Self::Inherent(f(on)),
            Self::Interface { on, interface } => Self::Interface { on: f(on), interface },
            Self::Free => Self::Free,
        }
    }
}

impl<'hir> ArmBody<'hir> {
    #[inline]
    pub const fn value(self) -> Option<&'hir Expression<'hir>> {
        match self {
            Self::Expr(expr) | Self::Return(Some(expr)) => Some(expr),
            Self::Return(None) | Self::Break | Self::Continue => None,
        }
    }

    /// Whether the arm leaves the match instead of producing a value for it
    #[inline]
    pub const fn diverges(self) -> bool {
        !matches!(self, Self::Expr(_))
    }
}

/// establish the public HIR invariant that every [FunctionId] is the
/// function's position in [Hir::functions]
pub(in crate::hir) fn freeze_function_ids<'hir>(
    mut functions: IndexVec<FunctionId, Function<'hir>>,
) -> IndexVec<FunctionId, Function<'hir>> {
    let remap: HashMap<_, _> = functions
        .iter()
        .enumerate()
        .map(|(position, function)| (function.id, FunctionId(position as u32)))
        .collect();

    for (position, function) in functions.iter_mut().enumerate() {
        function.id = FunctionId(position as u32);
        for resolution in function.typeck.type_dependent_defs.values_mut() {
            let Res::Function(old) = resolution else {
                continue;
            };
            if let Some(&new) = remap.get(old) {
                *old = new;
            }
        }
    }

    functions
}

/// walk a place expression to the local it is rooted at, if any
pub fn place_base_local(expr: &Expression<'_>) -> Option<LocalId> {
    match &expr.kind {
        ExpressionKind::Local(local) => Some(*local),
        ExpressionKind::Field { base, .. } => place_base_local(base),
        ExpressionKind::Index { base, .. } => place_base_local(base),
        // place-producing methods (notably the canonical `index[_mut]` call)
        // borrow storage rooted at their receiver
        ExpressionKind::MethodCall { receiver, .. } => place_base_local(receiver),
        ExpressionKind::Unary { operator: UnaryOperator::Deref, expr } => place_base_local(expr),
        _ => None,
    }
}

impl Layout {
    pub const fn new(size: u32, align: u32, contains_float: bool) -> Self {
        Self { size, align, contains_float }
    }

    pub const fn contains_float(self) -> bool {
        self.contains_float
    }
}

impl FunctionKind<'_> {
    pub fn intrinsic(&self) -> Option<Intrinsic> {
        match self {
            Self::Intrinsic(i) => Some(*i),
            _ => None,
        }
    }
}

impl<'hir> TypeckResults<'hir> {
    #[inline(always)]
    pub fn type_of(&self, id: ExprId) -> Type<'hir> {
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
            Res::Intrinsic(_)
            | Res::Syscall(_)
            | Res::Variant { .. }
            | Res::ParamMethod { .. }
            | Res::ParamFunction { .. } => None,
        }
    }
}

impl Literal {
    #[inline(always)]
    pub const fn is_zero(self) -> bool {
        match self {
            Literal::Int(value) => value == 0,
            Literal::Float(value) => value == 0.0,
            Literal::Bool(value) => !value,
            Literal::Char(value) => value == '\0',
            Literal::Unit | Literal::Str(_) => true,
        }
    }

    #[inline]
    pub fn static_directive(self, size: u32) -> String {
        let bits = match self {
            Literal::Int(value) => value,
            Literal::Bool(value) => i64::from(value),
            Literal::Char(value) => i64::from(u32::from(value)),
            Literal::Float(value) => match size {
                4 => i64::from((value as f32).to_bits()),
                _ => value.to_bits() as i64,
            },
            Literal::Unit | Literal::Str(_) => {
                unreachable!("a zero-sized initialiser is laid out in .bss")
            },
        };

        match size {
            1 => format!(".byte {}", bits as u8),
            2 => format!(".short {}", bits as u16),
            4 => format!(".long {}", bits as u32),
            _ => format!(".quad {}", bits as u64),
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::new(0, 1, false)
    }
}

impl<'hir> Index<FunctionId> for Hir<'hir> {
    type Output = Function<'hir>;
    fn index(&self, id: FunctionId) -> &Function<'hir> {
        &self.functions[id]
    }
}

impl<'hir> Index<AdtId> for Hir<'hir> {
    type Output = AdtDef<'hir>;
    fn index(&self, id: AdtId) -> &AdtDef<'hir> {
        &self.adts[id]
    }
}

impl<'hir> Index<LocalId> for Function<'hir> {
    type Output = Local<'hir>;
    fn index(&self, id: LocalId) -> &Local<'hir> {
        &self.locals[id]
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

impl std::fmt::Debug for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SymbolId({})", self.0.into_usize())
    }
}

#[cfg(test)]
mod tests;
