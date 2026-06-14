use crate::{
    hir::{SyscallCode, Type, TypeKind},
    lir::{
        CheckedOperation, Layouts, MachineType, VReg, aggregate_chunks,
        target::{Instruction, MemOps, PhysicalReg, RegClass, Target, TargetOperand, TargetOps},
    },
    parser::expression::BinaryOperator,
};

mod codegen;
mod lower;

/// x86_64 target for the SysV AMD64 ABI.
pub struct X86_64;

/// An operand for an x86_64 LIR instruction.
#[derive(Debug, Clone)]
pub enum X86Operand {
    VReg(VReg),
    Imm(i64),
    /// RIP relative float constant in .rodata
    RipRel(String),
}

/// An x86_64 LIR instruction in 2-address form.
#[derive(Debug, Clone)]
#[rustfmt::skip]
pub enum X86Instr {
    Mov { dest: VReg, src: X86Operand, bytes: u8 },
    MovFloat { dest: VReg, src: X86Operand, bytes: u8 },
    /// load a parameter that was passed on the caller's stack
    MovFromStack {
        dest: VReg,
        /// positive offset from %rbp (always >= 16)
        rbp_offset: i32,
        bytes: u8,
    },
    Lea { dest: VReg, src: X86Operand },
    /// materialise the frame-pointer-relative address of a stack aggregate
    StackAddr { dest: VReg, origin: VReg },
    /// zero-extend -> dest_bytes
    Movzx { dest: VReg, src: X86Operand, src_bytes: u8, dest_bytes: u8 },
    /// sign-extend -> dest_bytes
    Movsx { dest: VReg, src: X86Operand, src_bytes: u8, dest_bytes: u8 },

    // integer arithmetic
    Add { dest: VReg, src: X86Operand, bytes: u8, checked: bool },
    Sub { dest: VReg, src: X86Operand, bytes: u8, checked: bool },
    Imul { dest: VReg, src: X86Operand, bytes: u8, checked: bool },
    Neg { dest: VReg, bytes: u8 },

    IDiv {
        result: VReg,
        dividend: VReg,
        divisor: X86Operand,
        bytes: u8,
        precoloured_uses: [(VReg, X86Reg); 1],
    },

    // float arithmetic
    AddFloat { dest: VReg, src: X86Operand, bytes: u8 },
    SubFloat { dest: VReg, src: X86Operand, bytes: u8 },
    MulFloat{ dest: VReg, src: X86Operand, bytes: u8 },
    DivFloat { dest: VReg, src: X86Operand, bytes: u8 },
    XorFloat { dest: VReg, src: X86Operand, bytes: u8 },

    // comparison
    Cmp { lhs: VReg, rhs: X86Operand, bytes: u8 },
    Test { lhs: VReg, rhs: X86Operand, bytes: u8 },
    /// float comparison
    /// uses `%xmm15` as a scratch, that register is never allocatable
    Ucomis { lhs: VReg, rhs: X86Operand, bytes: u8 },
    Setcc { dest: VReg, condition: Condition },

    // logical operations
    And { dest: VReg, src: X86Operand, bytes: u8 },
    Or { dest: VReg, src: X86Operand, bytes: u8 },
    Xor { dest: VReg, src: X86Operand, bytes: u8 },
    Not { dest: VReg, bytes: u8 },
    Shl { dest: VReg, src: X86Operand, bytes: u8, precoloured_uses: Vec<(VReg, X86Reg)> },
    Shr { dest: VReg, src: X86Operand, bytes: u8, precoloured_uses: Vec<(VReg, X86Reg)> },
    Sar { dest: VReg, src: X86Operand, bytes: u8, precoloured_uses: Vec<(VReg, X86Reg)> },

    /// load a scalar field struct on the stack
    FieldLoad {
        dest: VReg,
        origin: VReg,
        offset: i32,
        bytes: u8,
        is_float: bool,
    },
    // store a scalar value into a field of a struct on the stack
    FieldStore {
        origin: VReg,
        src: X86Operand,
        offset: i32,
        bytes: u8,
        is_float: bool,
    },
    PtrLoad {
        dest: VReg,
        ptr: VReg,
        offset: i32,
        bytes: u8,
        is_float: bool,
    },
    PtrStore {
        ptr: VReg,
        src: X86Operand,
        offset: i32,
        bytes: u8,
        is_float: bool,
    },

