use std::collections::{BTreeMap, BTreeSet};

use crate::lir::{Function, VReg, target::Target};

#[derive(Default)]
pub struct Interference {
    pub edges: BTreeMap<VReg, BTreeSet<VReg>>,
    /// VRegs whose live range crosses at least one call (or instruction with clobbers)
    /// These must not be placed in caller-saved registers
    pub call_crossed: BTreeSet<VReg>,
}

impl Interference {
    pub(in crate::lir::regalloc) fn build<T: Target>(function: Function<T>, liveness: ()) -> Self {
        todo!()
    }
}
