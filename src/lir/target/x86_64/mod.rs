use crate::lir::target::{PhysicalReg, RegClass};

pub struct X86_64;

/// Registers for x86_64 based on `SysV AMD64` ABI
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reg {
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

impl PhysicalReg for Reg {
    fn class(self) -> RegClass {
        match self >= Reg::Xmm0 {
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
