use super::{DefUse, definition, flags_reach_terminator};
use crate::lir::{Block, Term, target::Instruction, target::Target};

/// Fold the boolean a comparison materialises straight into the branch that consumes it
///
/// Both backends compare into the flags, spill the answer into a register, and
/// then test that register to branch on it:
///
/// ```text
/// x86_64    cmp / setcc / movzx  ->  test / jne / jmp     becomes  cmp / jcc / jmp
/// AArch64   cmp / cset           ->  cbnz / b             becomes  cmp / b.cc / b
/// ```
pub(super) fn compare_branch<T: Target>(
    block: &mut Block<T::Instruction>,
    counts: &DefUse,
) -> bool {
    let Term::Branch { cond, then_block, else_block } = block.term else {
        return false;
    };

    if !counts.is_single_use(cond) {
        return false;
    }

    let Some(boolean) = definition::<T>(block, cond) else {
        return false;
    };

    let flag = match block.instructions[boolean].zero_extension() {
        Some((_, src)) => match counts.is_single_use(src) {
            true => definition::<T>(block, src),
            false => return false,
        },
        None => Some(boolean),
    };

    let Some(flag) = flag else {
        return false;
    };
    let Some((_, code)) = block.instructions[flag].flag_to_bool() else {
        return false;
    };

    if !flags_reach_terminator::<T>(block, flag) {
        return false;
    }

    assert!(flag <= boolean, "a boolean cannot be widened before the flags are read");

    block.instructions.remove(boolean);
    if boolean != flag {
        block.instructions.remove(flag);
    }
    block.term = Term::BranchCc { cond: code, then_block, else_block };

    true
}
