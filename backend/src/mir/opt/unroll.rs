//! compile-time evaluation of constant-trip-count loops
//!
//! A loop whose entry state is fully known and whose body only computes is not worth
//! unrolling into repeated code: it can simply be run at compile time and replaced by the
//! values it would have produced

use crate::{
    Span,
    hir::ids::IndexVec,
    mir::{
        BlockId, Const, Function, Instruction, InstructionKind, Operand, Place, Terminator,
        ValueId,
        cfg::{self, CfgEditor},
        opt::{fold, propagate::Lattice},
    },
};

/// one natural loop, with the single block that enters it from outside
struct Loop {
    header: BlockId,
    /// membership by block index, so the containment test is an index
    blocks: IndexVec<BlockId, bool>,
    preheader: BlockId,
}

/// what running the loop produced: where control left, and the final value of everything
/// the loop assigned
struct Evaluated<'hir> {
    exit: BlockId,
    writes: Vec<(ValueId, Const<'hir>, Span)>,
}

/// a loop is evaluated only if it finishes inside this many instructions, one that runs
/// longer is left alone
const STEP_BUDGET: u32 = 100_000;

pub(super) fn run<'hir>(
    function: &mut Function<'hir>,
    exits: &IndexVec<BlockId, Option<Vec<Lattice<'hir>>>>,
) -> bool {
    if exits.len() != function.blocks.len() {
        return false;
    }

    let reachable: IndexVec<_, _> = exits.iter().map(Option::is_some).collect();
    let predecessors = cfg::predecessors(function, Some(&reachable));
    let dominators = dominators(function, &predecessors);

    for candidate in loops(function, &predecessors, &dominators, &reachable) {
        let Some(state) = exits[candidate.preheader].as_ref() else {
            continue;
        };
        let Some(evaluated) = evaluate(function, &candidate, state) else {
            continue;
        };

        replace(function, &candidate, evaluated);
        return true;
    }

    false
}

fn evaluate<'hir>(
    function: &Function<'hir>,
    target: &Loop,
    state: &[Lattice<'hir>],
) -> Option<Evaluated<'hir>> {
    let mut env: Vec<_> = state.iter().map(|value| value.constant()).collect();
    let mut written: Vec<_> = vec![None; env.len()];
    let mut fuel = STEP_BUDGET;
    let mut block = target.header;

    while target.blocks[block] {
        for instruction in &function.blocks[block].instructions {
            fuel = fuel.checked_sub(1)?;

            let slot = instruction.dest.id.0 as usize;
            env[slot] = Some(step(&instruction.kind, &env)?);
            written[slot] = Some(instruction.span);
        }

        block = match &function.blocks[block].terminator {
            Terminator::Jump(target) => *target,
            Terminator::Branch { condition, then_block, else_block } => {
                match resolve(*condition, &env)? {
                    Const::Bool(true) => *then_block,
                    Const::Bool(false) => *else_block,
                    _ => return None,
                }
            },
            Terminator::Return(_) => return None,
        };
    }

    let writes = written
        .iter()
        .enumerate()
        .filter_map(|(slot, span)| Some((ValueId(slot as u32), env[slot]?, (*span)?)))
        .collect();

    Some(Evaluated { exit: block, writes })
}

