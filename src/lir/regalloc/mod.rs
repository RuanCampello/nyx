use crate::lir::{
    Function, VReg,
    regalloc::{interference::Interference, liveness::Liveness},
    target::Target,
};

mod colouring;
mod interference;
mod liveness;

pub(in crate::lir) use colouring::Location;

#[derive(Debug)]
pub struct Allocation<T: Target> {
    locations: Vec<Location<T>>,
    pub(in crate::lir) frame_size: u32,
    pub(in crate::lir) used_callee_saved: Vec<T::Reg>,
}

impl<T: Target> Allocation<T> {
    pub fn location_of(&self, vreg: &VReg) -> Location<T> {
        self.locations[vreg.0 as usize]
    }
}

impl<T: Target> Default for Allocation<T> {
    fn default() -> Self {
        Self {
            locations: Vec::new(),
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

        let liveness = Liveness::analyse(self);
        let graph = Interference::build(self, &liveness);

        graph.colour::<T>(&self.vreg_types, &self.precolours)
    }
}
