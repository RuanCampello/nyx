//! LIR + Allocation → GAS (AT&T syntax) assembly emission.
//!
//! After register allocation every VReg has a concrete Location (register or
//! stack slot).
//!
//! Emission is meant to be very mechanical: look up, add mnemonic.

use crate::{
    emit, label,
    lir::{
        self, Function, MachineType, VReg,
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

            todo!("emit terminator");
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
        let saved = alloc.used_callee_saved.len();

        match instruction {
            X86Instr::ParamMov { dest, src_reg, bytes } => {
                let suffix = match self.is_float(dest) {
                    true => float_suffix(bytes),
                    _ => suffix(bytes),
                };

                let src = format!("%{}", src_reg.name(*bytes));
                let dest = Self::location(alloc, &dest, bytes);
            }
            _ => todo!("emit instruction"),
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
        bytes: u8,
        saved: usize,
    ) -> Cow<'s, str> {
        match operand {
            X86Operand::VReg(vreg) => todo!(),
            X86Operand::Imm(n) => Cow::Owned(n.to_string()),
            X86Operand::RipRel(s) => Cow::Borrowed(s.as_str()),
        }
    }

    #[inline(always)]
    fn location(alloc: &Allocation<X86_64>, vreg: &VReg, bytes: &u8) -> String {
        match alloc.location_of(vreg) {
            Location::Reg(reg) => format!("%{}", reg.name(*bytes)),
            Location::Stack(offset) => format!("{}(%rbp)", offset - (alloc.used_callee_saved.len() as i32 * 8)),
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
