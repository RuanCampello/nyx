use crate::{
    hir::{FunctionId, Intrinsic, SymbolTable, SyscallCode, Type, TypeKind},
    lir::{self, BlockId, Layouts, MachineType, Term, VReg, assembly_label, regalloc},
    mir::{self, Const, Function, Operand, ValueId},
};

mod aarch64;
mod x86_64;

pub use aarch64::AArch64;
pub use x86_64::X86_64;

const PANIC_EXIT_CODE: u8 = 101;

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
    #[inline(always)]
    fn param_stack_offset(stack_idx: usize, _class: RegClass) -> Option<i32> {
        Some(16 + (stack_idx as i32) * 8)
    }

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
        symbols: &SymbolTable,
        all_functions: &[mir::Function],
        struct_layouts: &[mir::Layout],
        enum_layouts: &[mir::Layout],
        array_layouts: &[mir::Layout],
    ) -> lir::Function<Self>;
}

/// Emits assembly text from a fully allocated LIR function.
///
/// After register allocation every VReg has a concrete location
/// The emitter just looks up locations and writes mnemonics
pub trait Emittable<T: Target> {
    fn emit(&self, alloc: regalloc::Allocation<T>, out: &mut String);
    fn start(out: &mut String, main: &str);
    fn emit_panic_handlers(out: &mut String);
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

    fn stack_forced(&self) -> &[VReg] {
        &[]
    }
}

/// Target-specific memory instruction factories
#[rustfmt::skip]
pub trait MemOps: Target {
    type Operand;

    fn vreg_operand(v: VReg) -> Self::Operand;

    /// load `bytes` bytes from `origin + offset` (a stack slot) into `dest`
    fn field_load(dest: VReg, origin: VReg, offset: i32, bytes: u8, is_float: bool, signed: bool) -> Self::Instruction;
    /// store `src` into `origin + offset` (a stack slot)
    fn field_store(origin: VReg, src: Self::Operand, offset: i32, bytes: u8, is_float: bool) -> Self::Instruction;
    /// load `bytes` bytes through the pointer in `ptr` at `ptr + offset` into `dest`
    fn ptr_load(dest: VReg, ptr: VReg, offset: i32, bytes: u8, is_float: bool, signed: bool) -> Self::Instruction;
    /// store `src` through the pointer in `ptr` at `ptr + offset`
    fn ptr_store(ptr: VReg, src: Self::Operand, offset: i32, bytes: u8, is_float: bool) -> Self::Instruction;

    /// emit a scalar load, choosing between a pointer dereference or a stack slot access based on `is_ref`
    #[inline(always)]
    fn scalar_load(is_ref: bool, dest: VReg, origin: VReg, offset: i32, bytes: u8, is_float: bool, signed: bool) -> Self::Instruction {
        match is_ref {
            true  => Self::ptr_load(dest, origin, offset, bytes, is_float, signed),
            false => Self::field_load(dest, origin, offset, bytes, is_float, signed),
        }
    }

    /// emit a scalar store, choosing between a pointer dereference or a stack slot access based on `is_ref`
    #[inline(always)]
    fn scalar_store(is_ref: bool, origin: VReg, src: Self::Operand, offset: i32, bytes: u8, is_float: bool) -> Self::Instruction {
        match is_ref {
            true  => Self::ptr_store(origin, src, offset, bytes, is_float),
            false => Self::field_store(origin, src, offset, bytes, is_float),
        }
    }
}

/// A target-independent representation of an operand
pub trait TargetOperand: Clone {
    fn from_vreg(v: VReg) -> Self;
    fn from_imm(imm: i64) -> Self;
    fn from_label(label: String) -> Self;
    fn as_vreg(&self) -> Option<VReg>;
}

