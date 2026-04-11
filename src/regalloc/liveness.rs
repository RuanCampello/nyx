//! Backward liveness analysis over the MIR CFG.
//!
//! Computes for each basic block:
//!   - `live_in`  — values live on entry to the block
//!   - `live_out` — values live on exit from the block
//!
//! Then reconstructs per-instruction live sets by replaying each block
//! backwards, which the interference graph builder needs.

use crate::mir::{Function, ValueId};
use std::collections::HashSet;

/// Full liveness for a funciton
pub struct Liveness {
    blocks: Vec<BlockLiveness>,
}

/// Live sets for each basic block
#[derive(Debug, Default)]
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
