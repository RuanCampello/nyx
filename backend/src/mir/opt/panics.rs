//! The known-panics lint: operations a reachable path can only ever get wrong
//!
//! This is the diagnostic half of constant propagation, and it is deliberately not the
//! optimising half. It runs at *every* optimisation level, never rewrites anything, and
//! never changes what the backend emits, exactly the shape of rustc's
//! `arithmetic_overflow` and `unconditional_panic` lints, which fire identically under
//! `-C opt-level=0` and `-C opt-level=3` and are unaffected by `-C overflow-checks`.
//!
//! Only what the analysis can *prove* is reported. An overflow that depends on a runtime
//! value is left entirely alone, to panic or wrap at runtime according to the level, and
//! the fixture `tests/single/overflow.nyx` pins that unproven behaviour down

use crate::{
    diagnostic::{Label, RichDiagnostic},
    lints::Lint,
    mir::{
        Const, Instruction, InstructionKind, Mir, Operand,
        opt::{Cache, Program, fold, fold::Panic, propagate},
    },
    optimisation::Level,
    parser::expression::BinaryOperator,
};

pub(crate) fn known_panics(mir: &Mir) -> Vec<RichDiagnostic> {
    let cache = Cache::default();
    let program = Program::new(mir, &cache);
    let mut diagnostics = Vec::new();

    for index in 0..mir.functions.len() {
        propagate::walk(&program, index, Level::Debug, |instruction, state| {
            if let Some(diagnostic) = examine(instruction, state) {
                diagnostics.push(diagnostic);
            }
        });
    }

    diagnostics
}

fn examine<'hir>(
    instruction: &Instruction<'hir>,
    state: &[propagate::Lattice<'hir>],
) -> Option<RichDiagnostic> {
    let resolve = |operand: Operand<'hir>| match operand {
        Operand::Const(value) => Some(value),
        Operand::Place(place) => state[place.id.0 as usize].constant(),
    };

    let (panic, detail) = match &instruction.kind {
        InstructionKind::Binary { wrapping: true, .. } => return None,
        InstructionKind::Binary { operation, lhs, rhs, .. } => {
            let (lhs, rhs) = (resolve(*lhs)?, resolve(*rhs)?);
            (fold::diagnose(*operation, lhs, rhs)?, binary_detail(*operation, lhs, rhs))
        },
        InstructionKind::Unary { operation, rhs } => {
            let value = resolve(*rhs)?;
            let detail =
                format!("attempt to negate `{}`, which overflows `{}`", show(value), value.typ());

            (fold::diagnose_unary(*operation, value)?, detail)
        },

        _ => return None,
    };

    let lint = panic.lint();
    Some(RichDiagnostic {
        severity: lint.default_level().severity(),
        code: None,
        lint: Some(lint),
        message: panic.headline().to_string(),
        primary: Some(Label { span: instruction.span, message: detail }),
        secondary: Vec::new(),
        note: None,
        help: panic.help().map(str::to_string),
        rendered: None,
    })
}

fn binary_detail(operation: BinaryOperator, lhs: Const, rhs: Const) -> String {
    match (operation, rhs) {
        (BinaryOperator::Div, Const::Int(0, _)) => {
            format!("attempt to divide `{}` by zero", show(lhs))
        },
        _ => format!(
            "attempt to compute `{} {} {}`, which overflows `{}`",
            show(lhs),
            symbol(operation),
            show(rhs),
            lhs.typ()
        ),
    }
}

fn show(value: Const) -> String {
    match value {
        Const::Int(value, _) => value.to_string(),
        Const::Bool(value) => value.to_string(),
        Const::Float(value, _) => format!("{value:?}"),
        Const::Str { id, .. } => format!("<str:{id}>"),
        Const::Unit => "()".to_string(),
    }
}

const fn symbol(operation: BinaryOperator) -> &'static str {
    match operation {
        BinaryOperator::Add => "+",
        BinaryOperator::Sub => "-",
        BinaryOperator::Mul => "*",
        BinaryOperator::Div => "/",
        BinaryOperator::Shl => "<<",
        BinaryOperator::Shr => ">>",
        // no other operator can be diagnosed, so none can reach this
        _ => "?",
    }
}