/// Target-specific instruction and operand factories
pub trait TargetOps: MemOps
where
    Self::Operand: TargetOperand,
{
    /// Emit an instruction to move an operand to a virtual register
    fn mov_op(dest: VReg, src: Self::Operand, bytes: u8, is_float: bool) -> Self::Instruction;

    /// Emit an instruction to load a label or constant's address into a virtual register
    fn load_label(dest: VReg, label: String, is_float: bool, bytes: u8) -> Self::Instruction;

    /// Move a parameter that arrived in its precoloured ABI register `src` into `dest`
    fn load_param_reg(dest: VReg, src: VReg, mt: MachineType) -> Self::Instruction;

    /// Load a stack-passed parameter from the caller's frame slot at `offset` into `dest`
    fn load_param_stack(dest: VReg, offset: i32, mt: MachineType) -> Self::Instruction;

    /// Materialise the frame-relative address of stack slot `origin` into `dest`
    fn load_stack_addr(dest: VReg, origin: VReg) -> Self::Instruction;

    /// Abort through the index-out-of-bounds handler when `index >= bound` (unsigned)
    fn bounds_check(index: VReg, bound: Self::Operand) -> Self::Instruction;

    /// Emit `dest = lhs * imm`
    fn mul_imm(lir: &mut lir::Function<Self>, block: &BlockId, dest: VReg, lhs: VReg, imm: i64);

    /// Emit `dest = lhs + rhs`
    fn add_vregs(lir: &mut lir::Function<Self>, block: &BlockId, dest: VReg, lhs: VReg, rhs: VReg);

    /// Emit `dest = dest + imm`
    fn add_imm(lir: &mut lir::Function<Self>, block: &BlockId, dest: VReg, imm: i64);

    /// Build a call instruction; `aggregate_ret` carries the precoloured VRegs a
    /// small aggregate is returned in
    fn build_call(
        target: String,
        moves: CallArgMoves<Self>,
        stack_args: StackArgs<Self>,
        ret: Option<VReg>,
        aggregate_ret: Vec<VReg>,
    ) -> Self::Instruction;

    /// The per-register `(offset, bytes, reg)` chunks a small aggregate is returned
    /// in, or `None` when it must go through an sret pointer
    #[inline(always)]
    fn small_aggregate_return(_typ: Type, _layouts: Layouts) -> Option<Vec<(i32, u8, Self::Reg)>> {
        None
    }

    /// Whether a value of type `typ` is returned through an implicit sret pointer
    /// argument rather than in registers
    #[inline(always)]
    fn uses_sret(typ: Type, layouts: Layouts) -> bool {
        typ.is_aggregate() && Self::small_aggregate_return(typ, layouts).is_none()
    }
}

/// MIR -> LIR lowering context, generic over the target architecture
pub(crate) struct Lower<'f, T: Target> {
    pub(crate) function: &'f Function,
    pub(crate) lir: lir::Function<T>,
    /// maps a MIR [ValueId] to its LIR [VReg]
    pub(crate) value: Vec<VReg>,
    pub(crate) symbols: &'f SymbolTable,
    pub(crate) all_functions: &'f [Function],
    pub(crate) layouts: Layouts<'f>,
    pub(crate) sret_ptr: Option<VReg>,
}

/// A target-independent representation of a register-to-register/stack-to-register move,
/// used to resolve argument placement
#[derive(Clone)]
pub struct ParallelMove<Reg> {
    pub src: String,
    pub src_reg: Option<Reg>,
    pub dest: String,
    pub dest_reg: Reg,
    pub bytes: u8,
    pub is_float: bool,
}

/// High-level register class.
///
/// Drives which physical register pool the allocator uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegClass {
    Int,
    Float,
}

#[derive(Clone, Copy)]
pub struct AggregateCopy {
    pub src: VReg,
    pub dest: VReg,
    pub src_ref: bool,
    pub dest_ref: bool,
    pub src_base: i32,
    pub dest_base: i32,
    pub size: u32,
}

pub type CallArgMoves<T> = Vec<(VReg, <T as Target>::Reg)>;
pub type StackArgs<T> = Vec<(<T as MemOps>::Operand, MachineType)>;
pub type SyscallMoves<T> = Vec<(<T as MemOps>::Operand, <T as Target>::Reg, u8)>;
pub type SyscallUses = Vec<VReg>;

/// Copy an aggregate value between two memory locations, chunk by chunk
pub fn aggregate_copy<T: MemOps>(
    lir: &mut lir::Function<T>,
    block: &lir::BlockId,
    copy: AggregateCopy,
) {
    for (offset, bytes) in lir::aggregate_chunks(copy.size) {
        let scratch = lir.new_vreg(MachineType::Int { bytes, signed: false });

        let load = T::scalar_load(
            copy.src_ref,
            scratch,
            copy.src,
            copy.src_base + offset,
            bytes,
            false,
            false,
        );
        lir.push_instr(block, load);

        let store = T::scalar_store(
            copy.dest_ref,
            copy.dest,
            T::vreg_operand(scratch),
            copy.dest_base + offset,
            bytes,
            false,
        );
        lir.push_instr(block, store);
    }
}

