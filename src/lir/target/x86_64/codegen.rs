//! LIR + Allocation → GAS (AT&T syntax) assembly emission.
//!
//! After register allocation every VReg has a concrete Location (register or
//! stack slot).
//!
//! Emission is meant to be very mechanical: look up, add mnemonic.

use crate::{
    emit, label,
    lir::{
        self, Function, MachineType, Term, VReg,
        regalloc::{Allocation, Location},
        target::{
            Emittable, PhysicalReg, RegClass, Target,
            x86_64::{X86_64, X86Instr, X86Operand, X86Reg},
        },
    },
};
use std::{borrow::Cow, fmt::Write};

impl Emittable<X86_64> for Function<X86_64> {
    fn emit(&self, alloc: Allocation<X86_64>, out: &mut String) {
        let name = &self.name;
        let frame_size = alloc.frame_size;
        let epilogue = format!(".L_{name}_epilogue");

        Self::emit_prologue(&alloc, name, frame_size, out);
        self.emit_body(&alloc, name, &epilogue, out);
        Self::emit_epilogue(&alloc, &epilogue, frame_size, out);
        self.emit_rodata(out);
    }

    #[inline(always)]
    fn start(out: &mut String) {
        label!(out, ".globl _start");
        label!(out, "_start:");

        emit!(out, "call    nyx_main");
        emit!(out, "movl    %eax, %edi"); // exit code = return value
        emit!(out, "movl    $60, %eax"); // syscall: exit
        emit!(out, "syscall");
    }
}

impl Function<X86_64> {
    fn emit_prologue(alloc: &Allocation<X86_64>, name: &str, frame_size: u32, out: &mut String) {
        label!(out, ".globl {name}");
        label!(out, "{name}:");
        emit!(out, "push    %rbp");
        emit!(out, "mov     %rsp, %rbp");

        for reg in &alloc.used_callee_saved {
            emit!(out, "push    %{}", reg.name(8));
        }

        if frame_size > 0 {
            emit!(out, "sub     ${frame_size}, %rsp");
        }
    }

    fn emit_epilogue(alloc: &Allocation<X86_64>, label: &str, frame_size: u32, out: &mut String) {
        label!(out, "{label}:");

        if frame_size > 0 {
            emit!(out, "add     ${frame_size}, %rsp");
        }

        for reg in alloc.used_callee_saved.iter().rev() {
            emit!(out, "pop     %{}", reg.name(8));
        }

        emit!(out, "pop     %rbp");
        emit!(out, "ret");
    }

    fn emit_body(&self, alloc: &Allocation<X86_64>, name: &str, epilogue: &str, out: &mut String) {
        let n = self.blocks.len();

        for (idx, block) in self.blocks.iter().enumerate() {
            if idx > 0 {
                label!(out, ".L_block_{name}_{idx}:");
            }

            for instruction in &block.instructions {
                self.emit_instruction(instruction, alloc, out);
            }

            self.emit_terminator(alloc, &block.term, name, epilogue, idx == n - 1, out);
        }
    }

    fn emit_rodata(&self, out: &mut String) {
        if self.floats.is_empty() {
            return;
        }

        label!(out, ".section .rodata");
        for (bits, label) in &self.floats {
            let is_32 = label.contains("_f32_");
            let align = if is_32 { 4 } else { 8 };

            label!(out, ".align {align}");
            label!(out, "{label}:");

            match is_32 {
                true => label!(out, "    .long {}", *bits as u32),
                _ => label!(out, "    .quad {bits}"),
            }
        }

        label!(out, ".text");
    }
}

impl Function<X86_64> {
    fn emit_instruction(&self, instruction: &X86Instr, alloc: &Allocation<X86_64>, out: &mut String) {
        use lir::target::x86_64::X86Instr as Inst;

        match instruction {
            Inst::ParamMov { dest, src_reg, bytes } => {
                let is_float = self.is_float(dest);
                let suffix = typed_suffix(bytes, is_float);
                let src = format!("%{}", src_reg.name(*bytes));
                let dest = alloc.location(&dest, bytes);

                mov_or_scratch(out, &src, &dest, suffix, is_float);
            }

            Inst::Mov { dest, src, bytes } => {
                let suffix = suffix(bytes);
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes);

                mov_or_scratch(out, &src, &dest, suffix, false);
            }

            Inst::MovFloat { dest, src, bytes } => {
                let suffix = float_suffix(bytes);
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes);

                mov_or_scratch(out, &src, &dest, suffix, true);
            }

