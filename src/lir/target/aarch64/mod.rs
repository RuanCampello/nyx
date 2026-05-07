use crate::{lir::{
    target::{PhysicalReg, RegClass, Target, Instruction}, VReg
}, };

pub struct AArch64;

/// An operand for AArch64 LIR instruction.
#[derive(Debug, Clone)]
pub enum A64Operand {
    VReg(VReg),
    /// signed immediate that fit's the instruction's imm12 or imm16 slot
    Imm(i64),
    Label(String),
}

/// AArch64 LIR instruction set.
///
/// Unlike x86_64, almost every arithmetic instruction is genuinely 3-address:
/// ```
/// ADD Xd, Xn, Xm -> Xd = Xn + Xm
/// ```
#[derive(Debug, Clone)]
pub enum A64Instr {
    MovImm {
        dest: VReg,
        imm: i64,
        bytes: u8,
    },
    Mov {
        dest: VReg,
        src: VReg,
        bytes: u8,
    },

    Add {
        dest: VReg,
        lhs: VReg,
        rhs: A64Operand,
        bytes: u8,
    },
    Sub {
        dest: VReg,
        lhs: VReg,
        rhs: A64Operand,
        bytes: u8,
    },
    Mul {
        dest: VReg,
        lhs: VReg,
        rhs: VReg,
        bytes: u8,
    },
    SDiv {
        dest: VReg,
        lhs: VReg,
        rhs: VReg,
        bytes: u8,
    },
    Neg {
        dest: VReg,
        src: VReg,
        bytes: u8,
    },

