//! `x86_64` code generation

use crate::{
    lir::{
        self, Function,
        target::{Emittable, Target, x86_64::X86_64},
    },
    mir::{self, Mir},
};
use std::fmt::Write;

impl Emittable<X86_64> for Function<X86_64> {
    fn emit(&self, alloc: (), out: &mut String) -> String {
        todo!()
    }

    fn start(&mut self, out: &mut String) {
        todo!()
    }
}