            Inst::Lea { dest, src } => {
                let dest = alloc.location(dest, &8);
                let src = self.operand(alloc, src, &8);

                emit!(out, "leaq   {src}, {dest}");
            }

            Inst::Movzx { dest, src } => {
                let dest = alloc.location(dest, &4);
                let src = alloc.location(src, &1);

                emit!(out, "movzbl   {src}, {dest}");
            }

            Inst::Add { dest, src, bytes }
            | Inst::Sub { dest, src, bytes }
            | Inst::Imul { dest, src, bytes }
            | Inst::AddFloat { dest, src, bytes }
            | Inst::SubFloat { dest, src, bytes }
            | Inst::MulFloat { dest, src, bytes }
            | Inst::DivFloat { dest, src, bytes }
            | Inst::And { dest, src, bytes }
            | Inst::Or { dest, src, bytes }
            | Inst::Xor { dest, src, bytes } => {
                let suffix = typed_suffix(bytes, self.is_float(dest));
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes);

                match instruction {
                    Inst::Add { .. } | Inst::AddFloat { .. } => emit!(out, "add{suffix}    {src}, {dest}"),
                    Inst::Sub { .. } | Inst::SubFloat { .. } => emit!(out, "sub{suffix}    {src}, {dest}"),
                    Inst::Imul { .. } => emit!(out, "imul{suffix}    {src}, {dest}"),
                    Inst::MulFloat { .. } => emit!(out, "mul{suffix}    {src}, {dest}"),
                    Inst::DivFloat { .. } => emit!(out, "div{suffix}    {src}, {dest}"),
                    Inst::And { .. } => emit!(out, "and{suffix}    {src}, {dest}"),
                    Inst::Or { .. } => emit!(out, "or{suffix}    {src}, {dest}"),
                    Inst::Xor { .. } => emit!(out, "xor{suffix}    {src}, {dest}"),

                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            Inst::Neg { dest, bytes } => {
                let suffix = suffix(bytes);
                let dest = alloc.location(dest, bytes);

                emit!(out, "neg{suffix}    {dest}");
            }

            Inst::IDiv {
                result,
                dividend,
                divisor,
                bytes,
                ..
            } => {
                let suffix = suffix(bytes);
                let (rax, extend) = match bytes {
                    8 => ("%rax", "cqto"),
                    _ => ("%eax", "cltd"),
                };
                let dividend = alloc.location(dividend, bytes);
                let result = alloc.location(result, bytes);

                if dividend != rax {
                    emit!(out, "mov{suffix}    {dividend}, {rax}");
                }
                emit!(out, "{extend}");

                match divisor {
                    X86Operand::Imm(_) => {
                        let div = self.operand(alloc, divisor, bytes);
                        emit!(out, "subq    $8, %rsp");
                        emit!(out, "mov{suffix}    {div}, (%rsp)");
                        emit!(out, "idiv{suffix}    (%rsp)");
                        emit!(out, "addq    $8, %rsp");
                    }

                    _ => {
                        let div = self.operand(alloc, divisor, bytes);
                        emit!(out, "idiv{suffix}    {div}");
                    }
                }

                if result != rax {
                    emit!(out, "mov{suffix}    {rax}, {result}");
                }
            }

            Inst::XorFloat { dest, src, bytes } => {
                let operand = if *bytes == 4 { "xorps" } else { "xorpd" };
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes);

                emit!(out, "{operand}   {src}, {dest}");
            }

            Inst::Ucomis { lhs, rhs, bytes, .. } => {
                let suffix = float_suffix(bytes);
                let lhs = alloc.location(lhs, bytes);
                let rhs = self.operand(alloc, rhs, bytes);

                // xmm15 is reserved scratch; safe to clobber here
                emit!(out, "mov{suffix}    {lhs}, %xmm15");
                emit!(out, "ucomi{suffix}  {rhs}, %xmm15");
            }

