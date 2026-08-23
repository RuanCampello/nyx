//! If-conversion: replacing a short diamond with a branch-free [InstructionKind::Select]
//!
//! ```text
//!   head:  branch c -> then, else          head:  t0 = <then arm>
//!   then:  x = a; jump join         ==>           t1 = <else arm>
//!   else:  x = b; jump join                       x  = select c ? t0 : t1
//!   join:  ...                                    jump join
//! ```

use crate::{
    Span, TargetArch,
    hir::{Type, TypeKind},
    mir::{
        Block, BlockId, Function, Instruction, InstructionKind, Mir, Operand, Place,
        Terminator as Term, ValueId, cfg,
    },
    optimisation::Level,
    parser::expression::BinaryOperator,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// A branch whose two arms rejoin immediately and belong to nobody else
struct Diamond<'hir> {
    then_arm: usize,
    else_arm: usize,
    join: usize,
    condition: Operand<'hir>,
}

/// What a target is willing to pay to lose a branch
struct Budget {
    /// speculated instructions, counted across both arms
    ///
    /// * LLVM's IR-level equivalent, `TwoEntryPHINodeFoldingThreshold`, is 4 cost units for
    /// the same decision
    /// * GCC's `max-rtl-if-conversion-insns` is 10 per block, but only
    /// before its RTX cost check runs
    speculated: usize,
    /// selects the diamond may leave behind
    selects: usize,
    /// reject a select whose result feeds the next iteration of an enclosing loop
    reject_loop_carried: bool,
}

/// `cmov` is a data dependency with no immediate form and a
/// long history of losing to a predicted branch, so `x86-64`
/// takes it only at `max`, only for one select, and only for
/// arms of a single instruction each
const X86_64: Budget = Budget { speculated: 2, selects: 1, reject_loop_carried: true };

/// the `csel` family is register-only, uniformly cheap and has no
/// memory-operand foot-gun, so `AArch64` can afford the wider window
/// from `sane` upwards
const AARCH64: Budget = Budget { speculated: 4, selects: 2, reject_loop_carried: false };

pub(super) fn run(mir: &mut Mir<'_>, target: TargetArch, level: Level) -> bool {
    let Some(budget) = Budget::for_target(target, level) else {
        return false;
    };

    let mut changed = false;
    for function in &mut mir.functions {
        while convert_one(function, &budget) {
            changed = true;
        }
    }

    changed
}

impl Budget {
    const fn for_target(target: TargetArch, level: Level) -> Option<Self> {
        match (target, level) {
            (TargetArch::AArch64, Level::Sane | Level::Max) => Some(AARCH64),
            (TargetArch::X86_64, Level::Max) => Some(X86_64),
            (TargetArch::AArch64 | TargetArch::X86_64, Level::Debug) => None,
            (TargetArch::X86_64, Level::Sane) => None,
        }
    }

    /// the values the diamond would have to select, when it is worth selecting them at all
    fn affords(
        &self,
        function: &Function<'_>,
        diamond: &Diamond<'_>,
        head: usize,
    ) -> Option<Vec<ValueId>> {
        let count = function.blocks[diamond.then_arm].instructions.len()
            + function.blocks[diamond.else_arm].instructions.len();
        if count > self.speculated {
            return None;
        }

        let then_assigned = assigned(function, diamond.then_arm);
        let else_assigned = assigned(function, diamond.else_arm);

        let outside = read_outside(function, diamond.then_arm, diamond.else_arm);
        let selected: Vec<_> = then_assigned
            .union(&else_assigned)
            .copied()
            .filter(|id| outside.contains(id))
            .collect();

        if selected.len() > self.selects {
            return None;
        }

        let carried = selected
            .iter()
            .any(|id| !then_assigned.contains(id) || !else_assigned.contains(id));

        match self.reject_loop_carried && carried && cyclic(function, head, diamond.join) {
            true => None,
            false => Some(selected),
        }
    }
}

