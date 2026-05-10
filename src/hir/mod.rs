//! High-level IR (HIR) produced by semantic analysis.
//!
//! HIR is a tree-structured, fully resolved and typed.
//! Identifiers are lowered to stable numeric IDs.

use crate::{
    hir::{
        error::{HirError, HirErrorKind},
        functions::{FunctionBuilder, FunctionSignature},
        symbols::SymbolTable,
    },
    lexer::token::Span,
    parser::{
        expression::{BinaryOperator, UnaryOperator},
        statement,
    },
};
use lasso::{Key, Spur};
use std::str::FromStr;

pub mod error;
mod functions;
pub(crate) mod module;
mod symbols;

#[derive(Debug, Clone, PartialEq)]
pub struct Hir {
    pub symbols: Vec<String>,
    pub structs: Vec<Struct>,
    pub functions: Vec<Function>,
}

// PERFORMANCE: I probably don't need a u32 right here for size and alignmnet
// but I need to reavaliate it

#[derive(Debug, Clone, PartialEq)]
pub struct Struct {
    id: StructId,
    name: SymbolId,
    pub(crate) fields: Vec<StructField>,
    pub(crate) size: u32,
    pub(crate) align: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
    pub(crate) name: SymbolId,
    pub(crate) typ: Type,
    pub(crate) offset: u32,
    declared_index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Type {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    Bool,
    Uptr,
    Iptr,
    Char,
    Str,
    String,
    Struct(StructId),
    Unit,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructId(pub u32);

#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Let {
        id: LocalId,
        init: Option<Expression>,
    },
    Expr(Expression),
    Return(Option<Expression>),
    If {
        condition: Expression,
        then_block: Block,
        else_block: Option<Block>,
    },
    While {
        condition: Expression,
        body: Block,
    },
    Block(Block),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    pub(crate) kind: ExpressionKind,
    pub(crate) typ: Type,
    span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub id: FunctionId,
    pub name: SymbolId,
    pub params: Vec<Parameter>,
    pub locals: Vec<Local>,
    pub return_type: Type,
    pub is_const: bool,
    pub inline: bool,
    pub is_pub: bool,
    pub intrinsic: Option<Intrinsic>,
    pub body: Block,
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
pub enum ExpressionKind {
    #[allow(dead_code)]
    Unit,
    Integer(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Local(LocalId),
    Unary {
        operator: UnaryOperator,
        expr: Box<Expression>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Assign {
        target: LocalId,
        value: Box<Expression>,
    },
    FieldAccess {
        local: LocalId,
        fields: Vec<SymbolId>,
        typ: Type,
    },
    FieldAssign {
        local: LocalId,
        fields: Vec<SymbolId>,
        value: Box<Expression>,
    },
    Struct {
        id: StructId,
        fields: Vec<(SymbolId, Expression)>,
    },
    Call {
        function: FunctionId,
        args: Vec<Expression>,
        inline: bool,
    },
    IntrinsicCall {
        intrinsic: Intrinsic,
        args: Vec<Expression>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    PrintLn,
    Print,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub Spur);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

// FIXME: maybe I should reavaliate latter using a better way to not
// need re-index this thing
// probably I could solve it by a stable index not a sequential one but be it for now :X

pub(crate) trait Offset {
    /// remaps module-local into merged index space by applying a per-module offset
    fn offset(&mut self, offset: u32);
}

/// Lowers the program AST to a HIR program.
pub fn lower<'h>(statements: Vec<statement::Statement<'h>>) -> Result<Hir, HirError<'h>> {
    let mut symbols = SymbolTable::new();
    let (structs, structs_map) = functions::collect_structs(&statements, &mut symbols)?;
    let (signatures, functions_map) =
        functions::collect_function_signatures(&statements, &mut symbols, &structs_map)?;

    let mut functions = Vec::new();
    for statement in statements {
        let function = match statement {
            statement::Statement::Fn(function) => function,
            statement::Statement::Struct(_) => continue,
            // 'use' declarations are valid at the top level but have no HIR representation
            // symbol resolution happens at the module loader level, not here
            statement::Statement::Use(_) => continue,
            other => {
                return Err(HirError {
                    kind: HirErrorKind::TopLevelNonFunction,
                    span: other.span(),
                });
            }
        };

        let function = FunctionBuilder::new(
            &signatures,
            &functions_map,
            &structs,
            &structs_map,
            &mut symbols,
            function,
        );
        functions.push(function.lower()?);
    }

    Ok(Hir {
        symbols: symbols.into_symbols(),
        structs,
        functions,
    })
}

impl From<&Function> for FunctionSignature {
    fn from(value: &Function) -> Self {
        Self {
            params: value.params.iter().map(|param| param.typ).collect(),
            return_type: value.return_type,
            name: value.name,
            is_const: value.is_const,
            inline: value.inline,
            is_pub: value.is_pub,
            intrinsic: value.intrinsic,
        }
    }
}

impl Type {
    pub(in crate::hir) const fn is_number(&self) -> bool {
        self.is_integer() || self.is_float()
    }

