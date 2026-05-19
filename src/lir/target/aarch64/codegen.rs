//! LIR + Allocation -> GAS assembly emission for AArch64.
//!
//! After register allocation every VReg has a concrete Location (register or
//! stack slot).
//!
//! Key differences from x86_64 codegen:
//! - No AT&T prefix sigils (`$`, `%`). ARM GAS uses bare register names, `#` for immediates
//! - Prologue/epilogue uses `STP`/`LDP` pairs (16-byte aligned)
//! - Wide immediates (> 16 bits) are materialised via `MOVZ` + `MOVK` sequences
//! - Branches use `CBNZ`/`CBZ` + `B` instead of `TEST` + `JNE`/`JMP`
//! - Syscalls use `SVC #0` with the syscall number in `X8`

use crate::{
    emit, label,
    lir::{
        Function, MachineType, Term, VReg,
        regalloc::{Allocation, Location},
        target::{
            Emittable, PhysicalReg, RegClass, Target,
            aarch64::{A64Instr, A64Operand, A64Reg, AArch64},
        },
    },
};
use std::fmt::Write;

impl Emittable<AArch64> for Function<AArch64> {
    fn emit(&self, alloc: Allocation<AArch64>, out: &mut String) {
        let name = &self.name;
        let frame_size = alloc.frame_size;
        let epilogue = format!(".L_{name}_epilogue");

        Self::emit_prologue(&alloc, name, frame_size, out);
        self.emit_body(&alloc, name, &epilogue, out);
        Self::emit_epilogue(&alloc, &epilogue, frame_size, out);
        self.emit_rodata(out);
    }

    #[inline(always)]
    fn start(out: &mut String) {
        label!(out, ".globl _start");
        label!(out, "_start:");

        emit!(out, "bl      nyx_main");
        emit!(out, "mov     x8, #93");
        emit!(out, "svc     #0");
    }
}

impl Function<AArch64> {
    fn emit_prologue(alloc: &Allocation<AArch64>, name: &str, frame_size: u32, out: &mut String) {
        label!(out, ".globl {name}");
        label!(out, "{name}:");

        emit!(out, "stp     x29, x30, [sp, #-16]!");
        emit!(out, "mov     x29, sp");

        emit_save_regs(out, &callee_saved_regs(alloc, RegClass::Int));
        emit_save_regs(out, &callee_saved_regs(alloc, RegClass::Float));

        if frame_size > 0 {
            emit!(out, "sub     sp, sp, #{frame_size}");
        }
    }

    fn emit_epilogue(alloc: &Allocation<AArch64>, label: &str, frame_size: u32, out: &mut String) {
        label!(out, "{label}:");

        if frame_size > 0 {
            emit!(out, "add     sp, sp, #{frame_size}");
        }

        emit_restore_regs(out, &callee_saved_regs(alloc, RegClass::Float));
        emit_restore_regs(out, &callee_saved_regs(alloc, RegClass::Int));

        emit!(out, "ldp     x29, x30, [sp], #16");
        emit!(out, "ret");
    }

    fn emit_body(&self, alloc: &Allocation<AArch64>, name: &str, epilogue: &str, out: &mut String) {
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

    fn emit_rodata(&self, out: &mut String) {
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
                true => label!(out, "    .word {}", *bits as u32),
                false => label!(out, "    .xword {bits}"),
            }
        }

        label!(out, ".text");
    }
}

