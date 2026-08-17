//! Compile-time evaluation of `const fn` calls

use crate::{
    hir::FunctionId,
    mir::{
        Const, Function, InstructionKind, Operand, Terminator,
        opt::{Key, Program, fold},
    },
    optimisation::Level,
};

struct Interpreter<'p, 'a, 'hir> {
    program: &'p Program<'a, 'hir>,
    fuel: u32,
    depth: u32,
}

/// Instructions one compile-time evaluation may execute, shared across the whole call
/// tree. Anchored on MSVC's `constexpr` default
const STEP_BUDGET: u32 = 100_000;

/// Recursion is bounded separately because a tight infinite recursion would exhaust the
/// host stack long before the step budget
const DEPTH_BUDGET: u32 = 128;

pub(super) fn call<'hir>(
    program: &Program<'_, 'hir>,
    callee: FunctionId,
    args: &[Const<'hir>],
    level: Level,
) -> Option<Const<'hir>> {
    let function = program.function(callee)?;
    if !function.is_const {
        return None;
    }

    assert!(level >= Level::Sane, "compile-time evaluation must not run at `debug`");

    let key = Key::new(callee, args);
    if let Some(cached) = program.cached(&key) {
        return cached;
    }

    let mut itrp = Interpreter { program, fuel: STEP_BUDGET, depth: DEPTH_BUDGET };
    let result = itrp.run(function, args);
    program.memoise(key, result);

    result
}

