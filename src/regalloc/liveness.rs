//! Backward liveness analysis over the MIR CFG.
//!
//! Computes for each basic block:
//!   - `live_in`  — values live on entry to the block
//!   - `live_out` — values live on exit from the block
//!
//! Then reconstructs per-instruction live sets by replaying each block
//! backwards, which the interference graph builder needs.

use crate::mir::{Block, BlockId, Function, Terminator, ValueId};
use std::collections::HashSet;

/// Full liveness for a funciton
pub struct Liveness {
    blocks: Vec<BlockLiveness>,
}

/// Live sets for each basic block
#[derive(Debug, Default, Clone)]
pub struct BlockLiveness {
    live_in: HashSet<ValueId>,
    live_out: HashSet<ValueId>,
}

/// Live set at each instruction boundary within a basic block.
///
/// `points[i]` is the live set *after* instruction `i` executes
/// (i.e. before instruction `i+1`).
/// `points[instructions.len()]` is `live_out` for the block.
#[derive(Debug)]
pub struct InstructionLiveness {
    points: Vec<HashSet<ValueId>>,
}

impl Liveness {
    pub fn analyse(function: &Function) -> Self {
        let n = function.blocks.len();
        let mut blocks = vec![BlockLiveness::default(); n];

        let mut changed = true;

        while changed {
            changed = false;

            for idx in (0..n).rev() {
                let block = &function.blocks[idx];

                let mut new_out = HashSet::new();
                for successor in block.successors() {
                    for &value in &blocks[successor.0 as usize].live_in {
                        new_out.insert(value);
                    }
                }

                let (uses, defs) = block.use_def();
                let mut new_in = new_out
                    .iter()
                    .filter(|v| !defs.contains(v))
                    .copied()
                    .collect::<HashSet<_>>();

                for value in uses {
                    new_in.insert(value);
                }

                if new_in != blocks[idx].live_in || new_out != blocks[idx].live_out {
                    blocks[idx].live_in = new_in;
                    blocks[idx].live_out = new_out;

                    changed = true;
                }
            }
        }

        Liveness { blocks }
    }

    pub fn instruction_liveness(&self, function: &Function, idx: usize) -> InstructionLiveness {
        let block = &function.blocks[idx];
        let mut live = self.blocks[idx].live_out.clone();

        let n = block.instructions.len();
        let mut points = vec![HashSet::new(); n + 1];
        points[n] = live.clone();

        for idx in (0..n).rev() {
            let instruction = &block.instructions[idx];

            live.remove(&instruction.dest.id);

            for used in instruction.kind.uses_of() {
                live.insert(used);
            }

            points[idx] = live.clone();
        }

        InstructionLiveness { points }
    }
}

impl Block {
    /// Compute `(use, def)` sets for a block
    ///
    /// - `use` = values read before being written in this block
    /// - `def` = values written in this block
    fn use_def(&self) -> (HashSet<ValueId>, HashSet<ValueId>) {
        let mut uses = HashSet::new();
        let mut defs = HashSet::new();

        for instruction in &self.instructions {
            // uses: every operand read that hasn't been defined in this block yet
            for id in instruction.kind.uses_of() {
                if !defs.contains(&id) {
                    uses.insert(id);
                }
            }

            // def: the destination
            defs.insert(instruction.dest.id);
        }

        for id in self.terminator.uses_of() {
            if !defs.contains(&id) {
                uses.insert(id);
            }
        }

        (uses, defs)
    }

    #[inline(always)]
    fn successors(&self) -> Vec<BlockId> {
        match self.terminator {
            Terminator::Jump(target) => vec![target],
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![then_block, else_block],
            Terminator::Return(_) => vec![],
        }
    }
}