            Inst::Setcc { dest, condition } => {
                let dest = alloc.location(dest, &1);

                emit!(out, "set{}  {dest}", condition.as_str())
            }

            Inst::Test { lhs, rhs, bytes, .. } | Inst::Cmp { lhs, rhs, bytes, .. } => {
                let suffix = suffix(bytes);
                let lhs = alloc.location(lhs, bytes);
                let rhs = self.operand(alloc, rhs, bytes);

                match matches!(instruction, Inst::Cmp { .. }) {
                    true => emit!(out, "cmp{suffix}    {rhs}, {lhs}"),
                    _ => emit!(out, "test{suffix}    {rhs}, {lhs}"),
                }
            }

            Inst::Call { target, moves, ret, .. } => {
                let arg_moves: Vec<_> = moves
                    .iter()
                    .map(|(vreg, reg)| {
                        let bytes = self.reg_bytes(vreg);
                        let is_float = self.is_float(vreg);
                        let suffix = typed_suffix(&bytes, is_float);
                        let src = alloc.location(vreg, &bytes);
                        let dest = format!("%{}", reg.name(bytes));

                        (src, dest, suffix, is_float)
                    })
                    .collect();

                resolve_parallel_moves(out, arg_moves);

                emit!(out, "call    {target}");

                if let Some(ret) = ret {
                    let bytes = self.reg_bytes(ret);
                    let is_float = self.is_float(ret);
                    let class = match is_float {
                        true => RegClass::Float,
                        _ => RegClass::Int,
                    };

                    if let Some(abi_ret) = X86_64::ret(class) {
                        let suffix = typed_suffix(&bytes, is_float);
                        let src = format!("%{}", abi_ret.name(bytes));
                        let dest = alloc.location(ret, &bytes);

                        mov_or_scratch(out, &src, &dest, suffix, is_float);
                    }
                }
            }

            Inst::Syscall {
                id: syscall_id,
                moves,
                ret,
                ..
            } => {
                let arg_moves: Vec<_> = moves
                    .iter()
                    .map(|(vreg, reg)| {
                        let bytes = self.reg_bytes(vreg);
                        let is_float = self.is_float(vreg);
                        let suffix = typed_suffix(&bytes, is_float);
                        let src = alloc.location(vreg, &bytes);
                        let dest = format!("%{}", reg.name(bytes));

                        (src, dest, suffix, is_float)
                    })
                    .collect();

                resolve_parallel_moves(out, arg_moves);

                emit!(out, "movl    ${syscall_id}, %eax");
                emit!(out, "syscall");

                if let Some(ret) = ret {
                    let bytes = self.reg_bytes(ret);
                    let is_float = self.is_float(ret);
                    let suffix = typed_suffix(&bytes, is_float);
                    let src = format!("%{}", X86Reg::Rax.name(bytes));
                    let dest = alloc.location(ret, &bytes);

                    mov_or_scratch(out, &src, &dest, suffix, is_float);
                }
            }
        }
    }

    fn emit_terminator(
        &self,
        alloc: &Allocation<X86_64>,
        term: &Term,
        name: &str,
        epilogue: &str,
        is_last: bool,
        out: &mut String,
    ) {
        match term {
            Term::Return(None) if !is_last => emit!(out, "jmp       {epilogue}"),
            Term::Return(None) => {}
            Term::Jump(block) => emit!(out, "jmp      .L_block_{name}_{}", block.0),

            Term::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let condition = alloc.location(cond, &4);

                emit!(out, "testl       {condition}, {condition}");
                emit!(out, "jne         .L_block_{name}_{}", then_block.0);
                emit!(out, "jmp         .L_block_{name}_{}", else_block.0);
            }

            Term::Return(Some(vreg)) => {
                let bytes = self.reg_bytes(vreg);
                let is_float = self.is_float(vreg);
                let class = match is_float {
                    true => RegClass::Float,
                    _ => RegClass::Int,
                };

                if let Some(ret_reg) = X86_64::ret(class) {
                    let suffix = typed_suffix(&bytes, is_float);
                    let src = alloc.location(vreg, &bytes);
                    let dest = format!("%{}", ret_reg.name(bytes));

                    mov_or_scratch(out, &src, &dest, suffix, is_float);
                }

                if !is_last {
                    emit!(out, "jmp        {epilogue}");
                }
            }
        }
    }

    #[inline(always)]
    fn reg_bytes(&self, vreg: &VReg) -> u8 {
        self.vreg_types.get(vreg.0 as usize).map(|typ| typ.bytes()).unwrap_or(4)
    }

    #[inline(always)]
    fn is_float(&self, vreg: &VReg) -> bool {
        matches!(self.vreg_types.get(vreg.0 as usize), Some(MachineType::Float { .. }))
    }

    #[inline(always)]
    fn operand<'s>(&self, alloc: &Allocation<X86_64>, operand: &'s X86Operand, bytes: &u8) -> Cow<'s, str> {
        match operand {
            X86Operand::VReg(vreg) => Cow::Owned(alloc.location(vreg, bytes)),
            X86Operand::Imm(n) => Cow::Owned(format!("${n}")),
            X86Operand::RipRel(s) => Cow::Borrowed(s.as_str()),
        }
    }
}

