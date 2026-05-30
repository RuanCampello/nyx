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
    hir::{FunctionId, Intrinsic, SyscallCode, Type, TypeKind},
    parser::expression::{BinaryOperator, UnaryOperator},
};

pub use lower::lower;

pub mod error;
mod layout;
mod lower;

/// Complete MIR program.
/// That's a flat list of functions.
#[derive(Debug, PartialEq)]
pub struct Mir {
    pub(crate) symbols: Vec<String>,
    pub(crate) strings: Vec<String>,
    pub(crate) functions: Vec<Function>,
    pub(crate) struct_layouts: Vec<Layout>,
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
    pub(crate) id: FunctionId,
    pub(crate) intrinsic: Option<Intrinsic>,
    /// index into `Mir::symbols` giving function's source name
    pub(crate) name_symbol: usize,
    pub(crate) return_type: Type,
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
/// All instructions execute Unconditionally top-to-bottom.
/// Only one [terminator](self::Terminator) can transfer the control.
/// This invariant allow code generation to translate blocks independently.
#[derive(Debug, PartialEq)]
pub struct Block {
    id: BlockId,
    pub(crate) instructions: Vec<Instruction>,
    pub(crate) terminator: Terminator,
}

#[derive(Debug, PartialEq, Clone, Copy)]
/// Fully resolved aggregate size and alignment
pub struct Layout {
    size: u32,
    align: u32,
    contains_float: bool,
}

#[derive(Debug, PartialEq, Clone)]
#[rustfmt::skip]
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
        checked: bool,
    },

    /// load `typ` bytes from an aggregate place at byte `offset`
    FieldLoad { src: Operand, offset: u32, typ: Type },

    /// store `value` into the destination aggregate at byte `offset`
    FieldStore { value: Operand, offset: u32 },

    AddressOf { src: Place, offset: u32 },

    Call {
        callee: FunctionId,
        args: Vec<Operand>,
    },

    Syscall {
        code: SyscallCode,
        args: Vec<Operand>,
        returns: bool,
    },

    Cast { src: Operand, typ: Type },
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
    // A string literal interned into the function's string pool
    Str { id: usize, len: usize },
    Unit,
}

/// The last instruction of a [basic block](self::Block). Always exactly one per block.
/// Terminator's are the *only* place where control flow is expressed.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Terminator {
    /// Unconditional jump
    Jump(BlockId),

    /// Conditional branch: if `condition` is true
    Branch { condition: Operand, then_block: BlockId, else_block: BlockId },

    /// Return from the function, optionally carrying a returned value
    Return(Option<Operand>),
}

/// An assigned unique value id.
/// This covers both source locals (LocalId) and fresh temporaries
/// introduced during expresion lowering.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct ValueId(pub u32);

/// Stable index into a function's `blocks` vec.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct BlockId(pub u32);

impl Layout {
    pub(crate) const fn new(size: u32, align: u32, contains_float: bool) -> Self {
        Self { size, align, contains_float }
    }

    pub(crate) const fn contains_float(self) -> bool {
        self.contains_float
    }
}

impl From<Layout> for (u32, u32) {
    fn from(value: Layout) -> Self {
        (value.size, value.align)
    }
}

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
            Self::Bool(_) => Type::new(TypeKind::Bool),
            Self::Str { .. } => Type::new(TypeKind::Str),
            Self::Unit => Type::new(TypeKind::Unit),
        }
    }

    pub fn to_general_string(&self) -> String {
        match self {
            Const::Int(n, _) => format!("${n}"),
            Const::Bool(b) => format!(
                "${}",
                if *b {
                    1
                } else {
                    0
                }
            ),
            Const::Unit => unreachable!("Unit constant has no runtime representation"),
            Const::Str { .. } => panic!("string constant must be resolved through the string pool"),
            Const::Float(_, _) => panic!("float constant must be interned into the pool"),
        }
    }
}

