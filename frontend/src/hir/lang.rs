use crate::{
    hir::{Intrinsic, Syscall, error::CmpInterface as Cmp},
    parser::expression::BinaryOperator as BinOp,
};
use std::str::FromStr;

pub struct Comparison<'s> {
    pub method: &'s str,
    pub symbol: &'s str,
    pub interface: Cmp,
}

pub struct IndexOverload<'s> {
    pub interface: &'s str,
    pub method: &'s str,
}

const COMPARISONS: &[(BinOp, Comparison)] = &[
    (BinOp::Eq, Comparison { method: "eq", symbol: "==", interface: Cmp::Equality }),
    (BinOp::Ne, Comparison { method: "ne", symbol: "!=", interface: Cmp::Equality }),
    (BinOp::Lt, Comparison { method: "lt", symbol: "<", interface: Cmp::Ordering }),
    (BinOp::LtEq, Comparison { method: "le", symbol: "<=", interface: Cmp::Ordering }),
    (BinOp::Gt, Comparison { method: "gt", symbol: ">", interface: Cmp::Ordering }),
    (BinOp::GtEq, Comparison { method: "ge", symbol: ">=", interface: Cmp::Ordering }),
];

const INTRINSICS: &[(&str, Intrinsic)] = &[
    ("println", Intrinsic::PrintLn),
    ("print", Intrinsic::Print),
    ("syscall", Intrinsic::Syscall),
    ("len", Intrinsic::Len),
];

const INDEX_OVERLOADS: &[(bool, IndexOverload)] = &[
    (false, IndexOverload { interface: "Index", method: "index" }),
    (true, IndexOverload { interface: "IndexMutable", method: "index_mut" }),
];

const SYSCALLS: &[(&str, Syscall)] = &[
    ("SYS_WRITE", Syscall::Write),
    ("SYS_EXIT", Syscall::Exit),
    ("SYS_MMAP", Syscall::Mmap),
    ("SYS_MUNMAP", Syscall::Munmap),
    ("SYS_MREMAP", Syscall::Mremap),
    ("SYS_MADVISE", Syscall::Madvise),
];

#[inline(always)]
pub fn comparison<'s>(operator: BinOp) -> Option<&'s Comparison<'s>> {
    COMPARISONS
        .iter()
        .find(|(candidate, _)| *candidate == operator)
        .map(|(_, entry)| entry)
}

#[inline(always)]
pub fn index_overload<'s>(mutable: bool) -> &'s IndexOverload<'s> {
    INDEX_OVERLOADS
        .iter()
        .find(|(candidate, _)| *candidate == mutable)
        .map(|(_, entry)| entry)
        .expect("INDEX_OVERLOADS covers both mutability cases")
}

#[inline(always)]
pub fn intrinsic_method(receiver: &str, method: &str) -> Option<Intrinsic> {
    match (receiver, method) {
        ("str" | "[]", "len") => Some(Intrinsic::Len),
        (_, "wrapping_add") => Some(Intrinsic::WrappingAdd),
        (_, "wrapping_sub") => Some(Intrinsic::WrappingSub),
        (_, "wrapping_mul") => Some(Intrinsic::WrappingMul),
        _ => None,
    }
}

impl Intrinsic {
    #[inline(always)]
    pub const fn is_wrapping(self) -> bool {
        self.binary_operator().is_some()
    }

    #[inline(always)]
    pub const fn binary_operator(self) -> Option<BinOp> {
        match self {
            Self::WrappingAdd => Some(BinOp::Add),
            Self::WrappingSub => Some(BinOp::Sub),
            Self::WrappingMul => Some(BinOp::Mul),
            _ => None,
        }
    }
}

impl FromStr for Intrinsic {
    type Err = ();

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        INTRINSICS
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, intrinsic)| *intrinsic)
            .ok_or(())
    }
}

impl FromStr for Syscall {
    type Err = ();

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        SYSCALLS
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, syscall)| *syscall)
            .ok_or(())
    }
}