    Call {
        target: String,
        /// register-passed arguments
        moves: Vec<(VReg, X86Reg)>,
        /// all VRegs consumed by the call (union of `moves` and `stack_args`)
        uses: Vec<VReg>,
        ret: Option<VReg>,
        aggregate_ret: Vec<VReg>,
        /// stack based arguments in call order
        /// the emitter will push then in **reverse** order to match SysV ABI layout
        stack_args: Vec<(X86Operand, MachineType)>,
    },

    Syscall {
        id: u32,
        moves: Vec<(X86Operand, X86Reg, u8)>,
        uses: Vec<VReg>,
        ret: Option<VReg>,
    },
}

/// Physical registers for x86_64 under the SysV AMD64 ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[rustfmt::skip]
pub enum X86Reg {
    // gp caller-saved
    Rax, Rcx, Rdx, Rsi, Rdi, R8, R9, R10,
    // gp callee-saved
    Rbx, R12, R13, R14, R15,
    // xmm
    Xmm0, Xmm1, Xmm2, Xmm3, Xmm4, Xmm5, Xmm6, Xmm7,
    Xmm8, Xmm9, Xmm10, Xmm11, Xmm12, Xmm13, Xmm14,
}

/// x86 condition codes for `setcc` / `jcc`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[rustfmt::skip]
pub enum Condition {
    E, Ne,
    L, Le,
    G, Ge,
    B, Be,
    A, Ae,
}

impl Target for X86_64 {
    type Reg = X86Reg;
    type Instruction = X86Instr;

    #[inline(always)]
    #[rustfmt::skip]
    fn gprs<'r>() -> &'r [Self::Reg] {
        &[
            X86Reg::Rax, X86Reg::Rcx, X86Reg::Rdx, X86Reg::Rsi,
            X86Reg::Rdi, X86Reg::R8, X86Reg::R9, X86Reg::R10,
            X86Reg::Rbx, X86Reg::R12, X86Reg::R13, X86Reg::R14,
            X86Reg::R15,
        ]
    }

    #[inline(always)]
    #[rustfmt::skip]
    fn fprs<'r>() -> &'r [Self::Reg] {
        &[
            X86Reg::Xmm0, X86Reg::Xmm1, X86Reg::Xmm2, X86Reg::Xmm3,
            X86Reg::Xmm4, X86Reg::Xmm5, X86Reg::Xmm6, X86Reg::Xmm7,
            X86Reg::Xmm8, X86Reg::Xmm9, X86Reg::Xmm10, X86Reg::Xmm11,
            X86Reg::Xmm12, X86Reg::Xmm13, X86Reg::Xmm14,
        ]
    }

    #[inline(always)]
    fn callee_saved<'r>() -> &'r [Self::Reg] {
        &[X86Reg::Rbx, X86Reg::R12, X86Reg::R13, X86Reg::R14, X86Reg::R15]
    }

    #[inline(always)]
    #[rustfmt::skip]
    fn caller_saved<'r>() -> &'r [Self::Reg] {
        &[
            X86Reg::Rax, X86Reg::Rcx, X86Reg::Rdx,
            X86Reg::Rsi, X86Reg::Rdi, X86Reg::R8,
            X86Reg::R9, X86Reg::R10,
            X86Reg::Xmm0, X86Reg::Xmm1, X86Reg::Xmm2,
            X86Reg::Xmm3, X86Reg::Xmm4, X86Reg::Xmm5,
            X86Reg::Xmm6, X86Reg::Xmm7, X86Reg::Xmm8,
            X86Reg::Xmm9, X86Reg::Xmm10, X86Reg::Xmm11,
            X86Reg::Xmm12, X86Reg::Xmm13, X86Reg::Xmm14,
        ]
    }

    fn param(idx: usize, class: RegClass) -> Option<Self::Reg> {
        use X86Reg as R;

        match class {
            RegClass::Int => {
                const REGS: [R; 6] = [R::Rdi, R::Rsi, R::Rdx, R::Rcx, R::R8, R::R9];

                REGS.get(idx).copied()
            },
            RegClass::Float => {
                const REGS: [R; 8] =
                    [R::Xmm0, R::Xmm1, R::Xmm2, R::Xmm3, R::Xmm4, R::Xmm5, R::Xmm6, R::Xmm7];

                REGS.get(idx).copied()
            },
        }
    }

