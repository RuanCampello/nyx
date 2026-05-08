use crate::parser::expression::BinaryOperator;
use crate::{
    lir::{
        MachineType, VReg,
        target::{Instruction, PhysicalReg, RegClass, Target},
    },
    mir::SyscallCode,
};

mod codegen;
mod lower;

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
/// ```text
/// ADD Xd, Xn, Xm -> Xd = Xn + Xm
/// ```
#[derive(Debug, Clone)]
#[allow(unused)]
#[rustfmt::skip]
pub enum A64Instr {
    // integer movs
    MovImm { dest: VReg, imm: i64, bytes: u8 },
    Mov { dest: VReg, src: VReg, bytes: u8 },
    /// load a stack-passed param
    LdrParam { dest: VReg, fp_offset: i32, bytes: u8 },

    // integer arithmetic
    Add { dest: VReg, lhs: VReg, rhs: A64Operand, bytes: u8 },
    Sub { dest: VReg, lhs: VReg, rhs: A64Operand, bytes: u8 },
    Mul { dest: VReg, lhs: VReg, rhs: VReg, bytes: u8 },
    SDiv { dest: VReg, lhs: VReg, rhs: VReg, bytes: u8 },
    Neg { dest: VReg, src: VReg, bytes: u8 },

    // logical operations
    And { dest: VReg, lhs: VReg, rhs: A64Operand, bytes: u8 },
    Or { dest: VReg, lhs: VReg, rhs: A64Operand, bytes: u8 },
    Eor { dest: VReg, lhs: VReg, rhs: A64Operand, bytes: u8 },

    // comparisons
    Cmp { lhs: VReg, rhs: A64Operand, bytes: u8 },
    Cset { dest: VReg, cond: A64Cond },
    Cmn { lhs: VReg, rhs: A64Operand, bytes: u8 },
    Tst { lhs: VReg, rhs: A64Operand, bytes: u8 },

    // float movs
    FMov { dest: VReg, src: VReg, bytes: u8 },
    FLiteral { dest: VReg, label: String, bytes: u8 },

    // float arithmetic
    FAdd { dest: VReg, lhs: VReg, rhs: VReg, bytes: u8 },
    FSub { dest: VReg, lhs: VReg, rhs: VReg, bytes: u8 },
    FMul { dest: VReg, lhs: VReg, rhs: VReg, bytes: u8 },
    FDiv { dest: VReg, lhs: VReg, rhs: VReg, bytes: u8 },
    FNeg { dest: VReg, src: VReg, bytes: u8 },

    // float comparison
    FCmp { lhs: VReg, rhs: VReg, bytes: u8 },

    Adr { dest: VReg, label: String },

    Call {
        target: String,
        /// register-passed arguments
        moves: Vec<(VReg, A64Reg)>,
        /// all vregs consumed by the call
        uses: Vec<VReg>,
        /// stack-passed arguments in call order
        stack_args: Vec<(A64Operand, MachineType)>,
        ret: Option<VReg>,
    },
 
    /// linux syscall (SVC #0)
    Syscall {
        id: u64,
        moves: Vec<(A64Operand, A64Reg, u8)>,
        uses: Vec<VReg>,
        ret: Option<VReg>,
    },
}

/// AArch64 condition codes for `cset` / `b.cond`
///
/// float comparisons (`fcmp`) set NZCV with unsigned semantics,
/// so we use `Lo`/`Ls`/`Hi`/`Hs` for float ordering
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[rustfmt::skip]
pub enum A64Cond {
    Eq, Ne,
    Lt, Le,
    Gt, Ge,
    // unsigned — used after FCMP
    Lo, Ls,
    Hi, Hs,
}

