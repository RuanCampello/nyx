//! Constant folding of individual MIR operations
//!
//! Every entry point is total and side-effect free: it returns `None` whenever the
//! operation cannot be folded *without changing observable behaviour*

use crate::{
    hir::{EnumRepr, Type, TypeKind},
    mir::Const,
    parser::expression::{BinaryOperator, UnaryOperator},
};
use std::cmp::Ordering;

/// Width and signedness of an integral MIR type
#[derive(Clone, Copy)]
struct IntRepr {
    bits: u32,
    signed: bool,
}

/// What a proven-constant operation would do if it were executed
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Panic {
    /// the exact result is not representable in the type it is computed in
    Overflow,
    DivisionByZero,
    /// `INT_MIN / -1`, whose quotient is one past the maximum
    DivisionOverflow,
    /// a shift count at least as wide as the operand
    ShiftOutOfRange,
}

pub(super) fn diagnose(operation: BinaryOperator, lhs: Const, rhs: Const) -> Option<Panic> {
    use BinaryOperator as Op;

    let (Const::Int(a, typ), Const::Int(b, _)) = (lhs, rhs) else {
        return None;
    };

    let repr = IntRepr::of(typ)?;
    let (a, b) = (repr.normalise(a), repr.normalise(b));
    let overflows = |exact: i128| (!repr.holds(exact)).then_some(Panic::Overflow);

    match operation {
        Op::Add => overflows(repr.widen(a) + repr.widen(b)),
        Op::Sub => overflows(repr.widen(a) - repr.widen(b)),
        Op::Mul => overflows(repr.widen(a) * repr.widen(b)),

        Op::Div if b == 0 => Some(Panic::DivisionByZero),
        Op::Div if repr.is_division_overflow(a, b) => Some(Panic::DivisionOverflow),

        Op::Shl | Op::Shr if !repr.is_shift_in_range(b) => Some(Panic::ShiftOutOfRange),

        _ => None,
    }
}

pub(super) fn diagnose_unary(operation: UnaryOperator, operand: Const) -> Option<Panic> {
    let (UnaryOperator::Neg, Const::Int(value, typ)) = (operation, operand) else {
        return None;
    };

    let repr = IntRepr::of(typ)?;
    match repr.signed && repr.holds(repr.widen(value)) {
        true => (!repr.holds(-repr.widen(value))).then_some(Panic::Overflow),
        false => None,
    }
}

pub(super) fn binary(operation: BinaryOperator, lhs: Const, rhs: Const) -> Option<Const> {
    match (lhs, rhs) {
        (Const::Int(a, typ), Const::Int(b, _)) => integer(operation, a, b, typ),
        (Const::Float(a, typ), Const::Float(b, _)) => float(operation, a, b, typ),
        (Const::Bool(a), Const::Bool(b)) => boolean(operation, a, b),
        _ => None,
    }
}

pub(super) fn unary(operation: UnaryOperator, operand: Const) -> Option<Const> {
    match (operation, operand) {
        (UnaryOperator::Neg, Const::Int(value, typ)) => {
            let repr = IntRepr::of(typ)?;
            Some(Const::Int(repr.normalise(value.wrapping_neg()), typ))
        },
        (UnaryOperator::Neg, Const::Float(value, typ)) => Some(Const::Float(-value, typ)),
        (UnaryOperator::Not, Const::Bool(value)) => Some(Const::Bool(!value)),
        (UnaryOperator::Not, Const::Int(value, typ)) => {
            let repr = IntRepr::of(typ)?;
            Some(Const::Int(repr.normalise(!value), typ))
        },
        _ => None,
    }
}

/// fold a `Cast` between primitive types
pub(super) fn cast(value: Const, target: Type) -> Option<Const> {
    match (value, target.kind()) {
        (_, TypeKind::Bool) => None,

        (Const::Int(value, _), TypeKind::F32) => Some(Const::Float(value as f32 as f64, target)),
        (Const::Int(value, _), TypeKind::F64) => Some(Const::Float(value as f64, target)),

        (Const::Int(value, from), _) => {
            let (from, to) = (IntRepr::of(from)?, IntRepr::of(target)?);
            // narrowing truncates, widening extends according to the *source* sign
            Some(Const::Int(to.normalise(from.normalise(value)), target))
        },

        (Const::Float(value, _), TypeKind::F32) => Some(Const::Float(value as f32 as f64, target)),
        (Const::Float(value, _), TypeKind::F64) => Some(Const::Float(value, target)),

        (Const::Bool(value), _) => {
            let repr = IntRepr::of(target)?;
            Some(Const::Int(repr.normalise(value as i64), target))
        },

        _ => None,
    }
}