impl std::fmt::Display for Const {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Const::Int { .. } | Const::Bool { .. } => write!(f, "{}", self.to_general_string()),
            Const::Float(v, _) => write!(f, "{v:?}"),
            Const::Str { id, .. } => write!(f, "<str:{id}>"),
            Const::Unit => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hir, mir, parser::Parser};

    fn parse_and_lower(src: &str) -> Mir {
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(src).parse().unwrap();
        let hir = hir::lower(statements, &arena).unwrap();

        mir::lower(hir).unwrap()
    }

    #[test]
    fn trivial_return_unit() {
        let mir = parse_and_lower("fn main() { }");
        assert_eq!(mir.functions.len(), 1);

        let function = &mir.functions[0];

        assert_eq!(function.return_type, Type::new(TypeKind::Unit));
        assert_eq!(function.blocks.len(), 1);
        assert_eq!(function.blocks[0].instructions.len(), 0);
        assert_eq!(function.blocks[0].terminator, Terminator::Return(None));
    }

    #[test]
    fn let_binding_and_return() {
        let mir = parse_and_lower("fn foo(): i32 { let x: i32 = 42; x }");
        let f = &mir.functions[0];
        assert_eq!(f.return_type, Type::new(TypeKind::I32));

        let assigns: Vec<_> = f.blocks[0]
            .instructions
            .iter()
            .filter(|i| matches!(i.kind, InstructionKind::Assign(_)))
            .collect();
        assert!(!assigns.is_empty(), "expected at least one Assign instruction");

        assert!(f.locals.iter().any(|(_, t)| *t == Type::new(TypeKind::I32)));
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
    fn inline_call_does_not_produce_call_instruction() {
        let mir = parse_and_lower(
            r#"
            inline fn add(a: i32, b: i32): i32 { a + b }
            fn main() { add(1, 2); }
        "#,
        );

        let main = &mir.functions[1];
        let has_call = main.blocks[0]
            .instructions
            .iter()
            .any(|i| matches!(i.kind, InstructionKind::Call { .. }));

        assert!(!has_call, "expected no call instruction in main since add is inlined");

        let has_add = main.blocks[0].instructions.iter().any(|i| {
            matches!(i.kind, InstructionKind::Binary { operation: BinaryOperator::Add, .. })
        });
        assert!(has_add, "expected inlined binary(add) instruction in main");
    }

    #[test]
    fn binary_expression_lowers_to_instructions() {
        let mir = parse_and_lower("fn add(a: i32, b: i32): i32 { a + b }");
        let f = &mir.functions[0];

        assert!(f.locals.len() >= 2);
        assert!(f.locals.iter().all(|(_, t)| *t == Type::new(TypeKind::I32)));

        let has_add = f.blocks[0].instructions.iter().any(|i| {
            matches!(i.kind, InstructionKind::Binary { operation: BinaryOperator::Add, .. })
        });
        assert!(has_add, "expected Binary(Add) instruction");

        assert!(matches!(f.blocks[0].terminator, Terminator::Return(Some(_))));
    }

    #[test]
    fn struct_literal_lowers_to_field_stores() {
        let mir = parse_and_lower(
            r#"
            struct Point { x: i64, y: i64 }
            fn main() { let p = Point { x: 1, y: 2 }; }
        "#,
        );

        let main = &mir.functions[0];
        let offsets = main.blocks[0]
            .instructions
            .iter()
            .filter_map(|instruction| match &instruction.kind {
                InstructionKind::FieldStore { offset, .. } => Some(*offset),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(offsets, vec![0, 8]);
    }

    #[test]
    fn default_struct_layout_reorders_fields_in_mir() {
        let mir = parse_and_lower(
            r#"
            struct Packed { a: i8, b: i64, c: i32 }
            fn main() { let p = Packed { a: 1, b: 2, c: 3 }; }
        "#,
        );

        let main = &mir.functions[0];
        let offsets = main.blocks[0]
            .instructions
            .iter()
            .filter_map(|instruction| match &instruction.kind {
                InstructionKind::FieldStore { offset, .. } => Some(*offset),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(offsets, vec![12, 0, 8]);
        let layout: (u32, u32) = mir.struct_layouts[0].into();
        assert_eq!(layout, (16, 8));
    }

    #[test]
    fn type_intrinsics_are_resolved_during_mir_lowering() {
        let mir = parse_and_lower(
            r#"
            struct Packed { a: i8, b: i64, c: i32 }
            fn size_of(): uptr { 0 }
            fn align_of(): uptr { 0 }
            fn main(): uptr {
                size_of(Packed) + align_of(Packed)
            }
        "#,
        );

        let main = mir
            .functions
            .iter()
            .find(|function| mir.symbols[function.name_symbol] == "nyx::main")
            .unwrap();

        let constants = main.blocks[0]
            .instructions
            .iter()
            .flat_map(|instruction| match &instruction.kind {
                InstructionKind::Binary { lhs, rhs, .. } => vec![*lhs, *rhs],
                _ => Vec::new(),
            })
            .filter_map(|operand| match operand {
                Operand::Const(Const::Int(value, _)) => Some(value),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(constants, vec![16, 8]);
    }

    #[test]
    fn nested_field_load_uses_combined_offset() {
        let mir = parse_and_lower(
            r#"
            struct Point { x: i64, y: i64 }
            struct Rect { top_left: Point, bottom_right: Point }
            fn main(): i64 {
                let p1 = Point { x: 0, y: 10 };
                let p2 = Point { x: 10, y: 0 };
                let r = Rect { top_left: p1, bottom_right: p2 };
                r.bottom_right.x
            }
        "#,
        );

        let main = &mir.functions[0];
        let has_combined_load = main.blocks[0].instructions.iter().any(|instruction| {
            matches!(
                instruction.kind,
                InstructionKind::FieldLoad { offset: 16, typ, .. } if typ == Type::new(TypeKind::I64)
            )
        });

        assert!(has_combined_load, "expected bottom_right.x to load at byte offset 16");
    }

    #[test]
    fn nested_field_assignment_uses_assigned_value_and_combined_offset() {
        let mir = parse_and_lower(
            r#"
            struct Point { x: i64, y: i64 }
            struct Rect { top_left: Point, bottom_right: Point }
            fn main(): i64 {
                let p1 = Point { x: 0, y: 10 };
                let p2 = Point { x: 10, y: 0 };
                let mut r = Rect { top_left: p1, bottom_right: p2 };
                r.bottom_right.x = 5;
                r.bottom_right.x
            }
        "#,
        );

        let main = &mir.functions[0];
        let has_assignment = main.blocks[0].instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                InstructionKind::FieldStore {
                    offset: 16,
                    value: Operand::Const(Const::Int(5, typ)),
                } if *typ == Type::new(TypeKind::I64)
            )
        });

        assert!(has_assignment, "expected bottom_right.x assignment at byte offset 16");
    }

    #[test]
    fn struct_call_arguments_are_not_flattened_in_mir() {
        let mir = parse_and_lower(
            r#"
            struct Pair { x: i64, y: i64 }
            fn id(p: Pair): Pair { p }
            fn main() {
                let p = Pair { x: 1, y: 2 };
                let q = id(p);
            }
        "#,
        );

        let main = &mir.functions[1];
        let call = main.blocks[0]
            .instructions
            .iter()
            .find_map(|instruction| match &instruction.kind {
                InstructionKind::Call { args, .. } => Some((instruction.dest.typ, args.len())),
                _ => None,
            })
            .expect("expected a call instruction");

        assert_eq!(call, (Type::new(TypeKind::Struct(crate::hir::StructId(0))), 1));
    }
}
