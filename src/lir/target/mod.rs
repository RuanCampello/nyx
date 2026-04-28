use crate::lir::VReg;

mod x86_64;

pub trait Target: Sized {
    type Reg: PhysicalReg;
    type Instruction: Instruction<Self>;

    /// allocatable general-purpose registers
    fn gprs<'r>() -> &'r [Self::Reg];
    /// allocatable floating-point registers
    fn fprs<'r>() -> &'r [Self::Reg];

    /// general-purpose registers that are **callee saved** (non-volatile)
    fn callee_saved<'r>() -> &'r [Self::Reg];
    /// general-purpose registers that are **caller saved** (volatile)
    fn caller_saved<'r>() -> &'r [Self::Reg];

    /// physical register used for the `n-th` argument of the given class
    fn param(idx: usize, class: RegClass) -> Option<Self::Reg>;
    /// physical register for the return value of the given class
    fn ret(class: RegClass) -> Option<Self::Reg>;
}

pub trait PhysicalReg: Copy + Eq + Ord + std::fmt::Debug {
    fn class(self) -> RegClass;
    fn name<'s>(self, bytes: u8) -> &'s str;
}

pub trait Instruction<T: Target> {
    /// the virtual registers explicitly **written** by this instruction
    fn defs(&self) -> &[VReg];
    /// the virtual registers explicitly **read** by this instruction
    fn uses(&self) -> &[VReg];

    /// `(destination, source)` if the instruction just copies its
    /// value from `source` to `destination` without modifying it
    fn as_copy(&self) -> Option<(VReg, VReg)>;

    /// physical registers that are modified as an architectural side-effect
    /// of the given instruction
    ///
    /// one example of this behaviour is `idiv` on `x86_64` that
    /// inherently overwrites `rax` and `rdx`
    fn clobbers<'r>(&self) -> &'r [T::Reg];

    fn precoloured_def(&self) -> Option<(VReg, T::Reg)> {
        None
    }
    fn precoloured_uses(&self) -> &[(VReg, T::Reg)] {
        &[]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegClass {
    Int,
    Float,
}