impl A64Cond {
    pub const fn as_str<'s>(&self) -> &'s str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
            Self::Lo => "lo",
            Self::Ls => "ls",
            Self::Hi => "hi",
            Self::Hs => "hs",
        }
    }

    pub fn new(operator: &BinaryOperator, is_float: bool) -> Self {
        match (operator, is_float) {
            (BinaryOperator::Eq, _) => Self::Eq,
            (BinaryOperator::Ne, _) => Self::Ne,

            (BinaryOperator::Lt, true) => Self::Lo,
            (BinaryOperator::Lt, false) => Self::Lt,

            (BinaryOperator::LtEq, true) => Self::Ls,
            (BinaryOperator::LtEq, false) => Self::Le,

            (BinaryOperator::Gt, true) => Self::Hi,
            (BinaryOperator::Gt, false) => Self::Gt,

            (BinaryOperator::GtEq, true) => Self::Hs,
            (BinaryOperator::GtEq, false) => Self::Ge,

            _ => unreachable!("invalid comparison operator"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(unused)]
#[rustfmt::skip]
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
                    A64Reg::X0,
                    A64Reg::X1,
                    A64Reg::X2,
                    A64Reg::X3,
                    A64Reg::X4,
                    A64Reg::X5,
                    A64Reg::X6,
                    A64Reg::X7,
                ];

                REGS.get(idx).copied()
            }

            RegClass::Float => {
                const REGS: [A64Reg; 8] = [
                    A64Reg::D0,
                    A64Reg::D1,
                    A64Reg::D2,
                    A64Reg::D3,
                    A64Reg::D4,
                    A64Reg::D5,
                    A64Reg::D6,
                    A64Reg::D7,
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
    fn syscall_param(idx: usize) -> Option<Self::Reg> {
        // Linux AArch64 syscall ABI
        // syscall number in X8, args in X0-X5
        const REGS: [A64Reg; 6] =
            [A64Reg::X0, A64Reg::X1, A64Reg::X2, A64Reg::X3, A64Reg::X4, A64Reg::X5];

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

    #[inline(always)]
    fn syscall_code(code: SyscallCode) -> u64 {
        match code {
            SyscallCode::Write => 64,
            SyscallCode::Exit => 93,
        }
    }
}

impl Instruction<AArch64> for A64Instr {
    fn defs(&self) -> &[VReg] {
        match self {
            Self::MovImm { dest, .. }
            | Self::Mov { dest, .. }
            | Self::LdrParam { dest, .. }
            | Self::Add { dest, .. }
            | Self::Sub { dest, .. }
            | Self::Mul { dest, .. }
            | Self::SDiv { dest, .. }
            | Self::Neg { dest, .. }
            | Self::And { dest, .. }
            | Self::Or { dest, .. }
            | Self::Eor { dest, .. }
            | Self::Cset { dest, .. }
            | Self::FMov { dest, .. }
            | Self::FLiteral { dest, .. }
            | Self::FAdd { dest, .. }
            | Self::FSub { dest, .. }
            | Self::FMul { dest, .. }
            | Self::FDiv { dest, .. }
            | Self::FNeg { dest, .. }
            | Self::Adr { dest, .. } => std::slice::from_ref(dest),

            Self::Cmp { .. } | Self::Cmn { .. } | Self::Tst { .. } | Self::FCmp { .. } => &[],
            Self::Call { ret: Some(r), .. } | Self::Syscall { ret: Some(r), .. } => {
                std::slice::from_ref(r)
            }
            Self::Call { ret: None, .. } | Self::Syscall { ret: None, .. } => &[],
        }
    }

    fn uses(&self, uses: &mut Vec<VReg>) {
        match self {
            Self::Mov { src, .. }
            | Self::Neg { src, .. }
            | Self::FMov { src, .. }
            | Self::FNeg { src, .. } => uses.push(*src),

            Self::Add { lhs, rhs, .. }
            | Self::Sub { lhs, rhs, .. }
            | Self::And { lhs, rhs, .. }
            | Self::Or { lhs, rhs, .. }
            | Self::Eor { lhs, rhs, .. }
            | Self::Cmp { lhs, rhs, .. }
            | Self::Cmn { lhs, rhs, .. }
            | Self::Tst { lhs, rhs, .. } => {
                uses.push(*lhs);

                if let A64Operand::VReg(rhs) = rhs {
                    uses.push(*rhs);
                }
            }

            Self::Mul { lhs, .. }
            | Self::SDiv { lhs, .. }
            | Self::FAdd { lhs, .. }
            | Self::FSub { lhs, .. }
            | Self::FMul { lhs, .. }
            | Self::FDiv { lhs, .. } => {
                uses.push(*lhs);
                match self {
                    Self::Mul { rhs, .. }
                    | Self::SDiv { rhs, .. }
                    | Self::FAdd { rhs, .. }
                    | Self::FSub { rhs, .. }
                    | Self::FMul { rhs, .. }
                    | Self::FDiv { rhs, .. } => uses.push(*rhs),
                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            Self::FCmp { lhs, rhs, .. } => {
                uses.push(*lhs);
                uses.push(*rhs);
            }

            Self::Call {
                uses: instruction_uses,
                ..
            }
            | Self::Syscall {
                uses: instruction_uses,
                ..
            } => uses.extend_from_slice(instruction_uses),

            Self::MovImm { .. }
            | Self::LdrParam { .. }
            | Self::FLiteral { .. }
            | Self::Adr { .. }
            | Self::Cset { .. } => {}
        }
    }

    fn clobbers<'r>(&self) -> &'r [<AArch64 as Target>::Reg] {
        match self {
            Self::Call { .. } | Self::Syscall { .. } => AArch64::caller_saved(),
            _ => &[],
        }
    }
}

impl A64Instr {
    pub(super) fn call(
        target: String,
        moves: Vec<(VReg, A64Reg)>,
        stack_args: Vec<(A64Operand, MachineType)>,
        ret: Option<VReg>,
    ) -> Self {
        let mut uses: Vec<VReg> = moves.iter().map(|(v, _)| *v).collect();

        for (operand, _) in &stack_args {
            if let A64Operand::VReg(vreg) = operand {
                uses.push(*vreg);
            }
        }

        Self::Call {
            target,
            moves,
            uses,
            ret,
            stack_args,
        }
    }
}

impl PhysicalReg for A64Reg {
    fn class(self) -> RegClass {
        match self {
            r if r >= Self::D0 && r <= Self::D31 => RegClass::Float,
            _ => RegClass::Int,
        }
    }

    #[inline(always)]
    fn name<'s>(self, bytes: u8) -> &'s str {
        macro_rules! r {
            ($reg64:expr, $reg32:expr) => {
                if bytes == 8 { $reg64 } else { $reg32 }
            };
        }

        match self {
            Self::X0 => r!("x0", "w0"),
            Self::X1 => r!("x1", "w1"),
            Self::X2 => r!("x2", "w2"),
            Self::X3 => r!("x3", "w3"),
            Self::X4 => r!("x4", "w4"),
            Self::X5 => r!("x5", "w5"),
            Self::X6 => r!("x6", "w6"),
            Self::X7 => r!("x7", "w7"),
            Self::X8 => r!("x8", "w8"),
            Self::X9 => r!("x9", "w9"),
            Self::X10 => r!("x10", "w10"),
            Self::X11 => r!("x11", "w11"),
            Self::X12 => r!("x12", "w12"),
            Self::X13 => r!("x13", "w13"),
            Self::X14 => r!("x14", "w14"),
            Self::X15 => r!("x15", "w15"),
            Self::X16 => r!("x16", "w16"),
            Self::X17 => r!("x17", "w17"),
            Self::X19 => r!("x19", "w19"),
            Self::X20 => r!("x20", "w20"),
            Self::X21 => r!("x21", "w21"),
            Self::X22 => r!("x22", "w22"),
            Self::X23 => r!("x23", "w23"),
            Self::X24 => r!("x24", "w24"),
            Self::X25 => r!("x25", "w25"),
            Self::X26 => r!("x26", "w26"),
            Self::X27 => r!("x27", "w27"),
            Self::X28 => r!("x28", "w28"),

            Self::X29 => "x29",
            Self::X30 => "x30",

            Self::D0 => r!("d0", "s0"),
            Self::D1 => r!("d1", "s1"),
            Self::D2 => r!("d2", "s2"),
            Self::D3 => r!("d3", "s3"),
            Self::D4 => r!("d4", "s4"),
            Self::D5 => r!("d5", "s5"),
            Self::D6 => r!("d6", "s6"),
            Self::D7 => r!("d7", "s7"),
            Self::D8 => r!("d8", "s8"),
            Self::D9 => r!("d9", "s9"),
            Self::D10 => r!("d10", "s10"),
            Self::D11 => r!("d11", "s11"),
            Self::D12 => r!("d12", "s12"),
            Self::D13 => r!("d13", "s13"),
            Self::D14 => r!("d14", "s14"),
            Self::D15 => r!("d15", "s15"),
            Self::D16 => r!("d16", "s16"),
            Self::D17 => r!("d17", "s17"),
            Self::D18 => r!("d18", "s18"),
            Self::D19 => r!("d19", "s19"),
            Self::D20 => r!("d20", "s20"),
            Self::D21 => r!("d21", "s21"),
            Self::D22 => r!("d22", "s22"),
            Self::D23 => r!("d23", "s23"),
            Self::D24 => r!("d24", "s24"),
            Self::D25 => r!("d25", "s25"),
            Self::D26 => r!("d26", "s26"),
            Self::D27 => r!("d27", "s27"),
            Self::D28 => r!("d28", "s28"),
            Self::D29 => r!("d29", "s29"),
            Self::D30 => r!("d30", "s30"),
            Self::D31 => r!("d31", "s31"),
        }
    }
}
