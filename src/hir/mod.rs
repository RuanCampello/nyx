//! High-level IR (HIR) produced by semantic analysis.
//!
//! HIR is a tree-structured, fully resolved and typed.
//! Identifiers are lowered to stable numeric IDs.

use crate::{
    lexer::token::Span,
    parser::expression::{BinaryOperator, UnaryOperator},
};
use lasso::{Key, Spur};

pub mod error;
mod symbols;

#[derive(Debug, Clone, PartialEq)]
pub struct Hir {
    symbols: Vec<String>,
    functions: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    Let { id: LocalId },
    Expr(),
    Return(),
    If,
    While,
    Block(Block),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    kind: ExpressionKind,
    typ: Type,
    span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

impl std::fmt::Debug for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SymbolId({})", self.0.into_usize())
    }
}
