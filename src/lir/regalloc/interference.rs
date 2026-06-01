use crate::lir::{
    Function, VReg,
    regalloc::liveness::Liveness,
    target::{Instruction, Target},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(in crate::lir::regalloc) struct Interference {
    pub edges: Vec<BTreeSet<VReg>>,
    /// VRegs whose live range crosses at least one call (or instruction with clobbers)
    /// These must not be placed in caller-saved registers
    pub call_crossed: BTreeSet<VReg>,
    pub stack_forced: BTreeSet<VReg>,
}

impl Interference {
    pub fn build<T: Target>(function: &Function<T>, liveness: &Liveness) -> Self {
        let mut graph = Self {
            edges: vec![BTreeSet::new(); function.vreg_types.len()],
            call_crossed: BTreeSet::new(),
            stack_forced: BTreeSet::new(),
        };

        for (idx, block) in function.blocks.iter().enumerate() {
            let mut live = liveness.blocks[idx].live_out.clone();
            let mut uses = Vec::new();

            live.extend(block.term.uses_of().iter().copied());

            for instruction in block.instructions.iter().rev() {
                if !instruction.clobbers().is_empty() {
                    graph.call_crossed.extend(live.iter().copied());
                }

                for &def in instruction.defs() {
                    for &live_vreg in &live {
                        graph.add_edge(def, live_vreg);
                    }
                }

                for def in instruction.defs() {
                    live.remove(def);
                }

                uses.clear();
                instruction.uses(&mut uses);
                for &used in &uses {
                    live.insert(used);
                }

                for (vreg, _) in instruction.precoloured_uses() {
                    live.insert(*vreg);
                }

                graph.stack_forced.extend(instruction.stack_forced().iter().copied());
            }
        }

        graph
    }

    #[inline(always)]
    pub fn nodes(&self) -> impl Iterator<Item = VReg> + '_ {
        (0..self.edges.len()).map(|idx| VReg(idx as u32))
    }

    pub fn neighbours(&self, vreg: &VReg) -> &BTreeSet<VReg> {
        &self.edges[vreg.0 as usize]
    }

    pub fn degree(&self, vreg: &VReg) -> usize {
        self.neighbours(vreg).len()
    }

    pub fn push_node(
        &self,
        id: VReg,
        stack: &mut Vec<VReg>,
        removed: &mut BTreeSet<VReg>,
        degree: &mut BTreeMap<VReg, usize>,
    ) {
        removed.insert(id);
        stack.push(id);

        for &neighbour in self.neighbours(&id) {
            if !removed.contains(&neighbour)
                && let Some(d) = degree.get_mut(&neighbour)
            {
                *d = d.saturating_sub(1);
            }
        }
    }

    pub fn add_edge(&mut self, a: VReg, b: VReg) {
        if a != b {
            self.edges[a.0 as usize].insert(b);
            self.edges[b.0 as usize].insert(a);
        }
    }
}
