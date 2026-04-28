//! Graph-colouring register allocation algorithm.
//!
//! Algorithm:
//!  - 1. Coalesce: merge non-interfering move-related nodes
//!  - 2. Simplify: repeatedly remove nodes with degree < K, push to stack
//!  - 3. Spill:    if stuck, pick the highest-degree node as a potential spill
//!  - 4. Select:   pop stack, assign the lowest-cost free colour;
//!                 if none available the node becomes an actual spill → stack slot

use crate::{
    hir::Type,
    mir::{Function, ValueId},
    regalloc::interference::Interference,
};
use std::collections::{HashMap, HashSet};

/// Complete allocation result for a function.
#[derive(Debug, Default)]
pub struct Allocation {
    pub(crate) locations: HashMap<ValueId, Location>,
    /// Total stack frame size in bytes, 16 aligned.
    pub(crate) frame_size: u32,
}

/// x86-64 physical register
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

    // XMM (all caller-saved under SysV AMD64)
    Xmm0,
    Xmm1,
    Xmm2,
    Xmm3,
    Xmm4,
    Xmm5,
    Xmm6,
    Xmm7,
    Xmm8,
    Xmm9,
    Xmm10,
    Xmm11,
    Xmm12,
    Xmm13,
    Xmm14,
    Xmm15,
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
        let mut graph = Interference::build(function);

        graph.coalesce(function);
        graph.colour(local_types)
    }
}

impl Interference {
    pub fn colour(self, types: HashMap<ValueId, Type>) -> Allocation {
        let all = self.nodes().collect::<Vec<_>>();

        if all.is_empty() {
            return Allocation::default();
        }

        #[inline(always)]
        fn is_float_type(types: &HashMap<ValueId, Type>, id: ValueId) -> bool {
            matches!(types.get(&id), Some(Type::F32 | Type::F64))
        }

        let (floats, ints): (Vec<_>, Vec<_>) =
            all.iter().partition(|&&id| is_float_type(&types, id));

        let mut locations = HashMap::new();
        let mut spill_offset = 0;

        self.colour_group(
            Self::K,
            &ints,
            Reg::ALL,
            Reg::CALLER_SAVED,
            &types,
            &mut locations,
            &mut spill_offset,
        );

        self.colour_group(
            Self::K_XMM,
            &floats,
            Reg::XMM_ALL,
            Reg::XMM_ALL,
            &types,
            &mut locations,
            &mut spill_offset,
        );

        let raw = spill_offset.unsigned_abs();
        let frame_size = (raw + 15) & !15;

        Allocation {
            locations,
            frame_size,
        }
    }

    fn colour_group(
        &self,
        k: usize,
        nodes: &[&ValueId],
        available: &[Reg],
        calleer_saved: &[Reg],
        local_types: &HashMap<ValueId, Type>,
        locations: &mut HashMap<ValueId, Location>,
        spill_offset: &mut i32,
    ) {
        if nodes.is_empty() {
            return;
        }

        let mut degree: HashMap<_, _> = nodes.iter().map(|&&id| (id, self.degree(id))).collect();

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

        while let Some(id) = stack.pop() {
            let mut forbidden: HashSet<_> = self
                .neighbours(id)
                .iter()
                .filter_map(|nb| colour_map.get(nb).and_then(|&r| r))
                .collect();

            // values whose live range crosses a call site must not land in caller-saved
            // registers, as those are clobbered by the call
            if self.call_crossed.contains(&id) {
                for &reg in calleer_saved {
                    forbidden.insert(reg);
                }
            }

            let assigned = available
                .iter()
                .find(|reg| !forbidden.contains(reg))
                .copied();

            colour_map.insert(id, assigned);

            let location = match assigned {
                Some(reg) => Location::Register(reg),
                None => {
                    let slot_size = slot_bytes(id, &local_types) as i32;
                    *spill_offset -= slot_size;
                    Location::Stack(*spill_offset)
                }
            };

            locations.insert(id, location);
        }
    }
}