fn integer(operation: BinaryOperator, a: i64, b: i64, typ: Type) -> Option<Const> {
    use BinaryOperator as Op;

    let repr = IntRepr::of(typ)?;
    let (a, b) = (repr.normalise(a), repr.normalise(b));

    let arithmetic = |value: i64| Some(Const::Int(repr.normalise(value), typ));
    let compare = |ordering: Ordering| repr.compare(a, b) == ordering;

    match operation {
        Op::Add => arithmetic(a.wrapping_add(b)),
        Op::Sub => arithmetic(a.wrapping_sub(b)),
        Op::Mul => arithmetic(a.wrapping_mul(b)),

        // both faults are raised by the hardware; folding would erase them
        Op::Div => match (b, repr.is_division_overflow(a, b)) {
            (0, _) | (_, true) => None,
            _ => match repr.signed {
                true => arithmetic(a.wrapping_div(b)),
                false => arithmetic(((a as u64).wrapping_div(b as u64)) as i64),
            },
        },

        Op::BitAnd => arithmetic(a & b),
        Op::BitOr => arithmetic(a | b),
        Op::BitXor => arithmetic(a ^ b),

        // the two targets mask out-of-range counts differently for sub-word types
        Op::Shl | Op::Shr if !repr.is_shift_in_range(b) => None,
        Op::Shl => arithmetic(a.wrapping_shl(b as u32)),
        Op::Shr => match repr.signed {
            true => arithmetic(a.wrapping_shr(b as u32)),
            false => arithmetic(((a as u64).wrapping_shr(b as u32)) as i64),
        },

        Op::Eq => Some(Const::Bool(a == b)),
        Op::Ne => Some(Const::Bool(a != b)),
        Op::Lt => Some(Const::Bool(compare(Ordering::Less))),
        Op::Gt => Some(Const::Bool(compare(Ordering::Greater))),
        Op::LtEq => Some(Const::Bool(!compare(Ordering::Greater))),
        Op::GtEq => Some(Const::Bool(!compare(Ordering::Less))),

        Op::And | Op::Or => None,
    }
}

fn float(operation: BinaryOperator, a: f64, b: f64, typ: Type) -> Option<Const> {
    use BinaryOperator as Op;

    let single = matches!(typ.kind(), TypeKind::F32);
    let arithmetic = |wide: fn(f64, f64) -> f64, narrow: fn(f32, f32) -> f32| {
        let value = match single {
            true => narrow(a as f32, b as f32) as f64,
            false => wide(a, b),
        };
        Some(Const::Float(value, typ))
    };

    match operation {
        Op::Add => arithmetic(|a, b| a + b, |a, b| a + b),
        Op::Sub => arithmetic(|a, b| a - b, |a, b| a - b),
        Op::Mul => arithmetic(|a, b| a * b, |a, b| a * b),
        Op::Div => arithmetic(|a, b| a / b, |a, b| a / b),

        Op::Eq => Some(Const::Bool(a == b)),
        Op::Ne => Some(Const::Bool(a != b)),
        Op::Lt => Some(Const::Bool(a < b)),
        Op::Gt => Some(Const::Bool(a > b)),
        Op::LtEq => Some(Const::Bool(a <= b)),
        Op::GtEq => Some(Const::Bool(a >= b)),

        _ => None,
    }
}

#[inline]
const fn boolean(operation: BinaryOperator, a: bool, b: bool) -> Option<Const> {
    match operation {
        BinaryOperator::And => Some(Const::Bool(a && b)),
        BinaryOperator::Or => Some(Const::Bool(a || b)),
        BinaryOperator::Eq => Some(Const::Bool(a == b)),
        BinaryOperator::Ne => Some(Const::Bool(a != b)),
        BinaryOperator::BitAnd => Some(Const::Bool(a & b)),
        BinaryOperator::BitOr => Some(Const::Bool(a | b)),
        BinaryOperator::BitXor => Some(Const::Bool(a ^ b)),
        _ => None,
    }
}

