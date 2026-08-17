use crate::analysis::{Snapshot, hover::layout_of};
use frontend::{
    hir::{self, Constant, Literal},
    parser,
};

// TODO: those things should be better integrated with the compiler
// in the future instead of ad-hoc resolution here
pub(super) fn const_value(constant: &Constant<'_>, hir: &Snapshot<'_>) -> Option<String> {
    use hir::ExpressionKind as Kind;
    use parser::expression::UnaryOperator;

    match &constant.value.kind {
        Kind::Literal(Literal::Float(value)) => Some(value.to_string()),
        Kind::Literal(Literal::Bool(value)) => Some(value.to_string()),
        Kind::Literal(Literal::Char(value)) => Some(format!("'{value}'")),
        Kind::Literal(Literal::Str(symbol)) => Some(format!("\"{}\"", hir.symbols.get(*symbol))),
        Kind::Unary { operator: UnaryOperator::Neg, expr }
            if matches!(expr.kind, Kind::Literal(Literal::Float(_))) =>
        {
            let Kind::Literal(Literal::Float(value)) = expr.kind else {
                return None;
            };

            Some((-value).to_string())
        },
        _ => Some(render_int(eval_const_int(constant.value, hir)?, constant.typ)),
    }
}

fn eval_const_int(expr: &hir::Expression<'_>, hir: &Snapshot<'_>) -> Option<i128> {
    use hir::ExpressionKind as Kind;
    use parser::expression::{TypeIntrinsicKind::*, UnaryOperator};

    match &expr.kind {
        Kind::Literal(Literal::Int(value)) => Some(*value as i128),
        Kind::Unary { operator: UnaryOperator::Neg, expr } => {
            eval_const_int(expr, hir).map(i128::wrapping_neg)
        },
        Kind::Unary { operator: UnaryOperator::Not, expr } => {
            eval_const_int(expr, hir).map(|value| !value)
        },
        Kind::Cast { from, .. } => eval_const_int(from, hir),
        Kind::TypeIntrinsic { kind, typ } => {
            let (size, align) = layout_of(hir, *typ)?;

            Some(match kind {
                SizeOf => size as i128,
                AlignOf => align as i128,
            })
        },
        Kind::Binary { operator, left, right } => {
            use parser::expression::BinaryOperator::*;

            let left = eval_const_int(left, hir)?;
            let right = eval_const_int(right, hir)?;
            match operator {
                Add => left.checked_add(right),
                Sub => left.checked_sub(right),
                Mul => left.checked_mul(right),
                Div => left.checked_div(right),
                Shl => left.checked_shl(u32::try_from(right).ok()?),
                Shr => left.checked_shr(u32::try_from(right).ok()?),
                BitAnd => Some(left & right),
                BitOr => Some(left | right),
                BitXor => Some(left ^ right),
                _ => None,
            }
        },
        _ => None,
    }
}

fn render_int(value: i128, typ: hir::Type<'_>) -> String {
    use hir::TypeKind::*;

    let bits = match typ.kind() {
        I8 | U8 => 8,
        I16 | U16 => 16,
        I32 | U32 | Char => 32,
        _ => 64,
    };
    let unsigned = matches!(typ.kind(), U8 | U16 | U32 | U64 | Uptr);
    let truncated = (value as u128) & (u128::MAX >> (128 - bits));

    match unsigned {
        true => truncated.to_string(),
        _ => {
            let signed = ((truncated << (128 - bits)) as i128) >> (128 - bits);
            match signed < 0 {
                true => format!("{signed} (0x{truncated:X})"),
                _ => signed.to_string(),
            }
        },
    }
}
