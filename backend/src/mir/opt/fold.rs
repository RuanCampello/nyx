//! Constant folding of individual MIR operations
//!
//! Every entry point is total and side-effect free: it returns `None` whenever the
//! operation cannot be folded *without changing observable behaviour*

use crate::{
    hir::{Type, TypeKind},
    mir::Const,
    parser::expression::{BinaryOperator as Binary, UnaryOperator as Unary},
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

pub(super) fn diagnose<'hir>(
    operation: Binary,
    lhs: Const<'hir>,
    rhs: Const<'hir>,
) -> Option<Panic> {
    let (Const::Int(a, typ), Const::Int(b, _)) = (lhs, rhs) else {
        return None;
    };

    let repr = IntRepr::of(typ)?;
    let (a, b) = (repr.normalise(a), repr.normalise(b));
    let overflows = |exact: i128| (!repr.holds(exact)).then_some(Panic::Overflow);

    match operation {
        Binary::Add => overflows(repr.widen(a) + repr.widen(b)),
        Binary::Sub => overflows(repr.widen(a) - repr.widen(b)),
        Binary::Mul => overflows(repr.widen(a) * repr.widen(b)),

        Binary::Div | Binary::Rem if b == 0 => Some(Panic::DivisionByZero),
        Binary::Div | Binary::Rem if repr.is_div_overflow(a, b) => Some(Panic::DivisionOverflow),

        Binary::Shl | Binary::Shr if !repr.is_shift_in_range(b) => Some(Panic::ShiftOutOfRange),

        _ => None,
    }
}

pub(super) fn diagnose_unary<'hir>(operation: Unary, operand: Const<'hir>) -> Option<Panic> {
    let (Unary::Neg, Const::Int(value, typ)) = (operation, operand) else {
        return None;
    };

    let repr = IntRepr::of(typ)?;
    match repr.signed && repr.holds(repr.widen(value)) {
        true => (!repr.holds(-repr.widen(value))).then_some(Panic::Overflow),
        false => None,
    }
}

pub(super) fn binary<'hir>(
    operation: Binary,
    lhs: Const<'hir>,
    rhs: Const<'hir>,
) -> Option<Const<'hir>> {
    match (lhs, rhs) {
        (Const::Int(a, typ), Const::Int(b, _)) => integer(operation, a, b, typ),
        (Const::Float(a, typ), Const::Float(b, _)) => float(operation, a, b, typ),
        (Const::Bool(a), Const::Bool(b)) => boolean(operation, a, b),
        _ => None,
    }
}

pub(super) fn unary<'hir>(operation: Unary, operand: Const<'hir>) -> Option<Const<'hir>> {
    match (operation, operand) {
        (Unary::Neg, Const::Int(value, typ)) => {
            let repr = IntRepr::of(typ)?;
            Some(Const::Int(repr.normalise(value.wrapping_neg()), typ))
        },
        (Unary::Neg, Const::Float(value, typ)) => Some(Const::Float(-value, typ)),
        (Unary::Not, Const::Bool(value)) => Some(Const::Bool(!value)),
        (Unary::Not, Const::Int(value, typ)) => {
            let repr = IntRepr::of(typ)?;
            Some(Const::Int(repr.normalise(!value), typ))
        },
        _ => None,
    }
}

/// fold a `Cast` between primitive types
pub(super) fn cast<'hir>(value: Const<'hir>, target: Type<'hir>) -> Option<Const<'hir>> {
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

fn integer<'hir>(operation: Binary, a: i64, b: i64, typ: Type<'hir>) -> Option<Const<'hir>> {
    let repr = IntRepr::of(typ)?;
    let (a, b) = (repr.normalise(a), repr.normalise(b));

    let arithmetic = |value: i64| Some(Const::Int(repr.normalise(value), typ));
    let compare = |ordering: Ordering| repr.compare(a, b) == ordering;

    match operation {
        Binary::Add => arithmetic(a.wrapping_add(b)),
        Binary::Sub => arithmetic(a.wrapping_sub(b)),
        Binary::Mul => arithmetic(a.wrapping_mul(b)),

        // both faults are raised by the hardware, folding would erase them
        Binary::Div => match (b, repr.is_div_overflow(a, b)) {
            (0, _) | (_, true) => None,
            _ => match repr.signed {
                true => arithmetic(a.wrapping_div(b)),
                false => arithmetic(((a as u64).wrapping_div(b as u64)) as i64),
            },
        },

        // 'INT_MIN % -1' is zero in exact arithmetic, but 'idiv' still raises #DE
        // on it, so folding would erase a fault the program would have taken
        Binary::Rem => match (b, repr.is_div_overflow(a, b)) {
            (0, _) | (_, true) => None,
            _ => match repr.signed {
                true => arithmetic(a.wrapping_rem(b)),
                false => arithmetic(((a as u64).wrapping_rem(b as u64)) as i64),
            },
        },

        Binary::BitAnd => arithmetic(a & b),
        Binary::BitOr => arithmetic(a | b),
        Binary::BitXor => arithmetic(a ^ b),

        // the two targets mask out-of-range counts differently for sub-word types
        Binary::Shl | Binary::Shr if !repr.is_shift_in_range(b) => None,
        Binary::Shl => arithmetic(a.wrapping_shl(b as u32)),
        Binary::Shr => match repr.signed {
            true => arithmetic(a.wrapping_shr(b as u32)),
            _ => arithmetic(((a as u64).wrapping_shr(b as u32)) as i64),
        },

        Binary::Eq => Some(Const::Bool(a == b)),
        Binary::Ne => Some(Const::Bool(a != b)),
        Binary::Lt => Some(Const::Bool(compare(Ordering::Less))),
        Binary::Gt => Some(Const::Bool(compare(Ordering::Greater))),
        Binary::LtEq => Some(Const::Bool(!compare(Ordering::Greater))),
        Binary::GtEq => Some(Const::Bool(!compare(Ordering::Less))),

        Binary::And | Binary::Or => None,
    }
}

