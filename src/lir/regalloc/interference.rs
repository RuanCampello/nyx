use crate::lir::{Function, VReg, regalloc::liveness::Liveness, target::Target};
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
        todo!()
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

    pub fn degree(&self, vreg: VReg) -> usize {
        self.neighbours(&vreg).len()
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

    fn merge(&mut self, keep: VReg, remove: VReg) {
        let neighbours = self.neighbours(&remove).iter().copied().collect::<Vec<_>>();
        for neighbour in neighbours {
            if neighbour != keep {
                self.add_edge(keep, neighbour);
            }
        }

        self.edges.remove(&remove);
    }
}
