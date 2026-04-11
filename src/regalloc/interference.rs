use crate::{
    mir::{Function, ValueId},
    regalloc::{
        colouring::{Allocation, Location, Reg},
        liveness::Liveness,
    },
};
use std::collections::{HashMap, HashSet};

/// Undirected interference graph.
///
/// Nodes are `ValueId`s. An edge means the two values must not share a register.
#[derive(Debug, Default, PartialEq)]
pub struct Interference {
    edges: HashMap<ValueId, HashSet<ValueId>>,
}

impl Interference {
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
