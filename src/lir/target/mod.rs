use crate::{
    lir::{self, VReg, regalloc},
    mir::{self, SyscallCode},
};

mod aarch64;
mod x86_64;

pub use aarch64::AArch64;
pub use x86_64::X86_64;

/// The trait that a target architecture must implement.
///
/// Defines the register file layout, calling convention, and associated types
/// for instructions and physical registers.
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

    /// byte offset **from the caller's stack pointer at the call site** for the `n-th`
    /// stack passed argument of the given class (i. e. those for which `param` returns `None`)
    ///
    /// returns `None` if all arguments of that class fit in registers (no stack slot exists)
    fn param_stack_offset(stack_idx: usize, class: RegClass) -> Option<i32>;

    /// physical register used for the `n-th` argument of the given class
    fn param(idx: usize, class: RegClass) -> Option<Self::Reg>;
    /// physical register used for the `n-th` argument in a syscall
    fn syscall_param(idx: usize) -> Option<Self::Reg>;
    /// physical register for the return value of the given class
    fn ret(class: RegClass) -> Option<Self::Reg>;

    /// map an abstract syscall code to this platform's numeric value
    fn syscall_code(code: SyscallCode) -> u64;
}

/// Lowers MIR into target-specific LIR.
///
/// The lowering translates 3-address MIR into the target ISA.
pub trait Lowerable: Target {
    fn lower(
        function: &mir::Function,
        symbols: &[String],
        all_functions: &[mir::Function],
    ) -> lir::Function<Self>;
}

/// Emits assembly text from a fully allocated LIR function.
///
/// After register allocation every VReg has a concrete location
/// The emitter just looks up locations and writes mnemonics
pub trait Emittable<T: Target> {
    fn emit(&self, alloc: regalloc::Allocation<T>, out: &mut String);
    fn start(out: &mut String);
}

/// A named physical register on a specific target.
pub trait PhysicalReg: Copy + Eq + Ord + std::fmt::Debug {
    fn class(self) -> RegClass;
    fn name<'s>(self, bytes: u8) -> &'s str;
}

/// What the register allocator needs to know about an instruction.
///
/// The allocator never sees target-specific details.
/// It only calls `defs()`, `uses()`, `clobbers()`, and the
/// precolouring accessors to build its interference graph.
pub trait Instruction<T: Target> {
    /// the virtual registers explicitly **written** by this instruction
    fn defs(&self) -> &[VReg];
    /// the virtual registers explicitly **read** by this instruction
    fn uses(&self, uses: &mut Vec<VReg>);

    /// `(destination, source)` if the instruction just copies its
    /// value from `source` to `destination` without modifying it
    fn as_copy(&self) -> Option<(VReg, VReg)>;

    /// physical registers that are modified as an architectural side-effect
    /// of the given instruction
    ///
    /// one example of this behaviour is `idiv` on `x86_64` that
    /// inherently overwrites `rax` and `rdx`
    fn clobbers<'r>(&self) -> &'r [T::Reg];

    fn precoloured_uses(&self) -> &[(VReg, T::Reg)] {
        &[]
    }
}

/// High-level register class.
///
/// Drives which physical register pool the allocator uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegClass {
    Int,
    Float,
}
