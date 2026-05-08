use crate::lir::{
    Block, BlockId, Function, Term, VReg,
    target::{Instruction, Target},
};
use std::collections::BTreeSet;

pub struct Liveness {
    pub(in crate::lir::regalloc) blocks: Vec<BlockLiveness>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct BlockLiveness {
    pub(in crate::lir::regalloc) live_in: BTreeSet<VReg>,
    pub(in crate::lir::regalloc) live_out: BTreeSet<VReg>,
}

pub struct InstructionLiveness {
    /// `points[i]` = live set *before* instruction `i` executes
    /// `points[n]` = live_out of the block
    pub(in crate::lir::regalloc) points: Vec<BTreeSet<VReg>>,
}

impl Liveness {
    pub fn analyse<T: Target>(function: &Function<T>) -> Self {
        let n = function.blocks.len();
        let mut blocks = vec![BlockLiveness::default(); n];
        let mut changed = true;

        while changed {
            changed = false;

            for idx in (0..n).rev() {
                let block = &function.blocks[idx];

                let mut new_out = BTreeSet::new();
                for successor in block.successors() {
                    for &vreg in &blocks[successor.0 as usize].live_in {
                        new_out.insert(vreg);
                    }
                }

                let (uses, defs) = block.uses_and_defs();

                let mut new_in = new_out
                    .iter()
                    .filter(|vreg| !defs.contains(vreg))
                    .copied()
                    .collect::<BTreeSet<_>>();

                for vreg in uses {
                    new_in.insert(vreg);
                }

                if new_in != blocks[idx].live_in || new_out != blocks[idx].live_out {
                    blocks[idx].live_in = new_in;
                    blocks[idx].live_out = new_out;
                    changed = true;
                }
            }
        }

        Self { blocks }
    }

    pub fn instruction_liveness<T: Target>(
        &self,
        function: &Function<T>,
        idx: usize,
    ) -> InstructionLiveness {
        let block = &function.blocks[idx];
        let n = block.instructions.len();
        let mut points = vec![BTreeSet::new(); n + 1];
        points[n] = self.blocks[idx].live_out.clone();

        let mut live = points[n].clone();

        for idx in (0..n).rev() {
            let instruction = &block.instructions[idx];

            for def in instruction.defs() {
                live.remove(def);
            }

            for &used in instruction.uses() {
                live.insert(used);
            }

            for &clob in instruction.clobbers() {
                let _ = clob;
            }

            for (vreg, _) in instruction.precoloured_uses() {
                live.insert(*vreg);
            }

            points[idx] = live.clone();
        }

        for &vreg in block.term.uses_of() {
            live.insert(vreg);
        }

        InstructionLiveness { points }
    }
}

impl<I> Block<I> {
    pub fn uses_and_defs<T>(&self) -> (BTreeSet<VReg>, BTreeSet<VReg>)
    where
        T: Target<Instruction = I>,
        I: Instruction<T>,
    {
        let mut uses = BTreeSet::new();
        let mut defs = BTreeSet::new();

        for instruction in &self.instructions {
            for &u in instruction.uses() {
                if !defs.contains(&u) {
                    uses.insert(u);
                }
            }

            for &d in instruction.defs() {
                defs.insert(d);
            }

            for (v, _) in instruction.precoloured_uses() {
                if !defs.contains(v) {
                    uses.insert(*v);
                }
            }
        }

        for &v in self.term.uses_of() {
            if !defs.contains(&v) {
                uses.insert(v);
            }
        }

        (uses, defs)
    }

    #[inline(always)]
    pub fn successors(&self) -> Vec<BlockId> {
        match self.term {
            Term::Jump(id) => vec![id],
            Term::Branch {
                then_block,
                else_block,
                ..
            } => {
                vec![then_block, else_block]
            }
            Term::Return(_) => vec![],
        }
    }
}
