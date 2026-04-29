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
            Emittable, PhysicalReg, Target,
            x86_64::{X86_64, X86Instr, X86Operand},
        },
    },
    mir::{self, Mir},
};
use std::{borrow::Cow, fmt::Write};

impl Emittable<X86_64> for Function<X86_64> {
    fn emit(&self, alloc: Allocation<X86_64>, out: &mut String) {
        let name = &self.name;
        let frame_size = 0;
        let epilogue = format!("L.{name}_epilogue");

        Self::emit_prologue(&alloc, name, frame_size, out);
        self.emit_body(&alloc, name, &epilogue, out);
        Self::emit_epilogue(&alloc, &epilogue, frame_size, out);
        self.emit_float(out);
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

    fn emit_float(&self, out: &mut String) {
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

        let saved = alloc.used_callee_saved.len();

        match instruction {
            Inst::ParamMov { dest, src_reg, bytes } => {
                let suffix = match self.is_float(dest) {
                    true => float_suffix(bytes),
                    _ => suffix(bytes),
                };

                let src = format!("%{}", src_reg.name(*bytes));
                let dest = alloc.location(&dest, bytes);
            }

            Inst::Mov { dest, src, bytes } => {
                let suffix = suffix(bytes);
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes, saved);
            }

            Inst::MovFloat { dest, src, bytes } => {
                let suffix = float_suffix(bytes);
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes, saved);

                mov_or_scratch(out, &src, &dest, suffix, true);
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
                let suffix = match self.is_float(dest) {
                    true => float_suffix(bytes),
                    _ => suffix(bytes),
                };
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes, saved);

                match instruction {
                    Inst::Add { .. } | Inst::AddFloat { .. } => emit!(out, "add{suffix}    {src}, {dest}"),
                    Inst::Sub { .. } | Inst::SubFloat { .. } => emit!(out, "sub{suffix}    {src}, {dest}"),
                    Inst::Imul { .. } => emit!(out, "imul{suffix}    {src}, {dest}"),
                    Inst::MulFloat { .. } => emit!(out, "mul{suffix}    {src}, {dest}"),
                    Inst::DivFloat { .. } => emit!(out, "div{suffix}    {src}, {dest}"),
                    Inst::And { .. } => emit!(out, "and{suffix}    {src}, {dest}"),
                    Inst::Or { .. } => emit!(out, "or{suffix}    {src}, {dest}"),
                    Inst::Xor { .. } => emit!(out, "or{suffix}    {src}, {dest}"),

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
                uses,
                precoloured_uses,
            } => todo!("idiv"),

            Inst::XorFloat { dest, src, bytes } => {
                let operand = if *bytes == 4 { "xorps" } else { "xorpd" };
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes, saved);

                emit!(out, "{operand}   {src}, {dest}");
            }

            Inst::Ucomis { lhs, rhs, bytes, .. } => {
                let suffix = float_suffix(bytes);
                let lhs = alloc.location(lhs, bytes);
                let rhs = self.operand(alloc, rhs, bytes, saved);

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
                let rhs = self.operand(alloc, rhs, bytes, saved);

                match matches!(instruction, Inst::Cmp { .. }) {
                    true => emit!(out, "cmp{suffix}    {lhs}, {rhs}"),
                    _ => emit!(out, "test{suffix}    {lhs}, {rhs}"),
                }
            }

            Inst::Call {
                target,
                moves,
                uses,
                ret,
                precoloured_def,
            } => todo!("call"),
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
            _ => todo!("emit return with some"),
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
    fn operand<'s>(
        &self,
        alloc: &Allocation<X86_64>,
        operand: &'s X86Operand,
        bytes: &u8,
        saved: usize,
    ) -> Cow<'s, str> {
        match operand {
            X86Operand::VReg(vreg) => Cow::Owned(alloc.location(vreg, bytes)),
            X86Operand::Imm(n) => Cow::Owned(n.to_string()),
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
