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
#[derive(Debug, PartialEq)]
pub struct Mir {
    pub(crate) symbols: Vec<String>,
    pub(crate) functions: Vec<Function>,
}

/// Single side-effecting or value-producing operation.
///
/// Every instruction has the form `dest = <rhs>`
/// where `dest` is the [place](self::Place) that receives the result.
/// Instructions never nest.
#[derive(Debug, PartialEq, Clone)]
pub struct Instruction {
    pub(crate) dest: Place,
    pub(crate) kind: InstructionKind,
}

#[derive(Debug, PartialEq)]
pub struct Function {
    id: FunctionId,
    /// index into `Mir::symbols` giving function's source name
    pub(crate) name_symbol: usize,
    return_type: Type,
    /// params in declaration order.
    /// these are the first entries of `locals` but are kept separated
    /// so that codegen can emit the correct argument-register moves without
    /// having to guess which locals were params
    pub(crate) params: Vec<(ValueId, Type)>,
    pub(crate) locals: Vec<(ValueId, Type)>,
    pub(crate) blocks: Vec<Block>,
}

/// A single-entry, single-exit sequence of instructions.
///
/// All instructions execute Unconditionally top-to-bottom. Only one [terminator](self::Terminator) can transfer the
/// control.
/// This invariant allow code generation to translate blocks independently.
#[derive(Debug, PartialEq)]
pub struct Block {
    id: BlockId,
    pub(crate) instructions: Vec<Instruction>,
    pub(crate) terminator: Terminator,
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
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct ValueId(pub u32);

/// Stable index into a function's `blocks` vec.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct BlockId(pub u32);

impl InstructionKind {
    #[inline(always)]
    pub fn uses_of(&self) -> Vec<ValueId> {
        match self {
            Self::Assign(op) => op.value().into_iter().collect(),
            Self::Unary { rhs, .. } => rhs.value().into_iter().collect(),
            Self::Binary { lhs, rhs, .. } => {
                [lhs.value(), rhs.value()].into_iter().flatten().collect()
            }
            Self::Call { args, .. } => args.iter().filter_map(|op| op.value()).collect(),
        }
    }
}

impl Terminator {
    #[inline(always)]
    pub fn uses_of(&self) -> Vec<ValueId> {
        match self {
            Terminator::Return(Some(op)) => op.value().into_iter().collect(),
            Terminator::Branch { condition, .. } => condition.value().into_iter().collect(),
            Terminator::Return(None) | Terminator::Jump(_) => vec![],
        }
    }
}

impl Operand {
    pub const fn typ(&self) -> Type {
        match self {
            Self::Place(p) => p.typ,
            Self::Const(c) => c.typ(),
        }
    }

    #[inline(always)]
    pub const fn value(&self) -> Option<ValueId> {
        match self {
            Self::Place(p) => Some(p.id),
            _ => None,
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

    #[test]
    fn let_binding_and_return() {
        let mir = parse_and_lower("fn foo(): i32 { let x: i32 = 42; x }");
        let f = &mir.functions[0];
        assert_eq!(f.return_type, Type::I32);

        println!("mir: {mir:?}");

        let assigns: Vec<_> = f.blocks[0]
            .instructions
            .iter()
            .filter(|i| matches!(i.kind, InstructionKind::Assign(_)))
            .collect();
        assert!(
            !assigns.is_empty(),
            "expected at least one Assign instruction"
        );

        assert!(f.locals.iter().any(|(_, t)| *t == Type::I32));
    }

    #[test]
    fn call_produces_call_instruction() {
        let mir = parse_and_lower(
            r#"
            fn add(a: i32, b: i32): i32 { a + b }
            fn main() { add(1, 2); }
        "#,
        );

        let main = &mir.functions[1];
        let has_call = main.blocks[0]
            .instructions
            .iter()
            .any(|i| matches!(i.kind, InstructionKind::Call { .. }));

        assert!(has_call, "expected a Call instruction in main");
    }

    #[test]
    fn binary_expression_lowers_to_instructions() {
        let mir = parse_and_lower("fn add(a: i32, b: i32): i32 { a + b }");
        let f = &mir.functions[0];

        assert!(f.locals.len() >= 2);
        assert!(f.locals.iter().all(|(_, t)| *t == Type::I32));

        let has_add = f.blocks[0].instructions.iter().any(|i| {
            matches!(
                i.kind,
                InstructionKind::Binary {
                    operation: BinaryOperator::Add,
                    ..
                }
            )
        });
        assert!(has_add, "expected Binary(Add) instruction");

        assert!(matches!(
            f.blocks[0].terminator,
            Terminator::Return(Some(_))
        ));
    }
}