    pub(in crate::hir) const fn is_integer(&self) -> bool {
        matches!(
            self,
            Self::I8
                | Self::U8
                | Self::I16
                | Self::U16
                | Self::I32
                | Self::U32
                | Self::I64
                | Self::U64
                | Self::Iptr
                | Self::Uptr
        )
    }

    pub const fn is_float(&self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    pub const fn is_32_bit(&self) -> bool {
        matches!(self, Self::F32 | Self::I32 | Self::U32)
    }

    #[allow(unused)]
    pub(in crate::hir) const fn is_signed(&self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I32 | Self::I64 | Self::Iptr
        )
    }

    #[allow(unused)]
    pub(in crate::hir) const fn is_unsigned(&self) -> bool {
        matches!(
            self,
            Self::U8 | Self::U16 | Self::U32 | Self::U64 | Self::Uptr
        )
    }

    #[inline(always)]
    /// returns (size, alignment) of the type
    const fn layout(&self, structs: &[Option<Struct>]) -> (u32, u32) {
        match self {
            Type::I8 | Type::U8 | Type::Bool | Type::Char => (1, 1),
            Type::I16 | Type::U16 => (2, 2),
            Type::I32 | Type::U32 | Type::F32 => (4, 4),
            Type::I64
            | Type::U64
            | Type::Iptr
            | Type::Uptr
            | Type::Str
            | Type::String
            | Type::F64 => (8, 8),
            Type::Unit => (0, 1),

            Type::Struct(id) => {
                let definition =
                    structs[id.0 as usize].as_ref().expect("dependent struct is already lowered");

                (definition.size, definition.align)
            }
        }
    }
}

impl Offset for Type {
    #[inline(always)]
    fn offset(&mut self, offset: u32) {
        if let Type::Struct(id) = *self {
            *self = Type::Struct(StructId(id.0 + offset))
        }
    }
}

impl Offset for Struct {
    fn offset(&mut self, offset: u32) {
        self.id = StructId(self.id.0 + offset);

        for StructField { typ, .. } in &mut self.fields {
            typ.offset(offset);
        }
    }
}

impl Offset for Expression {
    fn offset(&mut self, offset: u32) {
        self.typ.offset(offset);

        match &mut self.kind {
            ExpressionKind::Unit
            | ExpressionKind::Integer(_)
            | ExpressionKind::Float(_)
            | ExpressionKind::Bool(_)
            | ExpressionKind::String(_)
            | ExpressionKind::Local(_) => {}

            ExpressionKind::Unary { expr, .. } => expr.offset(offset),
            ExpressionKind::Binary { left, right, .. } => {
                left.offset(offset);
                right.offset(offset);
            }

            ExpressionKind::Assign { value, .. } => value.offset(offset),
            ExpressionKind::FieldAssign { value, .. } => value.offset(offset),
            ExpressionKind::FieldAccess { typ, .. } => typ.offset(offset),
            ExpressionKind::Struct { fields, id } => {
                *id = StructId(id.0 + offset);

                for (_, value) in fields {
                    value.offset(offset);
                }
            }
            ExpressionKind::Call { args, .. } | ExpressionKind::IntrinsicCall { args, .. } => {
                for arg in args {
                    arg.offset(offset);
                }
            }
        }
    }
}

impl Offset for Block {
    fn offset(&mut self, offset: u32) {
        for statement in &mut self.statements {
            match statement {
                Statement::Expr(expr)
                | Statement::Return(Some(expr))
                | Statement::Let {
                    init: Some(expr), ..
                } => {
                    expr.offset(offset);
                }
                Statement::Return(None) | Statement::Let { init: None, .. } => {}
                Statement::Block(block) => block.offset(offset),
                Statement::While { condition, body } => {
                    condition.offset(offset);
                    body.offset(offset);
                }
                Statement::If {
                    condition,
                    then_block,
                    else_block,
                } => {
                    condition.offset(offset);
                    then_block.offset(offset);
                    if let Some(else_block) = else_block {
                        else_block.offset(offset);
                    }
                }
            }
        }
    }
}

impl Offset for Function {
    fn offset(&mut self, offset: u32) {
        self.return_type.offset(offset);

        for param in &mut self.params {
            param.typ.offset(offset);
        }

        for local in &mut self.locals {
            local.typ.offset(offset);
        }

        self.body.offset(offset);
    }
}

impl From<&statement::Type<'_>> for Type {
    fn from(value: &statement::Type<'_>) -> Self {
        use statement::Type as AstType;

        match value {
            AstType::I8 => Type::I8,
            AstType::U8 => Type::U8,
            AstType::I16 => Type::I16,
            AstType::U16 => Type::U16,
            AstType::I32 => Type::I32,
            AstType::U32 => Type::U32,
            AstType::I64 => Type::I64,
            AstType::U64 => Type::U64,
            AstType::F32 => Type::F32,
            AstType::F64 => Type::F64,
            AstType::Bool => Type::Bool,
            AstType::Uptr => Type::Uptr,
            AstType::Iptr => Type::Iptr,
            AstType::Char => Type::Char,
            AstType::Str => Type::Str,
            AstType::String => Type::String,
            AstType::Unit => Type::Unit,
            AstType::Named(_) => unreachable!("already resolved by resolve_type"),
        }
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Type::I8 => "i8",
            Type::U8 => "u8",
            Type::I16 => "i16",
            Type::U16 => "u16",
            Type::I32 => "i32",
            Type::U32 => "u32",
            Type::I64 => "i64",
            Type::U64 => "u64",
            Type::F32 => "f32",
            Type::F64 => "f64",
            Type::Bool => "bool",
            Type::Char => "char",
            Type::Uptr => "uptr",
            Type::Iptr => "iptr",
            Type::Str => "&str",
            Type::String => "String",
            Type::Struct(id) => return write!(f, "struct#{}", id.0),
            Type::Unit => "unit",
        };

