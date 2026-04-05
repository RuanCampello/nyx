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

pub mod error;
pub use lower::lower;
mod lower;

/// Complete MIR program.
/// That's a flat list of functions.
///
/// `symbols` are carried through from HIR for diagostic/debug use.
pub struct Mir {
    symbols: Vec<String>,
    functions: Vec<Function>,
}

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

pub struct Function {
    id: FunctionId,
    return_type: Type,
    locals: Vec<(ValueId, Type)>,
    blocks: Vec<Block>,
}

/// A single-entry, single-exit sequence of instructions.
///
/// All instructions execute Unconditionally top-to-bottom. Only one [terminator](self::Terminator) can transfer the
/// control.
/// This invariant allow code generation to translate blocks independently.
pub struct Block {
    id: BlockId,
    instructions: Vec<Instruction>,
    terminator: Terminator,
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

/// The last instruction of a [basic block](self::Block). Always exactly one per block.
/// Terminator's are the *only* place where control flow is expressed.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Terminator {
    /// Unconditional jump
    Jump(BlockId),

    /// Conditional branch: if `condition` is true
    Branch {
        condition: Operand,
        then_block: BlockId,
        else_block: BlockId,
    },

    /// Return from the function, optionally carrying a returned value
    Return(Option<Operand>),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hir, mir, parser::Parser};

    fn parse_and_lower(src: &str) -> Mir {
        let statements = Parser::new(src).parse().unwrap();
        let hir = hir::lower(statements).unwrap();

        mir::lower(hir).unwrap()
    }

    #[test]
    fn trivial_return_unit() {
        let mir = parse_and_lower("fn main() { }");
        assert_eq!(mir.functions.len(), 1);

        let function = &mir.functions[0];

        assert_eq!(function.return_type, Type::Unit);
        assert_eq!(function.blocks.len(), 1);
        assert_eq!(function.blocks[0].instructions.len(), 0);
        assert_eq!(function.blocks[0].terminator, Terminator::Return(None));
    }
}
