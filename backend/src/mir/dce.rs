//! Removal of code nothing can reach or observe
//!
//! Each kind of removal is a sweep: a [Program] sweep decides which functions exist at all, a
//! [Body] sweep prunes inside one that survived. Adding a kind means writing the impl and naming
//! it in one of the three tables below, nothing else

use crate::{
    hir::FunctionId,
    mir::{BlockId, Const, Function, InstructionKind, Mir, Operand, Terminator as Term},
};
use std::collections::{HashMap, HashSet};

/// functions unreachable in the call graph
struct Functions;
/// blocks unreachable from a function's entry block
struct Blocks;
/// instructions whose destination nothing reads
struct Instructions;
/// string constants left without a referent
struct Strings;

/// a sweep over the whole program, run once
trait Program {
    fn sweep(&self, mir: &mut Mir);
}

/// a sweep over one surviving function, re-run until it settles
trait Body {
    /// whether anything was removed
    fn sweep(&self, function: &mut Function) -> bool;
}

/// run first: these decide which functions reach a [Body] sweep at all
const BEFORE: &[&dyn Program] = &[&Functions];
/// run over every surviving function, to a fixpoint
const BODIES: &[&dyn Body] = &[&Blocks, &Instructions];
/// run last: these collect what the [Body] sweeps stranded
const AFTER: &[&dyn Program] = &[&Strings];

pub(crate) fn eliminate_dead(mir: &mut Mir) {
    for sweep in BEFORE {
        sweep.sweep(mir);
    }

    for function in &mut mir.functions {
        while BODIES.iter().fold(false, |changed, sweep| changed | sweep.sweep(function)) {}
    }

    for sweep in AFTER {
        sweep.sweep(mir);
    }
}

impl Program for Functions {
    /// drop every function unreachable from `main`, retaining library modules
    fn sweep(&self, mir: &mut Mir) {
        let Some(entry) = entry(mir) else {
            return;
        };

        let by_id: HashMap<_, _> = mir
            .functions
            .iter()
            .enumerate()
            .map(|(index, function)| (function.id, index))
            .collect();

        let mut live = HashSet::from([entry]);
        let mut stack = vec![entry];

        while let Some(id) = stack.pop() {
            let Some(&index) = by_id.get(&id) else {
                continue;
            };

            for callee in callees(&mir.functions[index]) {
                if live.insert(callee) {
                    stack.push(callee);
                }
            }
        }

        mir.functions.retain(|function| live.contains(&function.id));
    }
}

impl Body for Blocks {
    /// drop blocks no path from the entry reaches, renumbering the survivors
    fn sweep(&self, function: &mut Function) -> bool {
        let count = function.blocks.len();
        let mut reachable = vec![false; count];
        let mut stack = vec![0];
        reachable[0] = true;

        while let Some(block) = stack.pop() {
            for successor in successors(&function.blocks[block].terminator) {
                if !reachable[successor] {
                    reachable[successor] = true;
                    stack.push(successor);
                }
            }
        }

        if reachable.iter().all(|live| *live) {
            return false;
        }

        let mut renumbered = vec![0u32; count];
        let mut next = 0;
        for (old, live) in reachable.iter().enumerate() {
            renumbered[old] = next;
            next += u32::from(*live);
        }

        let mut old = 0;
        function.blocks.retain(|_| {
            old += 1;
            reachable[old - 1]
        });

        for (position, block) in function.blocks.iter_mut().enumerate() {
            block.id = BlockId(position as u32);

            match &mut block.terminator {
                Term::Jump(target) => *target = BlockId(renumbered[target.0 as usize]),
                Term::Branch { then_block, else_block, .. } => {
                    *then_block = BlockId(renumbered[then_block.0 as usize]);
                    *else_block = BlockId(renumbered[else_block.0 as usize]);
                },
                Term::Return(_) => {},
            }
        }

        true
    }
}