impl Reg {
    /// All allocatable registers
    ///
    /// Callee-saved are listed first so the allocator
    /// prefers them for long-lived values: they survive calls without restriction
    pub const ALL: &'static [Reg] = &[
        Reg::Rax,
        Reg::Rcx,
        Reg::Rdx,
        Reg::Rsi,
        Reg::Rdi,
        Reg::R8,
        Reg::R9,
        Reg::R10,
        // Reg::R11,
        Reg::Rbx,
        Reg::R12,
        Reg::R13,
        Reg::R14,
        Reg::R15,
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
        // Reg::R11,
    ];

    /// Registers that must be preserved across calls (callee saves them).
    pub const CALLEE_SAVED: &'static [Reg] = &[Reg::Rbx, Reg::R12, Reg::R13, Reg::R14, Reg::R15];

    /// All 16 XMM registers; all caller-saved under SysV AMD64.
    pub const XMM_ALL: &'static [Reg] = &[
        Reg::Xmm0,
        Reg::Xmm1,
        Reg::Xmm2,
        Reg::Xmm3,
        Reg::Xmm4,
        Reg::Xmm5,
        Reg::Xmm6,
        Reg::Xmm7,
        Reg::Xmm8,
        Reg::Xmm9,
        Reg::Xmm10,
        Reg::Xmm11,
        Reg::Xmm12,
        Reg::Xmm13,
        Reg::Xmm14,
        // Reg::Xmm15,
    ];

    #[inline(always)]
    pub const fn as_str<'s, const S: usize>(&self) -> &'s str {
        match S {
            8 => self.as_str_8(),
            16 => self.as_str_16(),
            32 => self.as_str_32(),
            64 => self.as_str_64(),
            _ => panic!("invalid register size"),
        }
    }

    #[inline(always)]
    pub const fn as_str_xmm<'s>(self) -> &'s str {
        match self {
            Reg::Xmm0 => "xmm0",
            Reg::Xmm1 => "xmm1",
            Reg::Xmm2 => "xmm2",
            Reg::Xmm3 => "xmm3",
            Reg::Xmm4 => "xmm4",
            Reg::Xmm5 => "xmm5",
            Reg::Xmm6 => "xmm6",
            Reg::Xmm7 => "xmm7",
            Reg::Xmm8 => "xmm8",
            Reg::Xmm9 => "xmm9",
            Reg::Xmm10 => "xmm10",
            Reg::Xmm11 => "xmm11",
            Reg::Xmm12 => "xmm12",
            Reg::Xmm13 => "xmm13",
            Reg::Xmm14 => "xmm14",
            Reg::Xmm15 => "xmm15",
            _ => panic!("not an XMM register"),
        }
    }

    #[inline(always)]
    const fn as_str_8<'s>(self) -> &'s str {
        match self {
            Reg::Rax => "al",
            Reg::Rcx => "cl",
            Reg::Rdx => "dl",
            Reg::Rsi => "sil",
            Reg::Rdi => "dil",
            Reg::R8 => "r8b",
            Reg::R9 => "r9b",
            Reg::R10 => "r10b",
            Reg::R11 => "r11b",
            Reg::Rbx => "bl",
            Reg::R12 => "r12b",
            Reg::R13 => "r13b",
            Reg::R14 => "r14b",
            Reg::R15 => "r15b",
            _ => panic!("not a GP register"),
        }
    }

    #[inline(always)]
    const fn as_str_16<'s>(self) -> &'s str {
        match self {
            Reg::Rax => "ax",
            Reg::Rcx => "cx",
            Reg::Rdx => "dx",
            Reg::Rsi => "si",
            Reg::Rdi => "di",
            Reg::R8 => "r8w",
            Reg::R9 => "r9w",
            Reg::R10 => "r10w",
            Reg::R11 => "r11w",
            Reg::Rbx => "bx",
            Reg::R12 => "r12w",
            Reg::R13 => "r13w",
            Reg::R14 => "r14w",
            Reg::R15 => "r15w",
            _ => panic!("not a GP register"),
        }
    }

    #[inline(always)]
    const fn as_str_32<'s>(self) -> &'s str {
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
            _ => panic!("not a GP register"),
        }
    }

    #[inline(always)]
    const fn as_str_64<'s>(self) -> &'s str {
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
            _ => panic!("not a GP register"),
        }
    }
}

/// stack slot size in bytes for the given value
#[inline(always)]
fn slot_bytes(id: ValueId, local_types: &HashMap<ValueId, Type>) -> u32 {
    use crate::hir::Type as T;

    match local_types.get(&id) {
        Some(T::I64 | T::U64 | T::Iptr | T::Uptr | T::String | T::Str | T::F64) => 8,
        Some(T::F32) => 4,
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
        let (_, alloc) = allocate_for("fn add(a: i32, b: i32): i32 { a + b }");
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
        let (_, alloc) = allocate_for(src);

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

    #[test]
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