        f.write_str(s)
    }
}

impl FromStr for Intrinsic {
    type Err = ();

    fn from_str(str: &str) -> Result<Self, Self::Err> {
        Ok(match str {
            "println" => Self::PrintLn,
            "print" => Self::Print,
            "exit" => Self::Exit,

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
    use crate::parser::Parser;
    use lasso::Key;

    #[test]
    fn unknown_identifier() {
        let statements = Parser::new("fn main() { x + 1; }").parse().unwrap();
        let err = super::lower(statements).unwrap_err();

        assert_eq!(
            err.kind,
            HirErrorKind::UndeclaredIdentifier {
                name: "x".to_string()
            }
        )
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
                expected: Type::Bool,
                found: Type::I32
            }
        )
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
                expected: Type::Bool,
                found: Type::I64
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

        assert_eq!(
            err.kind,
            HirErrorKind::DuplicateFunction { name: "foo".into() }
        );
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
            HirErrorKind::ArityMismatch {
                name: "add".into(),
                expected: 2,
                found: 3,
            }
        );
    }

    #[test]
    fn unknown_function() {
        let statements = Parser::new("fn main() { foo(); }").parse().unwrap();

        let err = super::lower(statements).unwrap_err();

        assert_eq!(
            err.kind,
            HirErrorKind::UnknownFunction { name: "foo".into() }
        );
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
                expected: Type::Bool,
                found: Type::I32
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
        assert_eq!(foo.return_type, Type::I64);
        assert_eq!(foo.params.len(), 1);
        assert_eq!(foo.params[0].typ, Type::I64);

        let main = &hir.functions[1];
        let call_expr = match &main.body.statements[0] {
            Statement::Expr(expr) => expr,
            other => panic!("expected Expr statement, got {other:?}"),
        };
        assert_eq!(call_expr.typ, Type::I64);
        let arg = match &call_expr.kind {
            ExpressionKind::Call { args, .. } => {
                assert_eq!(args.len(), 1);
                &args[0]
            }
            other => panic!("expected Call expression, got {other:?}"),
        };
        assert_eq!(arg.typ, Type::I64);
        assert!(matches!(arg.kind, ExpressionKind::Integer(1)));
    }