impl Body for Instructions {
    /// drop instructions whose destination nothing reads
    ///
    /// MIR is not in SSA form, so a value may be assigned in several places and proving one of those
    /// assignments dead would need liveness
    fn sweep(&self, function: &mut Function) -> bool {
        let mut read = HashSet::new();

        for block in &function.blocks {
            for instruction in &block.instructions {
                each_operand(&instruction.kind, |operand| {
                    if let Operand::Place(place) = operand {
                        read.insert(place.id);
                    }
                });

                if let InstructionKind::AddressOf { src, .. } = &instruction.kind {
                    read.insert(src.id);
                }

                if writes_through_dest(&instruction.kind) {
                    read.insert(instruction.dest.id);
                }
            }

            if let Some(Operand::Place(place)) = terminator_operand(&block.terminator) {
                read.insert(place.id);
            }
        }

        let mut removed = false;
        for block in &mut function.blocks {
            let before = block.instructions.len();
            block.instructions.retain(|instruction| {
                !pure(&instruction.kind) || read.contains(&instruction.dest.id)
            });
            removed |= block.instructions.len() != before;
        }

        removed
    }
}

impl Program for Strings {
    /// drop string constants nothing refers to, renumbering the survivors
    fn sweep(&self, mir: &mut Mir) {
        let mut used = HashSet::new();

        for function in &mir.functions {
            for block in &function.blocks {
                for instruction in &block.instructions {
                    each_operand(&instruction.kind, |operand| {
                        if let Operand::Const(Const::Str { id, .. }) = operand {
                            used.insert(*id);
                        }
                    });
                }

                if let Some(Operand::Const(Const::Str { id, .. })) =
                    terminator_operand(&block.terminator)
                {
                    used.insert(*id);
                }
            }
        }

        if used.len() == mir.strings.len() {
            return;
        }

        let mut renumbered = vec![0; mir.strings.len()];
        let mut next = 0;
        for (old, slot) in renumbered.iter_mut().enumerate() {
            *slot = next;
            next += usize::from(used.contains(&old));
        }

        let mut old = 0;
        mir.strings.retain(|_| {
            old += 1;
            used.contains(&(old - 1))
        });

        for function in &mut mir.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    each_operand_mut(&mut instruction.kind, |operand| {
                        if let Operand::Const(Const::Str { id, .. }) = operand {
                            *id = renumbered[*id];
                        }
                    });
                }

                if let Some(Operand::Const(Const::Str { id, .. })) =
                    terminator_operand_mut(&mut block.terminator)
                {
                    *id = renumbered[*id];
                }
            }
        }
    }
}

#[inline(always)]
pub(in crate::mir) const fn writes_through_dest(kind: &InstructionKind) -> bool {
    matches!(kind, InstructionKind::FieldStore { .. } | InstructionKind::ElementStore { .. })
}

pub(in crate::mir) fn each_operand(kind: &InstructionKind, mut visit: impl FnMut(&Operand)) {
    use InstructionKind::*;

    match kind {
        Assign(operand)
        | Unary { rhs: operand, .. }
        | FieldLoad { src: operand, .. }
        | FieldStore { value: operand, .. }
        | Cast { src: operand, .. } => visit(operand),
        Binary { lhs, rhs, .. } => {
            visit(lhs);
            visit(rhs);
        },
        ElementLoad { base, index, bound, .. } | ElementAddr { base, index, bound, .. } => {
            visit(base);
            visit(index);
            visit(bound);
        },
        ElementStore { index, bound, value, .. } => {
            visit(index);
            visit(bound);
            visit(value);
        },
        Call { args, .. } | Syscall { args, .. } => args.iter().for_each(visit),
        Select { condition, then_value, else_value } => {
            visit(condition);
            visit(then_value);
            visit(else_value);
        },
        AddressOf { .. } => {},
    }
}

