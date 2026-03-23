//! High-level IR (HIR) produced by semantic analysis.
//!
//! HIR is a tree-structured, fully resolved and typed.
//! Identifiers are lowered to stable numeric IDs.

use crate::{
    hir::{
        error::{HirError, HirErrorKind},
        symbols::SymbolTable,
    },
    lexer::token::Span,
    parser::{
        expression::{BinaryOperator, UnaryOperator},
        statement,
    },
};
use lasso::{Key, Spur};

pub mod error;
mod functions;
mod symbols;

#[derive(Debug, Clone, PartialEq)]
pub struct Hir {
    symbols: Vec<String>,
    functions: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Let {
        id: LocalId,
    },
    Expr(Expression),
    Return(Option<Expression>),
    If {
        condition: Expression,
        then_block: Block,
        else_block: Option<Block>,
    },
    While,
    Block(Block),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    kind: ExpressionKind,
    typ: Type,
    span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    id: FunctionId,
    name: SymbolId,
    params: Vec<Parameter>,
    locals: Vec<Local>,
    return_type: Type,
    block: Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    id: LocalId,
    name: SymbolId,
    typ: Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    id: LocalId,
    name: SymbolId,
    typ: Type,
    mutable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    statements: Vec<Statement>,
    span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    I32,
    I64,
    F32,
    F64,
    Bool,
    String,
    Unit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExpressionKind {
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
    Call {
        function: FunctionId,
        args: Vec<Expression>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub Spur);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// Lowers the program AST to a HIR program.
pub fn lower<'h>(statements: Vec<statement::Statement<'h>>) -> Result<Hir, HirError<'h>> {
    let mut symbols = SymbolTable::new();
    let (signatures, functions_map) =
        functions::collect_function_signatures(&statements, &mut symbols)?;

    let mut functions = Vec::new();
    for statement in statements {
        let function = match statement {
            statement::Statement::Fn(function) => function,
            other => {
                return Err(HirError {
                    kind: HirErrorKind::TopLevelNonFunction,
                });
            }
        };

        let symbol = symbols.insert(function.name);
        let id = *functions_map
            .get(&symbol)
            .expect("function id assigned during signature collection");

        todo!()
    }

    Ok(Hir {
        symbols: symbols.into_symbols(),
        functions,
    })
}

impl From<statement::Type> for Type {
    fn from(value: statement::Type) -> Self {
        Self::from(&value)
    }
}

impl From<&statement::Type> for Type {
    fn from(value: &statement::Type) -> Self {
        use statement::Type as AstType;
        match value {
            AstType::I32 { .. } => Type::I32,
            AstType::I64 { .. } => Type::I64,
            AstType::F32 { .. } => Type::F32,
            AstType::F64 { .. } => Type::F64,
            AstType::Bool { .. } => Type::Bool,
            AstType::String { .. } => Type::String,
        }
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Type::I32 => "i32",
            Type::I64 => "i64",
            Type::F32 => "f32",
            Type::F64 => "f64",
            Type::Bool => "bool",
            Type::String => "String",
            Type::Unit => "unit",
        };

        f.write_str(s)
    }
}

impl std::fmt::Debug for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SymbolId({})", self.0.into_usize())
    }
}