impl<'hir> Function<'hir> {
    fn fresh_local(&mut self, typ: Type<'hir>) -> Place<'hir> {
        let id = ValueId(self.locals.len() as u32);
        assert!(
            self.locals.last().is_none_or(|(last, _)| last.0 + 1 == id.0),
            "locals must stay dense: the backends index them positionally"
        );

        self.locals.push((id, typ));
        Place { id, typ }
    }

    fn local(&self, id: ValueId) -> Place<'hir> {
        let (_, typ) = self.locals[id.0 as usize];
        Place { id, typ }
    }
}

impl Block<'_> {
    fn borrowed_span(&self, head: &Self) -> Span {
        self.instructions
            .first()
            .or_else(|| head.instructions.last())
            .map_or_else(Span::default, |instruction| instruction.span)
    }
}

/// rewrite one convertible diamond, or report that the function holds none
fn convert_one(function: &mut Function<'_>, budget: &Budget) -> bool {
    let predecessors = cfg::predecessors(function, None);

    for head in 0..function.blocks.len() {
        let Some(diamond) = match_diamond(function, head, &predecessors) else {
            continue;
        };

        let Diamond { then_arm, else_arm, join, condition } = diamond;
        let Some(selected) = budget.affords(function, &diamond, head) else {
            continue;
        };

        let mut speculated = Vec::new();
        let then_values = speculate(function, then_arm, &selected, &mut speculated);
        let else_values = speculate(function, else_arm, &selected, &mut speculated);

        let span = function.blocks[then_arm].borrowed_span(&function.blocks[head]);
        for id in selected {
            let place = function.local(id);
            let reaching = Operand::Place(place);

            speculated.push(Instruction {
                dest: place,
                kind: InstructionKind::Select {
                    condition,
                    then_value: then_values.get(&id).copied().unwrap_or(reaching),
                    else_value: else_values.get(&id).copied().unwrap_or(reaching),
                },
                span,
            });
        }

        function.blocks[head].instructions.append(&mut speculated);
        function.blocks[head].terminator = Term::Jump(BlockId(join as u32));

        return true;
    }

    false
}

fn match_diamond<'hir>(
    function: &Function<'hir>,
    head: usize,
    predecessors: &[Vec<usize>],
) -> Option<Diamond<'hir>> {
    let Term::Branch { condition, then_block, else_block } = &function.blocks[head].terminator
    else {
        return None;
    };
    let (condition, then_block, else_block) = (*condition, *then_block, *else_block);

    let (then_arm, else_arm) = (then_block.0 as usize, else_block.0 as usize);
    if then_arm == else_arm || then_arm == head || else_arm == head {
        return None;
    }

    let (then_term, else_term) =
        (&function.blocks[then_arm].terminator, &function.blocks[else_arm].terminator);
    let join = match (then_term, else_term) {
        (Term::Jump(then_target), Term::Jump(else_target)) if then_target.0 == else_target.0 => {
            then_target.0 as usize
        },
        _ => return None,
    };

    // an arm reached by another edge is not ours to delete,
    // and a join that lands back on the head or on an arm
    // is a loop wearing a diamond's shape
    if [head, then_arm, else_arm].contains(&join)
        || predecessors[then_arm].len() > 1
        || predecessors[else_arm].len() > 1
    {
        return None;
    }

    let arms = || {
        function.blocks[then_arm]
            .instructions
            .iter()
            .chain(&function.blocks[else_arm].instructions)
    };

    let convertible = arms()
        .all(|instruction| speculatable(&instruction.kind) && selectable(instruction.dest.typ));

    // the selects run in sequence, so a condition an arm
    // overwrites would be read by the second select after the
    // first had already clobbered it
    let condition_survives = match condition {
        Operand::Place(place) => arms().all(|instruction| instruction.dest.id != place.id),
        Operand::Const(_) => true,
    };

    (convertible && condition_survives).then_some(Diamond { then_arm, else_arm, join, condition })
}

