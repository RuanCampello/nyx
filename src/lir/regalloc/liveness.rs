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
                block.for_each_successor(|successor| {
                    new_out.extend(blocks[successor.0 as usize].live_in.iter().copied());
                });

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
}

impl<I> Block<I> {
    pub fn uses_and_defs<T>(&self) -> (BTreeSet<VReg>, BTreeSet<VReg>)
    where
        T: Target<Instruction = I>,
        I: Instruction<T>,
    {
        let mut uses = BTreeSet::new();
        let mut defs = BTreeSet::new();
        let mut instruction_uses = Vec::new();

        for instruction in &self.instructions {
            instruction_uses.clear();
            instruction.uses(&mut instruction_uses);
            for &u in &instruction_uses {
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
    pub fn for_each_successor(&self, mut f: impl FnMut(BlockId)) {
        match self.term {
            Term::Jump(id) => f(id),
            Term::Branch {
                then_block,
                else_block,
                ..
            } => {
                f(then_block);
                f(else_block);
            }
            Term::Return(_) => {}
        }
    }
}