impl Allocation<X86_64> {
    #[inline(always)]
    fn location(&self, vreg: &VReg, bytes: &u8) -> String {
        match self.location_of(vreg) {
            Location::Reg(reg) => format!("%{}", reg.name(*bytes)),
            Location::Stack(offset) => format!("{}(%rbp)", offset - (self.used_callee_saved.len() as i32 * 8)),
        }
    }
}

#[inline(always)]
const fn typed_suffix<'s>(bytes: &u8, is_float: bool) -> &'s str {
    match is_float {
        true => float_suffix(bytes),
        false => suffix(bytes),
    }
}

#[inline(always)]
const fn suffix<'s>(bytes: &u8) -> &'s str {
    match bytes {
        1 => "b",
        2 => "w",
        4 => "l",
        _ => "q",
    }
}

#[inline(always)]
const fn float_suffix<'s>(bytes: &u8) -> &'s str {
    match bytes {
        4 => "ss",
        _ => "sd",
    }
}

#[inline(always)]
// TODO: should we really do this here? reavaliate the process in the pipeline
fn mov_or_scratch(out: &mut String, src: &str, dest: &str, suffix: &str, is_float: bool) {
    if src == dest {
        return;
    }

    match src.contains("(%rbp)") && dest.contains("(%rbp)") {
        true => match is_float {
            true => {
                emit!(out, "mov{suffix}    {src}, %xmm15");
                emit!(out, "mov{suffix}    %xmm15, {dest}");
            }

            false => {
                let scratch = if suffix == "q" { "%r11" } else { "%r11d" };
                emit!(out, "mov{suffix}    {src}, {scratch}");
                emit!(out, "mov{suffix}    {scratch}, {dest}");
            }
        },

        false => emit!(out, "mov{suffix}    {src}, {dest}"),
    }
}

/// Serialise a set of parallel register moves without data corruption
///
/// - Chains (A->B then B->C) are resolved by topological ordering
/// - Cycles (A->B, B->A) are broken using a scratch register (`%r11`/`%xmm15`)
fn resolve_parallel_moves(out: &mut String, mut moves: Vec<(String, String, &str, bool)>) {
    moves.retain(|(s, d, _, _)| s != d);

    loop {
        // find a move whose dest is not read by any other pending move
        let safe = moves
            .iter()
            .position(|(_, dest, _, _)| !moves.iter().any(|(src, other_dest, _, _)| other_dest != dest && src == dest));

        match safe {
            Some(i) => {
                let (ref src, ref dest, suffix, is_float) = moves.swap_remove(i);
                mov_or_scratch(out, src, dest, suffix, is_float);
            }
            None if moves.is_empty() => break,
            None => {
                // in cycle, save first source to scratch, breaking the dependency
                let (_, _, suffix, is_float) = moves[0];
                let scratch = match (is_float, suffix) {
                    (true, _) => "%xmm15",
                    (_, "q") => "%r11",
                    _ => "%r11d",
                };

                emit!(out, "mov{suffix}    {}, {scratch}", moves[0].0);
                moves[0].0 = scratch.to_string();
            }
        }
    }
}