/// copy an arm's instructions onto `into`, renaming every destination to a fresh local
fn speculate<'hir>(
    function: &mut Function<'hir>,
    arm: usize,
    selected: &[ValueId],
    into: &mut Vec<Instruction<'hir>>,
) -> BTreeMap<ValueId, Operand<'hir>> {
    let mut renamed = BTreeMap::new();

    for index in 0..function.blocks[arm].instructions.len() {
        let mut instruction = function.blocks[arm].instructions[index].clone();
        instruction.kind.each_operand_mut(|operand| {
            if let Operand::Place(place) = operand
                && let Some(value) = renamed.get(&place.id)
            {
                *operand = *value;
            }
        });

        if let InstructionKind::Assign(source) = instruction.kind
            && !matches!(source, Operand::Place(place) if selected.contains(&place.id))
        {
            renamed.insert(instruction.dest.id, source);
            continue;
        }

        let fresh = function.fresh_local(instruction.dest.typ);
        renamed.insert(instruction.dest.id, Operand::Place(fresh));
        instruction.dest = fresh;
        into.push(instruction);
    }

    renamed
}

/// every value read anywhere but inside the two arms
fn read_outside(function: &Function<'_>, then_arm: usize, else_arm: usize) -> HashSet<ValueId> {
    let mut read = HashSet::new();

    for (index, block) in function.blocks.iter().enumerate() {
        if index == then_arm || index == else_arm {
            continue;
        }

        for instruction in &block.instructions {
            instruction.kind.each_operand(|operand| {
                if let Operand::Place(place) = operand {
                    read.insert(place.id);
                }
            });

            // a store reads its destination aggregate rather than replacing it
            if instruction.kind.writes_through_dest() {
                read.insert(instruction.dest.id);
            }
        }

        match &block.terminator {
            Term::Branch { condition: Operand::Place(place), .. }
            | Term::Return(Some(Operand::Place(place))) => {
                read.insert(place.id);
            },
            Term::Branch { .. } | Term::Jump(_) | Term::Return(_) => {},
        }
    }

    read
}

/// instructions that may run on a path that would not have reached then
#[inline(always)]
const fn speculatable(kind: &InstructionKind<'_>) -> bool {
    match kind {
        InstructionKind::Assign(_)
        | InstructionKind::Unary { .. }
        | InstructionKind::Cast { .. }
        | InstructionKind::Select { .. } => true,

        InstructionKind::Binary { operation, checked, .. } => {
            !*checked && !matches!(operation, BinaryOperator::Div | BinaryOperator::Rem)
        },

        _ => false,
    }
}

/// types a target select can hold
///
/// `cmov` and `csel` are integer instructions
/// x86-64 has no float form at all, LLVM lowers a float select to a blend,
/// and an aggregate lives on the stack, where selecting it would mean
/// a conditional `memcpy`
#[inline(always)]
const fn selectable(typ: Type<'_>) -> bool {
    matches!(
        typ.kind(),
        TypeKind::I32
            | TypeKind::U32
            | TypeKind::I64
            | TypeKind::U64
            | TypeKind::Iptr
            | TypeKind::Uptr
            | TypeKind::Ref { .. }
            | TypeKind::Raw { .. }
    )
}

fn assigned(function: &Function<'_>, arm: usize) -> BTreeSet<ValueId> {
    function.blocks[arm]
        .instructions
        .iter()
        .map(|instruction| instruction.dest.id)
        .collect()
}

