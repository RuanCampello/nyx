use crate::{
    hir::{
        Intrinsic, Syscall,
        error::CmpInterface as Cmp,
        ty::{Type, TypeKind},
    },
    parser::expression::BinaryOperator as BinOp,
};
use std::str::FromStr;

/// How a value is written out when a `{...}` interpolation prints it. The
/// compiler emits each of these itself, so this is the whole set of types an
/// interpolation accepts until a formatting interface exists
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintKind {
    /// a signed integer, widened to `i64`
    Int,
    /// an unsigned integer, widened to `u64`
    Uint,
    Bool,
    Char,
    Str,
}

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

/// indexed by `mutable as usize`: `Index`/`index` then `IndexMutable`/`index_mut`
const INDEX_OVERLOADS: [IndexOverload<'static>; 2] = [
    IndexOverload { interface: "Index", method: "index" },
    IndexOverload { interface: "IndexMutable", method: "index_mut" },
];

const SYSCALLS: &[(&str, Syscall)] = &[
    ("SYS_WRITE", Syscall::Write),
    ("SYS_EXIT", Syscall::Exit),
    ("SYS_MMAP", Syscall::Mmap),
    ("SYS_MUNMAP", Syscall::Munmap),
    ("SYS_MREMAP", Syscall::Mremap),
    ("SYS_MADVISE", Syscall::Madvise),
];

/// How `typ` is printed, or `None` when it has no printed form
pub fn print_kind(typ: Type<'_>) -> Option<PrintKind> {
    let kind = match typ.kind() {
        TypeKind::Ref { to, .. } => to.kind(),
        other => other,
    };

    Some(match kind {
        TypeKind::Bool => PrintKind::Bool,
        TypeKind::Char => PrintKind::Char,
        TypeKind::Str => PrintKind::Str,
        TypeKind::U8 | TypeKind::U16 | TypeKind::U32 | TypeKind::U64 | TypeKind::Uptr => {
            PrintKind::Uint
        },
        _ if typ.is_integer() => PrintKind::Int,
        _ => return None,
    })
}

#[inline]
pub fn comparison<'s>(operator: BinOp) -> Option<&'s Comparison<'s>> {
    COMPARISONS
        .iter()
        .find(|(candidate, _)| *candidate == operator)
        .map(|(_, entry)| entry)
}

#[inline]
pub fn index_overload(mutable: bool) -> &'static IndexOverload<'static> {
    &INDEX_OVERLOADS[mutable as usize]
}

#[inline]
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
    #[inline]
    pub const fn is_wrapping(self) -> bool {
        self.binary_operator().is_some()
    }

    #[inline]
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
