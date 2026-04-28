use crate::lir::{VReg, regalloc::colouring::Location, target::Target};
use std::collections::BTreeMap;

mod colouring;
mod interference;
mod liveness;

pub struct Allocator<T: Target> {
    locations: BTreeMap<VReg, Location<T>>,
}
