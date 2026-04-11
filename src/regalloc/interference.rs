use std::collections::{HashMap, HashSet};

use crate::mir::ValueId;

/// Undirected interference graph.
///
/// Nodes are `ValueId`s. An edge means the two values must not share a register.
#[derive(Debug, Default, PartialEq)]
pub struct Interference {
    edges: HashMap<ValueId, HashSet<ValueId>>,
}

impl Interference {
    pub fn add_node(&mut self, id: ValueId) {
        self.edges.entry(id).or_default();
    }

    pub fn add_edge(&mut self, a: ValueId, b: ValueId) {
        if a == b {
            return;
        }

        self.edges.entry(a).or_default().insert(b);
        self.edges.entry(b).or_default().insert(a);
    }

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