impl AggregateCopy {
    #[inline(always)]
    pub const fn new(src: VReg, dest: VReg, size: u32) -> Self {
        Self {
            src,
            dest,
            size,
            src_ref: false,
            dest_ref: false,
            src_base: 0,
            dest_base: 0,
        }
    }

    #[inline(always)]
    pub const fn with_dest_ref(mut self) -> Self {
        self.dest_ref = true;
        self
    }

    #[inline(always)]
    pub const fn with_src_ref(mut self) -> Self {
        self.src_ref = true;
        self
    }
}

impl<Reg: Copy + Eq> ParallelMove<Reg> {
    #[inline(always)]
    pub fn is_self_move(&self) -> bool {
        self.src_reg == Some(self.dest_reg) || self.src == self.dest
    }

    #[inline(always)]
    pub fn dest_is_read_by(&self, other: &Self) -> bool {
        other.src_reg == Some(self.dest_reg) || other.src == self.dest
    }
}

impl<'f, T: Target> Lower<'f, T> {
    pub(crate) fn new(
        function: &'f Function,
        symbols: &'f SymbolTable,
        all_functions: &'f [Function],
        structs: &'f [mir::Layout],
        enums: &'f [mir::Layout],
        arrays: &'f [mir::Layout],
    ) -> Self {
        let layouts = Layouts { structs, enums, arrays };
        let name = assembly_label(symbols.get(function.name_symbol));
        let mut lir = lir::Function::<T>::new(name);

        let value = function
            .locals
            .iter()
            .map(|(_, typ)| lir.new_vreg(typ.machine_type(layouts)))
            .collect();

        for _ in &function.blocks {
            lir.new_block();
        }

        Self {
            function,
            lir,
            value,
            symbols,
            all_functions,
            layouts,
            sret_ptr: None,
        }
    }
}

