//! Removal of code nothing can reach or observe
//!
//! Each kind of removal is a sweep: a [Program] sweep decides which functions exist at all, a
//! [Body] sweep prunes inside one that survived. Adding a kind means writing the impl and naming
//! it in one of the three tables below, nothing else

use crate::{
    hir::FunctionId,
    mir::{Const, Function, InstructionKind, Mir, Operand, Terminator as Term, cfg},
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
        cfg::CfgEditor::new(function).remove_unreachable()
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
                instruction.kind.each_operand(|operand| {
                    if let Operand::Place(place) = operand {
                        read.insert(place.id);
                    }
                });

                if let InstructionKind::AddressOf { src, .. } = &instruction.kind {
                    read.insert(src.id);
                }

                if instruction.kind.properties().writes_through_dest {
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
                !instruction.kind.can_discard() || read.contains(&instruction.dest.id)
            });
            removed |= block.instructions.len() != before;
        }

        removed
    }
}

impl Program for Strings {
    /// drop string constants nothing refers to, renumbering the survivors
    fn sweep(&self, mir: &mut Mir) {
        let mut used = vec![false; mir.strings.len()];

        for function in &mir.functions {
            for block in &function.blocks {
                for instruction in &block.instructions {
                    instruction.kind.each_operand(|operand| {
                        if let Operand::Const(Const::Str(id)) = operand {
                            used[id.index()] = true;
                        }
                    });
                }

                if let Some(Operand::Const(Const::Str(id))) = terminator_operand(&block.terminator)
                {
                    used[id.index()] = true;
                }
            }
        }

        if used.iter().all(|live| *live) {
            return;
        }

        let renumbered = mir.strings.retain_and_remap(&used);

        for function in &mut mir.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    instruction.kind.each_operand_mut(|operand| {
                        if let Operand::Const(Const::Str(id)) = operand {
                            *id = renumbered[id.index()];
                        }
                    });
                }

                if let Some(Operand::Const(Const::Str(id))) =
                    terminator_operand_mut(&mut block.terminator)
                {
                    *id = renumbered[id.index()];
                }
            }
        }
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

fn callees<'a>(function: &'a Function<'_>) -> impl Iterator<Item = FunctionId> + 'a {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match &instruction.kind {
            InstructionKind::Call { callee, .. } => Some(*callee),
            _ => None,
        })
}

#[inline(always)]
const fn terminator_operand<'a, 'hir>(terminator: &'a Term<'hir>) -> Option<&'a Operand<'hir>> {
    match terminator {
        Term::Branch { condition: operand, .. } | Term::Return(Some(operand)) => Some(operand),
        Term::Jump(_) | Term::Return(None) => None,
    }
}

#[inline(always)]
const fn terminator_operand_mut<'a, 'hir>(
    terminator: &'a mut Term<'hir>,
) -> Option<&'a mut Operand<'hir>> {
    match terminator {
        Term::Branch { condition: operand, .. } | Term::Return(Some(operand)) => Some(operand),
        Term::Jump(_) | Term::Return(None) => None,
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

    fn pick_blocks(source: &str) -> usize {
        assembly(source)
            .lines()
            .filter(|line| line.trim_start().starts_with(".L_block_nyx.pick_"))
            .count()
    }

    #[test]
    fn the_merge_block_after_a_diverging_branch_is_dropped() {
        let source = "
            fn pick(a: i32, b: i32): i32 { if a < b { return a; } else { return b; } }
            fn main(): i32 { pick(1, 2) }
        ";

        assert_eq!(pick_blocks(source), 2, "both arms return, so the merge is unreachable");
    }

    #[test]
    fn a_tail_if_returns_through_its_merge_block() {
        let source = "
            fn pick(a: i32, b: i32): i32 { if a < b { a } else { b } }
            fn main(): i32 { pick(1, 2) }
        ";

        assert_eq!(pick_blocks(source), 3, "the merge carries the value the function returns");
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