    #[inline(always)]
    fn syscall_param(idx: usize) -> Option<Self::Reg> {
        use X86Reg as R;

        const REGS: [R; 6] = [R::Rdi, R::Rsi, R::Rdx, R::R10, R::R8, R::R9];
        REGS.get(idx).copied()
    }

    #[inline(always)]
    fn ret(class: RegClass) -> Option<Self::Reg> {
        match class {
            RegClass::Int => Some(X86Reg::Rax),
            RegClass::Float => Some(X86Reg::Xmm0),
        }
    }

    #[inline(always)]
    fn syscall_code(code: SyscallCode) -> u64 {
        match code {
            SyscallCode::Write => 1,
            SyscallCode::Exit => 60,
        }
    }
}

#[rustfmt::skip]
impl MemOps for X86_64 {
    type Operand = X86Operand;

    #[inline(always)]
    fn vreg_operand(v: VReg) -> X86Operand { X86Operand::VReg(v) }

    #[inline(always)]
    fn field_load(dest: VReg, origin: VReg, offset: i32, bytes: u8, is_float: bool, _signed: bool) -> X86Instr {
        X86Instr::FieldLoad { dest, origin, offset, bytes, is_float }
    }

    #[inline(always)]
    fn field_store(origin: VReg, src: X86Operand, offset: i32, bytes: u8, is_float: bool) -> X86Instr {
        X86Instr::FieldStore { origin, src, offset, bytes, is_float }
    }

    #[inline(always)]
    fn ptr_load(dest: VReg, ptr: VReg, offset: i32, bytes: u8, is_float: bool, _signed: bool) -> X86Instr {
        X86Instr::PtrLoad { dest, ptr, offset, bytes, is_float }
    }

    #[inline(always)]
    fn ptr_store(ptr: VReg, src: X86Operand, offset: i32, bytes: u8, is_float: bool) -> X86Instr {
        X86Instr::PtrStore { ptr, src, offset, bytes, is_float }
    }
}

impl TargetOperand for X86Operand {
    #[inline(always)]
    fn from_vreg(v: VReg) -> Self {
        Self::VReg(v)
    }

    #[inline(always)]
    fn from_imm(imm: i64) -> Self {
        Self::Imm(imm)
    }

    #[inline(always)]
    fn from_label(label: String) -> Self {
        Self::RipRel(format!("{label}(%rip)"))
    }

    #[inline(always)]
    fn as_vreg(&self) -> Option<VReg> {
        match self {
            Self::VReg(v) => Some(*v),
            _ => None,
        }
    }
}

impl TargetOps for X86_64 {
    #[inline(always)]
    fn mov_op(dest: VReg, src: Self::Operand, bytes: u8, is_float: bool) -> Self::Instruction {
        match is_float {
            true => X86Instr::MovFloat { dest, src, bytes },
            _ => X86Instr::Mov { dest, src, bytes },
        }
    }

    #[inline(always)]
    fn load_label(dest: VReg, label: String, is_float: bool, bytes: u8) -> Self::Instruction {
        let rip_rel = format!("{label}(%rip)");
        match is_float {
            true => X86Instr::MovFloat { dest, src: X86Operand::RipRel(rip_rel), bytes },
            _ => X86Instr::Lea { dest, src: X86Operand::RipRel(rip_rel) },
        }
    }

    #[inline(always)]
    fn load_param_reg(dest: VReg, src: VReg, mt: MachineType) -> X86Instr {
        let src = X86Operand::VReg(src);
        let bytes = mt.bytes();
        match mt.class() {
            RegClass::Float => X86Instr::MovFloat { dest, src, bytes },
            RegClass::Int => X86Instr::Mov { dest, src, bytes },
        }
    }

    #[inline(always)]
    fn load_param_stack(dest: VReg, offset: i32, mt: MachineType) -> X86Instr {
        X86Instr::MovFromStack { dest, rbp_offset: offset, bytes: mt.bytes() }
    }

    #[inline(always)]
    fn load_stack_addr(dest: VReg, origin: VReg) -> X86Instr {
        X86Instr::StackAddr { dest, origin }
    }

    #[inline(always)]
    fn uses_sret(typ: Type, layouts: Layouts) -> bool {
        typ.is_aggregate() && small_integer_return(typ, layouts).is_none()
    }
}