fn step<'hir>(kind: &InstructionKind<'hir>, env: &[Option<Const<'hir>>]) -> Option<Const<'hir>> {
    match kind {
        InstructionKind::Assign(operand) => resolve(*operand, env),
        InstructionKind::Unary { operation, rhs } => fold::unary(*operation, resolve(*rhs, env)?),
        InstructionKind::Binary { operation, lhs, rhs, .. } => {
            fold::binary(*operation, resolve(*lhs, env)?, resolve(*rhs, env)?)
        },
        InstructionKind::Cast { src, typ } => fold::cast(resolve(*src, env)?, *typ),
        _ => None,
    }
}

fn resolve<'hir>(operand: Operand<'hir>, env: &[Option<Const<'hir>>]) -> Option<Const<'hir>> {
    match operand {
        Operand::Const(value) => Some(value),
        Operand::Place(place) => env[place.id.0 as usize],
    }
}

fn replace<'hir>(function: &mut Function<'hir>, target: &Loop, evaluated: Evaluated<'hir>) {
    let settled: Vec<_> = evaluated
        .writes
        .into_iter()
        .map(|(id, value, span)| Instruction {
            dest: Place { id, typ: function.locals[id.0 as usize].1 },
            kind: InstructionKind::Assign(Operand::Const(value)),
            span,
        })
        .collect();

    function.blocks[target.preheader].instructions.extend(settled);
    CfgEditor::new(function).replace_terminator(target.preheader, Terminator::Jump(evaluated.exit));
}

fn loops(
    function: &Function<'_>,
    predecessors: &IndexVec<BlockId, Vec<BlockId>>,
    dominators: &IndexVec<BlockId, IndexVec<BlockId, bool>>,
    reachable: &IndexVec<BlockId, bool>,
) -> Vec<Loop> {
    let mut found = Vec::new();

    for (latch, block) in function.blocks.iter_enumerated().filter(|(id, _)| reachable[*id]) {
        for header in
            cfg::successors(&block.terminator).into_iter().filter(|&h| dominators[latch][h])
        {
            let blocks = natural_loop(function, header, latch, predecessors);

            let mut outside = predecessors[header].iter().filter(|&&block| !blocks[block]);
            let (Some(&preheader), None) = (outside.next(), outside.next()) else {
                continue;
            };

            if function.blocks[preheader].terminator != Terminator::Jump(header) {
                continue;
            }

            let breached = function
                .blocks
                .indices()
                .filter(|&block| reachable[block] && !blocks[block])
                .any(|block| {
                    cfg::successors(&function.blocks[block].terminator)
                        .iter()
                        .any(|&edge| edge != header && blocks[edge])
                });

            if !breached {
                found.push(Loop { header, blocks, preheader });
            }
        }
    }

    found
}

fn natural_loop(
    function: &Function<'_>,
    header: BlockId,
    latch: BlockId,
    predecessors: &IndexVec<BlockId, Vec<BlockId>>,
) -> IndexVec<BlockId, bool> {
    let mut blocks = IndexVec::from_elem(false, function.blocks.len());
    blocks[header] = true;

    let mut stack = Vec::new();
    if latch != header {
        blocks[latch] = true;
        stack.push(latch);
    }

    while let Some(block) = stack.pop() {
        for &predecessor in &predecessors[block] {
            if !blocks[predecessor] {
                blocks[predecessor] = true;
                stack.push(predecessor);
            }
        }
    }

    blocks
}

fn dominators(
    function: &Function<'_>,
    predecessors: &IndexVec<BlockId, Vec<BlockId>>,
) -> IndexVec<BlockId, IndexVec<BlockId, bool>> {
    let count = function.blocks.len();

    let mut dominators = IndexVec::from_elem(IndexVec::from_elem(true, count), count);
    dominators[BlockId::ENTRY] = IndexVec::from_elem(false, count);
    dominators[BlockId::ENTRY][BlockId::ENTRY] = true;

    let mut changed = true;
    while changed {
        changed = false;

        for block in function.blocks.indices().skip(1) {
            // unreachable blocks dominate nothing, any loop they form has an unreachable
            // preheader, so no rewrite can come of it
            let mut next = IndexVec::from_elem(!predecessors[block].is_empty(), count);

            for &predecessor in &predecessors[block] {
                for (slot, dominated) in next.iter_enumerated_mut() {
                    *dominated &= dominators[predecessor][slot];
                }
            }
            next[block] = true;

            if next != dominators[block] {
                dominators[block] = next;
                changed = true;
            }
        }
    }

    dominators
}

#[cfg(test)]
mod tests {
    use crate::{TargetArch, optimisation};

    fn main_body(source: &str, level: optimisation::Level) -> String {
        optimisation::set(level);
        let asm = crate::compile_for(source, TargetArch::X86_64).expect("compilation failed");
        optimisation::set(optimisation::Level::Debug);

        let start = asm.find("\nnyx.main:\n").expect("main was not emitted");
        let rest = &asm[start + 1..];

        match rest.find(".globl") {
            Some(end) => rest[..end].to_string(),
            None => rest.to_string(),
        }
    }

    #[test]
    fn a_constant_trip_loop_is_evaluated_at_max() {
        let source = "fn main(): i32 { let mut t = 0; loop i in 1..=10 { t = t + i; } t }";
        let body = main_body(source, optimisation::Level::Max);

        assert!(body.contains("$55"), "expected the loop to fold to 55:\n{body}");
    }

    #[test]
    fn sane_leaves_the_loop_alone() {
        let source = "fn main(): i32 { let mut t = 0; loop i in 1..=10 { t = t + i; } t }";
        let body = main_body(source, optimisation::Level::Sane);

        assert!(!body.contains("$55"), "sane must not evaluate the loop:\n{body}");
    }

    #[test]
    fn break_and_continue_are_honoured() {
        let source = r#"
            fn main(): i32 {
                let mut t = 0;
                loop i in 0..100 {
                    if i == 7 { break; }
                    if i == 3 { continue; }
                    t = t + i;
                }
                t
            }
        "#;
        let body = main_body(source, optimisation::Level::Max);

        assert!(body.contains("$18"), "expected 0+1+2+4+5+6 = 18:\n{body}");
    }

    #[test]
    fn nested_loops_are_evaluated() {
        let source = r#"
            fn main(): i32 {
                let mut t = 0;
                loop i in 1..=4 { loop j in 1..=3 { t = t + (i * j); } }
                t
            }
        "#;
        let body = main_body(source, optimisation::Level::Max);

        assert!(body.contains("$60"), "expected 60:\n{body}");
    }

    #[test]
    fn a_runtime_bound_is_left_alone() {
        let source = r#"
            fn sum_to(n: i32): i32 { let mut t = 0; loop i in 1..=n { t = t + i; } t }
            fn main(): i32 { sum_to(10) }
        "#;
        let body = main_body(source, optimisation::Level::Max);

        assert!(body.contains("call"), "the call must survive:\n{body}");
    }

    #[test]
    fn a_loop_with_a_call_is_left_alone() {
        let source = r#"
            fn tick(x: i32): i32 { x }
            fn main(): i32 {
                let mut t = 0;
                loop i in 1..=3 { t = t + tick(i); }
                t
            }
        "#;
        let body = main_body(source, optimisation::Level::Max);

        assert!(body.contains("call"), "the call must survive:\n{body}");
        assert!(!body.contains("$6"), "the loop must not be evaluated:\n{body}");
    }

    #[test]
    fn a_divergent_loop_terminates_compilation() {
        let source = "fn main(): i32 { let mut n = 0; loop { n = n + 1; if n < 0 { break; } } n }";
        let body = main_body(source, optimisation::Level::Max);

        assert!(!body.is_empty());
    }
}
