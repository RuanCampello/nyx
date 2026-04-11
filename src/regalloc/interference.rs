use std::collections::{HashMap, HashSet};

use crate::{
    mir::ValueId,
    regalloc::colouring::{Allocation, Location, Reg},
};

/// Undirected interference graph.
///
/// Nodes are `ValueId`s. An edge means the two values must not share a register.
#[derive(Debug, Default, PartialEq)]
pub struct Interference {
    edges: HashMap<ValueId, HashSet<ValueId>>,
}

impl Interference {
    pub fn colour(self) -> Allocation {
        let k = Reg::k();
        let all = self.nodes().collect::<Vec<_>>();

        if all.is_empty() {
            return Allocation::default();
        }

        let mut degree: HashMap<_, _> = all.iter().map(|&id| (id, self.degree(id))).collect();

        let mut removed = HashSet::new();
        let mut stack = Vec::new();

        loop {
            // simplify: push any node with degree < K
            let simplifiable: Vec<_> = degree
                .iter()
                .filter(|&(id, degree)| !removed.contains(id) && *degree < k)
                .map(|(&id, _)| id)
                .collect();

            if !simplifiable.is_empty() {
                for id in simplifiable {
                    self.push_node(id, &mut stack, &mut removed, &mut degree);
                }

                continue;
            }

            // all remaining nodes have degree >= K — spill the highest-degree one
            let remaining: Vec<_> = degree
                .keys()
                .filter(|id| !removed.contains(id))
                .copied()
                .collect();

            match remaining.iter().max_by_key(|&&id| degree[&id]) {
                None => break, // graph is empty,
                Some(&spill) => self.push_node(spill, &mut stack, &mut removed, &mut degree),
            }
        }

        let mut colour_map: HashMap<ValueId, Option<Reg>> = HashMap::new();
        let mut spill_offset = 0;
        let mut locations = HashMap::new();

        while let Some(id) = stack.pop() {
            let forbidden: HashSet<_> = self
                .neighbours(id)
                .iter()
                .filter_map(|nb| match colour_map.get(nb) {
                    Some(Some(reg)) => Some(*reg),
                    _ => None,
                })
                .collect();

            let assigned = Reg::ALL
                .iter()
                .find(|reg| !forbidden.contains(reg))
                .copied();

            colour_map.insert(id, assigned);

            let location = match assigned {
                Some(reg) => Location::Register(reg),
                None => {
                    // TODO: i32 slot generalise to type size later

                    spill_offset -= 4;
                    Location::Stack(spill_offset)
                }
            };

            locations.insert(id, location);
        }

        let raw = spill_offset.unsigned_abs();
        let frame_size = (raw + 15) & !15;

        Allocation {
            locations,
            frame_size,
        }
    }

    #[inline(always)]
    fn nodes(&self) -> impl Iterator<Item = ValueId> + '_ {
        self.edges.keys().copied()
    }

    #[inline(always)]
    pub fn add_node(&mut self, id: ValueId) {
        self.edges.entry(id).or_default();
    }

    fn push_node(
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
