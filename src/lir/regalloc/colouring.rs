use crate::lir::{
    MachineType, VReg,
    regalloc::{Allocation, interference::Interference},
    target::{PhysicalReg, RegClass, Target},
};
use std::collections::{BTreeMap, BTreeSet};

/// Where a [virtual register](crate::lir::VReg) lives after allocation
#[derive(Debug, Eq)]
pub enum Location<T: Target> {
    Reg(T::Reg),
    /// Offset from %rbp
    Stack(i32),
}

impl Interference {
    pub fn colour<T: Target>(
        &self,
        vreg_types: &[MachineType],
        precolours: &[(VReg, T::Reg)],
    ) -> Allocation<T> {
        let all = self.nodes().collect::<Vec<_>>();

        if all.is_empty() {
            return Allocation::default();
        }

        let (floats, ints): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|&vreg| vreg_types[vreg.0 as usize].class() == RegClass::Float);

        let mut locations = vec![None; vreg_types.len()];
        let mut spill_offset = 0;

        for &(vreg, reg) in precolours {
            locations[vreg.0 as usize] = Some(Location::Reg(reg));
        }

        let int_caller_saved = T::caller_saved()
            .iter()
            .copied()
            .filter(|reg| reg.class() == RegClass::Int)
            .collect::<Vec<_>>();
        let float_caller_saved = T::caller_saved()
            .iter()
            .copied()
            .filter(|reg| reg.class() == RegClass::Float)
            .collect::<Vec<_>>();

        self.colour_group(
            T::gprs().len(),
            &ints,
            T::gprs(),
            &int_caller_saved,
            vreg_types,
            &mut locations,
            &mut spill_offset,
        );

        self.colour_group(
            T::fprs().len(),
            &floats,
            T::fprs(),
            &float_caller_saved,
            vreg_types,
            &mut locations,
            &mut spill_offset,
        );

        let raw = spill_offset.unsigned_abs();
        let frame_size = (raw + 15) & !15;

        let used_callee_saved = T::callee_saved()
            .iter()
            .copied()
            .filter(|&reg| locations.iter().any(|loc| *loc == Some(Location::Reg(reg))))
            .collect::<Vec<_>>();

        let locations = locations
            .into_iter()
            .map(|location| location.expect("every VReg must receive a location"))
            .collect();

        Allocation {
            locations,
            frame_size,
            used_callee_saved,
        }
    }

    fn colour_group<T: Target>(
        &self,
        k: usize,
        nodes: &[VReg],
        available: &[T::Reg],
        caller_saved: &[T::Reg],
        vreg_types: &[MachineType],
        locations: &mut [Option<Location<T>>],
        spill_offset: &mut i32,
    ) {
        if nodes.is_empty() {
            return;
        }

        let unpinned: Vec<_> =
            nodes.iter().copied().filter(|v| locations[v.0 as usize].is_none()).collect();

        let mut degree: BTreeMap<VReg, usize> =
            unpinned.iter().map(|&v| (v, self.degree(&v))).collect();
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

            let remaining: Vec<_> =
                degree.keys().filter(|v| !removed.contains(v)).copied().collect();

            match remaining.iter().max_by_key(|&&v| degree[&v]) {
                None => break,
                Some(&spill) => self.push_node(spill, &mut stack, &mut removed, &mut degree),
            }
        }

        let mut colour_map = vec![None; locations.len()];
        for (idx, loc) in locations.iter().enumerate() {
            if let Some(Location::Reg(reg)) = loc {
                colour_map[idx] = Some(*reg);
            }
        }

        while let Some(v) = stack.pop() {
            let mut forbidden: BTreeSet<T::Reg> =
                self.neighbours(&v).iter().filter_map(|nb| colour_map[nb.0 as usize]).collect();

            if self.call_crossed.contains(&v) {
                for &reg in caller_saved {
                    forbidden.insert(reg);
                }
            }

            let assigned = available.iter().find(|r| !forbidden.contains(r)).copied();
            colour_map[v.0 as usize] = assigned;

            let location = match assigned {
                Some(reg) => Location::Reg(reg),
                None => {
                    let slot = slot_bytes(v, vreg_types) as i32;
                    *spill_offset -= slot;
                    Location::Stack(*spill_offset)
                }
            };

            locations[v.0 as usize] = Some(location);
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
fn slot_bytes(v: VReg, types: &[MachineType]) -> u32 {
    match types[v.0 as usize].bytes() {
        8 => 8,
        _ => 4,
    }
}