impl Instruction<X86_64> for X86Instr {
    #[rustfmt::skip]
    fn defs(&self) -> &[VReg] {
        match self {
            Self::Mov { dest, .. } | Self::MovFloat { dest, .. }
            | Self::MovFromStack { dest, .. } | Self::Lea { dest, .. }
            | Self::StackAddr { dest, .. } | Self::Movzx { dest, .. }
            | Self::Movsx { dest, .. } | Self::Add { dest, .. }
            | Self::Sub { dest, .. } | Self::Imul { dest, .. }
            | Self::Neg { dest, .. } | Self::And { dest, .. }
            | Self::Or { dest, .. } | Self::Xor { dest, .. }
            | Self::Setcc { dest, .. } | Self::AddFloat { dest, .. }
            | Self::SubFloat { dest, .. } | Self::MulFloat { dest, .. }
            | Self::DivFloat { dest, .. } | Self::FieldLoad { dest, .. }
            | Self::PtrLoad { dest, .. } | Self::XorFloat { dest, .. }
            | Self::Not { dest, .. } | Self::Shl { dest, .. }
            | Self::Shr { dest, .. }
            | Self::Sar { dest, .. } => std::slice::from_ref(dest),

            Self::IDiv { result, .. } => std::slice::from_ref(result),

            Self::FieldStore { .. } | Self::PtrStore { .. }
            | Self::Cmp { .. } | Self::Test { .. }
            | Self::Ucomis { .. } => &[],

            Self::Call { ret: Some(ret), .. } | Self::Syscall { ret: Some(ret), .. } => {
                std::slice::from_ref(ret)
            },

            Self::Call { aggregate_ret, ret: None, .. } => aggregate_ret.as_slice(),
            Self::Syscall { ret: None, .. } => &[],
        }
    }

    #[rustfmt::skip]
    fn uses(&self, uses: &mut Vec<VReg>) {
        match self {
            Self::Mov { src: X86Operand::VReg(v), .. }
            | Self::MovFloat { src: X86Operand::VReg(v), .. }
            | Self::Movzx { src: X86Operand::VReg(v), .. }
            | Self::Movsx { src: X86Operand::VReg(v), .. }
            | Self::Lea { src: X86Operand::VReg(v), .. } => uses.push(*v),

            // 2-address: dest is read+write, src is read-only
            Self::Add { src: X86Operand::VReg(v), .. }
            | Self::Sub { src: X86Operand::VReg(v), .. }
            | Self::Imul { src: X86Operand::VReg(v), .. }
            | Self::And { src: X86Operand::VReg(v), .. }
            | Self::Or { src: X86Operand::VReg(v), .. }
            | Self::Xor { src: X86Operand::VReg(v), .. }
            | Self::AddFloat { src: X86Operand::VReg(v), .. }
            | Self::SubFloat { src: X86Operand::VReg(v), .. }
            | Self::MulFloat { src: X86Operand::VReg(v), .. }
            | Self::DivFloat { src: X86Operand::VReg(v), .. }
            | Self::XorFloat { src: X86Operand::VReg(v), .. }
            | Self::Shl { src: X86Operand::VReg(v), .. }
            | Self::Shr { src: X86Operand::VReg(v), .. }
            | Self::Sar { src: X86Operand::VReg(v), .. } => uses.push(*v),

            // immediate-source 2-address: only dest is used
            Self::Add { dest, .. }
            | Self::Sub { dest, .. }
            | Self::Imul { dest, .. }
            | Self::And { dest, .. }
            | Self::Or { dest, .. }
            | Self::Xor { dest, .. }
            | Self::AddFloat { dest, .. }
            | Self::SubFloat { dest, .. }
            | Self::MulFloat { dest, .. }
            | Self::DivFloat { dest, .. }
            | Self::XorFloat { dest, .. }
            | Self::Shl { dest, .. }
            | Self::Shr { dest, .. }
            | Self::Sar { dest, .. } => uses.push(*dest),

            Self::FieldStore {origin, src: X86Operand::VReg(vreg), ..} => {
                uses.push(*origin);
                uses.push(*vreg);
            },
            Self::StackAddr { origin , ..}
            | Self::FieldLoad { origin, .. }
            | Self::FieldStore {origin, ..} => uses.push(*origin),

            Self::PtrStore { ptr, src: X86Operand::VReg(vreg), .. } => {
                uses.push(*ptr);
                uses.push(*vreg);
            }
            Self::PtrLoad { ptr, .. } | Self::PtrStore { ptr, .. } => uses.push(*ptr),

            Self::Neg { dest, .. } | Self::Not { dest, .. } => uses.push(*dest),

            Self::Cmp { lhs, rhs, .. }
            | Self::Test { lhs, rhs, .. }
            | Self::Ucomis { lhs, rhs, .. } => {
                uses.push(*lhs);
                if let X86Operand::VReg(rhs) = rhs {
                    uses.push(*rhs);
                }
            }

            Self::IDiv { dividend, divisor, .. } => {
                uses.push(*dividend);
                if let X86Operand::VReg(divisor) = divisor {
                    uses.push(*divisor);
                }
            }
            Self::Call { uses: instruction_uses, .. }
            | Self::Syscall { uses: instruction_uses, .. } => uses.extend_from_slice(instruction_uses),

            _ => {}
        }
    }

