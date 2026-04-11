//! Graph-colouring register allocation algorithm.
//!
//! Algorithm:
//!   1. Simplify — repeatedly remove nodes with degree < K, push to stack.
//!   2. Spill    — if stuck, pick the highest-degree node as a potential spill.
//!   3. Select   — pop stack, assign the lowest-numbered free colour;
//!                 if none available the node becomes an actual spill → stack slot.

use std::collections::HashMap;

use crate::mir::ValueId;

/// Complete allocation result for a function.
#[derive(Debug)]
pub struct Allocation {
    locations: HashMap<ValueId, Location>,
    /// Total stack frame size in bytes, 16 aligned.
    frame_size: u32,
}

/// x86-64 general-purpose registers available for allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reg {
    Rax,
    Rcx,
    Rdx,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
}

/// Where a value lives in after allocation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Register(Reg),
    Stack(i32),
}

impl Allocation {
    pub fn location_of(&self, id: ValueId) -> Location {
        self.locations[&id]
    }
}

impl Reg {
    pub const ALL: &'static [Reg] = &[
        Reg::Rax,
        Reg::Rcx,
        Reg::Rdx,
        Reg::Rsi,
        Reg::Rdi,
        Reg::R8,
        Reg::R9,
        Reg::R10,
        Reg::R11,
    ];

    pub const fn k() -> usize {
        Self::ALL.len()
    }

    pub const fn as_str_32(self) -> &'static str {
        match self {
            Reg::Rax => "eax",
            Reg::Rcx => "ecx",
            Reg::Rdx => "edx",
            Reg::Rsi => "esi",
            Reg::Rdi => "edi",
            Reg::R8 => "r8d",
            Reg::R9 => "r9d",
            Reg::R10 => "r10d",
            Reg::R11 => "r11d",
        }
    }

    pub const fn as_str_64(self) -> &'static str {
        match self {
            Reg::Rax => "rax",
            Reg::Rcx => "rcx",
            Reg::Rdx => "rdx",
            Reg::Rsi => "rsi",
            Reg::Rdi => "rdi",
            Reg::R8 => "r8",
            Reg::R9 => "r9",
            Reg::R10 => "r10",
            Reg::R11 => "r11",
        }
    }
}
