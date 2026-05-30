//! High-level IR (HIR) produced by semantic analysis.
//!
//! HIR is a tree-structured, fully resolved and typed.
//! Identifiers are lowered to stable numeric IDs.

use crate::{
    hir::{
        declarations::Declarations,
        error::HirError,
        index_vec::{Idx, IndexVec},
        scope::{FunctionSignature, Scope},
        symbols::SymbolTable,
    },
    lexer::token::Span,
    parser::{
        expression::{BinaryOperator, TypeIntrinsicKind, UnaryOperator},
        statement::{self, StructRepr},
    },
};
use lasso::{Key, Spur};
use std::ops::Index;
use std::str::FromStr;

pub(crate) use structs::Visit;
pub use types::*;

mod constants;
mod declarations;
pub mod error;
pub mod index_vec;
mod interfaces;
mod lower;
pub(crate) mod module;
pub(crate) mod monomorph;
mod scope;
mod structs;
mod symbols;
mod type_resolver;
pub mod types;

#[derive(Debug, Clone, PartialEq)]
pub struct Hir {
    pub symbols: Vec<String>,
    pub structs: IndexVec<StructId, Struct>,
    pub enums: IndexVec<EnumId, Enum>,
    pub functions: IndexVec<FunctionId, Function>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Struct {
    id: StructId,
    name: SymbolId,
    /// Fields in source declaration order. Concrete layout belongs to MIR.
    pub(crate) fields: Vec<StructField>,
    pub(crate) repr: StructRepr,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StructField {
    pub(crate) name: SymbolId,
    pub(crate) typ: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Enum {
    id: EnumId,
    name: SymbolId,
    pub(crate) variants: Vec<EnumVariant>,
    pub(crate) repr: EnumRepr,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnumVariant {
    pub(crate) name: SymbolId,
    pub(crate) value: i64,
    pub(crate) payload: Option<Type>,
}

#[derive(Debug, Clone, PartialEq)]
#[rustfmt::skip]
pub enum Statement {
    LetInit { id: LocalId, init: Expression },
    LetUninit { id: LocalId },
    Expr(Expression),
    Return(Option<Expression>),
    If { condition: Expression, then_block: Block, else_block: Option<Block> },
    While { condition: Expression, body: Block },
    Block(Block),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    pub(crate) kind: ExpressionKind,
    pub(crate) typ: Type,
    span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub(crate) kind: PlaceKind,
    pub(crate) typ: Type,
    span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub id: FunctionId,
    pub name: SymbolId,
    pub kind: FunctionKind,
    pub params: Vec<Parameter>,
    pub locals: IndexVec<LocalId, Local>,
    pub return_type: Type,
    pub is_const: bool,
    pub is_pub: bool,
    pub inline: bool,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Constant {
    pub name: SymbolId,
    pub typ: Type,
    pub value: Expression,
    pub is_pub: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Method {
    pub(crate) receiver: Type,
    pub(crate) name: SymbolId,
    pub(crate) mutable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    pub(crate) id: LocalId,
    name: SymbolId,
    mutable: bool,
    pub(crate) typ: Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub(crate) id: LocalId,
    pub(crate) name: SymbolId,
    pub(crate) typ: Type,
    mutable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub(crate) statements: Vec<Statement>,
    span: Span,
}

#[derive(Debug, Clone, PartialEq)]
#[rustfmt::skip]
pub enum ExpressionKind {
    #[allow(dead_code)]
    Unit,
    Integer(i64),
    Float(f64),
    String(SymbolId),
    Char(char),
    Bool(bool),
    Local(LocalId),
    Unary { operator: UnaryOperator, expr: Box<Expression> },
    Binary { operator: BinaryOperator, left: Box<Expression>, right: Box<Expression> },
    Place(Place),
    Assign { target: Place, value: Box<Expression> },
    MethodCall { function: FunctionId, receiver: Receiver, args: Vec<Expression> },
    Struct {
        id: StructId,
        fields: Vec<(SymbolId, Expression)>,
    },
    Call { function: FunctionId, args: Vec<Expression> },
    Syscall { code: SyscallCode, args: Vec<Expression> },
    IntrinsicCall { intrinsic: Intrinsic, args: Vec<Expression> },
    TypeIntrinsic { kind: TypeIntrinsicKind, typ: Type },
    Cast { from: Box<Expression>, to: Type },
    /// read the discriminant of an enum value as its backing repr integer
    EnumTag { value: Box<Expression> },
    /// read a variant payload out of an enum value (typed as the payload)
    EnumPayload { value: Box<Expression> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Receiver {
    pub(crate) kind: ReceiverKind,
    pub(crate) typ: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaceKind {
    Local(LocalId),
    Field { base: Box<Place>, field: SymbolId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    Free,
    Method(Method),
    Intrinsic(Intrinsic),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiverKind {
    Place(Place),
    Computed(Box<Expression>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    PrintLn,
    Print,
    Syscall,
    Len,
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

impl Idx for FunctionId {
    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl Idx for LocalId {
    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

/// Lowers the program AST to a HIR program.
pub fn lower<'h>(mut statements: Vec<statement::Statement<'h>>) -> Result<Hir, HirError<'h>> {
    let bump = bumpalo::Bump::new();
    // SAFETY: bump lives until the end of this function
    // All &'h str refs from it are used only within `lower`. The returned `Hir` has no string references
    // (only SymbolIds). Dropping `statements` after `bump` is safe since &str::drop is a no-op, no memory is read during the drop.
    let bump_ref: &'h bumpalo::Bump =
        unsafe { &*(std::ptr::addr_of!(bump) as *const bumpalo::Bump) };

    monomorph::monomorphise(&mut statements, bump_ref)?;

    let interfaces: std::collections::HashMap<_, _> = statements
        .iter()
        .filter_map(|stmt| match stmt {
            statement::Statement::Interface(i) => Some((i.name, i.clone())),
            _ => None,
        })
        .collect();

    let mut symbols = SymbolTable::new();
    let declarations = Declarations::partition(&mut statements, |name| interfaces.get(name))?;

    let mut scope = Scope::new();
    scope.extend(&declarations, &mut symbols, false)?;

    let functions = scope.lower_functions(&declarations, &mut symbols, false)?;

    Ok(Hir {
        symbols: symbols.into_symbols(),
        structs: scope.structs,
        enums: scope.enums,
        functions,
    })
}

impl FunctionKind {
    pub fn intrinsic(&self) -> Option<Intrinsic> {
        match self {
            Self::Intrinsic(i) => Some(*i),
            _ => None,
        }
    }
}

impl ReceiverKind {
    pub fn base_local(&self) -> Option<LocalId> {
        match self {
            Self::Place(place) => place.base_local(),
            Self::Computed(_) => None,
        }
    }
}

impl Place {
    pub(crate) const fn local(id: LocalId, typ: Type, span: Span) -> Self {
        Self { kind: PlaceKind::Local(id), typ, span }
    }

    pub(crate) fn field(base: Self, field: SymbolId, typ: Type, span: Span) -> Self {
        Self {
            kind: PlaceKind::Field { base: Box::new(base), field },
            typ,
            span,
        }
    }

    pub(crate) fn base_local(&self) -> Option<LocalId> {
        match &self.kind {
            PlaceKind::Local(id) => Some(*id),
            PlaceKind::Field { base, .. } => base.base_local(),
        }
    }
}

impl Idx for StructId {
    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl Idx for EnumId {
    fn to_usize(self) -> usize {
        self.id() as usize
    }
}

impl Index<FunctionId> for Hir {
    type Output = Function;
    fn index(&self, id: FunctionId) -> &Function {
        &self.functions[id]
    }
}

impl Index<StructId> for Hir {
    type Output = Struct;
    fn index(&self, id: StructId) -> &Struct {
        &self.structs[id]
    }
}

impl Index<EnumId> for Hir {
    type Output = Enum;
    fn index(&self, id: EnumId) -> &Enum {
        &self.enums[id]
    }
}

impl Index<LocalId> for Function {
    type Output = Local;
    fn index(&self, id: LocalId) -> &Local {
        &self.locals[id]
    }
}

impl From<&Function> for FunctionSignature {
    fn from(value: &Function) -> Self {
        Self {
            params: value.params.iter().map(|param| param.typ).collect(),
            return_type: value.return_type,
            name: value.name,
            kind: value.kind,
            is_const: value.is_const,
            inline: value.inline,
        }
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
    use lasso::Key;

    #[test]
    fn unknown_identifier() {
        let statements = Parser::new("fn main() { x + 1; }").parse().unwrap();
        let err = super::lower(statements).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::UndeclaredIdentifier { name: "x".to_string() })
    }

    #[test]
    fn mutability() {
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

        let err = super::lower(statements).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "x".into() });

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

        assert!(super::lower(statements).is_ok());
    }

    #[test]
    fn while_condition_must_be_bool() {
        let statements = Parser::new(
            r#"
            fn main() {
                let x: i32 = 1;
                while x { }
            }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: Type::new(TypeKind::Bool),
                found: Type::new(TypeKind::I32)
            }
        )
    }

    #[test]
    fn bitwise_and_shifts_typechecking() {
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
        assert!(super::lower(statements).is_ok());

        let source_err_shift = r#"
            fn main() {
                let a: bool = true;
                let b: i32 = 2;
                let c: bool = a << b;
            }
        "#;
        let statements = Parser::new(source_err_shift).parse().unwrap();
        let err = super::lower(statements).unwrap_err();
        assert!(matches!(err.kind, HirErrorKind::TypeMismatch { .. }));

        let source_err_not = r#"
            fn main() {
                let a: f64 = 1.0;
                let b: f64 = !a;
            }
        "#;
        let statements = Parser::new(source_err_not).parse().unwrap();
        let err = super::lower(statements).unwrap_err();
        assert!(matches!(err.kind, HirErrorKind::TypeMismatch { .. }));
    }

    #[test]
    fn if_condition_must_be_bool() {
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

        let err = super::lower(statements).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: Type::new(TypeKind::Bool),
                found: Type::new(TypeKind::I64)
            }
        )
    }

    #[test]
    fn duplicated_function() {
        let statements = Parser::new(
            r#"
            fn foo(): i32 { 1 }
            fn foo(): i32 { 2 }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::DuplicateFunction { name: "foo".into() });
    }

    #[test]
    fn arity_mismatch_too_many() {
        let statements = Parser::new(
            r#"
            fn add(a: i32, b: i32): i32 { a + b }
            fn main() { add(1, 2, 3); }
        "#,
        )
        .parse()
        .unwrap();

        let err = super::lower(statements).unwrap_err();

        assert_eq!(
            err.kind,
            HirErrorKind::ArityMismatch { name: "nyx::add".into(), expected: 2, found: 3 }
        );
    }

    #[test]
    fn unknown_function() {
        let statements = Parser::new("fn main() { foo(); }").parse().unwrap();

        let err = super::lower(statements).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::UnknownFunction { name: "foo".into() });
    }

    #[test]
    fn type_mismatch_in_let() {
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

        let err = super::lower(statements).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: Type::new(TypeKind::Bool),
                found: Type::new(TypeKind::I32)
            }
        )
    }

    #[test]
    fn type_inference_from_expr() {
        let statements = Parser::new("fn main() { let x = 1 + 2; }").parse().unwrap();
        let hir = super::lower(statements);

        // TODO: exact assertion here of expected state
        assert!(hir.is_ok())
    }

    #[test]
    fn top_level_non_function() {
        let statements = Parser::new("let x: i64 = 1;").parse().unwrap();
        let err = super::lower(statements).unwrap_err();

        assert_eq!(err.kind, HirErrorKind::TopLevelNonFunction)
    }

    #[test]
    fn integer_literal_as_function_arg_typed_i64() {
        let statements = Parser::new(
            r#"
            fn foo(x: i64): i64 { x }
            fn main() { foo(1); }
        "#,
        )
        .parse()
        .unwrap();

        let hir = super::lower(statements).unwrap();

        assert_eq!(hir.functions.len(), 2);
        let foo = &hir.functions[0];
        assert_eq!(foo.return_type, Type::new(TypeKind::I64));
        assert_eq!(foo.params.len(), 1);
        assert_eq!(foo.params[0].typ, Type::new(TypeKind::I64));

        let main = &hir.functions[1];
        let call_expr = match &main.body.statements[0] {
            Statement::Expr(expr) => expr,
            other => panic!("expected Expr statement, got {other:?}"),
        };
        assert_eq!(call_expr.typ, Type::new(TypeKind::I64));
        let arg = match &call_expr.kind {
            ExpressionKind::Call { args, .. } => {
                assert_eq!(args.len(), 1);
                &args[0]
            },
            other => panic!("expected Call expression, got {other:?}"),
        };
        assert_eq!(arg.typ, Type::new(TypeKind::I64));
        assert!(matches!(arg.kind, ExpressionKind::Integer(1)));
    }

    #[test]
    fn float_literal_defaults_to_f64() {
        let statements = Parser::new("fn main() { let x = 3.14; }").parse().unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, Type::new(TypeKind::F64));
    }

    #[test]
    fn integer_literal_defaults_to_i32_in_binary_expr() {
        let statements = Parser::new("fn main() { let x = 1 + 2; }").parse().unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, Type::new(TypeKind::I32));

        let stmt = &func.body.statements[0];
        assert!(matches!(stmt, Statement::LetInit { id: LocalId(0), .. }));
    }

    #[test]
    fn float_literal_widens_to_f32() {
        let statements = Parser::new("fn main() { let x: f32 = 3.14; }").parse().unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, Type::new(TypeKind::F32));
    }

    #[test]
    fn mutable_assign_widens_literal() {
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
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, Type::new(TypeKind::I64));
        assert_eq!(func.locals[0].mutable, true);

        let assign_expr = match &func.body.statements[1] {
            Statement::Expr(expr) => expr,
            other => panic!("expected Expr statement, got {other:?}"),
        };
        assert_eq!(assign_expr.typ, Type::new(TypeKind::I64));
        let (target_id, value) = match &assign_expr.kind {
            ExpressionKind::Assign { target, value } => match &target.kind {
                PlaceKind::Local(id) => (*id, value.as_ref()),
                _ => panic!("expected local assignment target"),
            },
            other => panic!("expected Assign expression, got {other:?}"),
        };

        assert_eq!(target_id, LocalId(0));
        assert_eq!(value.typ, Type::new(TypeKind::I64));
        assert!(matches!(value.kind, ExpressionKind::Integer(99)));
    }

    #[test]
    fn integer_literal_widens_in_binary_with_i64_local() {
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
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 2);
        assert_eq!(func.locals[0].typ, Type::new(TypeKind::I64));
        assert_eq!(func.locals[1].typ, Type::new(TypeKind::I64));

        let y_stmt = &func.body.statements[1];
        assert!(matches!(y_stmt, Statement::LetInit { id: LocalId(1), .. }));
    }

    #[test]
    fn new_integer_types_accepted() {
        let src = r#"
            fn bytes(a: i8, b: u8, c: i16, d: u16): i32 {
                0
            }
        "#;

        assert!(super::lower(Parser::new(src).parse().unwrap()).is_ok());
    }

    #[test]
    fn integer_literal_widens() {
        let src = r#"
            fn main() {
                let x: i16 = 100;
                let y: u8 = 42;
            }
        "#;

        assert!(super::lower(Parser::new(src).parse().unwrap()).is_ok());
    }

    #[test]
    fn uptr_iptr_type_resolution() {
        let src = r#"
            fn main() {
                let a: uptr = 10;
                let b: iptr = 20;
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.locals[0].typ, Type::new(TypeKind::Uptr));
        assert_eq!(func.locals[1].typ, Type::new(TypeKind::Iptr));
    }

    #[test]
    fn uptr_iptr_literal_widening() {
        let src = r#"
            fn main() {
                let a: uptr = 100;
                let b: iptr = 200;
            }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];

        let init_a = match &func.body.statements[0] {
            Statement::LetInit { init: e, .. } => e,
            other => panic!("expected Let with init, got {other:?}"),
        };
        assert_eq!(init_a.typ, Type::new(TypeKind::Uptr));
        assert!(matches!(init_a.kind, ExpressionKind::Integer(100)));

        let init_b = match &func.body.statements[1] {
            Statement::LetInit { init: e, .. } => e,
            other => panic!("expected Let with init, got {other:?}"),
        };
        assert_eq!(init_b.typ, Type::new(TypeKind::Iptr));
        assert!(matches!(init_b.kind, ExpressionKind::Integer(200)));
    }

