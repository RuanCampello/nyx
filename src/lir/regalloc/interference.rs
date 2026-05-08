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
}

impl Interference {
    pub fn build<T: Target>(function: &Function<T>, liveness: &Liveness) -> Self {
        let mut graph = Self::default();

        for (idx, _) in function.vreg_types.iter().enumerate() {
            graph.add_node(VReg(idx as u32));
        }

        for (idx, block) in function.blocks.iter().enumerate() {
            let mut live = liveness.blocks[idx].live_out.clone();
            let mut uses = Vec::new();

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

                for &def in instruction.defs() {
                    graph.add_node(def);
                }
            }
        }

        graph
    }

    #[inline(always)]
    pub fn nodes(&self) -> impl Iterator<Item = VReg> + '_ {
        (0..self.edges.len()).map(|idx| VReg(idx as u32))
    }

    pub fn neighbours(&self, vreg: &VReg) -> &BTreeSet<VReg> {
        use std::sync::OnceLock;

        static EMPTY: OnceLock<BTreeSet<VReg>> = OnceLock::new();

        self.edges
            .get(vreg.0 as usize)
            .unwrap_or_else(|| EMPTY.get_or_init(BTreeSet::new))
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
            if !removed.contains(&neighbour) {
                if let Some(d) = degree.get_mut(&neighbour) {
                    *d = d.saturating_sub(1);
                }
            }
        }
    }

    pub fn add_edge(&mut self, a: VReg, b: VReg) {
        if a == b {
            return;
        }
        self.add_node(a);
        self.add_node(b);

        self.edges[a.0 as usize].insert(b);
        self.edges[b.0 as usize].insert(a);
    }

    pub fn add_node(&mut self, v: VReg) {
        let len = v.0 as usize + 1;
        if self.edges.len() < len {
            self.edges.resize_with(len, BTreeSet::new);
        }
    }
}