    #[test]
    fn float_literal_defaults_to_f64() {
        let statements = Parser::new("fn main() { let x = 3.14; }").parse().unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, Type::F64);
    }

    #[test]
    fn integer_literal_defaults_to_i32_in_binary_expr() {
        let statements = Parser::new("fn main() { let x = 1 + 2; }").parse().unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, Type::I32);

        let stmt = &func.body.statements[0];
        assert!(matches!(stmt, Statement::Let { id: LocalId(0), .. }));
    }

    #[test]
    fn float_literal_widens_to_f32() {
        let statements = Parser::new("fn main() { let x: f32 = 3.14; }").parse().unwrap();
        let hir = super::lower(statements).unwrap();

        let func = &hir.functions[0];
        assert_eq!(func.locals.len(), 1);
        assert_eq!(func.locals[0].typ, Type::F32);
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
        assert_eq!(func.locals[0].typ, Type::I64);
        assert_eq!(func.locals[0].mutable, true);

        let assign_expr = match &func.body.statements[1] {
            Statement::Expr(expr) => expr,
            other => panic!("expected Expr statement, got {other:?}"),
        };
        assert_eq!(assign_expr.typ, Type::I64);
        let (target_id, value) = match &assign_expr.kind {
            ExpressionKind::Assign { target, value } => (target, value.as_ref()),
            other => panic!("expected Assign expression, got {other:?}"),
        };

        assert_eq!(*target_id, LocalId(0));
        assert_eq!(value.typ, Type::I64);
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
        assert_eq!(func.locals[0].typ, Type::I64);
        assert_eq!(func.locals[1].typ, Type::I64);

        let y_stmt = &func.body.statements[1];
        assert!(matches!(y_stmt, Statement::Let { id: LocalId(1), .. }));
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

        assert_eq!(func.locals[0].typ, Type::Uptr);
        assert_eq!(func.locals[1].typ, Type::Iptr);
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
            Statement::Let { init: Some(e), .. } => e,
            other => panic!("expected Let with init, got {other:?}"),
        };
        assert_eq!(init_a.typ, Type::Uptr);
        assert!(matches!(init_a.kind, ExpressionKind::Integer(100)));

        let init_b = match &func.body.statements[1] {
            Statement::Let { init: Some(e), .. } => e,
            other => panic!("expected Let with init, got {other:?}"),
        };
        assert_eq!(init_b.typ, Type::Iptr);
        assert!(matches!(init_b.kind, ExpressionKind::Integer(200)));
    }

    #[test]
    fn uptr_arithmetic() {
        let src = r#"
            fn add(a: uptr, b: uptr): uptr { a + b }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.return_type, Type::Uptr);
        assert_eq!(func.params[0].typ, Type::Uptr);
        assert_eq!(func.params[1].typ, Type::Uptr);
    }

    #[test]
    fn iptr_arithmetic() {
        let src = r#"
            fn scale(base: iptr, factor: iptr): iptr { base * factor }
        "#;

        let hir = super::lower(Parser::new(src).parse().unwrap()).unwrap();
        let func = &hir.functions[0];

        assert_eq!(func.return_type, Type::Iptr);
        assert_eq!(func.params[0].typ, Type::Iptr);
        assert_eq!(func.params[1].typ, Type::Iptr);
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
                expected: Type::Iptr,
                found: Type::Uptr,
            }
        );
    }

    #[test]
    fn struct_layout_reorders_fields_to_reduce_padding() {
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
        let layout = &hir.structs[0];
        assert_eq!(layout.size, 16);
        assert_eq!(layout.align, 8);

        let field_names: Vec<_> = layout
            .fields
            .iter()
            .map(|field| {
                (
                    hir.symbols[field.name.0.into_usize()].as_str(),
                    field.offset,
                )
            })
            .collect();
        assert_eq!(field_names, vec![("b", 0), ("c", 8), ("a", 12)]);

        let func = &hir.functions[0];
        assert_eq!(func.locals[0].typ, Type::Struct(StructId(0)));
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
        assert_eq!(hir.structs[0].size, 4);
        assert_eq!(hir.structs[1].align, 4);

        let outer_inner = hir.structs[1]
            .fields
            .iter()
            .find(|field| hir.symbols[field.name.0.into_usize()] == "inner")
            .unwrap();
        assert_eq!(outer_inner.typ, Type::Struct(StructId(0)));
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
        assert_eq!(
            err.kind,
            HirErrorKind::CircularStruct {
                name: "A".to_string()
            }
        );
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
            HirErrorKind::MissingField {
                struct_name: "Point".to_string(),
                field: "y".to_string(),
            }
        );
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
}