impl<'f, T: TargetOps> Lower<'f, T>
where
    T::Operand: TargetOperand,
{
    /// materialise a MIR operand into a VReg, emitting a constant move if needed
    #[inline(always)]
    pub(crate) fn operand(&mut self, op: &Operand, block: &BlockId) -> VReg {
        let value = &self.value;
        let layouts = self.layouts;
        operand(&mut self.lir, op, block, layouts, |vid| value[vid])
    }

    /// translate a MIR operand into a target-specific operand
    #[inline(always)]
    pub(crate) fn lower_operand(&mut self, op: &Operand) -> T::Operand {
        let value = &self.value;
        lower_operand(&mut self.lir, op, |vid| value[vid])
    }

    /// materialise the frame-relative address of a stack slot into a fresh VReg
    pub(crate) fn stack_addr(&mut self, block: &BlockId, origin: VReg) -> VReg {
        let dest = self.lir.new_vreg(MachineType::Int { bytes: 8, signed: false });
        self.lir.push_instr(block, T::load_stack_addr(dest, origin));
        dest
    }

    /// copy incoming parameters from their ABI registers (or the caller's stack
    /// slots) into the vregs the function body reads
    pub(crate) fn lower_param_moves(&mut self) {
        let entry = BlockId(0);
        let mut int_idx = 0;
        let mut float_idx = 0;
        let mut int_stack_idx = 0;
        let mut float_stack_idx = 0;

        if T::uses_sret(self.function.return_type, self.layouts) {
            let ptr = self.lir.new_vreg(MachineType::Int { bytes: 8, signed: false });
            let reg = T::param(int_idx, RegClass::Int)
                .expect("sret pointer must fit in the first integer argument register");
            self.lir.add_precolour(ptr, reg);
            self.sret_ptr = Some(ptr);
            int_idx += 1;
        }

        for (vid, typ) in &self.function.params {
            let (vid, typ) = (*vid, *typ);

            if typ.is_aggregate() {
                let ptr_mt = MachineType::Int { bytes: 8, signed: false };
                let ptr = self.lir.new_vreg(ptr_mt);

                match T::param(int_idx, RegClass::Int) {
                    Some(reg) => self.lir.add_precolour(ptr, reg),
                    None => {
                        let offset = T::param_stack_offset(int_stack_idx, RegClass::Int)
                            .expect("param_stack_offset must be defined when param() returns None");
                        self.lir.push_instr(&entry, T::load_param_stack(ptr, offset, ptr_mt));
                        int_stack_idx += 1;
                    },
                }

                let size = typ.machine_type(self.layouts).stack_size() as u32;
                let copy = AggregateCopy::new(ptr, self.value[vid], size).with_src_ref();
                aggregate_copy(&mut self.lir, &entry, copy);
                int_idx += 1;
                continue;
            }

            let mt = typ.machine_type(self.layouts);
            let class = mt.class();
            let dest = self.value[vid];

            let (reg_idx, stack_idx) = match class {
                RegClass::Int => (&mut int_idx, &mut int_stack_idx),
                RegClass::Float => (&mut float_idx, &mut float_stack_idx),
            };

            match T::param(*reg_idx, class) {
                Some(reg) => {
                    let abi_vreg = self.lir.new_vreg(mt);
                    self.lir.add_precolour(abi_vreg, reg);
                    self.lir.push_instr(&entry, T::load_param_reg(dest, abi_vreg, mt));
                },
                None => {
                    let offset = T::param_stack_offset(*stack_idx, class)
                        .expect("param_stack_offset must be defined when param() returns None");
                    self.lir.push_instr(&entry, T::load_param_stack(dest, offset, mt));
                    *stack_idx += 1;
                },
            }

            *reg_idx += 1;
        }
    }

    /// bounds-checked element pointer: `base address + index * stride`,
    /// written into `dest` when given, a fresh VReg otherwise
    fn element_addr_into(
        &mut self,
        block: &BlockId,
        dest: Option<VReg>,
        base: VReg,
        base_is_ref: bool,
        index: &Operand,
        bound: &Operand,
        stride: u32,
    ) -> VReg {
        let int8 = MachineType::Int { bytes: 8, signed: false };

        let index_op = self.lower_operand(index);
        let index = match index_op.as_vreg() {
            Some(vreg) => vreg,
            None => {
                let dest = self.lir.new_vreg(int8);
                self.lir.push_instr(block, T::mov_op(dest, index_op, 8, false));
                dest
            },
        };
        let bound = self.lower_operand(bound);
        self.lir.push_instr(block, T::bounds_check(index, bound));

        let addr = self.lir.new_vreg(int8);
        match base_is_ref {
            true => {
                let src = T::Operand::from_vreg(base);
                self.lir.push_instr(block, T::mov_op(addr, src, 8, false));
            },
            false => self.lir.push_instr(block, T::load_stack_addr(addr, base)),
        }

        let offset = self.lir.new_vreg(int8);
        T::mul_imm(&mut self.lir, block, offset, index, stride as i64);

        let element = dest.unwrap_or_else(|| self.lir.new_vreg(int8));
        T::add_vregs(&mut self.lir, block, element, addr, offset);
        element
    }

    pub(crate) fn lower_element_load(
        &mut self,
        block: &BlockId,
        dest: VReg,
        base: &Operand,
        index: &Operand,
        bound: &Operand,
        stride: u32,
        typ: Type,
    ) {
        let Operand::Place(base) = base else {
            unreachable!("indexing a constant aggregate");
        };
        let origin = self.value[base.id];
        let is_ref = matches!(base.typ.kind(), TypeKind::Ref { .. });
        let element = self.element_addr_into(block, None, origin, is_ref, index, bound, stride);

        match typ.is_aggregate() {
            true => {
                let size = typ.machine_type(self.layouts).stack_size() as u32;
                let copy = AggregateCopy::new(element, dest, size).with_src_ref();
                aggregate_copy(&mut self.lir, block, copy);
            },
            false => {
                let mt = typ.machine_type(self.layouts);
                let load =
                    T::ptr_load(dest, element, 0, mt.bytes(), typ.is_float(), mt.is_signed());
                self.lir.push_instr(block, load);
            },
        }
    }

    pub(crate) fn lower_element_store(
        &mut self,
        block: &BlockId,
        dest: VReg,
        dest_typ: Type,
        index: &Operand,
        bound: &Operand,
        value: &Operand,
        stride: u32,
    ) {
        let is_ref = matches!(dest_typ.kind(), TypeKind::Ref { .. });
        let element = self.element_addr_into(block, None, dest, is_ref, index, bound, stride);

        let value_typ = value.typ();
        match value_typ.is_aggregate() {
            true => {
                let Operand::Place(src) = value else {
                    unreachable!("aggregate element store source must be a place");
                };
                let src = self.value[src.id];
                let size = value_typ.machine_type(self.layouts).stack_size() as u32;
                let copy = AggregateCopy::new(src, element, size).with_dest_ref();
                aggregate_copy(&mut self.lir, block, copy);
            },
            false => {
                let mt = value_typ.machine_type(self.layouts);
                let src = self.lower_operand(value);
                let store = T::ptr_store(element, src, 0, mt.bytes(), value_typ.is_float());
                self.lir.push_instr(block, store);
            },
        }
    }

    pub(crate) fn lower_element_addr(
        &mut self,
        block: &BlockId,
        dest: VReg,
        base: &Operand,
        index: &Operand,
        bound: &Operand,
        stride: u32,
    ) {
        let base = match base {
            Operand::Place(place) => place,
            Operand::Const(_) => unreachable!("indexing a constant aggregate"),
        };
        let origin = self.value[base.id];
        let is_ref = matches!(base.typ.kind(), TypeKind::Ref { .. });
        self.element_addr_into(block, Some(dest), origin, is_ref, index, bound, stride);
    }

    pub(crate) fn lower_address_of(
        &mut self,
        block: &BlockId,
        dest: VReg,
        src: &mir::Place,
        offset: u32,
    ) {
        let origin = self.value[src.id];
        match src.typ.kind() {
            TypeKind::Ref { .. } => {
                let src = T::Operand::from_vreg(origin);
                self.lir.push_instr(block, T::mov_op(dest, src, 8, false));
            },
            _ => self.lir.push_instr(block, T::load_stack_addr(dest, origin)),
        }

        if offset != 0 {
            T::add_imm(&mut self.lir, block, dest, offset as i64);
        }
    }

    pub(crate) fn lower_call(
        &mut self,
        block: &BlockId,
        dest: VReg,
        callee_id: FunctionId,
        args: &[Operand],
    ) {
        let callee_fn = self
            .all_functions
            .iter()
            .find(|f| f.id == callee_id)
            .unwrap_or_else(|| panic!("callee function {callee_id:?} not found"));

        if callee_fn.intrinsic == Some(Intrinsic::Len) {
            let ptr = self.operand(&args[0], block);
            let load = T::ptr_load(dest, ptr, 8, 8, false, false);
            return self.lir.push_instr(block, load);
        }

        let callee = assembly_label(self.symbols.get(callee_fn.name_symbol));
        let return_type = callee_fn.return_type;
        let aggregate_ret = match return_type.is_aggregate() {
            true => T::small_aggregate_return(return_type, self.layouts).unwrap_or_default(),
            false => Vec::new(),
        };

        let mut int_idx = 0;
        let mut moves = Vec::new();
        if return_type.is_aggregate() && aggregate_ret.is_empty() {
            let ptr = self.stack_addr(block, dest);
            let abi_reg = T::param(int_idx, RegClass::Int)
                .expect("sret pointer must fit in the first integer argument register");
            moves.push((ptr, abi_reg));
            int_idx += 1;
        }

        let value = &self.value;
        let layouts = self.layouts;
        let (arg_moves, stack_args) = prepare_call_args(
            &mut self.lir,
            block,
            args,
            layouts,
            |vid| value[vid],
            |lir, op, block| operand(lir, op, block, layouts, |vid| value[vid]),
            |lir, op, _| lower_operand(lir, op, |vid| value[vid]),
            |lir, block, origin| {
                let dest = lir.new_vreg(MachineType::Int { bytes: 8, signed: false });
                lir.push_instr(block, T::load_stack_addr(dest, origin));
                dest
            },
            int_idx,
            0,
        );
        moves.extend(arg_moves);

        let ret =
            (return_type.kind() != TypeKind::Unit && !return_type.is_aggregate()).then_some(dest);
        let mut ret_vregs = Vec::with_capacity(aggregate_ret.len());
        for &(_, bytes, reg) in &aggregate_ret {
            let vreg = self.lir.new_vreg(MachineType::Int { bytes, signed: false });
            self.lir.add_precolour(vreg, reg);
            ret_vregs.push(vreg);
        }

        let call = T::build_call(callee, moves, stack_args, ret, ret_vregs.clone());
        self.lir.push_instr(block, call);

        for ((offset, bytes, _), src) in aggregate_ret.into_iter().zip(ret_vregs) {
            let src = T::Operand::from_vreg(src);
            let store = T::field_store(dest, src, offset, bytes, false);
            self.lir.push_instr(block, store);
        }
    }

    pub(crate) fn lower_terminator(&mut self, block: &BlockId, terminator: mir::Terminator) {
        use mir::Terminator as MirTerm;

        let terminator = match terminator {
            MirTerm::Return(None) => Term::Return(None),
            MirTerm::Return(Some(operand)) if operand.typ().is_aggregate() => {
                let typ = operand.typ();
                let Operand::Place(place) = operand else {
                    unreachable!("aggregate return source must be a place");
                };
                let src_vreg = self.value[place.id];

                match T::small_aggregate_return(typ, self.layouts) {
                    Some(chunks) => {
                        for (offset, bytes, reg) in chunks {
                            let ret = self.lir.new_vreg(MachineType::Int { bytes, signed: false });
                            let load = T::field_load(ret, src_vreg, offset, bytes, false, false);
                            self.lir.add_precolour(ret, reg);
                            self.lir.push_instr(block, load);
                        }
                    },
                    None => {
                        let sret_ptr = self
                            .sret_ptr
                            .expect("struct-returning function must have an sret pointer");
                        let size = typ.machine_type(self.layouts).stack_size() as u32;
                        let copy = AggregateCopy::new(src_vreg, sret_ptr, size).with_dest_ref();
                        aggregate_copy(&mut self.lir, block, copy);
                    },
                }

                Term::Return(None)
            },
            MirTerm::Return(Some(operand)) => Term::Return(Some(self.operand(&operand, block))),
            MirTerm::Jump(target) => Term::Jump(target.into()),
            MirTerm::Branch { condition, then_block, else_block } => Term::Branch {
                cond: self.operand(&condition, block),
                then_block: then_block.into(),
                else_block: else_block.into(),
            },
        };

        self.lir.set_term(block, terminator);
    }
}