fn float<'hir>(operation: Binary, a: f64, b: f64, typ: Type<'hir>) -> Option<Const<'hir>> {
    let single = matches!(typ.kind(), TypeKind::F32);
    let arithmetic = |wide: fn(f64, f64) -> f64, narrow: fn(f32, f32) -> f32| {
        let value = match single {
            true => narrow(a as _, b as _) as _,
            false => wide(a, b),
        };
        Some(Const::Float(value, typ))
    };

    match operation {
        Binary::Add => arithmetic(|a, b| a + b, |a, b| a + b),
        Binary::Sub => arithmetic(|a, b| a - b, |a, b| a - b),
        Binary::Mul => arithmetic(|a, b| a * b, |a, b| a * b),
        Binary::Div => arithmetic(|a, b| a / b, |a, b| a / b),
        Binary::Rem => arithmetic(|a, b| a % b, |a, b| a % b),

        Binary::Eq => Some(Const::Bool(a == b)),
        Binary::Ne => Some(Const::Bool(a != b)),
        Binary::Lt => Some(Const::Bool(a < b)),
        Binary::Gt => Some(Const::Bool(a > b)),
        Binary::LtEq => Some(Const::Bool(a <= b)),
        Binary::GtEq => Some(Const::Bool(a >= b)),

        _ => None,
    }
}

