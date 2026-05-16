use crate::{
    lir::{self, MachineType, VReg, regalloc},
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
        layouts: &[mir::Layout],
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
/// It only calls `defs()`, `uses()`, `clobbers()`, and the precolouring
/// accessors to build its interference graph.
pub trait Instruction<T: Target> {
    /// the virtual registers explicitly **written** by this instruction
    fn defs(&self) -> &[VReg];
    /// the virtual registers explicitly **read** by this instruction
    fn uses(&self, uses: &mut Vec<VReg>);

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

/// Target-specific memory instruction factories
#[rustfmt::skip]
pub trait MemOps: Target {
    /// Load `bytes` bytes from `origin + offset` (a stack slot) into `dest`
    fn field_load( dest: VReg, origin: VReg, offset: i32, bytes: u8, is_float: bool) -> Self::Instruction;
    /// Store `src` into `origin + offset` (a stack slot)
    fn field_store( origin: VReg, src: VReg, offset: i32, bytes: u8, is_float: bool) -> Self::Instruction;
    /// Load `bytes` bytes through the pointer in `ptr` at `ptr + offset` into `dest`
    fn ptr_load(dest: VReg, ptr: VReg, offset: i32, bytes: u8, is_float: bool) -> Self::Instruction;
    /// Store `src` through the pointer in `ptr` at `ptr + offset`
    fn ptr_store(ptr: VReg, src: VReg, offset: i32, bytes: u8, is_float: bool) -> Self::Instruction;

    /// Emit a scalar load, choosing between a pointer dereference or a stack slot access based on `is_ref`
    #[inline(always)]
    fn scalar_load(
        is_ref: bool,
        dest: VReg,
        origin: VReg,
        offset: i32,
        bytes: u8,
        is_float: bool,
    ) -> Self::Instruction {
        match is_ref {
            true => Self::ptr_load(dest, origin, offset, bytes, is_float),
            false => Self::field_load(dest, origin, offset, bytes, is_float),
        }
    }

    /// Emit a scalar store, choosing between a pointer dereference or a stack slot access based on `is_ref`
    #[inline(always)]
    fn scalar_store(
        is_ref: bool,
        origin: VReg,
        src: VReg,
        offset: i32,
        bytes: u8,
        is_float: bool,
    ) -> Self::Instruction {
        match is_ref {
            true => Self::ptr_store(origin, src, offset, bytes, is_float),
            false => Self::field_store(origin, src, offset, bytes, is_float),
        }
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

/// copy an aggregate value between two memory locations, chunk by chunk
pub fn aggregate_copy<T: MemOps>(
    lir: &mut lir::Function<T>,
    block: &lir::BlockId,
    is_src_ref: bool,
    is_dest_ref: bool,
    src: VReg,
    dest: VReg,
    src_base: i32,
    dest_base: i32,
    size: u32,
) {
    for (offset, bytes) in lir::aggregate_chunks(size) {
        let scratch = lir.new_vreg(MachineType::Int { bytes });

        let load = T::scalar_load(is_src_ref, scratch, src, src_base + offset, bytes, false);
        lir.push_instr(block, load);

        let store = T::scalar_store(is_dest_ref, dest, scratch, dest_base + offset, bytes, false);
        lir.push_instr(block, store);
    }
}
