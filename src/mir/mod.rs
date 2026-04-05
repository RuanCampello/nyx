//! Mid-level IR (MIR).
//!
//! That's a control flow graph with basic blocks.
//!
//! The MIR flattens the MIR tree into a CFG whose nodes are *blocks* and whose
//! edges are explicit control-flow transfers.
//! Every basic block is a linear sequence of
//! three-address instructions that always execute top-to-bottom, terminated by exactly
//! one *terminator* that transfers the control elsewhere.
//!
//! ## Three-address form
//!
//! Every instruction does exactly one thing:
//!   `t2 = t0 + t1`  — binary op
//!   `t3 = -t2`      — unary op
//!   `t4 = call foo(t0, t1)` — call
//!   `t5 = 42`       — copy from immediate
//!   `x  = t5`       — copy to named local
//!

use crate::{
    hir::{FunctionId, Type},
    parser::expression::{BinaryOperator, UnaryOperator},
};

/// Single side-effecting or value-producing operation.
///
/// Every instruction has the form `dest = <rhs>`
/// where `dest` is the [place](self::Place) that receives the result.
/// Instructions never nest.
#[derive(Debug, PartialEq, Clone)]
pub struct Instruction {
    dest: Place,
    kind: InstructionKind,
}

#[derive(Debug, PartialEq, Clone)]
pub enum InstructionKind {
    /// `dest = operand` (copy or constant load)
    Assign(Operand),

    Unary {
        operation: UnaryOperator,
        rhs: Operand,
    },

    Binary {
        operation: BinaryOperator,
        rhs: Operand,
        lhs: Operand,
    },

    Call {
        callee: FunctionId,
        args: Vec<Operand>,
    },
}

/// This is a *input* of a instruction.
///
/// Instructions consume operands.
/// Operands are atomic: either a named local/temporary or inlined constant.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Operand {
    Place(Place),
    Const(Const),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Place {
    pub id: ValueId,
    pub typ: Type,
}

/// An inlined constant.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Const {
    Int(i64, Type),
    Float(f64, Type),
    Bool(bool),
    Unit,
}

/// An assigned unique value id.
/// This covers both source locals (LocalId) and fresh temporaries
/// introduced during expresion lowering.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct ValueId(pub u32);

/// Stable index into a function's `blocks` vec.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct BlockId(pub u32);

impl Operand {
    pub const fn typ(&self) -> Type {
        match self {
            Self::Place(p) => p.typ,
            Self::Const(c) => c.typ(),
        }
    }
}

impl Const {
    pub const fn typ(&self) -> Type {
        match self {
            Self::Int(_, typ) => *typ,
            Self::Float(_, typ) => *typ,
            Self::Bool(_) => Type::Bool,
            Self::Unit => Type::Unit,
        }
    }
}
