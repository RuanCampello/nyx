//! LIR + Allocation → GAS (AT&T syntax) assembly emission.
//!
//! After register allocation every VReg has a concrete Location (register or
//! stack slot).
//!
//! Emission is meant to be very mechanical: look up, add mnemonic.

use crate::{
    emit, label,
    lir::{
        self, Function,
        regalloc::Allocation,
        target::{Emittable, PhysicalReg, Target, x86_64::X86_64},
    },
    mir::{self, Mir},
};
use std::fmt::Write;

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

    fn emit_body(&self, alloc: &Allocation<X86_64>, name: &str, epilogue: &str, out: &mut String) {}

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
