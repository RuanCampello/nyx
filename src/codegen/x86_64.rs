//! x86_64 code emission (AT&T syntax)
//!
//! ## Conventions
//! - Syntax: AT&T (GAS) — `mov src, dest`, `%` registers, `$` immediates
//! - Sizes: `l` suffix for 32-bit (i32/bool), `q` for 64-bit (i64/pointers)
//! - Frame: System V AMD64 — `push %rbp; mov %rsp, %rbp; sub $N, %rsp`
//! - Args: First 6 int args in `%rdi %rsi %rdx %rcx %r8 %r9`
//! - Return: `%rax` (32-bit result in `%eax`)
//! - Spills: `-N(%rbp)` stack slots

use crate::{
    hir::Type,
    mir::{Const, Function, Mir, Operand, Place},
    regalloc::{Allocation, Location, Reg},
};
use std::fmt::{Display, Write};

struct FunctionEmitter<'e> {
    out: &'e mut String,
    allocation: &'e Allocation,
    function: &'e Function,
}

/// Emit full assembly program.
pub fn emit(mir: &Mir) -> String {
    const DEFAULT_SIZE: usize = 1 << 8;
    let mut out = String::with_capacity(DEFAULT_SIZE);

    writeln!(out, "    .text").unwrap();

    todo!()
}

impl<'e> FunctionEmitter<'e> {
    fn new(out: &'e mut String, alloc: &'e Allocation, function: &'e Function) -> Self {
        Self {
            out,
            allocation: alloc,
            function,
        }
    }

    #[inline(always)]
    fn emit_body(&mut self) {
        todo!()
    }

    /// Prologue: label and frame setup
    #[inline(always)]
    fn emit_prologue(&mut self) {
        writeln!(self.out, ".nyx_function:").unwrap();
        writeln!(self.out, "    push    %rbp").unwrap();
        writeln!(self.out, "    mov     %rsp, %rbp").unwrap();

        if self.allocation.frame_size > 0 {
            writeln!(
                self.out,
                "    sub     ${}, %rsp",
                self.allocation.frame_size
            )
            .unwrap();
        }
    }

    /// Epilogue: clean-up and return
    #[inline(always)]
    fn emit_epilogue(&mut self) {
        writeln!(self.out, "    mov     %rbp, %rsp").unwrap();
        writeln!(self.out, "    pop     %rbp").unwrap();
        writeln!(self.out, "    ret").unwrap();
    }
}

impl Function {
    fn emit(&self, out: &mut String) {
        let alloc = Allocation::allocate(self);
        let mut emitter = FunctionEmitter::new(out, &alloc, self);

        emitter.emit_prologue();
        emitter.emit_body();
        emitter.emit_epilogue();
    }
}

impl Type {
    #[inline(always)]
    const fn size_suffix<'s>(&self) -> &'s str {
        match self {
            Type::I32 | Type::Bool => "l",
            Type::I64 | Type::String => "q",
            Type::F32 | Type::F64 => panic!("float size suffix"),
            Type::Unit => unreachable!(),
        }
    }
}

#[inline(always)]
const fn reg_name<'r>(reg: Reg, typ: Type) -> &'r str {
    match typ {
        Type::I32 | Type::Bool => reg.as_str_32(),
        Type::I64 | Type::String => reg.as_str_64(),
        Type::F32 | Type::F64 => panic!("float registers not yet supported"),
        Type::Unit => panic!("unit type has no runtime representation"),
    }
}

/// Format an operand as AT&T assembly source
#[inline(always)]
fn operand_str(op: &Operand, alloc: &Allocation) -> String {
    match op {
        Operand::Place(place) => place_str(place, alloc),
        Operand::Const(c) => c.to_string(),
    }
}

impl Display for Const {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Const::Int(n, _) => writeln!(f, "${}", n),
            Const::Bool(b) => writeln!(f, "${}", if *b { 1 } else { 0 }),
            Const::Float(_, _) => unimplemented!("float constants not yet supported"),
            Const::Unit => unreachable!("Unit constant has no runtime representation"),
        }
    }
}

#[inline(always)]
fn place_str(place: &Place, allocation: &Allocation) -> String {
    let loc = allocation.location_of(place.id);
    match loc {
        Location::Register(reg) => format!("%{}", reg_name(reg, place.typ)),
        Location::Stack(offset) => format!("{}(%rbp)", offset),
    }
}
