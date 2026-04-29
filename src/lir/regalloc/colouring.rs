use std::collections::{BTreeMap, BTreeSet};

use crate::lir::{
    MachineType, VReg,
    regalloc::{Allocation, interference::Interference},
    target::{RegClass, Target},
};

/// Where a [virtual register](crate::lir::VReg) lives after allocation
#[derive(Debug, Eq)]
pub enum Location<T: Target> {
    Reg(T::Reg),
    /// Offset from %rbp
    Stack(i32),
}

impl Interference {
    pub fn colour<T: Target>(&self, vreg_types: &BTreeMap<VReg, MachineType>) -> Allocation<T> {
        let all = self.nodes().collect::<Vec<_>>();

        if all.is_empty() {
            return Allocation::default();
        }

        let (floats, ints): (Vec<_>, Vec<_>) = all
            .iter()
            .partition(|&vreg| matches!(vreg_types.get(vreg), Some(t) if t.class() == RegClass::Float));

        let mut locations = BTreeMap::new();
        let mut spill_offset = 0;

        self.colour_group(
            T::gprs().len(),
            &ints,
            T::gprs(),
            T::caller_saved(),
            vreg_types,
            &mut locations,
            &mut spill_offset,
        );

        self.colour_group(
            T::fprs().len(),
            &floats,
            T::fprs(), // all xmm regs are caller saved :D
            T::fprs(),
            vreg_types,
            &mut locations,
            &mut spill_offset,
        );

        let raw = spill_offset.unsigned_abs();
        let frame_size = (raw + 15) & !15;

        let used_callee_saved = T::callee_saved()
            .iter()
            .copied()
            .filter(|&reg| locations.values().any(|loc| *loc == Location::Reg(reg)))
            .collect::<Vec<_>>();

        Allocation {
            locations,
            frame_size,
            used_callee_saved,
        }
    }

    fn colour_group<T: Target>(
        &self,
        k: usize,
        nodes: &[&VReg],
        available: &[T::Reg],
        caller_saved: &[T::Reg],
        vreg_types: &BTreeMap<VReg, MachineType>,
        locations: &mut BTreeMap<VReg, Location<T>>,
        spill_offset: &mut i32,
    ) {
        if nodes.is_empty() {
            return;
        }

        let mut degree: BTreeMap<VReg, usize> = nodes.iter().map(|&v| (*v, self.degree(&v))).collect();
        let mut removed = BTreeSet::new();
        let mut stack = Vec::new();

        loop {
            let simplifiable: Vec<_> = degree
                .iter()
                .filter(|&(v, d)| !removed.contains(v) && *d < k)
                .map(|(&v, _)| v)
                .collect();

            if !simplifiable.is_empty() {
                for v in simplifiable {
                    self.push_node(v, &mut stack, &mut removed, &mut degree);
                }

                continue;
            }

            let remaining: Vec<_> = degree.keys().filter(|v| !removed.contains(v)).copied().collect();

            match remaining.iter().max_by_key(|&&v| degree[&v]) {
                None => break,
                Some(&spill) => self.push_node(spill, &mut stack, &mut removed, &mut degree),
            }
        }

        let mut colour_map = BTreeMap::new();

        while let Some(v) = stack.pop() {
            let mut forbidden: BTreeSet<T::Reg> = self
                .neighbours(&v)
                .iter()
                .filter_map(|nb| colour_map.get(nb).and_then(|&r| r))
                .collect();

            if self.call_crossed.contains(&v) {
                for &reg in caller_saved {
                    forbidden.insert(reg);
                }
            }

            let assigned = available.iter().find(|r| !forbidden.contains(r)).copied();
            colour_map.insert(v, assigned);

            let location = match assigned {
                Some(reg) => Location::Reg(reg),
                None => {
                    let slot = slot_bytes(v, vreg_types) as i32;
                    *spill_offset -= slot;
                    Location::Stack(*spill_offset)
                }
            };

            locations.insert(v, location);
        }
    }
}

impl<T: Target> Clone for Location<T>
where
    T::Reg: Copy,
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Target> Copy for Location<T> where T::Reg: Copy {}

impl<T: Target> PartialEq for Location<T>
where
    T::Reg: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Reg(a), Self::Reg(b)) => a == b,
            (Self::Stack(a), Self::Stack(b)) => a == b,
            _ => false,
        }
    }
}

#[inline(always)]
fn slot_bytes(v: VReg, types: &BTreeMap<VReg, MachineType>) -> u32 {
    match types.get(&v) {
        Some(t) => match t.bytes() {
            8 => 8,
            _ => 4,
        },
        None => 4,
    }
}