    #[inline]
    fn clobbers<'r>(&self) -> &'r [X86Reg] {
        match self {
            Self::IDiv { .. } => &[X86Reg::Rdx],
            Self::Call { .. } | Self::Syscall { .. } => X86_64::caller_saved(),
            _ => &[],
        }
    }

    #[rustfmt::skip]
    #[inline]
    fn precoloured_uses(&self) -> &[(VReg, X86Reg)] {
        match self {
            Self::IDiv { precoloured_uses, .. } => precoloured_uses,
            Self::Shl { precoloured_uses, .. }
            | Self::Shr { precoloured_uses, .. }
            | Self::Sar { precoloured_uses, .. } => precoloured_uses.as_slice(),
            _ => &[],
        }
    }

    #[inline(always)]
    fn stack_forced(&self) -> &[VReg] {
        match self {
            Self::StackAddr { origin, .. }
            | Self::FieldLoad { origin, .. }
            | Self::FieldStore { origin, .. } => std::slice::from_ref(origin),
            _ => &[],
        }
    }
}

impl X86Instr {
    pub(self) fn call(
        target: String,
        moves: Vec<(VReg, X86Reg)>,
        stack_args: Vec<(X86Operand, MachineType)>,
        ret: Option<VReg>,
        aggregate_ret: Vec<VReg>,
    ) -> Self {
        let mut uses: Vec<VReg> = moves.iter().map(|(v, _)| *v).collect();

        for (operand, _) in &stack_args {
            if let X86Operand::VReg(vreg) = operand {
                uses.push(*vreg);
            }
        }

        Self::Call { target, moves, uses, ret, aggregate_ret, stack_args }
    }

    /// Creates a comparation instruction depending on `O`
    ///
    /// - *0*: `cmp`
    /// - *1*: `ucomis`
    /// - *2*: `test`
    #[inline(always)]
    #[rustfmt::skip]
    pub const fn cmp<const O: u8>(lhs: VReg, rhs: X86Operand, bytes: u8) -> Self {
        match O {
            0 => Self::Cmp { lhs, rhs, bytes },
            1 => Self::Test { lhs, rhs, bytes },
            2 => Self::Ucomis { lhs, rhs, bytes },
            _ => unsafe { std::hint::unreachable_unchecked() },
        }
    }

    #[inline(always)]
    pub const fn idiv(result: VReg, dividend: VReg, divisor: X86Operand, bytes: u8) -> Self {
        Self::IDiv {
            bytes,
            result,
            dividend,
            divisor,
            precoloured_uses: [(dividend, X86Reg::Rax)],
        }
    }
}

impl PhysicalReg for X86Reg {
    fn class(self) -> RegClass {
        match self >= X86Reg::Xmm0 {
            true => RegClass::Float,
            _ => RegClass::Int,
        }
    }

