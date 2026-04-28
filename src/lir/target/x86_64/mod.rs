use crate::lir::{
    VReg,
    target::{Instruction, PhysicalReg, RegClass, Target},
};

mod lower;

pub struct X86_64;

#[derive(Debug, Clone)]
pub enum X86Operand {
    VReg(VReg),
    Imm(i64),
    RipRel(String),
}

#[derive(Debug, Clone)]
pub enum X86Instr {
    /// Copy from a physical ABI parameter register into a VReg at function entry
    /// `src_reg` is a physical register live-in from the caller: it has no VReg
    ParamMov {
        dest: VReg,
        src_reg: X86Reg,
        bytes: u8,
    },

    Mov {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    MovFloat {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    /// zero-extend 1-byte `setcc` result → 4 bytes.
    Movzx {
        dest: VReg,
        src: VReg,
    },

    // integer arithmetic
    Add {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    Sub {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    Imul {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    Neg {
        dest: VReg,
        bytes: u8,
    },
    /// Allocator constraints:
    ///   `dividend`  → rax  (fixed_use, stored in `fixed_uses_buf`)
    ///   `result`    → rax  (fixed_def)
    ///   rdx         clobbered
    ///
    /// `uses_buf[0]` = dividend always
    /// `uses_buf[1]` = divisor VReg when divisor is `X86Operand::VReg` otherwise duplicate of dividend.
    IDiv {
        result: VReg,
        dividend: VReg,
        divisor: X86Operand,
        bytes: u8,
        uses: [VReg; 2],
        precoloured_uses: [(VReg, X86Reg); 1],
    },

    // float arithmetic
    AddFloat {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    SubFloat {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    MulFloat {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    DivFloat {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    XorFloat {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },

    // comparison
    Cmp {
        lhs: VReg,
        rhs: X86Operand,
        bytes: u8,
        uses: [VReg; 2],
        uses_len: u8,
    },
    /// float comparison
    /// uses `%xmm15` as a scratch, that register is never allocatable
    Ucomis {
        lhs: VReg,
        rhs: X86Operand,
        bytes: u8,
        uses: [VReg; 2],
        uses_len: u8,
    },
    Setcc {
        dest: VReg,
        condition: Condition,
    },

    // logical operations
    And {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    Or {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },
    Xor {
        dest: VReg,
        src: X86Operand,
        bytes: u8,
    },

    /// Direct Call
    Call {
        target: String,
        moves: Vec<(VReg, X86Reg)>,
        uses: Vec<VReg>,
        ret: Option<VReg>,
        precoloured_def: Option<(VReg, X86Reg)>,
    },
}

/// Registers for x86_64 based on `SysV AMD64` ABI
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum X86Reg {
    // gp caller-saved
    Rax,
    Rcx,
    Rdx,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    // gp callee-saved
    Rbx,
    R12,
    R13,
    R14,
    R15,

    // xmm
    Xmm0,
    Xmm1,
    Xmm2,
    Xmm3,
    Xmm4,
    Xmm5,
    Xmm6,
    Xmm7,
    Xmm8,
    Xmm9,
    Xmm10,
    Xmm11,
    Xmm12,
    Xmm13,
    Xmm14,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Condition {
    E,
    Ne,
    L,
    Le,
    G,
    Ge,
    B,
    Be,
    A,
    Ae,
}

impl Target for X86_64 {
    type Reg = X86Reg;
    type Instruction = X86Instr;

    fn gprs<'r>() -> &'r [Self::Reg] {
        &[
            X86Reg::Rax,
            X86Reg::Rcx,
            X86Reg::Rdx,
            X86Reg::Rsi,
            X86Reg::Rdi,
            X86Reg::R8,
            X86Reg::R9,
            X86Reg::R10,
            X86Reg::Rbx,
            X86Reg::R12,
            X86Reg::R13,
            X86Reg::R14,
            X86Reg::R15,
        ]
    }

    fn fprs<'r>() -> &'r [Self::Reg] {
        &[
            X86Reg::Xmm0,
            X86Reg::Xmm1,
            X86Reg::Xmm2,
            X86Reg::Xmm3,
            X86Reg::Xmm4,
            X86Reg::Xmm5,
            X86Reg::Xmm6,
            X86Reg::Xmm7,
            X86Reg::Xmm8,
            X86Reg::Xmm9,
            X86Reg::Xmm10,
            X86Reg::Xmm11,
            X86Reg::Xmm12,
            X86Reg::Xmm13,
            X86Reg::Xmm14,
        ]
    }

    fn callee_saved<'r>() -> &'r [Self::Reg] {
        &[
            X86Reg::Rax,
            X86Reg::Rcx,
            X86Reg::Rdx,
            X86Reg::Rsi,
            X86Reg::Rdi,
            X86Reg::R8,
            X86Reg::R9,
            X86Reg::R10,
        ]
    }

    fn caller_saved<'r>() -> &'r [Self::Reg] {
        &[X86Reg::Rbx, X86Reg::R12, X86Reg::R13, X86Reg::R14, X86Reg::R15]
    }

    fn param(idx: usize, class: RegClass) -> Option<Self::Reg> {
        use self::X86Reg as R;

        match class {
            RegClass::Int => {
                const REGS: [R; 6] = [R::Rdi, R::Rsi, R::Rdx, R::Rcx, R::R8, R::R9];

                REGS.get(idx).copied()
            }
            RegClass::Float => {
                const REGS: [R; 8] = [R::Xmm0, R::Xmm1, R::Xmm2, R::Xmm3, R::Xmm4, R::Xmm5, R::Xmm6, R::Xmm7];

                REGS.get(idx).copied()
            }
        }
    }

    fn ret(class: RegClass) -> Option<Self::Reg> {
        match class {
            RegClass::Int => Some(X86Reg::Rax),
            RegClass::Float => Some(X86Reg::Xmm0),
        }
    }
}

impl Instruction<X86_64> for X86Instr {
    fn defs(&self) -> &[VReg] {
        match self {
            Self::ParamMov { dest, .. }
            | Self::Mov { dest, .. }
            | Self::MovFloat { dest, .. }
            | Self::Movzx { dest, .. }
            | Self::Add { dest, .. }
            | Self::Sub { dest, .. }
            | Self::Imul { dest, .. }
            | Self::Neg { dest, .. }
            | Self::And { dest, .. }
            | Self::Or { dest, .. }
            | Self::Xor { dest, .. }
            | Self::Setcc { dest, .. }
            | Self::AddFloat { dest, .. }
            | Self::SubFloat { dest, .. }
            | Self::MulFloat { dest, .. }
            | Self::DivFloat { dest, .. }
            | Self::XorFloat { dest, .. } => std::slice::from_ref(dest),

            Self::IDiv { result, .. } => std::slice::from_ref(result),

            Self::Cmp { uses, uses_len, .. } | Self::Ucomis { uses, uses_len, .. } => &uses[..*uses_len as usize],

            Self::Call { ret: Some(ret), .. } => std::slice::from_ref(ret),
            Self::Call { ret: None, .. } => &[],
        }
    }

    #[rustfmt::skip]
    fn uses(&self) -> &[VReg] {
        match self {
            Self::Mov { src: X86Operand::VReg(v), .. }
            | Self::MovFloat { src: X86Operand::VReg(v), .. }
            | Self::Add { src: X86Operand::VReg(v), .. }
            | Self::Sub { src: X86Operand::VReg(v), .. }
            | Self::Imul { src: X86Operand::VReg(v), .. }
            | Self::And { src: X86Operand::VReg(v), .. }
            | Self::Or { src: X86Operand::VReg(v), .. }
            | Self::Xor { src: X86Operand::VReg(v), .. } => std::slice::from_ref(v),

            Self::Neg { dest, .. } => std::slice::from_ref(dest),
            Self::Movzx { src, ..  } => std::slice::from_ref(src),

            Self::Call { uses, .. } => uses.as_slice(),

            _ => &[],
        }
    }

    fn as_copy(&self) -> Option<(VReg, VReg)> {
        match self {
            Self::Mov {
                dest,
                src: X86Operand::VReg(src),
                ..
            }
            | Self::MovFloat {
                dest,
                src: X86Operand::VReg(src),
                ..
            } => Some((*dest, *src)),

            _ => None,
        }
    }

    fn clobbers<'r>(&self) -> &'r [X86Reg] {
        match self {
            Self::IDiv { .. } => X86_64::caller_saved(),
            Self::Call { .. } => &[X86Reg::Rdx],
            _ => &[],
        }
    }

    fn precoloured_def(&self) -> Option<(VReg, X86Reg)> {
        match self {
            Self::IDiv { result, .. } => Some((*result, X86Reg::Rax)),
            Self::Call { precoloured_def, .. } => *precoloured_def,
            _ => None,
        }
    }

    fn precoloured_uses(&self) -> &[(VReg, X86Reg)] {
        match self {
            Self::IDiv { precoloured_uses, .. } => precoloured_uses,
            _ => &[],
        }
    }
}

impl X86Instr {
    pub(in crate::lir::target::x86_64) fn call(target: String, moves: Vec<(VReg, X86Reg)>, ret: Option<VReg>) -> Self {
        let uses = moves.iter().map(|(v, _)| *v).collect();
        let precoloured_def = ret.map(|v| (v, X86Reg::Rax));

        Self::Call {
            target,
            precoloured_def,
            uses,
            moves,
            ret,
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

impl Condition {
    pub const fn as_str<'s>(&self) -> &'s str {
        match self {
            Self::E => "e",
            Self::Ne => "ne",
            Self::L => "l",
            Self::Le => "le",
            Self::G => "g",
            Self::Ge => "ge",
            Self::B => "b",
            Self::Be => "be",
            Self::A => "a",
            Self::Ae => "ae",
        }
    }
}
