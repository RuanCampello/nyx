use super::combine;
use crate::lir::{
    BlockId, Function, MachineType, Term, VReg,
    target::{
        CondCode, X86_64,
        x86_64::{Condition, X86Instr, X86Operand},
    },
};

const INT: MachineType = MachineType::Int { bytes: 4, signed: true };
const BYTE: MachineType = MachineType::Int { bytes: 1, signed: false };

/// `cmp` / `setcc` / `movzx` feeding a `Branch`, the shape every x86_64 comparison lowers to
fn comparison() -> (Function<X86_64>, BlockId, VReg) {
    let mut function = Function::<X86_64>::new("test".into());
    let entry = function.new_block();
    let then_block = function.new_block();
    let else_block = function.new_block();

    let lhs = function.new_vreg(INT);
    let rhs = function.new_vreg(INT);
    let flag = function.new_vreg(BYTE);
    let cond = function.new_vreg(INT);

    function.push_instr(&entry, X86Instr::Cmp { lhs, rhs: X86Operand::VReg(rhs), bytes: 4 });
    function.push_instr(&entry, X86Instr::Setcc { dest: flag, condition: Condition::L });
    function.push_instr(
        &entry,
        X86Instr::Movzx {
            dest: cond,
            src: X86Operand::VReg(flag),
            src_bytes: 1,
            dest_bytes: 4,
        },
    );
    function.set_term(&entry, Term::Branch { cond, then_block, else_block });

    (function, entry, cond)
}

fn block(function: &Function<X86_64>, id: BlockId) -> (&[X86Instr], &Term) {
    let block = &function.blocks[id.0 as usize];
    (&block.instructions, &block.term)
}

#[test]
fn a_comparison_feeding_a_branch_collapses_into_the_terminator() {
    let (mut function, entry, _) = comparison();
    combine(&mut function);

    let (instructions, term) = block(&function, entry);

    assert!(matches!(instructions, [X86Instr::Cmp { .. }]), "{instructions:?}");
    assert!(
        matches!(term, Term::BranchCc { cond: CondCode::Lt, .. }),
        "the branch should carry the `setcc` condition, got {term:?}"
    );
}

#[test]
fn a_boolean_wanted_elsewhere_is_left_alone() {
    let (mut function, entry, cond) = comparison();

    // a second consumer: the boolean is also returned, so it must stay materialised
    let sink = function.new_vreg(INT);
    function
        .push_instr(&entry, X86Instr::Mov { dest: sink, src: X86Operand::VReg(cond), bytes: 4 });

    combine(&mut function);

    let (instructions, term) = block(&function, entry);

    assert_eq!(instructions.len(), 4, "nothing should have been removed: {instructions:?}");
    assert!(matches!(term, Term::Branch { .. }), "{term:?}");
}

#[test]
fn a_clobbered_flag_blocks_the_rewrite() {
    let (mut function, entry, _) = comparison();

    let victim = function.new_vreg(INT);
    function.push_instr(
        &entry,
        X86Instr::Add {
            dest: victim,
            src: X86Operand::Imm(1),
            bytes: 4,
            checked: false,
        },
    );

    combine(&mut function);

    let (instructions, term) = block(&function, entry);

    assert_eq!(instructions.len(), 4, "{instructions:?}");
    assert!(matches!(term, Term::Branch { .. }), "{term:?}");
}

#[test]
fn a_flag_preserving_instruction_does_not_block_the_rewrite() {
    let (mut function, entry, _) = comparison();

    let spare = function.new_vreg(INT);
    function.push_instr(&entry, X86Instr::Mov { dest: spare, src: X86Operand::Imm(7), bytes: 4 });

    combine(&mut function);

    let (instructions, term) = block(&function, entry);

    assert!(
        matches!(instructions, [X86Instr::Cmp { .. }, X86Instr::Mov { .. }]),
        "{instructions:?}"
    );
    assert!(matches!(term, Term::BranchCc { cond: CondCode::Lt, .. }), "{term:?}");
}

#[test]
fn a_call_between_the_comparison_and_the_branch_blocks_the_rewrite() {
    let (mut function, entry, _) = comparison();

    function.push_instr(
        &entry,
        X86Instr::Call {
            target: "nyx.other".into(),
            moves: Vec::new(),
            uses: Vec::new(),
            ret: None,
            aggregate_ret: Vec::new(),
            stack_args: Vec::new(),
        },
    );

    combine(&mut function);

    let (_, term) = block(&function, entry);
    assert!(matches!(term, Term::Branch { .. }), "{term:?}");
}

#[test]
fn a_branch_on_a_plain_boolean_is_untouched() {
    let mut function = Function::<X86_64>::new("test".into());
    let entry = function.new_block();
    let then_block = function.new_block();
    let else_block = function.new_block();

    let cond = function.new_vreg(INT);
    function.push_instr(&entry, X86Instr::Mov { dest: cond, src: X86Operand::Imm(1), bytes: 4 });
    function.set_term(&entry, Term::Branch { cond, then_block, else_block });

    combine(&mut function);

    let (instructions, term) = block(&function, entry);

    assert_eq!(instructions.len(), 1, "{instructions:?}");
    assert!(matches!(term, Term::Branch { .. }), "{term:?}");
}