impl Function<AArch64> {
    fn emit_instruction(
        &self,
        instruction: &A64Instr,
        alloc: &Allocation<AArch64>,
        out: &mut String,
    ) {
        match instruction {
            A64Instr::Mov { dest, src, bytes } => {
                let dest = alloc.location(dest, bytes);
                let src = alloc.location(src, bytes);

                if dest != src {
                    emit_move(out, &dest, &src, *bytes, false);
                }
            }

            A64Instr::MovImm { dest, imm, bytes } => {
                let dest_loc = alloc.location(dest, bytes);
                match is_mem(&dest_loc) {
                    true => {
                        emit_wide_immediate(out, "x16", *imm, *bytes);
                        emit_store(out, "x16", &dest_loc, *bytes);
                    }
                    false => emit_wide_immediate(out, &dest_loc, *imm, *bytes),
                }
            }

            A64Instr::LdrParam {
                dest,
                fp_offset,
                bytes,
            } => {
                let suffix = mem_suffix(bytes);
                let dest = alloc.location(dest, bytes);
                match is_mem(&dest) {
                    true => {
                        emit!(out, "ldr{suffix}    x16, [x29, #{fp_offset}]");
                        emit_store(out, "x16", &dest, *bytes);
                    }
                    false => emit!(out, "ldr{suffix}    {dest}, [x29, #{fp_offset}]"),
                }
            }

            A64Instr::FMov { dest, src, bytes } => {
                let dest = alloc.location(dest, bytes);
                let src = alloc.location(src, bytes);

                if dest != src {
                    emit_move(out, &dest, &src, *bytes, true);
                }
            }

            A64Instr::FLiteral { dest, label, bytes } => {
                let dest = alloc.location(dest, bytes);
                let scratch = match is_mem(&dest) {
                    true => A64Reg::D16.name(*bytes),
                    false => dest.as_str(),
                };

                emit!(out, "adrp    x16, {label}");
                emit!(out, "ldr     {scratch}, [x16, :lo12:{label}]");

                if is_mem(&dest) {
                    emit_store(out, scratch, &dest, *bytes);
                }
            }

            A64Instr::Adr { dest, label } => {
                let dest = alloc.location(dest, &8);
                let scratch = if is_mem(&dest) { "x16" } else { dest.as_str() };

                emit!(out, "adrp    {scratch}, {label}");
                emit!(out, "add     {scratch}, {scratch}, :lo12:{label}");

                if is_mem(&dest) {
                    emit_store(out, scratch, &dest, 8);
                }
            }

            A64Instr::FieldLoad {
                dest,
                origin,
                offset,
                bytes,
            } => {
                let origin_offset = alloc.struct_offset(origin);
                let offset = origin_offset + offset;
                let dest = alloc.location(dest, bytes);
                let suffix = mem_suffix(bytes);

                match is_mem(&dest) {
                    true => {
                        emit!(out, "ldr{suffix}    x16, [x29, #{offset}]");
                        emit_store(out, "x16", &dest, *bytes);
                    }
                    false => emit!(out, "ldr{suffix}    {dest}, [x29, #{offset}]"),
                }
            }

            A64Instr::FieldStore {
                origin,
                src,
                offset,
                bytes,
                is_float,
            } => {
                let origin_offset = alloc.struct_offset(origin);
                let offset = origin_offset + offset;
                let dest = format!("[x29, #{offset}]");

                emit_store_operand(out, alloc, src, &dest, *bytes, *is_float);
            }

            A64Instr::StackAddr { dest, origin } => {
                let offset = alloc.struct_offset(origin);
                let dest = alloc.location(dest, &8);
                match is_mem(&dest) {
                    true => {
                        emit!(out, "add     x16, x29, #{offset}");
                        emit_store(out, "x16", &dest, 8);
                    }
                    false => emit!(out, "add     {dest}, x29, #{offset}"),
                }
            }

            A64Instr::PtrLoad {
                dest,
                ptr,
                offset,
                bytes,
                is_float,
            } => {
                let ptr = alloc.location(ptr, &8);
                let dest = alloc.location(dest, bytes);
                let addr = load_ptr_addr(out, &ptr);
                let suffix = mem_suffix(bytes);
                let scratch = match (*is_float, is_mem(&dest)) {
                    (true, true) => A64Reg::D16.name(*bytes),
                    (false, true) => A64Reg::X16.name(*bytes),
                    _ => dest.as_str(),
                };

                emit!(out, "ldr{suffix}    {scratch}, [{addr}, #{offset}]");
                if is_mem(&dest) {
                    emit_store(out, scratch, &dest, *bytes);
                }
            }

            A64Instr::PtrStore {
                ptr,
                src,
                offset,
                bytes,
                is_float,
            } => {
                let ptr = alloc.location(ptr, &8);
                let addr = load_ptr_addr(out, &ptr);
                let dest = format!("[{addr}, #{offset}]");

                emit_store_operand(out, alloc, src, &dest, *bytes, *is_float);
            }

            // integer arithmetic
            #[rustfmt::skip]
            A64Instr::Add { dest, lhs, rhs, bytes }
            | A64Instr::Sub { dest, lhs, rhs, bytes } => {
                let dest = alloc.location(dest, bytes);
                let lhs = alloc.location(lhs, bytes);
                let rhs = self.operand(alloc, rhs, bytes);

                match instruction {
                    A64Instr::Sub { .. } => emit!(out, "sub     {dest}, {lhs}, {rhs}"),
                    A64Instr::Add { .. } => emit!(out, "add     {dest}, {lhs}, {rhs}"),
                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            #[rustfmt::skip]
            A64Instr::Mul { dest, lhs, rhs, bytes }
            | A64Instr::SDiv { dest, lhs, rhs, bytes } => {
                let dest = alloc.location(dest, bytes);
                let lhs = alloc.location(lhs, bytes);
                let rhs = alloc.location(rhs, bytes);

                match instruction {
                    A64Instr::Mul { .. } => emit!(out, "mul     {dest}, {lhs}, {rhs}"),
                    A64Instr::SDiv { .. } => emit!(out, "sdiv     {dest}, {lhs}, {rhs}"),
                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            #[rustfmt::skip]
            A64Instr::And { dest, lhs, rhs, bytes }
            | A64Instr::Or { dest, lhs, rhs, bytes }
            | A64Instr::Eor { dest, lhs, rhs, bytes } => {
                let dest = alloc.location(dest, bytes);
                let lhs = alloc.location(lhs, bytes);
                let rhs = self.operand(alloc, rhs, bytes);

                match instruction {
                    A64Instr::And { .. } => emit!(out, "and     {dest}, {lhs}, {rhs}"),
                    A64Instr::Or { .. }  => emit!(out, "orr     {dest}, {lhs}, {rhs}"),
                    A64Instr::Eor { .. } => emit!(out, "eor     {dest}, {lhs}, {rhs}"),
                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            A64Instr::Cmp { lhs, rhs, bytes }
            | A64Instr::Cmn { lhs, rhs, bytes }
            | A64Instr::Tst { lhs, rhs, bytes } => {
                let lhs = alloc.location(lhs, bytes);
                let rhs = self.operand(alloc, rhs, bytes);

                match instruction {
                    A64Instr::Cmp { .. } => emit!(out, "cmp     {lhs}, {rhs}"),
                    A64Instr::Cmn { .. } => emit!(out, "cmn     {lhs}, {rhs}"),
                    A64Instr::Tst { .. } => emit!(out, "tst     {lhs}, {rhs}"),
                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            A64Instr::Cset { dest, cond } => {
                let dest = alloc.location(dest, &4);
                emit!(out, "cset    {dest}, {}", cond.as_str());
            }

            A64Instr::FCmp { lhs, rhs, bytes } => {
                let lhs = alloc.location(lhs, bytes);
                let rhs = alloc.location(rhs, bytes);

                emit!(out, "fcmp    {lhs}, {rhs}");
            }

            #[rustfmt::skip]
            A64Instr::FAdd { dest, lhs, rhs, bytes }
            | A64Instr::FSub { dest, lhs, rhs, bytes }
            | A64Instr::FMul { dest, lhs, rhs, bytes }
            | A64Instr::FDiv { dest, lhs, rhs, bytes } => {
                let dest = alloc.location(dest, bytes);
                let lhs = alloc.location(lhs, bytes);
                let rhs = alloc.location(rhs, bytes);

                match instruction {
                    A64Instr::FAdd { .. } => emit!(out, "fadd    {dest}, {lhs}, {rhs}"),
                    A64Instr::FSub { .. } => emit!(out, "fsub    {dest}, {lhs}, {rhs}"),
                    A64Instr::FMul { .. } => emit!(out, "fmul    {dest}, {lhs}, {rhs}"),
                    A64Instr::FDiv { .. } => emit!(out, "fdiv    {dest}, {lhs}, {rhs}"),
                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            A64Instr::Neg { dest, src, bytes } | A64Instr::FNeg { dest, src, bytes } => {
                let dest = alloc.location(dest, bytes);
                let src = alloc.location(src, bytes);

                match instruction {
                    A64Instr::Neg { .. } => emit!(out, "neg     {dest}, {src}"),
                    A64Instr::FNeg { .. } => emit!(out, "fneg    {dest}, {src}"),
                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            A64Instr::Call {
                target,
                moves,
                ret,
                stack_args,
                ..
            } => {
                let n_stack = stack_args.len();

                if n_stack > 0 {
                    // pre-allocate stack space (16-byte aligned)
                    let slot_bytes = n_stack * 8;
                    let aligned = (slot_bytes + 15) & !15;
                    emit!(out, "sub     sp, sp, #{aligned}");

                    for (i, (operand, mt)) in stack_args.iter().enumerate() {
                        let bytes = mt.bytes();
                        let offset = i * 8;

                        match operand {
                            A64Operand::Imm(n) => {
                                emit!(out, "mov     x16, #{n}");
                                emit!(out, "str     x16, [sp, #{offset}]");
                            }
                            A64Operand::VReg(vreg) => {
                                let src = alloc.location(vreg, &bytes);
                                let suffix = mem_suffix(&bytes);
                                emit!(out, "str{suffix}    {src}, [sp, #{offset}]");
                            }
                            A64Operand::Label(label) => match mt {
                                MachineType::Float { .. } => {
                                    let scratch = A64Reg::D16.name(bytes);
                                    let suffix = mem_suffix(&bytes);
                                    emit!(out, "adrp    x16, {label}");
                                    emit!(out, "ldr     {scratch}, [x16, :lo12:{label}]");
                                    emit!(out, "str{suffix}    {scratch}, [sp, #{offset}]");
                                }
                                MachineType::Int { .. } => {
                                    emit!(out, "adrp    x16, {label}");
                                    emit!(out, "add     x16, x16, :lo12:{label}");
                                    emit!(out, "str     x16, [sp, #{offset}]");
                                }
                                _ => unimplemented!(),
                            },
                        }
                    }
                }

                let n_moves = moves.len();
                if n_moves > 0 {
                    let slot_bytes = n_moves * 8;
                    let aligned = (slot_bytes + 15) & !15;
                    emit!(out, "sub     sp, sp, #{aligned}");

                    for (idx, (vreg, _)) in moves.iter().enumerate() {
                        let bytes = self.reg_bytes(vreg);
                        let src = alloc.location(vreg, &bytes);
                        let suffix = mem_suffix(&bytes);
                        let offset = idx * 8;
                        emit!(out, "str{suffix}    {src}, [sp, #{offset}]");
                    }

                    for (idx, (vreg, reg)) in moves.iter().enumerate() {
                        let bytes = self.reg_bytes(vreg);
                        let dest = reg.name(bytes);
                        let suffix = mem_suffix(&bytes);
                        let offset = idx * 8;
                        emit!(out, "ldr{suffix}    {dest}, [sp, #{offset}]");
                    }

                    emit!(out, "add     sp, sp, #{aligned}");
                }

                emit!(out, "bl      {target}");

                // reclaim stack arguments
                if n_stack > 0 {
                    let slot_bytes = n_stack * 8;
                    let aligned = (slot_bytes + 15) & !15;
                    emit!(out, "add     sp, sp, #{aligned}");
                }

                if let Some(ret) = ret {
                    let bytes = self.reg_bytes(ret);
                    let is_float = self.is_float(ret);
                    let class = match is_float {
                        true => RegClass::Float,
                        _ => RegClass::Int,
                    };

                    if let Some(abi_ret) = AArch64::ret(class) {
                        let src = abi_ret.name(bytes);
                        let dest = alloc.location(ret, &bytes);

                        if src != dest {
                            let mnemonic = if is_float { "fmov" } else { "mov" };
                            emit!(out, "{mnemonic}    {dest}, {src}");
                        }
                    }
                }
            }

            A64Instr::Syscall {
                id: syscall_id,
                moves,
                ret,
                ..
            } => {
                for (operand, reg, bytes) in moves {
                    let dest = reg.name(*bytes);

                    match operand {
                        A64Operand::Label(label) => {
                            emit!(out, "adrp    {dest}, {label}");
                            emit!(out, "add     {dest}, {dest}, :lo12:{label}");
                        }
                        _ => {
                            let src = self.operand(alloc, operand, bytes);
                            if src != dest {
                                emit!(out, "mov     {dest}, {src}");
                            }
                        }
                    }
                }

                emit!(out, "mov     x8, #{syscall_id}");
                emit!(out, "svc     #0");

                if let Some(ret) = ret {
                    let bytes = self.reg_bytes(ret);
                    let src = A64Reg::X0.name(bytes);
                    let dest = alloc.location(ret, &bytes);

                    if src != dest {
                        emit!(out, "mov     {dest}, {src}");
                    }
                }
            }
        }
    }

    fn emit_terminator(
        &self,
        alloc: &Allocation<AArch64>,
        term: &Term,
        name: &str,
        epilogue: &str,
        is_last: bool,
        out: &mut String,
    ) {
        match term {
            Term::Return(None) if !is_last => emit!(out, "b       {epilogue}"),
            Term::Return(None) => {}
            Term::Jump(block) => emit!(out, "b       .L_block_{name}_{}", block.0),

            Term::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let condition = alloc.location(cond, &4);

                emit!(out, "cbnz    {condition}, .L_block_{name}_{}", then_block.0);
                emit!(out, "b       .L_block_{name}_{}", else_block.0);
            }

            Term::Return(Some(vreg)) => {
                let bytes = self.reg_bytes(vreg);
                let is_float = self.is_float(vreg);
                let class = match is_float {
                    true => RegClass::Float,
                    _ => RegClass::Int,
                };

                if let Some(ret_reg) = AArch64::ret(class) {
                    let src = alloc.location(vreg, &bytes);
                    let dest = ret_reg.name(bytes);

                    if src != dest {
                        let mnemonic = if is_float { "fmov" } else { "mov" };
                        emit!(out, "{mnemonic}    {dest}, {src}");
                    }
                }

                if !is_last {
                    emit!(out, "b       {epilogue}");
                }
            }
        }
    }

    #[inline(always)]
    fn reg_bytes(&self, vreg: &VReg) -> u8 {
        self.vreg_types.get(vreg.0 as usize).map(|typ| typ.bytes()).unwrap_or(4)
    }

    #[inline(always)]
    fn is_float(&self, vreg: &VReg) -> bool {
        matches!(
            self.vreg_types.get(vreg.0 as usize),
            Some(MachineType::Float { .. })
        )
    }

    #[inline(always)]
    fn operand<'s>(
        &self,
        alloc: &Allocation<AArch64>,
        operand: &'s A64Operand,
        bytes: &u8,
    ) -> String {
        match operand {
            A64Operand::VReg(vreg) => alloc.location(vreg, bytes),
            A64Operand::Imm(n) => format!("#{n}"),
            A64Operand::Label(s) => s.clone(),
        }
    }
}

impl Allocation<AArch64> {
    fn location(&self, vreg: &VReg, bytes: &u8) -> String {
        match self.location_of(vreg) {
            Location::Reg(reg) => reg.name(*bytes).to_string(),
            Location::Stack(offset) => format!("[x29, #{}]", self.stack_offset(offset)),
        }
    }

    #[inline(always)]
    fn struct_offset(&self, vreg: &VReg) -> i32 {
        match self.location_of(vreg) {
            Location::Stack(offset) => self.stack_offset(offset),
            Location::Reg(_) => panic!("struct VReg unexpectedly allocated to a register"),
        }
    }

    fn stack_offset(&self, offset: i32) -> i32 {
        offset - callee_saved_area(self) as i32
    }
}

/// emit a wide immediate using MOVZ + MOVK sequence
///
/// AArch64 can only load 16-bit chunks at a time into a register
/// for values that fit in 16 bits we use a single MOV (alias for MOVZ)
/// for wider values we emit MOVZ for the lowest chunk and MOVK for each
/// subsequent non-zero 16-bit chunk
fn emit_wide_immediate(out: &mut String, dest: &str, value: i64, bytes: u8) {
    let bits = value as u64;

    // for small non-negative values a single MOV is sufficient :D
    if value >= 0 && value <= 0xFFFF {
        emit!(out, "mov     {dest}, #{value}");
        return;
    }

    // for small negative values in 32-bit context use MOVN
    if bytes <= 4 && value < 0 && value >= -0x10000 {
        let inverted = (!value as u64) & 0xFFFF;
        emit!(out, "movn    {dest}, #{inverted}");
        return;
    }

    // general case MOVZ lowest chunk, MOVK upper chunks
    let n_chunks = if bytes <= 4 { 2 } else { 4 };

    let mut first = true;
    for shift in 0..n_chunks {
        let chunk = (bits >> (shift * 16)) & 0xFFFF;

        if first {
            emit!(out, "movz    {dest}, #{chunk}, lsl #{}", shift * 16);
            first = false;
        } else if chunk != 0 {
            emit!(out, "movk    {dest}, #{chunk}, lsl #{}", shift * 16);
        }
    }
}

fn emit_move(out: &mut String, dest: &str, src: &str, bytes: u8, is_float: bool) {
    match (is_mem(dest), is_mem(src), is_float) {
        (false, false, true) => emit!(out, "fmov    {dest}, {src}"),
        (false, false, false) => emit!(out, "mov     {dest}, {src}"),
        (false, true, _) => emit_load(out, dest, src, bytes),
        (true, false, _) => emit_store(out, src, dest, bytes),
        (true, true, true) => {
            let scratch = A64Reg::D16.name(bytes);
            emit_load(out, scratch, src, bytes);
            emit_store(out, scratch, dest, bytes);
        }
        (true, true, false) => {
            let scratch = A64Reg::X16.name(bytes);
            emit_load(out, scratch, src, bytes);
            emit_store(out, scratch, dest, bytes);
        }
    }
}

fn emit_load(out: &mut String, dest: &str, src: &str, bytes: u8) {
    let suffix = mem_suffix(&bytes);
    emit!(out, "ldr{suffix}    {dest}, {src}");
}

fn emit_store(out: &mut String, src: &str, dest: &str, bytes: u8) {
    let suffix = mem_suffix(&bytes);
    emit!(out, "str{suffix}    {src}, {dest}");
}

fn load_ptr_addr<'s>(out: &mut String, ptr: &'s str) -> &'s str {
    match is_mem(ptr) {
        true => {
            emit!(out, "ldr     x16, {ptr}");
            "x16"
        }
        false => ptr,
    }
}

fn load_src_if_mem<'s>(out: &mut String, src: &'s str, bytes: u8, is_float: bool) -> &'s str {
    match is_mem(src) {
        true => {
            let scratch = match is_float {
                true => A64Reg::D16.name(bytes),
                false => A64Reg::X16.name(bytes),
            };
            emit_load(out, scratch, src, bytes);
            scratch
        }
        false => src,
    }
}

fn emit_store_operand(
    out: &mut String,
    alloc: &Allocation<AArch64>,
    src: &A64Operand,
    dest: &str,
    bytes: u8,
    is_float: bool,
) {
    match src {
        A64Operand::VReg(vreg) => {
            let src = alloc.location(vreg, &bytes);
            let src = load_src_if_mem(out, &src, bytes, is_float);
            emit_store(out, &src, dest, bytes);
        }
        A64Operand::Imm(n) => {
            emit!(out, "mov     x16, #{n}");
            emit_store(out, "x16", dest, bytes);
        }
        A64Operand::Label(label) => {
            emit!(out, "adrp    x16, {label}");
            emit!(out, "ldr     x16, [x16, :lo12:{label}]");
            emit_store(out, "x16", dest, bytes);
        }
    }
}

#[inline(always)]
fn is_mem(location: &str) -> bool {
    location.starts_with('[')
}

#[inline(always)]
fn emit_save_regs(out: &mut String, regs: &[A64Reg]) {
    for pair in regs.chunks(2) {
        match pair {
            [a, b] => emit!(out, "stp     {}, {}, [sp, #-16]!", a.name(8), b.name(8)),
            [a] => emit!(out, "str     {}, [sp, #-16]!", a.name(8)),
            _ => unsafe { std::hint::unreachable_unchecked() },
        }
    }
}

fn emit_restore_regs(out: &mut String, regs: &[A64Reg]) {
    let mut len = regs.len();

    if len % 2 == 1 {
        len -= 1;
        emit!(out, "ldr     {}, [sp], #16", regs[len].name(8));
    }

    while len > 0 {
        len -= 2;
        emit!(
            out,
            "ldp     {}, {}, [sp], #16",
            regs[len].name(8),
            regs[len + 1].name(8)
        );
    }
}

fn callee_saved_regs(alloc: &Allocation<AArch64>, class: RegClass) -> Vec<A64Reg> {
    alloc
        .used_callee_saved
        .iter()
        .copied()
        .filter(|reg| reg.class() == class)
        .collect()
}

fn callee_saved_area(alloc: &Allocation<AArch64>) -> u32 {
    let ints = callee_saved_regs(alloc, RegClass::Int).len() as u32;
    let floats = callee_saved_regs(alloc, RegClass::Float).len() as u32;

    align_pair(ints * 8) + align_pair(floats * 8)
}

const fn align_pair(bytes: u32) -> u32 {
    (bytes + 15) & !15
}

#[inline(always)]
const fn mem_suffix<'s>(bytes: &u8) -> &'s str {
    match bytes {
        1 => "b",
        2 => "h",
        _ => "",
    }
}
