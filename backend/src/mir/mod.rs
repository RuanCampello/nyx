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
    Span,
    hir::{
        EnumRepr, FunctionId, Intrinsic, Static, StaticId, SymbolId, SymbolTable, Syscall,
        TyInterner, Type, TypeKind,
    },
    parser::expression::{BinaryOperator, UnaryOperator},
};
use std::collections::HashMap;

pub use crate::hir::Layout;
pub(crate) use dce::eliminate_dead;
pub use lower::lower;
pub(crate) use opt::known_panics;
pub use opt::optimise;

mod cfg;
mod dce;
pub mod error;
mod lower;
mod opt;

/// Complete MIR program
/// That's a flat list of functions
#[derive(Debug, PartialEq)]
pub struct Mir<'hir> {
    pub(in crate::mir) types: TyInterner<'hir>,
    pub(crate) symbols: SymbolTable,
    pub(crate) strings: Vec<String>,
    /// module-level globals, in [StaticId] order, laid out by the backend
    pub(crate) statics: Vec<Static<'hir>>,
    pub(crate) functions: Vec<Function<'hir>>,
    pub(crate) layouts: Layouts<'hir>,
    pub(crate) reprs: Vec<Option<EnumRepr>>,
    pub(crate) array_layouts: Vec<Layout>,
}

/// Single side-effecting or value-producing operation.
///
/// Every instruction has the form `dest = <rhs>`
/// where `dest` is the [place](self::Place) that receives the result.
/// Instructions never nest.
#[derive(Debug, PartialEq, Clone)]
pub struct Instruction<'hir> {
    pub(crate) dest: Place<'hir>,
    pub(crate) kind: InstructionKind<'hir>,
    pub(in crate::mir) span: Span,
}

#[derive(Debug, PartialEq)]
pub struct Function<'hir> {
    pub(crate) id: FunctionId,
    pub(crate) intrinsic: Option<Intrinsic>,
    pub(in crate::mir) is_const: bool,
    /// key into `Mir::symbols` giving function's source name
    pub(crate) name_symbol: SymbolId,
    pub(crate) return_type: Type<'hir>,
    /// params in declaration order.
    /// these are the first entries of `locals` but are kept separated
    /// so that codegen can emit the correct argument-register moves without
    /// having to guess which locals were params
    pub(crate) params: Vec<(ValueId, Type<'hir>)>,
    pub(crate) locals: Vec<(ValueId, Type<'hir>)>,
    pub(crate) blocks: Vec<Block<'hir>>,
}

/// A single-entry, single-exit sequence of instructions.
///
/// All instructions execute Unconditionally top-to-bottom.
/// Only one [terminator](self::Terminator) can transfer the control.
/// This invariant allow code generation to translate blocks independently.
#[derive(Debug, PartialEq)]
pub struct Block<'hir> {
    id: BlockId,
    pub(crate) instructions: Vec<Instruction<'hir>>,
    pub(crate) terminator: Terminator<'hir>,
}

