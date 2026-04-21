//! Graph-colouring register allocation algorithm.
//!
//! Algorithm:
//!  - 1. Simplify — repeatedly remove nodes with degree < K, push to stack.
//!  - 2. Spill    — if stuck, pick the highest-degree node as a potential spill.
//!  - 3. Select   — pop stack, assign the lowest-numbered free colour;
//!                 if none available the node becomes an actual spill → stack slot.

use crate::{
    hir::Type,
    mir::{Function, ValueId},
    regalloc::interference::Interference,
};
use std::collections::{HashMap, HashSet};

/// Complete allocation result for a function.
#[derive(Debug, Default)]
pub struct Allocation {
    pub(in crate::regalloc) locations: HashMap<ValueId, Location>,
    /// Total stack frame size in bytes, 16 aligned.
    pub(crate) frame_size: u32,
}

/// x86-64 general-purpose registers available for allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reg {
    // caller-saved (clobbered across calls: safe only for values that don't cross call sites)
    Rax,
    Rcx,
    Rdx,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
    // callee-saved (preserved across calls: safe for values that live across call sites)
    Rbx,
    R12,
    R13,
    R14,
    R15,
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

    pub fn allocate(function: &Function) -> Allocation {
        let local_types: HashMap<ValueId, Type> =
            function.locals.iter().map(|&(id, typ)| (id, typ)).collect();
        let graph = Interference::build(function);
        graph.colour(local_types)
    }
}

impl Interference {
    pub fn colour(self, local_types: HashMap<ValueId, Type>) -> Allocation {
        let k = Reg::k();
        let all = self.nodes().collect::<Vec<_>>();

        if all.is_empty() {
            return Allocation::default();
        }

        let mut degree: HashMap<_, _> = all.iter().map(|&id| (id, self.degree(id))).collect();

        let mut removed = HashSet::new();
        let mut stack = Vec::new();

        loop {
            // simplify: push any node with degree < K
            let simplifiable: Vec<_> = degree
                .iter()
                .filter(|&(id, degree)| !removed.contains(id) && *degree < k)
                .map(|(&id, _)| id)
                .collect();

            if !simplifiable.is_empty() {
                for id in simplifiable {
                    self.push_node(id, &mut stack, &mut removed, &mut degree);
                }

                continue;
            }

            // all remaining nodes have degree >= K — spill the highest-degree one
            let remaining: Vec<_> = degree
                .keys()
                .filter(|id| !removed.contains(id))
                .copied()
                .collect();

            match remaining.iter().max_by_key(|&&id| degree[&id]) {
                None => break, // graph is empty,
                Some(&spill) => self.push_node(spill, &mut stack, &mut removed, &mut degree),
            }
        }

        let mut colour_map: HashMap<ValueId, Option<Reg>> = HashMap::new();
        let mut spill_offset = 0;
        let mut locations = HashMap::new();

        while let Some(id) = stack.pop() {
            let mut forbidden: HashSet<_> = self
                .neighbours(id)
                .iter()
                .filter_map(|nb| match colour_map.get(nb) {
                    Some(Some(reg)) => Some(*reg),
                    _ => None,
                })
                .collect();

            // values whose live range crosses a call site must not land in caller-saved
            // registers, as those are clobbered by the call
            if self.call_crossed.contains(&id) {
                for &reg in Reg::CALLER_SAVED {
                    forbidden.insert(reg);
                }
            }

            let assigned = Reg::ALL
                .iter()
                .find(|reg| !forbidden.contains(reg))
                .copied();

            colour_map.insert(id, assigned);

            let location = match assigned {
                Some(reg) => Location::Register(reg),
                None => {
                    let slot_size = slot_bytes(id, &local_types) as i32;
                    spill_offset -= slot_size;
                    Location::Stack(spill_offset)
                }
            };

            locations.insert(id, location);
        }

        let raw = spill_offset.unsigned_abs();
        // `push %rbp` in the prologue shifts %rsp by 8
        // compensate so that (8 + frame_size) is a multiple of 16, keeping %rsp 16-byte aligned
        // at every interior call site
        let frame_size = ((raw + 8 + 15) & !15).saturating_sub(8);

        Allocation {
            locations,
            frame_size,
        }
    }
}

impl Reg {
    /// All allocatable registers
    ///
    /// Callee-saved are listed first so the allocator
    /// prefers them for long-lived values: they survive calls without restriction
    pub const ALL: &'static [Reg] = &[
        // callee-saved
        Reg::Rbx,
        Reg::R12,
        Reg::R13,
        Reg::R14,
        Reg::R15,
        // caller-saved
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

