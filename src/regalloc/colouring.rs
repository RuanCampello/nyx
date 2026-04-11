//! Graph-colouring register allocation algorithm.
//!
//! Algorithm:
//!  - 1. Simplify — repeatedly remove nodes with degree < K, push to stack.
//!  - 2. Spill    — if stuck, pick the highest-degree node as a potential spill.
//!  - 3. Select   — pop stack, assign the lowest-numbered free colour;
//!                 if none available the node becomes an actual spill → stack slot.

use crate::{
    mir::{Function, ValueId},
    regalloc::interference::Interference,
};
use std::collections::{HashMap, HashSet};

/// Complete allocation result for a function.
#[derive(Debug, Default)]
pub struct Allocation {
    pub(in crate::regalloc) locations: HashMap<ValueId, Location>,
    /// Total stack frame size in bytes, 16 aligned.
    pub(in crate::regalloc) frame_size: u32,
}

/// x86-64 general-purpose registers available for allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reg {
    Rax,
    Rcx,
    Rdx,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
}

/// Where a value lives in after allocation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Register(Reg),
    Stack(i32),
}

impl Allocation {
    pub fn location_of(&self, id: ValueId) -> Location {
        self.locations[&id]
    }

    pub fn allocate(function: &Function) -> Allocation {
        let graph = Interference::build(function);
        Interference::colour(graph)
    }
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
}

impl Reg {
    pub const ALL: &'static [Reg] = &[
        Reg::Rax,
        Reg::Rcx,
        Reg::Rdx,
        Reg::Rsi,
        Reg::Rdi,
        Reg::R8,
        Reg::R9,
        Reg::R10,
        Reg::R11,
    ];

    pub const fn k() -> usize {
        Self::ALL.len()
    }

    pub const fn as_str_32(self) -> &'static str {
        match self {
            Reg::Rax => "eax",
            Reg::Rcx => "ecx",
            Reg::Rdx => "edx",
            Reg::Rsi => "esi",
            Reg::Rdi => "edi",
            Reg::R8 => "r8d",
            Reg::R9 => "r9d",
            Reg::R10 => "r10d",
            Reg::R11 => "r11d",
        }
    }

    pub const fn as_str_64(self) -> &'static str {
        match self {
            Reg::Rax => "rax",
            Reg::Rcx => "rcx",
            Reg::Rdx => "rdx",
            Reg::Rsi => "rsi",
            Reg::Rdi => "rdi",
            Reg::R8 => "r8",
            Reg::R9 => "r9",
            Reg::R10 => "r10",
            Reg::R11 => "r11",
        }
    }
}
