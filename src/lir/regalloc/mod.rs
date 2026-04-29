use crate::lir::{
    Function, VReg,
    regalloc::{colouring::Location, interference::Interference, liveness::Liveness},
    target::Target,
};
use std::collections::BTreeMap;

// PERFORMANCE: we probably should reavaliate the algortihms in this module
// after doing a bunch of hacky things trying to solve the mad pipeline of before
// this is very populed
// there's a lot of iterations, linear or lookups, cloning, probing, wacky derefecings etc
// we can surely simplify this by a lot with the same results and probably better performance

mod colouring;
mod interference;
mod liveness;

#[derive(Debug)]
pub struct Allocation<T: Target> {
    locations: BTreeMap<VReg, Location<T>>,
    frame_size: u32,
    used_callee_saved: Vec<T::Reg>,
}

impl<T: Target> Allocation<T> {
    pub fn location_of(&self, vreg: VReg) -> Location<T> {
        self.locations[&vreg]
    }
}

impl<T: Target> Default for Allocation<T> {
    fn default() -> Self {
        Self {
            locations: BTreeMap::new(),
            frame_size: 0,
            used_callee_saved: Vec::new(),
        }
    }
}

impl<T: Target> Function<T> {
    pub fn allocate(&self) -> Allocation<T> {
        if self.vreg_types.is_empty() {
            return Allocation::default();
        }

        let vreg_types = self
            .vreg_types
            .iter()
            .enumerate()
            .map(|(i, &t)| (VReg(i as u32), t))
            .collect::<BTreeMap<_, _>>();

        let liveness = Liveness::analyse(self);
        let mut graph = Interference::build(self, &liveness);
        graph.coalesce(self);

        graph.colour::<T>(&vreg_types)
    }
}