/// aggregates of at most 16 bytes and no float members are returned directly in
/// the two integer return registers, the per-register `(offset, bytes, reg)`
/// chunks are produced by the same walk both abis share
pub fn small_aggregate_chunks<R: Copy>(
    regs: [R; 2],
    typ: Type,
    layouts: Layouts,
) -> Option<Vec<(i32, u8, R)>> {
    let size = typ.machine_type(layouts).stack_size() as u32;
    let contains_float = match typ.kind() {
        TypeKind::Struct(sid) => layouts.structs[sid.0 as usize].contains_float(),
        _ => false,
    };
    if size == 0 || size > 16 || contains_float {
        return None;
    }

    Some(
        lir::aggregate_chunks(size)
            .zip(regs)
            .map(|((offset, bytes), reg)| (offset, bytes, reg))
            .collect(),
    )
}

/// Serialise a set of parallel register moves without data corruption
///
/// - Chains (A->B then B->C) are resolved by topological ordering
/// - Cycles (A->B, B->A) are broken using a target-specific scratch register
pub fn resolve_parallel_moves<Reg, Ctx, FMove, FCycle>(
    mut moves: Vec<ParallelMove<Reg>>,
    ctx: &mut Ctx,
    mut emit_move: FMove,
    mut emit_cycle_break: FCycle,
) where
    Reg: Eq + Copy,
    FMove: FnMut(&mut Ctx, ParallelMove<Reg>),
    FCycle: FnMut(&mut Ctx, &mut ParallelMove<Reg>),
{
    moves.retain(|m| !m.is_self_move());

    loop {
        // find a move whose dest is not read by any other pending move
        let safe = moves.iter().position(|m| {
            !moves.iter().any(|other| !std::ptr::eq(m, other) && m.dest_is_read_by(other))
        });

        match safe {
            Some(i) => {
                let m = moves.swap_remove(i);
                emit_move(ctx, m);
            },
            None if moves.is_empty() => break,
            None => {
                // in cycle, save first source to scratch, breaking the dependency
                emit_cycle_break(ctx, &mut moves[0]);
            },
        }
    }
}

