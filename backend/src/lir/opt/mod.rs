//! Pre-allocation combiner over LIR
//!
//! Modelled on GCC's `combine` rather than its [define_peephole2](https://gcc.gnu.org/onlinedocs/gccint/define_005fpeephole2.html): the rules
//! here are data-flow driven, matching a short def-use chain and folding it
//! into one instruction
//!
//! Running before register allocation is what makes that possible, virtual
//! registers are still infinite, so a chain that collapses takes its intermediate
//! values with it instead of leaving copies behind
//!
//! Syntactic rewrites that need physical registers, such as dropping a jump to
//! the next block, belong in a separate post-allocation pass

use crate::lir::{Block, Function, VReg, target::Instruction, target::Target};

mod fuse;
#[cfg(test)]
mod tests;

/// How often each virtual register is defined and used across a whole function
///
/// Function-wide rather than per-block on purpose
/// A rule that deletes a definition has to know the value
/// is not wanted anywhere else, and counting globally answers that
/// without a liveness analysis: a vreg defined once and
/// used once, with both sites in the same block, cannot be live across an edge
pub(super) struct DefUse {
    defs: Vec<u32>,
    uses: Vec<u32>,
}

pub(super) fn combine<T: Target>(function: &mut Function<T>) {
    let counts = DefUse::of(function);

    for block in &mut function.blocks {
        fuse::compare_branch::<T>(block, &counts);
    }
}

impl DefUse {
    fn of<T: Target>(function: &Function<T>) -> Self {
        let width = function.next_vreg as usize;
        let mut counts = Self { defs: vec![0; width], uses: vec![0; width] };
        let mut uses = Vec::new();

        for block in &function.blocks {
            for instruction in &block.instructions {
                for def in instruction.defs() {
                    counts.defs[def.0 as usize] += 1;
                }

                uses.clear();
                instruction.uses(&mut uses);
                for use_of in &uses {
                    counts.uses[use_of.0 as usize] += 1;
                }
            }

            for use_of in block.term.uses_of() {
                counts.uses[use_of.0 as usize] += 1;
            }
        }

        counts
    }

    /// whether `vreg` has exactly one definition and exactly one consumer, so
    /// that folding the two together strands nothing
    #[inline(always)]
    fn is_single_use(&self, vreg: VReg) -> bool {
        let idx = vreg.0 as usize;
        self.defs[idx] == 1 && self.uses[idx] == 1
    }
}

/// The position of the sole instruction in `block` defining `vreg`, if it is
/// defined in this block at all
#[inline]
fn definition<T: Target>(block: &Block<T::Instruction>, vreg: VReg) -> Option<usize> {
    block
        .instructions
        .iter()
        .position(|instruction| instruction.defs().contains(&vreg))
}

/// whether the flags surviving to the end of the block are still the ones set
/// before `from`
#[inline]
fn flags_reach_terminator<T: Target>(block: &Block<T::Instruction>, from: usize) -> bool {
    !block.instructions[from + 1..].iter().any(Instruction::writes_flags)
}