impl Panic {
    const fn lint(self) -> Lint {
        match self {
            Self::Overflow | Self::ShiftOutOfRange => Lint::ArithmeticOverflow,
            Self::DivisionByZero | Self::DivisionOverflow => Lint::UnconditionalPanic,
        }
    }

    const fn headline<'s>(self) -> &'s str {
        match self {
            Self::Overflow | Self::ShiftOutOfRange => "this arithmetic operation will overflow",
            Self::DivisionByZero | Self::DivisionOverflow => "this operation will panic at runtime",
        }
    }

    const fn help<'s>(self) -> Option<&'s str> {
        match self {
            Self::Overflow => Some(
                r#"use `wrapping_add`, `wrapping_sub` or `wrapping_mul` from `std::int` if wrapping is intended"#,
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{hir, lints::Lint, mir, optimisation, parser::Parser};

    fn lints_of(source: &str, level: optimisation::Level) -> Vec<Lint> {
        optimisation::set(level);

        let arena = bumpalo::Bump::new();
        let statements = Parser::new(source).parse().expect("parse failed");
        let hir = hir::lower(statements, &arena).expect("lowering failed");
        let mir = mir::lower(hir).expect("mir lowering failed");
        let found = mir::known_panics(&mir).into_iter().filter_map(|d| d.lint).collect();

        optimisation::set(optimisation::Level::Debug);
        found
    }

    /// The whole point of a diagnostic pass separate from the optimiser: what the
    /// compiler reports must not change with how hard it was asked to optimise.
    #[test]
    fn findings_do_not_depend_on_the_optimisation_level() {
        let source = "fn main(): i32 { let x: i32 = 2147483647; let y = x + 1; y }";

        let debug = lints_of(source, optimisation::Level::Debug);
        let sane = lints_of(source, optimisation::Level::Sane);
        let max = lints_of(source, optimisation::Level::Max);

        assert_eq!(debug, vec![Lint::ArithmeticOverflow]);
        assert_eq!(debug, sane);
        assert_eq!(debug, max);
    }

    #[test]
    fn provable_division_by_zero_is_reported() {
        let source = "fn main(): i32 { let z: i32 = 0; let q = 10 / z; q }";
        assert_eq!(lints_of(source, optimisation::Level::Sane), vec![Lint::UnconditionalPanic]);
    }

    #[test]
    fn an_overflow_that_cannot_be_proven_is_left_alone() {
        let source = "fn add(a: i32, b: i32): i32 { a + b } fn main(): i32 { add(1, 2) }";
        assert!(lints_of(source, optimisation::Level::Sane).is_empty());
    }

    /// Reachability is what makes a *conditional* propagation safe to lint from: the
    /// overflow sits on a branch whose condition is known false.
    #[test]
    fn unreachable_code_is_not_reported() {
        let source = r#"
            fn main(): i32 {
                let x: i32 = 2147483647;
                if false { let y = x + 1; return y; }
                0
            }
        "#;
        assert!(lints_of(source, optimisation::Level::Sane).is_empty());
    }

    /// A literal at the minimum has no positive counterpart in its own type, so it must
    /// be folded to one constant rather than read as a negation that overflows. `i64` is
    /// the case that matters: there the magnitude wraps back into range, so nothing but
    /// the fold itself distinguishes it from a genuine `-INT_MIN`.
    #[test]
    fn a_negative_literal_at_the_minimum_is_not_reported() {
        for source in [
            "fn main(): i32 { let x: i32 = -2147483648; x }",
            "fn main(): i64 { let x: i64 = -9223372036854775808; x }",
            "fn main(): i32 { let x: i8 = -128; x as i32 }",
        ] {
            assert!(
                lints_of(source, optimisation::Level::Sane).is_empty(),
                "must not report a negative literal:\n{source}"
            );
        }
    }

    #[test]
    fn negating_the_minimum_is_reported() {
        let source = "fn main(): i32 { let x: i32 = -2147483648; let y = -x; y }";
        assert_eq!(lints_of(source, optimisation::Level::Sane), vec![Lint::ArithmeticOverflow]);
    }

    #[test]
    fn a_const_fn_body_is_reported_at_its_declaration() {
        let source = r#"
            const fn boom(): i32 { let x: i32 = 2147483647; x + 1 }
            fn main(): i32 { boom() }
        "#;
        assert_eq!(lints_of(source, optimisation::Level::Sane), vec![Lint::ArithmeticOverflow]);
    }
}