pub(in crate::mir) fn each_operand_mut(
    kind: &mut InstructionKind,
    mut visit: impl FnMut(&mut Operand),
) {
    use InstructionKind::*;

    match kind {
        Assign(operand)
        | Unary { rhs: operand, .. }
        | FieldLoad { src: operand, .. }
        | FieldStore { value: operand, .. }
        | Cast { src: operand, .. } => visit(operand),
        Binary { lhs, rhs, .. } => {
            visit(lhs);
            visit(rhs);
        },
        ElementLoad { base, index, bound, .. } | ElementAddr { base, index, bound, .. } => {
            visit(base);
            visit(index);
            visit(bound);
        },
        ElementStore { index, bound, value, .. } => {
            visit(index);
            visit(bound);
            visit(value);
        },
        Call { args, .. } | Syscall { args, .. } => args.iter_mut().for_each(visit),
        Select { condition, then_value, else_value } => {
            visit(condition);
            visit(then_value);
            visit(else_value);
        },
        AddressOf { .. } => {},
    }
}

fn entry(mir: &Mir) -> Option<FunctionId> {
    mir.functions
        .iter()
        .find(|function| {
            let name = mir.symbols.get(function.name_symbol);
            name == "main" || name.ends_with("::main")
        })
        .map(|function| function.id)
}

fn callees(function: &Function) -> impl Iterator<Item = FunctionId> + '_ {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match &instruction.kind {
            InstructionKind::Call { callee, .. } => Some(*callee),
            _ => None,
        })
}

/// whether dropping the instruction can change what the program does
#[inline(always)]
const fn pure(kind: &InstructionKind) -> bool {
    matches!(
        kind,
        InstructionKind::Assign(_)
            | InstructionKind::Unary { .. }
            | InstructionKind::Binary { checked: false, .. }
            | InstructionKind::FieldLoad { .. }
            | InstructionKind::AddressOf { .. }
            | InstructionKind::Cast { .. }
            | InstructionKind::Select { .. }
    )
}

#[inline(always)]
const fn terminator_operand(terminator: &Term) -> Option<&Operand> {
    match terminator {
        Term::Branch { condition: operand, .. } | Term::Return(Some(operand)) => Some(operand),
        Term::Jump(_) | Term::Return(None) => None,
    }
}

#[inline(always)]
const fn terminator_operand_mut(terminator: &mut Term) -> Option<&mut Operand> {
    match terminator {
        Term::Branch { condition: operand, .. } | Term::Return(Some(operand)) => Some(operand),
        Term::Jump(_) | Term::Return(None) => None,
    }
}