    fn name<'s>(self, bytes: u8) -> &'s str {
        if self.class() == RegClass::Float {
            return match self {
                Self::Xmm0 => "xmm0",
                Self::Xmm1 => "xmm1",
                Self::Xmm2 => "xmm2",
                Self::Xmm3 => "xmm3",
                Self::Xmm4 => "xmm4",
                Self::Xmm5 => "xmm5",
                Self::Xmm6 => "xmm6",
                Self::Xmm7 => "xmm7",
                Self::Xmm8 => "xmm8",
                Self::Xmm9 => "xmm9",
                Self::Xmm10 => "xmm10",
                Self::Xmm11 => "xmm11",
                Self::Xmm12 => "xmm12",
                Self::Xmm13 => "xmm13",
                Self::Xmm14 => "xmm14",
                _ => unreachable!("invalid float register"),
            };
        }

        let size_idx = match bytes {
            8 => 0,
            4 => 1,
            2 => 2,
            1 => 3,
            _ => panic!("invalid GPR size: {}", bytes),
        };

        match self {
            Self::Rax => ["rax", "eax", "ax", "al"][size_idx],
            Self::Rcx => ["rcx", "ecx", "cx", "cl"][size_idx],
            Self::Rdx => ["rdx", "edx", "dx", "dl"][size_idx],
            Self::Rbx => ["rbx", "ebx", "bx", "bl"][size_idx],
            Self::Rsi => ["rsi", "esi", "si", "sil"][size_idx],
            Self::Rdi => ["rdi", "edi", "di", "dil"][size_idx],

            Self::R8 => ["r8", "r8d", "r8w", "r8b"][size_idx],
            Self::R9 => ["r9", "r9d", "r9w", "r9b"][size_idx],
            Self::R10 => ["r10", "r10d", "r10w", "r10b"][size_idx],
            Self::R12 => ["r12", "r12d", "r12w", "r12b"][size_idx],
            Self::R13 => ["r13", "r13d", "r13w", "r13b"][size_idx],
            Self::R14 => ["r14", "r14d", "r14w", "r14b"][size_idx],
            Self::R15 => ["r15", "r15d", "r15w", "r15b"][size_idx],

            _ => unreachable!("invalid register and operand size combination"),
        }
    }
}

impl CheckedOperation for X86Instr {
    fn flag(&self) -> Option<u8> {
        match self {
            Self::Add { checked: true, .. } => Some(Self::ADD),
            Self::Sub { checked: true, .. } => Some(Self::SUB),
            Self::Imul { checked: true, .. } => Some(Self::MUL),
            _ => None,
        }
    }
}

impl Condition {
    #[rustfmt::skip]
    pub const fn as_str<'s>(&self) -> &'s str {
        match self {
            Self::E => "e", Self::Ne => "ne",
            Self::L => "l", Self::Le => "le",
            Self::G => "g", Self::Ge => "ge",
            Self::B => "b", Self::Be => "be",
            Self::A => "a", Self::Ae => "ae",
        }
    }

    pub fn new(operator: &BinaryOperator, is_float: bool) -> Self {
        match (operator, is_float) {
            (BinaryOperator::Eq, _) => Self::E,
            (BinaryOperator::Ne, _) => Self::Ne,

            (BinaryOperator::Lt, true) => Self::B,
            (BinaryOperator::Lt, false) => Self::L,

            (BinaryOperator::LtEq, true) => Self::Be,
            (BinaryOperator::LtEq, false) => Self::Le,

            (BinaryOperator::Gt, true) => Self::A,
            (BinaryOperator::Gt, false) => Self::G,

            (BinaryOperator::GtEq, true) => Self::Ae,
            (BinaryOperator::GtEq, false) => Self::Ge,

            _ => unreachable!("invalid combination of binary operator and float flag"),
        }
    }
}

/// Small aggregates (<= 16 bytes, no floats) are returned directly in RAX/RDX
/// under the SysV ABI. Returns the per-register `(offset, bytes, reg)` chunks
/// when `typ` qualifies, or `None` when it must be returned via an sret pointer.
fn small_integer_return(typ: Type, layouts: Layouts) -> Option<Vec<(i32, u8, X86Reg)>> {
    let size = typ.machine_type(layouts).stack_size() as u32;
    let contains_float = match typ.kind() {
        TypeKind::Struct(sid) => layouts.structs[sid.0 as usize].contains_float(),
        _ => false,
    };
    if size == 0 || size > 16 || contains_float {
        return None;
    }

    let regs = [X86Reg::Rax, X86Reg::Rdx];
    Some(
        aggregate_chunks(size)
            .zip(regs)
            .map(|((offset, bytes), reg)| (offset, bytes, reg))
            .collect(),
    )
}