    /// Registers clobbered by calls under System V AMD64.
    pub const CALLER_SAVED: &'static [Reg] = &[
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

    pub const fn as_str_32<'s>(self) -> &'s str {
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
            Reg::Rbx => "ebx",
            Reg::R12 => "r12d",
            Reg::R13 => "r13d",
            Reg::R14 => "r14d",
            Reg::R15 => "r15d",
        }
    }

    pub const fn as_str_64<'s>(self) -> &'s str {
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
            Reg::Rbx => "rbx",
            Reg::R12 => "r12",
            Reg::R13 => "r13",
            Reg::R14 => "r14",
            Reg::R15 => "r15",
        }
    }
}

/// stack slot size in bytes for the given value
#[inline(always)]
fn slot_bytes(id: ValueId, local_types: &HashMap<ValueId, Type>) -> u32 {
    match local_types.get(&id) {
        Some(Type::I64 | Type::String) => 8,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hir, mir, parser::Parser};

    fn allocate_for(src: &str) -> (mir::Mir, Allocation) {
        let stmts = Parser::new(src).parse().unwrap();
        let hir = hir::lower(stmts).unwrap();
        let mir = mir::lower(hir).unwrap();
        let alloc = Allocation::allocate(&mir.functions[0]);

        (mir, alloc)
    }

    #[test]
    fn empty_function_needs_no_allocation() {
        let (_, alloc) = allocate_for("fn main() { }");

        assert!(alloc.locations.is_empty());
        assert_eq!(alloc.frame_size, 0);
    }

    #[test]
    fn single_value_gets_a_register() {
        let (mir, alloc) = allocate_for("fn foo(): i32 { let x: i32 = 1; x }");
        let f = &mir.functions[0];

        for (id, _) in &f.locals {
            let loc = alloc.location_of(*id);
            assert!(
                matches!(loc, Location::Register(_)),
                "single value with no pressure should land in a register, got {loc:?}"
            );
        }
    }

    #[test]
    fn two_non_interfering_values_may_share_register() {
        // x and y never live at the same time so they CAN get the same register
        let (mir, alloc) = allocate_for(
            r#"
            fn foo(): i32 {
                let x: i32 = 1; let y: i32 = 2; y;
            }
        "#,
        );
        let f = &mir.functions[0];

        for (id, _) in &f.locals {
            let _ = alloc.location_of(*id);
        }
    }

    #[test]
    fn interfering_values_get_different_registers() {
        // a and b are simultaneously live for `a + b`
        let (mir, alloc) = allocate_for("fn add(a: i32, b: i32): i32 { a + b }");
        let a = ValueId(0);
        let b = ValueId(1);

        let loc_a = alloc.location_of(a);
        let loc_b = alloc.location_of(b);

        assert_ne!(
            loc_a, loc_b,
            "interfering values must not share a location: both got {loc_a:?}"
        );
    }

    #[test]
    fn more_values_than_registers_causes_spill() {
        // K = 14 (5 callee-saved + 9 caller-saved); need > 14 simultaneously live
        // values to guarantee at least one spill.
        let src = r#"
            fn pressure(
                a: i32, b: i32, c: i32, d: i32, e: i32,
                f: i32, g: i32, h: i32, i: i32, j: i32,
                k: i32, l: i32, m: i32, n: i32, o: i32, p: i32
            ): i32 {
                a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p
            }
        "#;
        let (mir, alloc) = allocate_for(src);
        let f = &mir.functions[0];

        let spilled = alloc
            .locations
            .values()
            .filter(|l| matches!(l, Location::Stack(_)))
            .count();

        assert!(
            spilled > 0,
            "with 16 simultaneously live values and K=14, at least one must spill"
        );
        assert!(
            alloc.frame_size > 0,
            "spilled values require stack frame space"
        );

        assert_eq!(
            alloc.frame_size % 16,
            0,
            "frame size must be 16-byte aligned"
        );
    }

    fn frame_size_is_always_16_byte_aligned() {
        let src = r#"
            fn many(
                a: i32, b: i32, c: i32, d: i32, e: i32,
                f: i32, g: i32, h: i32, i: i32, j: i32
            ): i32 {
                a + b + c + d + e + f + g + h + i + j
            }
        "#;

        let (_, alloc) = allocate_for(src);
        assert_eq!(alloc.frame_size % 16, 0);
    }

    #[test]
    fn all_values_are_allocated() {
        let src = "fn add(a: i32, b: i32): i32 { a + b }";
        let (mir, alloc) = allocate_for(src);
        let f = &mir.functions[0];

        for (id, _) in &f.locals {
            assert!(
                alloc.locations.contains_key(id),
                "every local must receive a location: {id:?} missing"
            );
        }
    }
}
