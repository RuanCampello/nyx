use crate::{
    mir::{Function, InstructionKind, Operand, ValueId},
    regalloc::{colouring::Reg, liveness::Liveness},
};
use std::collections::{HashMap, HashSet};

/// Undirected interference graph.
///
/// Nodes are `ValueId`s. An edge means the two values must not share a register.
#[derive(Debug, Default, PartialEq)]
pub struct Interference {
    edges: HashMap<ValueId, HashSet<ValueId>>,
    /// values whose live range crosses at least one `call` instruction
    ///
    /// the colourer must not assign these to caller-saved registers
    pub(in crate::regalloc) call_crossed: HashSet<ValueId>,
}

impl Interference {
    pub(in crate::regalloc) const K: usize = Reg::ALL.len();

    pub fn build(function: &Function) -> Self {
        let liveness = Liveness::analyse(function);
        let mut graph = Self::default();

        // ensure every value has a node, even isolated ones
        for (id, _) in &function.locals {
            graph.add_node(*id);
        }

        for (block_idx, block) in function.blocks.iter().enumerate() {
            let il = liveness.instruction_liveness(function, block_idx);

            // walk instructions forward: il.points[i] is live before instruction i
            for (i, instr) in block.instructions.iter().enumerate() {
                let live_before = &il.points[i];

                // the defined value interferes with everything live at this point
                for &live_val in live_before {
                    graph.add_edge(instr.dest.id, live_val);
                }

                // all simultaneously live values interfere with each other
                let live_vec: Vec<ValueId> = live_before.iter().copied().collect();
                for j in 0..live_vec.len() {
                    for k in (j + 1)..live_vec.len() {
                        graph.add_edge(live_vec[j], live_vec[k]);
                    }
                }

                // values live after a call cross the call boundary and must not
                // be placed in caller-saved registers (they will be clobbered)
                if matches!(instr.kind, InstructionKind::Call { .. }) {
                    let live_after = &il.points[i + 1];
                    for &val in live_after {
                        graph.call_crossed.insert(val);
                    }
                }

                graph.add_node(instr.dest.id);
            }

            // terminator: values it reads are live at the block exit
            // they all interfere with each other
            let live_out = &liveness.blocks[block_idx].live_out;
            let live_out_vec: Vec<ValueId> = live_out.iter().copied().collect();

            for i in 0..live_out_vec.len() {
                for j in (i + 1)..live_out_vec.len() {
                    graph.add_edge(live_out_vec[i], live_out_vec[j]);
                }
            }
        }

        graph
    }

    pub fn coalesce(&mut self, function: &Function) {
        let mut worklist = Vec::new();

        for block in &function.blocks {
            for instruction in &block.instructions {
                if let InstructionKind::Assign(Operand::Place(src)) = &instruction.kind {
                    let dest = instruction.dest.id;
                    let src = src.id;

                    if dest != src && !self.interferes(&dest, &src) {
                        worklist.push((dest, src))
                    }
                }
            }
        }

        for (a, b) in worklist {
            if self.can_coalesce(a, b) {
                self.merge(a, b);
            }
        }
    }

    fn can_coalesce(&self, a: ValueId, b: ValueId) -> bool {
        if self.interferes(&a, &b) {
            return false;
        }

        let a = self.neighbours(a).iter().copied().collect::<HashSet<_>>();
        let b = self.neighbours(b).iter().copied().collect::<HashSet<_>>();

        let degrees = a.union(&b).count();
        degrees < Self::K
    }

    fn merge(&mut self, keep: ValueId, remove: ValueId) {
        let neighbours = self.neighbours(remove).iter().copied().collect::<Vec<_>>();

        for neighbour in neighbours {
            if neighbour != keep {
                self.add_edge(keep, neighbour);
            }
        }

        self.edges.remove(&remove);
    }

    #[inline(always)]
    pub(in crate::regalloc) fn nodes(&self) -> impl Iterator<Item = ValueId> + '_ {
        self.edges.keys().copied()
    }

    #[inline(always)]
    pub fn add_node(&mut self, id: ValueId) {
        self.edges.entry(id).or_default();
    }

    pub(in crate::regalloc) fn push_node(
        &self,
        id: ValueId,
        stack: &mut Vec<ValueId>,
        removed: &mut HashSet<ValueId>,
        degree: &mut HashMap<ValueId, usize>,
    ) {
        removed.insert(id);
        stack.push(id);

        // decrement degree of neighbours
        for &nb in self.neighbours(id) {
            if !removed.contains(&nb) {
                if let Some(d) = degree.get_mut(&nb) {
                    *d = d.saturating_sub(1);
                }
            }
        }
    }

    #[inline(always)]
    pub fn add_edge(&mut self, a: ValueId, b: ValueId) {
        if a == b {
            return;
        }

        self.edges.entry(a).or_default().insert(b);
        self.edges.entry(b).or_default().insert(a);
    }

    #[inline(always)]
    pub fn neighbours(&self, id: ValueId) -> &HashSet<ValueId> {
        static EMPTY: std::sync::OnceLock<HashSet<ValueId>> = std::sync::OnceLock::new();

        self.edges
            .get(&id)
            .unwrap_or_else(|| EMPTY.get_or_init(HashSet::new))
    }

    #[inline(always)]
    pub fn degree(&self, id: ValueId) -> usize {
        self.neighbours(id).len()
    }

    #[inline(always)]
    pub fn interferes(&self, a: &ValueId, b: &ValueId) -> bool {
        self.edges.get(a).is_some_and(|nb| nb.contains(b))
    }
}