impl<'hir> Interpreter<'_, '_, 'hir> {
    fn run(&mut self, function: &Function<'hir>, args: &[Const<'hir>]) -> Option<Const<'hir>> {
        // an intrinsic is lowered by the backend, so there is no body to execute
        if function.intrinsic.is_some() || function.blocks.is_empty() {
            return None;
        }

        if function.params.len() != args.len() {
            return None;
        }

        let mut env: Vec<Option<Const<'hir>>> = vec![None; function.locals.len()];
        for ((id, _), &value) in function.params.iter().zip(args) {
            env[id.0 as usize] = Some(value);
        }

        let mut block = 0usize;
        loop {
            let current = function.blocks.get(block)?;

            for instruction in &current.instructions {
                self.fuel = self.fuel.checked_sub(1)?;

                let value = self.evaluate(&instruction.kind, &env)?;
                env[instruction.dest.id.0 as usize] = Some(value);
            }

            match &current.terminator {
                Terminator::Jump(target) => block = target.0 as usize,
                Terminator::Branch { condition, then_block, else_block } => {
                    let taken = match self.operand(*condition, &env)? {
                        Const::Bool(true) => then_block,
                        Const::Bool(false) => else_block,
                        _ => return None,
                    };
                    block = taken.0 as usize;
                },
                Terminator::Return(value) => return self.operand((*value)?, &env),
            }
        }
    }

    fn evaluate(
        &mut self,
        kind: &InstructionKind<'hir>,
        env: &[Option<Const<'hir>>],
    ) -> Option<Const<'hir>> {
        match kind {
            InstructionKind::Assign(operand) => self.operand(*operand, env),
            InstructionKind::Unary { operation, rhs } => {
                fold::unary(*operation, self.operand(*rhs, env)?)
            },
            InstructionKind::Binary { operation, lhs, rhs, .. } => {
                fold::binary(*operation, self.operand(*lhs, env)?, self.operand(*rhs, env)?)
            },
            InstructionKind::Cast { src, typ } => fold::cast(self.operand(*src, env)?, *typ),
            InstructionKind::Call { callee, args } => {
                let callee = self.program.function(*callee)?;
                if !callee.is_const {
                    return None;
                }

                let values =
                    args.iter().map(|&arg| self.operand(arg, env)).collect::<Option<Vec<_>>>()?;

                let key = Key::new(callee.id, &values);
                if let Some(cached) = self.program.cached(&key) {
                    return cached;
                }

                self.depth = self.depth.checked_sub(1)?;
                let result = self.run(callee, &values);
                self.depth += 1;

                if result.is_some() {
                    self.program.memoise(key, result);
                }

                result
            },

            _ => None,
        }
    }

    fn operand(&self, operand: Operand<'hir>, env: &[Option<Const<'hir>>]) -> Option<Const<'hir>> {
        match operand {
            Operand::Const(value) => Some(value),
            Operand::Place(place) => env[place.id.0 as usize],
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{TargetArch, optimisation};

    fn assembly(source: &str, level: optimisation::Level) -> String {
        optimisation::set(level);
        let asm = crate::compile_for(source, TargetArch::X86_64).expect("compilation failed");
        optimisation::set(optimisation::Level::Debug);
        asm
    }

    fn body_of<'a>(assembly: &'a str, symbol: &str) -> &'a str {
        let start = assembly
            .find(&format!("\n{symbol}:\n"))
            .unwrap_or_else(|| panic!("{symbol} was not emitted:\n{assembly}"));
        let rest = &assembly[start + 1..];

        match rest.find(".globl") {
            Some(end) => &rest[..end],
            None => rest,
        }
    }

    #[test]
    fn const_fn_call_is_evaluated_at_compile_time() {
        let source = r#"
            const fn square(n: i32): i32 { n * n }
            fn main(): i32 { square(7) }
        "#;

        let optimised = assembly(source, optimisation::Level::Sane);
        let main = body_of(&optimised, "nyx.main");

        assert!(main.contains("$49"), "expected the call to fold to 49:\n{main}");
        assert!(!main.contains("imul"), "expected no multiply to survive:\n{main}");
        assert!(!main.contains("call"), "expected the call itself to be gone:\n{main}");
    }

    #[test]
    fn const_fn_loop_is_evaluated_at_compile_time() {
        let source = r#"
            const fn sum_to(n: i32): i32 {
                let mut total = 0;
                loop i in 1..=n {
                    total = total + i;
                }
                total
            }
            fn main(): i32 { sum_to(10) }
        "#;

        let optimised = assembly(source, optimisation::Level::Sane);
        let main = body_of(&optimised, "nyx.main");

        assert!(main.contains("$55"), "expected the loop to fold to 55:\n{main}");
        assert!(!main.contains("call"), "expected no call to survive:\n{main}");
    }

    #[test]
    fn recursive_const_fn_is_evaluated() {
        let source = r#"
            const fn fib(n: i32): i32 {
                if n < 2 { return n; }
                fib(n - 1) + fib(n - 2)
            }
            fn main(): i32 { fib(10) }
        "#;

        let optimised = assembly(source, optimisation::Level::Sane);
        let main = body_of(&optimised, "nyx.main");

        assert!(main.contains("$55"), "expected fib(10) to fold to 55:\n{main}");
    }

    #[test]
    fn exponentially_recursive_const_fn_still_folds() {
        let source = r#"
            const fn fib(n: i32): i32 {
                if n < 2 { return n; }
                fib(n - 1) + fib(n - 2)
            }
            fn main(): i32 { fib(30) }
        "#;

        let optimised = assembly(source, optimisation::Level::Sane);
        let main = body_of(&optimised, "nyx.main");

        assert!(main.contains("$832040"), "expected fib(30) to fold to 832040:\n{main}");
    }

    #[test]
    fn non_const_fn_is_not_evaluated() {
        let source = r#"
            fn square(n: i32): i32 { n * n }
            fn main(): i32 { square(7) }
        "#;

        let optimised = assembly(source, optimisation::Level::Sane);
        let main = body_of(&optimised, "nyx.main");

        assert!(main.contains("call    nyx.square"), "a plain fn must still be called:\n{main}");
    }

    #[test]
    fn debug_level_evaluates_nothing() {
        let source = r#"
            const fn square(n: i32): i32 { n * n }
            fn main(): i32 { square(7) }
        "#;

        let naive = assembly(source, optimisation::Level::Debug);
        let main = body_of(&naive, "nyx.main");

        assert!(main.contains("call    nyx.square"), "debug must keep the call:\n{main}");
    }
}