impl IntRepr {
    const fn of(typ: Type) -> Option<Self> {
        let (bits, signed) = match typ.kind() {
            TypeKind::I8 => (8, true),
            TypeKind::U8 => (8, false),
            TypeKind::I16 => (16, true),
            TypeKind::U16 => (16, false),
            TypeKind::I32 => (32, true),
            TypeKind::U32 | TypeKind::Char => (32, false),
            TypeKind::I64 => (64, true),
            TypeKind::U64 => (64, false),
            TypeKind::Iptr => (64, true),
            TypeKind::Uptr => (64, false),
            TypeKind::Enum(id) => match id.repr() {
                EnumRepr::I8 => (8, true),
                EnumRepr::U8 => (8, false),
                EnumRepr::I16 => (16, true),
                EnumRepr::U16 => (16, false),
                EnumRepr::I32 => (32, true),
                EnumRepr::U32 => (32, false),
                EnumRepr::I64 | EnumRepr::Iptr => (64, true),
                EnumRepr::U64 | EnumRepr::Uptr => (64, false),
            },
            _ => return None,
        };

        Some(Self { bits, signed })
    }

    const fn normalise(self, value: i64) -> i64 {
        match self.bits {
            64 => value,
            bits => {
                let shift = 64 - bits;
                match self.signed {
                    true => (value << shift) >> shift,
                    false => (((value as u64) << shift) >> shift) as i64,
                }
            },
        }
    }

    #[inline]
    const fn compare(self, a: i64, b: i64) -> Ordering {
        match self.signed {
            true => match a < b {
                true => Ordering::Less,
                _ => match a > b {
                    true => Ordering::Greater,
                    _ => Ordering::Equal,
                },
            },
            false => match (a as u64) < (b as u64) {
                true => Ordering::Less,
                _ => match (a as u64) > (b as u64) {
                    true => Ordering::Greater,
                    _ => Ordering::Equal,
                },
            },
        }
    }

    #[inline]
    const fn widen(self, value: i64) -> i128 {
        match self.signed {
            true => value as i128,
            false => value as u64 as i128,
        }
    }

    #[inline]
    const fn holds(self, value: i128) -> bool {
        let (low, high) = match self.signed {
            true => (-(1i128 << (self.bits - 1)), (1i128 << (self.bits - 1)) - 1),
            false => (0, (1i128 << self.bits) - 1),
        };

        value >= low && value <= high
    }

    #[inline]
    const fn is_division_overflow(self, a: i64, b: i64) -> bool {
        self.signed && b == -1 && a == self.normalise(1i64 << (self.bits - 1))
    }