fn successors(terminator: &Term) -> Vec<usize> {
    match terminator {
        Term::Jump(target) => vec![target.0 as usize],
        Term::Branch { then_block, else_block, .. } => {
            vec![then_block.0 as usize, else_block.0 as usize]
        },
        Term::Return(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::{TargetArch, optimisation};

    fn assembly(source: &str) -> String {
        crate::compile_for(source, TargetArch::X86_64).expect("compilation failed")
    }

    fn assembly_at(source: &str, level: optimisation::Level) -> String {
        optimisation::set(level);
        let asm = assembly(source);
        optimisation::set(optimisation::Level::Debug);

        asm
    }

    fn emitted(source: &str) -> Vec<String> {
        assembly(source)
            .lines()
            .filter_map(|line| line.strip_suffix(':'))
            .filter(|label| label.starts_with("nyx."))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn an_uncalled_function_is_dropped() {
        let source = "fn unused(n: i32): i32 { n } fn main(): i32 { 42 }";
        let emitted = emitted(source);

        assert!(!emitted.iter().any(|f| f == "nyx.unused"), "{emitted:?}");
        assert!(emitted.iter().any(|f| f == "nyx.main"), "{emitted:?}");
    }

    #[test]
    fn functions_without_main_are_retained_for_library_output() {
        let source = "fn first(): i32 { 1 } fn second(): i32 { first() + 1 }";
        let emitted = emitted(source);

        assert!(emitted.iter().any(|function| function == "nyx.first"), "{emitted:?}");
        assert!(emitted.iter().any(|function| function == "nyx.second"), "{emitted:?}");
    }

    #[test]
    fn a_transitively_called_function_is_kept() {
        let source = "
            fn deep(n: i32): i32 { n + 1 }
            fn middle(n: i32): i32 { deep(n) }
            fn main(): i32 { middle(41) }
        ";
        let emitted = emitted(source);

        assert!(emitted.iter().any(|f| f == "nyx.deep"), "{emitted:?}");
        assert!(emitted.iter().any(|f| f == "nyx.middle"), "{emitted:?}");
    }

    #[test]
    fn a_recursive_function_terminates_the_walk() {
        let source = "
            fn countdown(n: i32): i32 { if n <= 0 { 0 } else { countdown(n - 1) } }
            fn main(): i32 { countdown(3) }
        ";
        let emitted = emitted(source);

        assert!(emitted.iter().any(|f| f == "nyx.countdown"), "{emitted:?}");
    }

    #[test]
    fn a_comparison_does_not_drag_in_every_implementation() {
        let source = "fn main(): i32 { if 1 < 2 { 42 } else { 0 } }";
        let emitted = emitted(source);

        assert!(
            !emitted.iter().any(|f| f.contains("f64") || f.contains("char")),
            "comparing two integers must not emit the float or char implementations: {emitted:?}"
        );
    }

    #[test]
    fn mutual_recursion_is_reached_from_either_side() {
        let source = "
            fn even(n: i32): bool { if n == 0 { true } else { odd(n - 1) } }
            fn odd(n: i32): bool { if n == 0 { false } else { even(n - 1) } }
            fn main(): i32 { if even(4) { 42 } else { 0 } }
        ";
        let emitted = emitted(source);

        assert!(emitted.iter().any(|f| f == "nyx.even"), "{emitted:?}");
        assert!(emitted.iter().any(|f| f == "nyx.odd"), "{emitted:?}");
    }

    #[test]
    fn the_merge_block_after_a_diverging_branch_is_dropped() {
        let source = "
            fn pick(a: i32, b: i32): i32 { if a < b { a } else { b } }
            fn main(): i32 { pick(1, 2) }
        ";
        let labels = assembly(source)
            .lines()
            .filter(|line| line.trim_start().starts_with(".L_block_nyx.pick_"))
            .count();

        assert_eq!(labels, 2, "both arms return, so the merge block is unreachable");
    }

    #[test]
    fn an_unread_computation_is_dropped() {
        let source = "
            fn take(n: i32): i32 { let ignored = n * 6; 42 }
            fn main(): i32 { take(7) }
        ";
        let body = assembly_at(source, optimisation::Level::Sane);

        assert!(!body.contains("imul"), "the unread multiply must not be emitted:\n{body}");
    }

    #[test]
    fn a_trapping_computation_is_kept_although_unread() {
        let source = "
            fn take(n: i32): i32 { let ignored = n * 6; 42 }
            fn main(): i32 { take(7) }
        ";
        let body = assembly(source);

        assert!(
            body.contains("imul"),
            "at debug the multiply is checked, so dropping it would drop a panic:\n{body}"
        );
    }

    #[test]
    fn a_call_is_kept_even_when_its_result_is_ignored() {
        let source = "
            fn effect(n: i32): i32 { n }
            fn main(): i32 { let ignored = effect(1); 42 }
        ";
        let body = assembly(source);

        assert!(body.contains("call    nyx.effect"), "a call may have effects:\n{body}");
    }

    #[test]
    fn a_string_left_without_a_referent_is_dropped() {
        let source = r#"fn main(): i32 { let ignored = "dead"; 42 }"#;
        let body = assembly(source);

        assert!(!body.contains("dead"), "the string pool must not keep it:\n{body}");
        assert!(!body.contains(".L_str_"), "no string label may remain:\n{body}");
    }
}