/// whether `head` is reachable from `join`, for exmaple the diamond sits inside a loop
fn cyclic(function: &Function<'_>, head: usize, join: usize) -> bool {
    let mut seen = HashSet::from([join]);
    let mut queue = vec![join];

    while let Some(block) = queue.pop() {
        for successor in cfg::successors(&function.blocks[block].terminator) {
            if successor == head {
                return true;
            }
            if seen.insert(successor) {
                queue.push(successor);
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compile_for, hir,
        mir::{self, Terminator},
        optimisation,
        parser::Parser,
    };
    use rstest::rstest;

    fn lower(source: &'static str, target: TargetArch, level: Level) -> Mir<'static> {
        let arena = Box::leak(Box::new(bumpalo::Bump::new()));
        let statements = Parser::new(source).parse().unwrap();
        let hir = hir::lower(statements, arena).unwrap();
        let mut mir = mir::lower(hir).unwrap();

        optimisation::set(level);
        mir::optimise(&mut mir, target);
        optimisation::set(Level::Debug);

        mir
    }

    fn assembly(source: &str, target: TargetArch, level: Level) -> String {
        optimisation::set(level);
        let assembly = compile_for(source, target).unwrap();
        optimisation::set(Level::Debug);

        assembly
    }

    fn selects_in<'hir>(mir: &Mir<'hir>, name: &str) -> Vec<InstructionKind<'hir>> {
        mir.functions
            .iter()
            .find(|function| mir.symbols.get(function.name_symbol) == name)
            .expect("function not found")
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(instruction.kind, InstructionKind::Select { .. }))
            .map(|instruction| instruction.kind.clone())
            .collect()
    }

    const PICK: &str = r#"
        fn pick(c: bool, a: i32, b: i32): i32 {
            let mut r: i32 = 0;
            if c { r = a; } else { r = b; }
            r
        }
        fn main(): i32 { pick(true, 1, 2) }
    "#;

    #[test]
    fn a_diamond_collapses_into_one_select() {
        let mir = lower(PICK, TargetArch::X86_64, Level::Max);
        assert_eq!(selects_in(&mir, "nyx::pick").len(), 1);

        let pick = mir
            .functions
            .iter()
            .find(|function| mir.symbols.get(function.name_symbol) == "nyx::pick")
            .unwrap();
        assert!(
            !pick
                .blocks
                .iter()
                .any(|block| matches!(block.terminator, Terminator::Branch { .. })),
            "the branch the select replaced must be gone"
        );
    }

    #[test]
    fn x86_64_will_not_form_a_cmov_below_max() {
        let mir = lower(PICK, TargetArch::X86_64, Level::Sane);
        assert!(selects_in(&mir, "nyx::pick").is_empty());
    }

    #[test]
    fn aarch64_forms_a_csel_from_sane() {
        let mir = lower(PICK, TargetArch::AArch64, Level::Sane);
        assert_eq!(selects_in(&mir, "nyx::pick").len(), 1);
    }

    #[test]
    fn nothing_is_converted_without_optimisation() {
        for target in [TargetArch::X86_64, TargetArch::AArch64] {
            let mir = lower(PICK, target, Level::Debug);
            assert!(selects_in(&mir, "nyx::pick").is_empty(), "{target:?} converted at debug");
        }
    }

    #[test]
    fn an_empty_arm_selects_the_value_that_reached_the_branch() {
        let source = r#"
            fn clamp(x: i32): i32 {
                let mut r: i32 = x;
                if x < 0 { r = 0; }
                r
            }
            fn main(): i32 { clamp(-1) }
        "#;

        let mir = lower(source, TargetArch::AArch64, Level::Sane);
        let selects = selects_in(&mir, "nyx::clamp");
        let [InstructionKind::Select { then_value, else_value, .. }] = selects.as_slice() else {
            panic!("expected exactly one select");
        };

        assert_eq!(*then_value, Operand::Const(mir::Const::Int(0, Type::from(TypeKind::I32))));
        assert!(matches!(else_value, Operand::Place(_)), "the else side reads `r` back");
    }

    #[test]
    fn a_call_in_an_arm_is_never_speculated() {
        let source = r#"
            fn f(x: i32): i32 { x + 1 }
            fn choose(c: bool, x: i32): i32 {
                let mut r: i32 = 0;
                if c { r = f(x); } else { r = x; }
                r
            }
            fn main(): i32 { choose(true, 1) }
        "#;

        for target in [TargetArch::X86_64, TargetArch::AArch64] {
            let mir = lower(source, target, Level::Max);
            assert!(selects_in(&mir, "nyx::choose").is_empty(), "{target:?} speculated a call");
        }
    }

    /// both halves of idiv fault on a zero divisor, so neither may run
    /// on a path the branch was guarding against
    #[rstest]
    #[case::division(
        r#"fn guarded(d: i32): i32 { let mut r: i32 = -1; if d != 0 { r = 100 / d; } r }
        fn main(): i32 { guarded(0) }"#
    )]
    #[case::remainder(
        r#"fn guarded(d: i32): i32 { let mut r: i32 = -1; if d != 0 { r = 100 % d; } r }
        fn main(): i32 { guarded(0) }"#
    )]
    fn a_faulting_division_in_an_arm_is_never_speculated(
        #[case] source: &'static str,
        #[values(TargetArch::X86_64, TargetArch::AArch64)] target: TargetArch,
    ) {
        let mir = lower(source, target, Level::Max);
        assert!(selects_in(&mir, "nyx::guarded").is_empty(), "{target:?} speculated a fault");
    }

    #[test]
    fn a_loop_carried_select_is_refused_on_x86_64_and_taken_on_aarch64() {
        let source = r#"
            fn largest(n: i32): i32 {
                let mut best: i32 = 0;
                let mut i: i32 = 1;
                loop {
                    if i > n { break; }
                    if i > best { best = i; }
                    i = i + 1;
                }
                best
            }
            fn main(): i32 { largest(3) }
        "#;

        assert!(
            selects_in(&lower(source, TargetArch::X86_64, Level::Max), "nyx::largest").is_empty()
        );
        assert_eq!(
            selects_in(&lower(source, TargetArch::AArch64, Level::Sane), "nyx::largest").len(),
            1
        );
    }

    #[test]
    fn arms_that_read_what_the_other_writes_keep_the_incoming_values() {
        // without renaming, speculating `a = b` and then `b = a` would leave both holding
        // the same value on both paths
        let source = r#"
            fn cross(c: bool, x: i32, y: i32): i32 {
                let mut a: i32 = x;
                let mut b: i32 = y;
                if c { a = b; } else { b = a; }
                a * 10 + b
            }
            fn main(): i32 { cross(true, 1, 2) }
        "#;

        let mir = lower(source, TargetArch::AArch64, Level::Sane);
        let selects = selects_in(&mir, "nyx::cross");
        assert_eq!(selects.len(), 2);

        let sources: Vec<_> = selects
            .iter()
            .map(|kind| match kind {
                InstructionKind::Select { then_value, else_value, .. } => {
                    (*then_value, *else_value)
                },
                _ => unreachable!(),
            })
            .collect();

        assert_ne!(sources[0].0, sources[1].0, "both arms would otherwise select one value");
    }

    #[test]
    fn x86_64_emits_a_cmov_and_aarch64_a_csel() {
        assert!(assembly(PICK, TargetArch::X86_64, Level::Max).contains("cmovne"));
        assert!(assembly(PICK, TargetArch::AArch64, Level::Sane).contains("csel"));
    }

    #[test]
    fn neither_target_leaves_an_immediate_in_a_select_operand() {
        let source = r#"
            fn sign(x: i32): i32 {
                let mut r: i32 = 1;
                if x < 0 { r = 0; } else { r = 2; }
                r
            }
            fn main(): i32 { sign(-1) }
        "#;

        for line in assembly(source, TargetArch::X86_64, Level::Max).lines() {
            assert!(!(line.contains("cmov") && line.contains('$')), "immediate cmov: {line}");
        }
        for line in assembly(source, TargetArch::AArch64, Level::Sane).lines() {
            assert!(!(line.contains("csel") && line.contains('#')), "immediate csel: {line}");
        }
    }
}