    And {
        dest: VReg,
        lhs: VReg,
        rhs: A64Operand,
        bytes: u8,
    },
    Or {
        dest: VReg,
        lhs: VReg,
        rhs: A64Operand,
        bytes: u8,
    },
    Eor {
        dest: VReg,
        lhs: VReg,
        rhs: A64Operand,
        bytes: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum A64Reg {
    // integer caller-saved
    X0, X1, X2, X3, X4, X5, X6, X7,
    X8, // indirect result register
    X9, X10, X11, X12, X13, X14, X15,
    X16, X17, // intra-procedure-call scratch (IP0 and IP1)


    // integer callee-saved
    X19, X20, X21, X22, X23, X24, X25, X26, X27, X28,

    X29, // frame-pointer -- never allocated
    X30, // link register -- never allocated

    // float caller-saved
    D0, D1, D2, D3, D4, D5, D6, D7,
    D16, D17, D18, D19, D20, D21, D22, D23,
    D24, D25, D26, D27, D28, D29, D30, D31,

    // float callee-saved
    D8, D9, D10, D11, D12, D13, D14, D15,
}

impl Target for AArch64 {
    type Reg = A64Reg;
    type Instruction = A64Instr;

    #[rustfmt::skip]
    #[inline(always)]
    fn gprs<'r>() -> &'r [Self::Reg] {
        &[
            A64Reg::X0,  A64Reg::X1,  A64Reg::X2,  A64Reg::X3,
            A64Reg::X4,  A64Reg::X5,  A64Reg::X6,  A64Reg::X7,
            A64Reg::X8,  A64Reg::X9,  A64Reg::X10, A64Reg::X11,
            A64Reg::X12, A64Reg::X13, A64Reg::X14, A64Reg::X15,
            A64Reg::X16, A64Reg::X17,
            A64Reg::X19, A64Reg::X20, A64Reg::X21, A64Reg::X22,
            A64Reg::X23, A64Reg::X24, A64Reg::X25, A64Reg::X26,
            A64Reg::X27, A64Reg::X28,
        ]
    }

    #[rustfmt::skip]
    #[inline(always)]
    fn fprs<'r>() -> &'r [Self::Reg] {
        &[
            A64Reg::D0,  A64Reg::D1,  A64Reg::D2,  A64Reg::D3,
            A64Reg::D4,  A64Reg::D5,  A64Reg::D6,  A64Reg::D7,
            A64Reg::D8,  A64Reg::D9,  A64Reg::D10, A64Reg::D11,
            A64Reg::D12, A64Reg::D13, A64Reg::D14, A64Reg::D15,
            A64Reg::D16, A64Reg::D17, A64Reg::D18, A64Reg::D19,
            A64Reg::D20, A64Reg::D21, A64Reg::D22, A64Reg::D23,
            A64Reg::D24, A64Reg::D25, A64Reg::D26, A64Reg::D27,
            A64Reg::D28, A64Reg::D29, A64Reg::D30, A64Reg::D31,
        ]
    }

    #[rustfmt::skip]
    #[inline(always)]
    fn callee_saved<'r>() -> &'r [Self::Reg] {
        &[
            A64Reg::X19, A64Reg::X20, A64Reg::X21, A64Reg::X22,
            A64Reg::X23, A64Reg::X24, A64Reg::X25, A64Reg::X26,
            A64Reg::X27, A64Reg::X28,
            A64Reg::D8,  A64Reg::D9,  A64Reg::D10, A64Reg::D11,
            A64Reg::D12, A64Reg::D13, A64Reg::D14, A64Reg::D15,
        ]
    }

    #[rustfmt::skip]
    #[inline(always)]
    fn caller_saved<'r>() -> &'r [Self::Reg] {
        &[
            A64Reg::X0,  A64Reg::X1,  A64Reg::X2,  A64Reg::X3,
            A64Reg::X4,  A64Reg::X5,  A64Reg::X6,  A64Reg::X7,
            A64Reg::X8,  A64Reg::X9,  A64Reg::X10, A64Reg::X11,
            A64Reg::X12, A64Reg::X13, A64Reg::X14, A64Reg::X15,
            A64Reg::X16, A64Reg::X17,
            A64Reg::D0,  A64Reg::D1,  A64Reg::D2,  A64Reg::D3,
            A64Reg::D4,  A64Reg::D5,  A64Reg::D6,  A64Reg::D7,
            A64Reg::D16, A64Reg::D17, A64Reg::D18, A64Reg::D19,
            A64Reg::D20, A64Reg::D21, A64Reg::D22, A64Reg::D23,
            A64Reg::D24, A64Reg::D25, A64Reg::D26, A64Reg::D27,
            A64Reg::D28, A64Reg::D29, A64Reg::D30, A64Reg::D31,
        ]
    }

    #[inline(always)]
    fn param(idx: usize, class: RegClass) -> Option<Self::Reg> {
        match class {
            RegClass::Int => {
                const REGS: [A64Reg; 8] = [
                    A64Reg::X0, A64Reg::X1, A64Reg::X2, A64Reg::X3,
                    A64Reg::X4, A64Reg::X5, A64Reg::X6, A64Reg::X7,
                ];

                REGS.get(idx).copied()
            }

            RegClass::Float => {
                const REGS: [A64Reg; 8] = [
                    A64Reg::D0, A64Reg::D1, A64Reg::D2, A64Reg::D3,
                    A64Reg::D4, A64Reg::D5, A64Reg::D6, A64Reg::D7,
                ];

                REGS.get(idx).copied()
            }
        }
    }

    #[inline(always)]
    fn ret(class: RegClass) -> Option<Self::Reg> {
        match class {
            RegClass::Int => Some(A64Reg::X0),
            RegClass::Float => Some(A64Reg::D0),
        }
    }


    #[inline(always)]
    fn n_reg_params(class: RegClass) -> usize {
        match class {
            RegClass::Int => 8,
            RegClass::Float => 8,
        }
    }

    #[inline(always)]
    fn syscall_param(idx: usize) -> Option<Self::Reg> {
        // Linux AArch64 syscall ABI
        // syscall number in X8, args in X0-X5
        const REGS: [A64Reg; 6] = [
            A64Reg::X0, A64Reg::X1, A64Reg::X2,
            A64Reg::X3, A64Reg::X4, A64Reg::X5,
        ];

        REGS.get(idx).copied()
    }


    /// AAPCS64 stack argument layout
    ///
    /// after the prologue, [x29, #0] = saved fp, [x29, #8] = saved lr
    /// stack arguments begin at [x29, #16] for the first, [x29, #24] for the second, so on
    #[inline(always)]
    fn param_stack_offset(stack_idx: usize, _class: RegClass) -> Option<i32> {
        Some(16 + (stack_idx as i32) * 8)
    }
}

impl Instruction<AArch64> for A64Instr {
    fn defs(&self) -> &[VReg] {
        todo!()
    }

    fn uses(&self) -> &[VReg] {
        todo!()
    }

    fn as_copy(&self) -> Option<(VReg, VReg)> {
        todo!()
    }

    fn clobbers<'r>(&self) -> &'r [<AArch64 as Target>::Reg] {
        todo!()
    }
}

impl PhysicalReg for A64Reg {
    fn class(self) -> RegClass {
        match self {
            _ => RegClass::Int,
        }
    }

    #[rustfmt::skip]
    #[inline(always)]
    fn name<'s>(self, bytes: u8) -> &'s str {
        match self {
            Self::X0  => if bytes == 8 { "x0"  } else { "w0"  },
            Self::X1  => if bytes == 8 { "x1"  } else { "w1"  },
            Self::X2  => if bytes == 8 { "x2"  } else { "w2"  },
            Self::X3  => if bytes == 8 { "x3"  } else { "w3"  },
            Self::X4  => if bytes == 8 { "x4"  } else { "w4"  },
            Self::X5  => if bytes == 8 { "x5"  } else { "w5"  },
            Self::X6  => if bytes == 8 { "x6"  } else { "w6"  },
            Self::X7  => if bytes == 8 { "x7"  } else { "w7"  },
            Self::X8  => if bytes == 8 { "x8"  } else { "w8"  },
            Self::X9  => if bytes == 8 { "x9"  } else { "w9"  },
            Self::X10 => if bytes == 8 { "x10" } else { "w10" },
            Self::X11 => if bytes == 8 { "x11" } else { "w11" },
            Self::X12 => if bytes == 8 { "x12" } else { "w12" },
            Self::X13 => if bytes == 8 { "x13" } else { "w13" },
            Self::X14 => if bytes == 8 { "x14" } else { "w14" },
            Self::X15 => if bytes == 8 { "x15" } else { "w15" },
            Self::X16 => if bytes == 8 { "x16" } else { "w16" },
            Self::X17 => if bytes == 8 { "x17" } else { "w17" },
            Self::X19 => if bytes == 8 { "x19" } else { "w19" },
            Self::X20 => if bytes == 8 { "x20" } else { "w20" },
            Self::X21 => if bytes == 8 { "x21" } else { "w21" },
            Self::X22 => if bytes == 8 { "x22" } else { "w22" },
            Self::X23 => if bytes == 8 { "x23" } else { "w23" },
            Self::X24 => if bytes == 8 { "x24" } else { "w24" },
            Self::X25 => if bytes == 8 { "x25" } else { "w25" },
            Self::X26 => if bytes == 8 { "x26" } else { "w26" },
            Self::X27 => if bytes == 8 { "x27" } else { "w27" },
            Self::X28 => if bytes == 8 { "x28" } else { "w28" },
            Self::X29 => "x29",
            Self::X30 => "x30",
 
            Self::D0  => if bytes == 8 { "d0"  } else { "s0"  },
            Self::D1  => if bytes == 8 { "d1"  } else { "s1"  },
            Self::D2  => if bytes == 8 { "d2"  } else { "s2"  },
            Self::D3  => if bytes == 8 { "d3"  } else { "s3"  },
            Self::D4  => if bytes == 8 { "d4"  } else { "s4"  },
            Self::D5  => if bytes == 8 { "d5"  } else { "s5"  },
            Self::D6  => if bytes == 8 { "d6"  } else { "s6"  },
            Self::D7  => if bytes == 8 { "d7"  } else { "s7"  },
            Self::D8  => if bytes == 8 { "d8"  } else { "s8"  },
            Self::D9  => if bytes == 8 { "d9"  } else { "s9"  },
            Self::D10 => if bytes == 8 { "d10" } else { "s10" },
            Self::D11 => if bytes == 8 { "d11" } else { "s11" },
            Self::D12 => if bytes == 8 { "d12" } else { "s12" },
            Self::D13 => if bytes == 8 { "d13" } else { "s13" },
            Self::D14 => if bytes == 8 { "d14" } else { "s14" },
            Self::D15 => if bytes == 8 { "d15" } else { "s15" },
            Self::D16 => if bytes == 8 { "d16" } else { "s16" },
            Self::D17 => if bytes == 8 { "d17" } else { "s17" },
            Self::D18 => if bytes == 8 { "d18" } else { "s18" },
            Self::D19 => if bytes == 8 { "d19" } else { "s19" },
            Self::D20 => if bytes == 8 { "d20" } else { "s20" },
            Self::D21 => if bytes == 8 { "d21" } else { "s21" },
            Self::D22 => if bytes == 8 { "d22" } else { "s22" },
            Self::D23 => if bytes == 8 { "d23" } else { "s23" },
            Self::D24 => if bytes == 8 { "d24" } else { "s24" },
            Self::D25 => if bytes == 8 { "d25" } else { "s25" },
            Self::D26 => if bytes == 8 { "d26" } else { "s26" },
            Self::D27 => if bytes == 8 { "d27" } else { "s27" },
            Self::D28 => if bytes == 8 { "d28" } else { "s28" },
            Self::D29 => if bytes == 8 { "d29" } else { "s29" },
            Self::D30 => if bytes == 8 { "d30" } else { "s30" },
            Self::D31 => if bytes == 8 { "d31" } else { "s31" },
        }
    }
}