    #[test]
    fn uptr_arithmetic() {
        let src = r#"
            fn add(a: uptr, b: uptr): uptr { a + b }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.return_type, Type::new(TypeKind::Uptr));
        assert_eq!(func.params[0].typ, Type::new(TypeKind::Uptr));
        assert_eq!(func.params[1].typ, Type::new(TypeKind::Uptr));
    }

    #[test]
    fn iptr_arithmetic() {
        let src = r#"
            fn scale(base: iptr, factor: iptr): iptr { base * factor }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.return_type, Type::new(TypeKind::Iptr));
        assert_eq!(func.params[0].typ, Type::new(TypeKind::Iptr));
        assert_eq!(func.params[1].typ, Type::new(TypeKind::Iptr));
    }

    #[test]
    fn uptr_while_comparison() {
        let src = r#"
            fn triangle(limit: uptr): uptr {
                let mut acc: uptr = 0;
                let mut i: uptr = 1;
                while i <= limit {
                    acc = acc + i;
                    i = i + 1;
                }
                acc
            }
        "#;

        assert!(super::lower(Parser::new(src).parse().unwrap()).is_ok());
    }

    #[test]
    fn uptr_iptr_mixed_type_mismatch() {
        let src = r#"
            fn main() {
                let a: uptr = 1;
                let b: iptr = a;
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::TypeMismatch {
                expected: Type::new(TypeKind::Iptr),
                found: Type::new(TypeKind::Uptr)
            }
        );
    }

    #[test]
    fn struct_fields_remain_in_source_order() {
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

        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();

        assert_eq!(hir.structs.len(), 1);
        let field_names: Vec<_> = hir.structs[0]
            .fields
            .iter()
            .map(|field| hir.symbols[field.name.0.into_usize()].as_str())
            .collect();
        assert_eq!(field_names, vec!["a", "b", "c"]);

        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, Type::new(TypeKind::Struct(StructId(0))));
    }

    #[test]
    fn nested_struct_fields_are_resolved() {
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

        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        assert_eq!(hir.structs.len(), 2);

        let outer_inner = hir.structs[1]
            .fields
            .iter()
            .find(|field| hir.symbols[field.name.0.into_usize()] == "inner")
            .unwrap();
        assert_eq!(outer_inner.typ, Type::new(TypeKind::Struct(StructId(0))));
    }

    #[test]
    fn circular_structs_are_rejected() {
        let src = r#"
            struct A {
                b: B,
            }

            struct B {
                a: A,
            }

            fn main() { }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::CircularStruct { name: "A".to_string() });
    }

    #[test]
    fn struct_literal_requires_all_fields() {
        let src = r#"
            struct Point {
                x: i32,
                y: i32,
            }

            fn main() {
                let point = Point { x: 1 };
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::MissingField { struct_name: "Point".to_string(), field: "y".to_string() }
        );
    }

    #[test]
    fn struct_literal_rejects_unknown_field_with_span() {
        let src = "struct Point{x:i32}\nfn main(){let p=Point{z:1};}";

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::UnknownField { struct_name: "Point".to_string(), field: "z".to_string() }
        );
        assert_eq!(err.span.start.column, 23);
        assert_eq!(err.span.end.column, 26);
    }

    #[test]
    fn struct_literal_rejects_duplicate_field_with_span() {
        let src = "struct Point{x:i32}\nfn main(){let p=Point{x:1,x:2};}";

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::DuplicateField { name: "x".to_string() });
        assert_eq!(err.span.start.column, 27);
        assert_eq!(err.span.end.column, 30);
    }

    #[test]
    fn immutable_field_assignment_reports_assignment_span() {
        let src = "struct Point{x:i32}\nfn main(){let p=Point{x:1};p.x=2;}";

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "p".to_string() });
        assert_eq!(err.span.start.column, 28);
        assert_eq!(err.span.end.column, 31);
    }

    #[test]
    fn chained_field_access() {
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

        assert!(super::lower(Parser::new(src).parse().unwrap()).is_ok());
    }

    #[test]
    fn impl_blocks_collect_methods_for_same_struct() {
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

        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        assert_eq!(hir.functions.len(), 3);
        assert!(hir.functions.iter().any(|f| matches!(f.kind, FunctionKind::Method(_))));
    }

    #[test]
    fn duplicate_methods_across_impl_blocks_are_rejected() {
        let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn value(&self): i32 { self.value }
            }

            impl Counter {
                fn value(&self): i32 { self.value }
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(
            err.kind,
            HirErrorKind::DuplicateMethod {
                struct_name: "Counter".to_string(),
                name: "value".to_string(),
            }
        );
        assert_eq!(err.span.start.column, 17);
    }

    #[test]
    fn mut_self_method_requires_mutable_receiver() {
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

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "counter".to_string() });
    }

    #[test]
    fn shared_self_cannot_assign_fields() {
        let src = r#"
            struct Counter { value: i32 }

            impl Counter {
                fn set(&self, value: i32) {
                    self.value = value;
                }
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::ImmutableBind { name: "self".to_string() });
    }

    #[test]
    fn wrong_interface_parameters_impl() {
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

        let err = super::lower(Parser::new(src).parse().unwrap()).err().expect("known bug");
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
        let src = r#"
            impl i64 {
                fn val(&self): i64 { *self }
            }
        "#;

        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::OrphanImpl { name: "i64".to_string() });
    }

    #[test]
    fn const_top_level() {
        let src = r#"
            const ANSWER: i32 = 42;
            fn main(): i32 {
                ANSWER
            }
        "#;
        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(ret_expr.typ, Type::new(TypeKind::I32));
        assert!(matches!(ret_expr.kind, ExpressionKind::Integer(42)));
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
        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(ret_expr.typ, Type::new(TypeKind::Uptr));
        assert!(matches!(ret_expr.kind, ExpressionKind::Integer(127)));
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
        let mut symbols = SymbolTable::new();
        let mut statements = Parser::new(src).parse().unwrap();
        let declarations = Declarations::partition(&mut statements, |_| None).unwrap();
        let mut scope = Scope::new();
        scope.extend(&declarations, &mut symbols, true).unwrap();
        let functions = scope.lower_functions(&declarations, &mut symbols, true).unwrap();
        let main_func = &functions[0];
        let ret_expr = match &main_func.body.statements[0] {
            Statement::Return(Some(expr)) => expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(ret_expr.typ, Type::new(TypeKind::Uptr));
        assert!(matches!(ret_expr.kind, ExpressionKind::Integer(127)));
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
        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[0] {
            Statement::Return(Some(expr)) => expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(ret_expr.typ, Type::new(TypeKind::I32));
        match &ret_expr.kind {
            ExpressionKind::Binary { left, operator, right } => {
                assert_eq!(*operator, BinaryOperator::Add);
                assert!(matches!(left.kind, ExpressionKind::Integer(10)));
                assert!(matches!(right.kind, ExpressionKind::Integer(2)));
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
        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert!(matches!(
            err.kind,
            HirErrorKind::CircularConstant { ref name } if name == "A" || name == "B"
        ));
    }

    #[test]
    fn const_duplicate_declaration() {
        let src = r#"
            const X: i32 = 1;
            const X: i32 = 2;
            fn main() {}
        "#;
        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::DuplicateConstant { name: "X".to_string() });
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
        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::DuplicateConstant { name: "Dummy::VALUE".to_string() });
    }

    #[test]
    fn const_undefined_reference() {
        let src = r#"
            const A: i32 = UNDEFINED;
            fn main() {}
        "#;
        let err = super::lower(Parser::new(src).parse().unwrap()).unwrap_err();
        assert_eq!(err.kind, HirErrorKind::UndeclaredIdentifier { name: "UNDEFINED".to_string() });
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
        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];
        let ret_expr = match &func.body.statements[1] {
            Statement::Return(Some(expr)) => expr,
            other => panic!("expected Return statement, got {other:?}"),
        };
        assert_eq!(ret_expr.typ, Type::new(TypeKind::I32));
        assert!(matches!(ret_expr.kind, ExpressionKind::Local(_)));
    }

    #[test]
    fn type_size_of() {
        assert_eq!(size_of::<Type>(), 8);
    }
}