#[inline]
const fn boolean<'hir>(operation: Binary, a: bool, b: bool) -> Option<Const<'hir>> {
    match operation {
        Binary::And => Some(Const::Bool(a && b)),
        Binary::Or => Some(Const::Bool(a || b)),
        Binary::Eq => Some(Const::Bool(a == b)),
        Binary::Ne => Some(Const::Bool(a != b)),
        Binary::BitAnd => Some(Const::Bool(a & b)),
        Binary::BitOr => Some(Const::Bool(a | b)),
        Binary::BitXor => Some(Const::Bool(a ^ b)),
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
            // the representation belongs to the enum definition, which this
            // context-free folder does not carry
            TypeKind::Adt(_, _) => return None,
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
    const fn is_div_overflow(self, a: i64, b: i64) -> bool {
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
    use crate::parser::expression::{BinaryOperator, UnaryOperator};
    use rstest::rstest;

    fn int(value: i64, kind: TypeKind) -> Const {
        Const::Int(value, Type::from(kind))
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

    /// both faults are raised by the hardware, so folding either away would erase a trap the program was going to take
    #[rstest]
    #[case::divide_by_zero(BinaryOperator::Div, 1, 0, TypeKind::I32)]
    #[case::remainder_by_zero(BinaryOperator::Rem, 1, 0, TypeKind::I32)]
    #[case::unsigned_divide_by_zero(BinaryOperator::Div, 1, 0, TypeKind::U64)]
    #[case::unsigned_remainder_by_zero(BinaryOperator::Rem, 1, 0, TypeKind::U64)]
    #[case::divide_min_by_minus_one(BinaryOperator::Div, i32::MIN as i64, -1, TypeKind::I32)]
    #[case::remainder_min_by_minus_one(BinaryOperator::Rem, i32::MIN as i64, -1, TypeKind::I32)]
    #[case::narrow_min_by_minus_one(BinaryOperator::Rem, i8::MIN as i64, -1, TypeKind::I8)]
    fn faulting_division_is_left_to_trap(
        #[case] operation: BinaryOperator,
        #[case] a: i64,
        #[case] b: i64,
        #[case] kind: TypeKind,
    ) {
        assert_eq!(binary(operation, int(a, kind), int(b, kind)), None);
        assert!(diagnose(operation, int(a, kind), int(b, kind)).is_some());
    }

    /// the sign of a truncated remainder follows the dividend, never the divisor
    #[rstest]
    #[case::positive_by_positive(7, 3, 1)]
    #[case::negative_by_positive(-7, 3, -1)]
    #[case::positive_by_negative(7, -3, 1)]
    #[case::negative_by_negative(-7, -3, -1)]
    #[case::exact(9, 3, 0)]
    #[case::divisor_larger_than_dividend(3, 9, 3)]
    #[case::negative_divisor_larger_than_dividend(-3, 9, -3)]
    #[case::by_one(7, 1, 0)]
    #[case::by_minus_one(7, -1, 0)]
    #[case::min_by_two(i32::MIN as i64, 2, 0)]
    #[case::min_by_three(i32::MIN as i64, 3, -2)]
    #[case::max_by_two(i32::MAX as i64, 2, 1)]
    fn signed_remainder_takes_the_sign_of_the_dividend(
        #[case] a: i64,
        #[case] b: i64,
        #[case] expected: i64,
    ) {
        let folded = binary(BinaryOperator::Rem, int(a, TypeKind::I32), int(b, TypeKind::I32));
        assert_eq!(folded, Some(int(expected, TypeKind::I32)));
    }

    /// an unsigned remainder must not read the top bit as a sign
    #[rstest]
    #[case::max_by_ten(u32::MAX as i64, 10, 5, TypeKind::U32)]
    #[case::above_the_signed_maximum(0x8000_0000, 3, 2, TypeKind::U32)]
    #[case::u8_wraps_nothing(255, 16, 15, TypeKind::U8)]
    #[case::u64_max(u64::MAX as i64, 10, 5, TypeKind::U64)]
    fn unsigned_remainder_ignores_the_sign_bit(
        #[case] a: i64,
        #[case] b: i64,
        #[case] expected: i64,
        #[case] kind: TypeKind,
    ) {
        assert_eq!(
            binary(BinaryOperator::Rem, int(a, kind), int(b, kind)),
            Some(int(expected, kind))
        );
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
        let typ = Type::from(TypeKind::F32);
        let tenth = Const::Float(0.1f32 as f64, typ);
        let folded = binary(BinaryOperator::Add, tenth, tenth);
        assert_eq!(folded, Some(Const::Float((0.1f32 + 0.1f32) as f64, typ)));
    }

    #[test]
    fn nan_comparisons_follow_ieee() {
        let typ = Type::from(TypeKind::F64);
        let nan = Const::Float(f64::NAN, typ);
        assert_eq!(binary(BinaryOperator::Eq, nan, nan), Some(Const::Bool(false)));
        assert_eq!(binary(BinaryOperator::Ne, nan, nan), Some(Const::Bool(true)));
        assert_eq!(binary(BinaryOperator::Lt, nan, nan), Some(Const::Bool(false)));
        assert_eq!(binary(BinaryOperator::GtEq, nan, nan), Some(Const::Bool(false)));
    }

    #[test]
    fn float_division_by_zero_folds_to_infinity() {
        let typ = Type::from(TypeKind::F64);
        let folded = binary(BinaryOperator::Div, Const::Float(1.0, typ), Const::Float(0.0, typ));
        assert_eq!(folded, Some(Const::Float(f64::INFINITY, typ)));
    }

    #[rstest]
    #[case::positive(7.5, 2.0, 1.5)]
    #[case::negative_dividend(-7.5, 2.0, -1.5)]
    #[case::negative_divisor(7.5, -2.0, 1.5)]
    #[case::exact(8.0, 2.0, 0.0)]
    #[case::divisor_larger_than_dividend(1.5, 4.0, 1.5)]
    fn float_remainder_takes_the_sign_of_the_dividend(
        #[case] a: f64,
        #[case] b: f64,
        #[case] expected: f64,
    ) {
        let typ = Type::from(TypeKind::F64);
        let folded = binary(BinaryOperator::Rem, Const::Float(a, typ), Const::Float(b, typ));
        assert_eq!(folded, Some(Const::Float(expected, typ)));
    }

    #[test]
    fn float_remainder_by_zero_folds_to_nan() {
        let typ = Type::from(TypeKind::F64);
        let folded = binary(BinaryOperator::Rem, Const::Float(1.0, typ), Const::Float(0.0, typ));
        let Some(Const::Float(value, _)) = folded else {
            panic!("expected a folded float, got {folded:?}");
        };

        assert!(value.is_nan());
    }

    #[test]
    fn narrowing_cast_truncates() {
        let folded = cast(int(300, TypeKind::I32), Type::from(TypeKind::U8));
        assert_eq!(folded, Some(int(44, TypeKind::U8)));
    }

    #[test]
    fn widening_cast_extends_with_the_source_sign() {
        let folded = cast(int(-1, TypeKind::I8), Type::from(TypeKind::I32));
        assert_eq!(folded, Some(int(-1, TypeKind::I32)));

        let folded = cast(int(-1, TypeKind::U8), Type::from(TypeKind::I32));
        assert_eq!(folded, Some(int(255, TypeKind::I32)));
    }

    #[test]
    fn float_to_int_cast_is_refused() {
        let value = Const::Float(1.5, Type::from(TypeKind::F64));
        assert_eq!(cast(value, Type::from(TypeKind::I32)), None);
    }

    #[test]
    fn negating_int_min_wraps_to_itself() {
        let folded = unary(UnaryOperator::Neg, int(i32::MIN as i64, TypeKind::I32));
        assert_eq!(folded, Some(int(i32::MIN as i64, TypeKind::I32)));
    }
}