    #[inline]
    const fn is_shift_in_range(self, count: i64) -> bool {
        count >= 0 && (count as u64) < self.bits as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn int(value: i64, kind: TypeKind) -> Const {
        Const::Int(value, Type::new(kind))
    }

    #[test]
    fn addition_wraps_in_the_operand_width() {
        let folded = binary(BinaryOperator::Add, int(127, TypeKind::I8), int(1, TypeKind::I8));
        assert_eq!(folded, Some(int(-128, TypeKind::I8)));
    }

    #[test]
    fn unsigned_addition_wraps_without_sign_extending() {
        let folded = binary(BinaryOperator::Add, int(255, TypeKind::U8), int(1, TypeKind::U8));
        assert_eq!(folded, Some(int(0, TypeKind::U8)));
    }

    #[test]
    fn multiplication_wraps_at_i32() {
        let folded =
            binary(BinaryOperator::Mul, int(1 << 30, TypeKind::I32), int(4, TypeKind::I32));
        assert_eq!(folded, Some(int(0, TypeKind::I32)));
    }

    #[test]
    fn division_by_zero_is_left_to_trap() {
        assert_eq!(binary(BinaryOperator::Div, int(1, TypeKind::I32), int(0, TypeKind::I32)), None);
    }

    #[test]
    fn signed_division_overflow_is_left_to_trap() {
        let folded = binary(
            BinaryOperator::Div,
            int(i32::MIN as i64, TypeKind::I32),
            int(-1, TypeKind::I32),
        );
        assert_eq!(folded, None);
    }

    #[test]
    fn unsigned_comparison_does_not_use_the_sign_bit() {
        let max = int(u32::MAX as i64, TypeKind::U32);
        assert_eq!(binary(BinaryOperator::Gt, max, int(1, TypeKind::U32)), Some(Const::Bool(true)));

        let signed = int(-1, TypeKind::I32);
        assert_eq!(
            binary(BinaryOperator::Gt, signed, int(1, TypeKind::I32)),
            Some(Const::Bool(false))
        );
    }

    #[test]
    fn out_of_range_shift_is_refused() {
        assert_eq!(
            binary(BinaryOperator::Shl, int(1, TypeKind::I32), int(32, TypeKind::I32)),
            None
        );
        assert_eq!(
            binary(BinaryOperator::Shr, int(1, TypeKind::I32), int(-1, TypeKind::I32)),
            None
        );
        assert_eq!(
            binary(BinaryOperator::Shl, int(1, TypeKind::I32), int(31, TypeKind::I32)),
            Some(int(i32::MIN as i64, TypeKind::I32))
        );
    }

    #[test]
    fn unsigned_right_shift_does_not_replicate_the_sign_bit() {
        let folded = binary(
            BinaryOperator::Shr,
            int(u32::MAX as i64, TypeKind::U32),
            int(31, TypeKind::U32),
        );
        assert_eq!(folded, Some(int(1, TypeKind::U32)));
    }

    #[test]
    fn f32_arithmetic_rounds_at_f32() {
        let typ = Type::new(TypeKind::F32);
        let tenth = Const::Float(0.1f32 as f64, typ);
        let folded = binary(BinaryOperator::Add, tenth, tenth);
        assert_eq!(folded, Some(Const::Float((0.1f32 + 0.1f32) as f64, typ)));
    }

    #[test]
    fn nan_comparisons_follow_ieee() {
        let typ = Type::new(TypeKind::F64);
        let nan = Const::Float(f64::NAN, typ);
        assert_eq!(binary(BinaryOperator::Eq, nan, nan), Some(Const::Bool(false)));
        assert_eq!(binary(BinaryOperator::Ne, nan, nan), Some(Const::Bool(true)));
        assert_eq!(binary(BinaryOperator::Lt, nan, nan), Some(Const::Bool(false)));
        assert_eq!(binary(BinaryOperator::GtEq, nan, nan), Some(Const::Bool(false)));
    }

    #[test]
    fn float_division_by_zero_folds_to_infinity() {
        let typ = Type::new(TypeKind::F64);
        let folded = binary(BinaryOperator::Div, Const::Float(1.0, typ), Const::Float(0.0, typ));
        assert_eq!(folded, Some(Const::Float(f64::INFINITY, typ)));
    }

    #[test]
    fn narrowing_cast_truncates() {
        let folded = cast(int(300, TypeKind::I32), Type::new(TypeKind::U8));
        assert_eq!(folded, Some(int(44, TypeKind::U8)));
    }

    #[test]
    fn widening_cast_extends_with_the_source_sign() {
        let folded = cast(int(-1, TypeKind::I8), Type::new(TypeKind::I32));
        assert_eq!(folded, Some(int(-1, TypeKind::I32)));

        let folded = cast(int(-1, TypeKind::U8), Type::new(TypeKind::I32));
        assert_eq!(folded, Some(int(255, TypeKind::I32)));
    }

    #[test]
    fn float_to_int_cast_is_refused() {
        let value = Const::Float(1.5, Type::new(TypeKind::F64));
        assert_eq!(cast(value, Type::new(TypeKind::I32)), None);
    }

    #[test]
    fn negating_int_min_wraps_to_itself() {
        let folded = unary(UnaryOperator::Neg, int(i32::MIN as i64, TypeKind::I32));
        assert_eq!(folded, Some(int(i32::MIN as i64, TypeKind::I32)));
    }
}