/// Target-independent operand lowering
pub fn lower_operand<T: TargetOps>(
    lir: &mut lir::Function<T>,
    op: &Operand,
    mut vreg_map: impl FnMut(ValueId) -> VReg,
) -> T::Operand
where
    T::Operand: TargetOperand,
{
    match op {
        Operand::Place(p) => T::Operand::from_vreg(vreg_map(p.id)),
        Operand::Const(Const::Int(n, _)) => T::Operand::from_imm(*n),
        Operand::Const(Const::Bool(b)) => T::Operand::from_imm(match *b {
            true => 1,
            _ => 0,
        }),
        Operand::Const(Const::Float(v, typ)) => {
            let is_32 = typ.kind() == TypeKind::F32;
            let bits = match is_32 {
                true => (*v as f32).to_bits() as u64,
                _ => v.to_bits(),
            };

            let label = lir.new_float(bits, is_32);
            T::Operand::from_label(label)
        },
        Operand::Const(Const::Str { id, .. }) => T::Operand::from_label(format!(".L_str_{id}")),
        Operand::Const(Const::Unit) => unreachable!("unit operand"),
    }
}

/// Constant move generation
pub fn constant_mov<T: TargetOps>(
    lir: &mut lir::Function<T>,
    dest: VReg,
    c: &Const,
    layouts: Layouts,
) -> T::Instruction
where
    T::Operand: TargetOperand,
{
    let bytes = c.typ().machine_type(layouts).bytes();
    let src = lower_operand(lir, &Operand::Const(*c), |_| unreachable!());

    T::mov_op(dest, src, bytes, c.typ().is_float())
}

