use crate::lir::{
    Function, VReg,
    regalloc::liveness::Liveness,
    target::{Instruction, Target},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(in crate::lir::regalloc) struct Interference {
    pub edges: BTreeMap<VReg, BTreeSet<VReg>>,
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
            let instr_liveness = liveness.instruction_liveness(function, idx);

            for (i, instruction) in block.instructions.iter().enumerate() {
                let live_before = &instr_liveness.points[i];

                for &def in instruction.defs() {
                    for &live in live_before {
                        graph.add_edge(def, live);
                    }
                }

                let live: Vec<_> = live_before.iter().copied().collect();
                for j in 0..live.len() {
                    for k in (j + 1)..live.len() {
                        graph.add_edge(live[j], live[k]);
                    }
                }

                if !instruction.clobbers().is_empty() {
                    let live_after = &instr_liveness.points[i + 1];

                    for &vreg in live_after {
                        graph.call_crossed.insert(vreg);
                    }
                }

                for &def in instruction.defs() {
                    graph.add_node(def);
                }
            }

            let live_out: Vec<_> = liveness.blocks[idx].live_out.iter().copied().collect();

            for i in 0..live_out.len() {
                for j in (i + 1)..live_out.len() {
                    graph.add_edge(live_out[i], live_out[j]);
                }
            }
        }

        graph
    }

    pub fn coalesce<T: Target>(&mut self, func: &Function<T>) {
        let mut worklist = Vec::new();

        for block in &func.blocks {
            for instruction in &block.instructions {
                if let Some((dest, src)) = instruction.as_copy() {
                    if dest != src && !self.interferes(&dest, &src) {
                        worklist.push((dest, src));
                    }
                }
            }
        }

        for (a, b) in worklist {
            if self.can_coalesce(&a, &b, T::gprs().len()) {
                self.merge(&a, &b);
            }
        }
    }

    #[inline(always)]
    pub fn interferes(&self, a: &VReg, b: &VReg) -> bool {
        self.edges.get(a).is_some_and(|neigbour| neigbour.contains(b))
    }

    fn can_coalesce(&self, a: &VReg, b: &VReg, k: usize) -> bool {
        if self.interferes(a, b) {
            return false;
        }
        let na: BTreeSet<_> = self.neighbours(a).iter().copied().collect();
        let nb: BTreeSet<_> = self.neighbours(b).iter().copied().collect();
        na.union(&nb).count() < k
    }

    #[inline(always)]
    pub fn nodes(&self) -> impl Iterator<Item = VReg> + '_ {
        self.edges.keys().copied()
    }

    pub fn neighbours(&self, vreg: &VReg) -> &BTreeSet<VReg> {
        use std::sync::OnceLock;

        static EMPTY: OnceLock<BTreeSet<VReg>> = OnceLock::new();

        self.edges.get(vreg).unwrap_or_else(|| EMPTY.get_or_init(BTreeSet::new))
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
        self.edges.entry(a).or_default().insert(b);
        self.edges.entry(b).or_default().insert(a);
    }

    pub fn add_node(&mut self, v: VReg) {
        self.edges.entry(v).or_default();
    }

    fn merge(&mut self, keep: &VReg, remove: &VReg) {
        let neighbours = self.neighbours(&remove).iter().copied().collect::<Vec<_>>();
        for neighbour in neighbours {
            if neighbour != *keep {
                self.add_edge(*keep, neighbour);
            }
        }

        self.edges.remove(&remove);
    }
}
