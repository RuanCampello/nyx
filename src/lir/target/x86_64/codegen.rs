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
        target::{Emittable, Target, x86_64::X86_64},
    },
    mir::{self, Mir},
};
use std::fmt::Write;

impl Emittable<X86_64> for Function<X86_64> {
    fn emit(&self, alloc: (), out: &mut String) {
        let name = &self.name;
        let frame_size = 0;
        let epilogue = format!("L.{name}_epilogue");

        Self::emit_prologue(alloc, name, frame_size, out);
        self.emit_body(alloc, name, &epilogue, out);
        Self::emit_epilogue(alloc, &epilogue, frame_size, out);
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
    fn emit_prologue(alloc: (), name: &str, frame_size: u32, out: &mut String) {}
    fn emit_epilogue(alloc: (), label: &str, frame_size: u32, out: &mut String) {}
    fn emit_body(&self, alloc: (), name: &str, epilogue: &str, out: &mut String) {}
    fn emit_float(&self, out: &mut String) {}
}