#[derive(Debug, PartialEq, Clone)]
#[rustfmt::skip]
pub enum InstructionKind<'hir> {
    /// `dest = operand` (copy or constant load)
    Assign(Operand<'hir>),
    Unary {
        operation: UnaryOperator,
        rhs: Operand<'hir>,
    },
    Binary {
        operation: BinaryOperator,
        rhs: Operand<'hir>,
        lhs: Operand<'hir>,
        /// emit a runtime overflow check: set by the optimisation level, read by the
        /// backends. It says nothing about intent — below `sane` *every* arithmetic
        /// instruction is unchecked
        checked: bool,
        /// the program asked for modular arithmetic, via one of the `wrapping_*`
        /// intrinsics. Kept apart from `checked` because that flag varies with the
        /// optimisation level and this one must not: it is what lets the known-panics
        /// lint stay silent about an overflow that was deliberate
        wrapping: bool,
    },
    /// load `typ` bytes from an aggregate place at byte `offset`
    FieldLoad { src: Operand<'hir>, offset: u32, typ: Type<'hir> },
    /// load `typ` from `base[index]` in a row-major aggregate of `stride`-byte elements
    ///
    /// `bound` is the element count the index is checked against, the bounds check and
    /// its panic are materialised entirely by the backend
    ElementLoad {
        base: Operand<'hir>,
        index: Operand<'hir>,
        bound: Operand<'hir>,
        stride: u32,
        typ: Type<'hir>,
    },
    /// store `value` into the destination aggregate at byte `offset`
    FieldStore { value: Operand<'hir>, offset: u32 },
    /// store `value` into `dest[index]` of a row-major aggregate of `stride`-byte elements
    ///
    /// the destination aggregate is the instruction's `dest` place, `bound` drives the
    /// same backend-only bounds check as [InstructionKind::ElementLoad]
    ElementStore {
        index: Operand<'hir>,
        bound: Operand<'hir>,
        value: Operand<'hir>,
        stride: u32,
    },
    /// the address of `base[index]` (i.e. `&base[index]`): like [InstructionKind::ElementLoad]
    /// but yields the element pointer instead of loading it, with the same bounds check
    ElementAddr {
        base: Operand<'hir>,
        index: Operand<'hir>,
        bound: Operand<'hir>,
        stride: u32,
    },
    AddressOf { src: Place<'hir>, offset: u32 },
    /// the address of a module-level global
    ///
    /// Reads and writes of a static go through this, so they reuse the same
    /// [InstructionKind::FieldLoad]/[InstructionKind::FieldStore] pair a raw
    /// pointer dereference already uses
    StaticAddr { id: StaticId },
    Call {
        callee: FunctionId,
        args: Vec<Operand<'hir>>,
    },
    Syscall {
        code: Syscall,
        args: Vec<Operand<'hir>>,
        returns: bool,
    },
    Cast { src: Operand<'hir>, typ: Type<'hir> },
    /// `dest = condition ? then_value : else_value`, with both values already computed
    Select {
        condition: Operand<'hir>,
        then_value: Operand<'hir>,
        else_value: Operand<'hir>,
    },
}

/// This is a *input* of a instruction
///
/// Instructions consume operands
/// Operands are atomic: either a named local/temporary or inlined constant
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Operand<'hir> {
    Place(Place<'hir>),
    Const(Const<'hir>),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Place<'hir> {
    pub id: ValueId,
    pub typ: Type<'hir>,
}

/// An inlined constant
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Const<'hir> {
    Int(i64, Type<'hir>),
    Float(f64, Type<'hir>),
    Bool(bool),
    // A string literal interned into the function's string pool
    Str { id: usize, len: usize },
    Unit,
}

/// The last instruction of a [basic block](self::Block)
///
/// Always exactly one per block
/// Terminator's are the *only* place where control flow is expressed
#[derive(Debug, PartialEq, Clone)]
pub enum Terminator<'hir> {
    /// Unconditional jump
    Jump(BlockId),
    /// Conditional branch: if `condition` is true
    Branch { condition: Operand<'hir>, then_block: BlockId, else_block: BlockId },
    /// Return from the function, optionally carrying a returned value
    Return(Option<Operand<'hir>>),
}

/// An assigned unique value id
/// This covers both source locals [local id](frontend::hir::LocalId) and fresh temporaries
/// introduced during expresion lowering
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct ValueId(pub u32);

/// Stable index into a function's [blocks](Function::blocks) vec
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct BlockId(pub u32);

pub(crate) type Layouts<'hir> = HashMap<Type<'hir>, Layout>;

impl<'hir> Operand<'hir> {
    pub fn typ(&self) -> Type<'hir> {
        match self {
            Self::Place(p) => p.typ,
            Self::Const(c) => c.typ(),
        }
    }
}

impl<'hir> Const<'hir> {
    pub fn typ(&self) -> Type<'hir> {
        match self {
            Self::Int(_, typ) => *typ,
            Self::Float(_, typ) => *typ,
            Self::Bool(_) => Type::from(TypeKind::Bool),
            Self::Str { .. } => Type::from(TypeKind::Str),
            Self::Unit => Type::from(TypeKind::Unit),
        }
    }

    pub fn to_string(self) -> String {
        match self {
            Const::Int(n, _) => format!("${n}"),
            Const::Bool(b) => format!(
                "${}",
                match b {
                    true => 1,
                    _ => 0,
                }
            ),
            Const::Unit => unreachable!("Unit constant has no runtime representation"),
            Const::Str { .. } => panic!("string constant must be resolved through the string pool"),
            Const::Float(_, _) => panic!("float constant must be interned into the pool"),
        }
    }
}

impl std::fmt::Display for Const<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Const::Int { .. } | Const::Bool { .. } => write!(f, "{}", (*self).to_string()),
            Const::Float(v, _) => write!(f, "{v:?}"),
            Const::Str { id, .. } => write!(f, "<str:{id}>"),
            Const::Unit => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use frontend::hir::AdtId;

    use super::*;
    use crate::{hir, mir, parser::Parser};

    fn parse_and_lower(src: &'static str) -> Mir<'static> {
        let arena = Box::leak(Box::new(bumpalo::Bump::new()));
        let statements = Parser::new(src).parse().unwrap();
        let hir = hir::lower(statements, arena).unwrap();

        mir::lower(hir).unwrap()
    }

    #[test]
    fn trivial_return_unit() {
        let mir = parse_and_lower("fn main() { }");
        assert_eq!(mir.functions.len(), 1);

        let function = &mir.functions[0];

        assert_eq!(function.return_type, TypeKind::Unit.into());
        assert_eq!(function.blocks.len(), 1);
        assert_eq!(function.blocks[0].instructions.len(), 0);
        assert_eq!(function.blocks[0].terminator, Terminator::Return(None));
    }

    #[test]
    fn let_binding_and_return() {
        let mir = parse_and_lower("fn foo(): i32 { let x: i32 = 42; x }");
        let f = &mir.functions[0];
        assert_eq!(f.return_type, TypeKind::I32.into());

        let assigns: Vec<_> = f.blocks[0]
            .instructions
            .iter()
            .filter(|i| matches!(i.kind, InstructionKind::Assign(_)))
            .collect();
        assert!(!assigns.is_empty(), "expected at least one Assign instruction");

        assert!(f.locals.iter().any(|(_, t)| *t == TypeKind::I32.into()));
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
        assert!(f.locals.iter().all(|(_, t)| *t == TypeKind::I32.into()));

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
        let typ = mir.types.adt(AdtId(0), &[]);
        let layout: (u32, u32) = mir.layouts[&typ].into();
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
            .find(|function| mir.symbols.get(function.name_symbol) == "nyx::main")
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
                InstructionKind::FieldLoad { offset: 16, typ, .. } if typ == TypeKind::I64.into()
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
                } if *typ == TypeKind::I64.into()
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

        assert_eq!(call, (mir.types.adt(AdtId(0), &[]), 1));
    }

    #[test]
    fn literal_pattern_emits_eq_comparison() {
        let mir = parse_and_lower(
            r#"
            fn classify(x: i32): i32 {
                match x {
                    0 -> 10,
                    _ -> 20,
                }
            }
        "#,
        );

        let func = &mir.functions[0];
        let has_eq = func.blocks.iter().any(|b| {
            b.instructions.iter().any(|instr| {
                matches!(
                    &instr.kind,
                    InstructionKind::Binary {
                        operation: BinaryOperator::Eq,
                        rhs: Operand::Const(Const::Int(0, _)),
                        ..
                    }
                )
            })
        });
        assert!(has_eq, "expected an Eq comparison against 0 for the literal pattern");
    }

    #[test]
    fn or_pattern_produces_two_check_blocks() {
        let mir = parse_and_lower(
            r#"
            enum Dir { N = 0, S = 1, E = 2, W = 3 } as u8
            fn is_horizontal(d: Dir): i32 {
                match d {
                    Dir::E | Dir::W -> 1,
                    _ -> 0,
                }
            }
        "#,
        );

        let func = &mir.functions[0];
        let branch_count = func
            .blocks
            .iter()
            .filter(|b| matches!(b.terminator, Terminator::Branch { .. }))
            .count();
        assert!(
            branch_count >= 2,
            "or-pattern with 2 alternatives must produce at least 2 branch terminators, got {branch_count}"
        );
    }

    #[test]
    fn guard_adds_conditional_branch_in_body_block() {
        let mir = parse_and_lower(
            r#"
            fn sign(x: i32): i32 {
                match x {
                    n if n > 0 -> 1,
                    _ -> 0,
                }
            }
        "#,
        );

        let func = &mir.functions[0];
        let has_guard_branch = func.blocks.iter().any(|b| {
            matches!(b.terminator, Terminator::Branch { .. })
                && b.instructions.iter().any(|i| {
                    matches!(&i.kind, InstructionKind::Binary { operation: BinaryOperator::Gt, .. })
                })
        });
        assert!(has_guard_branch, "expected a Branch from guard evaluating n > 0");
    }
}