/// Operand materialisation
pub fn operand<T: TargetOps>(
    lir: &mut lir::Function<T>,
    op: &Operand,
    block: &BlockId,
    layouts: Layouts,
    mut vreg_map: impl FnMut(ValueId) -> VReg,
) -> VReg
where
    T::Operand: TargetOperand,
{
    match op {
        Operand::Place(p) => vreg_map(p.id),
        Operand::Const(c) => {
            let vreg = lir.new_vreg(c.typ().machine_type(layouts));
            let instruction = constant_mov(lir, vreg, c, layouts);
            lir.push_instr(block, instruction);
            vreg
        },
    }
}

/// Lower a constant string aggregate structure onto the stack
pub fn lower_const_str_aggregate<T: TargetOps>(
    lir: &mut lir::Function<T>,
    block: &BlockId,
    str_id: usize,
    len: usize,
    mut stack_addr: impl FnMut(&mut lir::Function<T>, &BlockId, VReg) -> VReg,
) -> VReg
where
    T::Operand: TargetOperand,
{
    let temp = lir.new_vreg(MachineType::Struct { size: 16, align: 8 });
    let ptr = lir.new_vreg(MachineType::Int { bytes: 8, signed: false });
    let label = format!(".L_str_{str_id}");

    lir.push_instr(block, T::load_label(ptr, label, false, 8));
    lir.push_instr(block, T::field_store(temp, T::Operand::from_vreg(ptr), 0, 8, false));
    lir.push_instr(block, T::field_store(temp, T::Operand::from_imm(len as i64), 8, 8, false));

    stack_addr(lir, block, temp)
}

