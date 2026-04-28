//! `x86_64` code generation

use crate::{
    lir::{
        self, Function,
        target::{Emittable, Target, x86_64::X86_64},
    },
    mir::{self, Mir},
};
use std::fmt::Write;

const DEFAULT_CAPACITY: usize = 1 << 10;

macro_rules! emit {
    ($dst:expr, $($arg:tt)*) => {
        writeln!($dst, "    {}", format_args!($($arg)*)).unwrap()
    };
}

macro_rules! label {
    ($dst:expr, $($arg:tt)*) => {
        writeln!($dst, "{}", format_args!($($arg)*)).unwrap()
    }
}

pub fn emit<T: Target>(mir: &Mir, target: T) -> String
where
    Function<T>: Emittable<T>,
{
    let mut out = String::with_capacity(DEFAULT_CAPACITY);

    label!(out, ".text");

    for function in &mir.functions {
        let lir = lir::lower::<T>(function, &mir.symbols, &mir.functions);
        lir.emit((), &mut out);
    }

    // emit a `_start` trampoline if the program defines `fn main`
    //
    // this allows the binary to be linked with `ld` directly
    // `_start` calls `nyx_main`, passes its return value to the exit syscall
    let has_main = mir.symbols.iter().any(|name| name == "main");
    if has_main {
        label!(out, ".globl _start");
        label!(out, "_start:");

        emit!(out, "call    nyx_main");
        emit!(out, "movl    %eax, %edi"); // exit code = return value
        emit!(out, "movl    $60, %eax"); // syscall: exit
        emit!(out, "syscall");
    }

    out
}

impl Emittable<X86_64> for Function<X86_64> {
    fn emit(&self, alloc: (), out: &mut String) -> String {
        todo!()
    }
}