/// Target-independent assignment lowering
pub fn lower_assign<T: TargetOps>(
    lir: &mut lir::Function<T>,
    block: &BlockId,
    dest: VReg,
    typ: Type,
    op: &Operand,
    layouts: Layouts,
    mut vreg_map: impl FnMut(ValueId) -> VReg,
    mut lower_operand: impl FnMut(&mut lir::Function<T>, &Operand, &BlockId) -> T::Operand,
) -> Option<T::Instruction>
where
    T::Operand: TargetOperand,
{
    if typ.is_aggregate_lir(layouts) {
        if typ.kind() == TypeKind::Str && matches!(op, Operand::Const(Const::Str { .. })) {
            let Operand::Const(Const::Str { id: str_id, len }) = op else {
                unreachable!()
            };

            let ptr = lir.new_vreg(MachineType::Int { bytes: 8, signed: false });
            let label = format!(".L_str_{str_id}");
            lir.push_instr(block, T::load_label(ptr, label, false, 8));

            let src_ptr = T::Operand::from_vreg(ptr);
            let src_len = T::Operand::from_imm(*len as i64);

            lir.push_instr(block, T::field_store(dest, src_ptr, 0, 8, false));
            lir.push_instr(block, T::field_store(dest, src_len, 8, 8, false));
            return None;
        }

        let Operand::Place(src) = op else {
            unreachable!("aggregate copy source must be a place");
        };
        let size = typ.machine_type(layouts).stack_size() as u32;
        let src_vreg = vreg_map(src.id);
        aggregate_copy(lir, block, AggregateCopy::new(src_vreg, dest, size));
        return None;
    }

    let bytes = typ.machine_type(layouts).bytes();
    let src = lower_operand(lir, op, block);

    Some(T::mov_op(dest, src, bytes, typ.is_float()))
}

/// Call argument preparation
pub fn prepare_call_args<T: TargetOps>(
    lir: &mut lir::Function<T>,
    block: &BlockId,
    args: &[Operand],
    layouts: Layouts,
    mut get_vreg: impl FnMut(ValueId) -> VReg,
    mut operand: impl FnMut(&mut lir::Function<T>, &Operand, &BlockId) -> VReg,
    mut lower_operand: impl FnMut(&mut lir::Function<T>, &Operand, &BlockId) -> T::Operand,
    mut stack_addr: impl FnMut(&mut lir::Function<T>, &BlockId, VReg) -> VReg,
    mut int_idx: usize,
    mut float_idx: usize,
) -> (CallArgMoves<T>, StackArgs<T>)
where
    T::Operand: TargetOperand,
{
    let mut moves = Vec::with_capacity(args.len());
    let mut stack_args = Vec::new();

    for arg in args {
        if arg.typ().is_aggregate_lir(layouts) {
            let ptr = match arg {
                Operand::Place(place) => stack_addr(lir, block, get_vreg(place.id)),
                Operand::Const(Const::Str { id: str_id, len }) => {
                    lower_const_str_aggregate(lir, block, *str_id, *len, &mut stack_addr)
                },
                _ => unreachable!("invalid aggregate argument"),
            };

            match T::param(int_idx, RegClass::Int) {
                Some(abi_reg) => moves.push((ptr, abi_reg)),
                None => stack_args.push((
                    T::Operand::from_vreg(ptr),
                    MachineType::Int { bytes: 8, signed: false },
                )),
            }

            int_idx += 1;
            continue;
        }

        let mt = arg.typ().machine_type(layouts);
        let class = mt.class();

        match class {
            RegClass::Int => {
                match T::param(int_idx, RegClass::Int) {
                    Some(abi_reg) => moves.push((operand(lir, arg, block), abi_reg)),
                    None => stack_args.push((lower_operand(lir, arg, block), mt)),
                }
                int_idx += 1;
            },
            RegClass::Float => {
                match T::param(float_idx, RegClass::Float) {
                    Some(abi_reg) => moves.push((operand(lir, arg, block), abi_reg)),
                    None => stack_args.push((lower_operand(lir, arg, block), mt)),
                }
                float_idx += 1;
            },
        }
    }

    (moves, stack_args)
}

/// Syscall argument preparation
pub fn prepare_syscall_args<T: TargetOps>(
    lir: &mut lir::Function<T>,
    block: &BlockId,
    args: &[Operand],
    layouts: Layouts,
    mut lower_operand_helper: impl FnMut(&mut lir::Function<T>, &Operand, &BlockId) -> T::Operand,
) -> (SyscallMoves<T>, SyscallUses)
where
    T::Operand: TargetOperand,
{
    let mut moves = Vec::with_capacity(args.len());
    let mut uses = Vec::with_capacity(args.len());

    for (i, arg) in args.iter().enumerate() {
        let abi_reg = T::syscall_param(i).expect("too many syscall arguments");
        let operand = lower_operand_helper(lir, arg, block);
        let bytes = arg.typ().machine_type(layouts).bytes();

        if let Some(vreg) = operand.as_vreg() {
            uses.push(vreg);
        }

        moves.push((operand, abi_reg, bytes));
    }

    (moves, uses)
}
