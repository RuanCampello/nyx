//! LIR + Allocation -> GAS (AT&T syntax) assembly emission.
//!
//! After register allocation every VReg has a concrete Location (register or
//! stack slot).
//!
//! Emission is meant to be very mechanical: look up, add mnemonic.

use crate::{
    emit, label,
    lir::{
        self, Function, MachineType, Term, VReg,
        regalloc::{Allocation, Location},
        target::{
            Emittable, ParallelMove, PhysicalReg, RegClass, Target, resolve_parallel_moves,
            x86_64::{X86_64, X86Instr, X86Operand, X86Reg},
        },
    },
};
use std::{borrow::Cow, fmt::Write};

impl Emittable<X86_64> for Function<X86_64> {
    fn emit(&self, alloc: Allocation<X86_64>, out: &mut String) {
        let name = &self.name;

        let mut max_outgoing_args = 0;
        for block in &self.blocks {
            for instr in &block.instructions {
                if let X86Instr::Call { stack_args, .. } = instr {
                    let size = stack_args.len() * 8;
                    if size > max_outgoing_args {
                        max_outgoing_args = size;
                    }
                }
            }
        }
        let callee_saved_size = alloc.used_callee_saved.len() * 8;
        let total_size = alloc.frame_size as usize + max_outgoing_args + callee_saved_size;
        let aligned_total = (total_size + 15) & !15;
        let frame_size = (aligned_total - callee_saved_size) as u32;

        let epilogue = format!(".L_{name}_epilogue");

        Self::emit_prologue(&alloc, name, frame_size, out);
        self.emit_body(&alloc, name, &epilogue, out);
        Self::emit_epilogue(&alloc, &epilogue, frame_size, out);
        self.emit_rodata(out);
    }

    #[inline(always)]
    fn start(out: &mut String, main: &str) {
        label!(out, ".globl _start");
        label!(out, "_start:");

        emit!(out, "call    {main}");
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
            let align = if is_32 {
                4
            } else {
                8
            };

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
    fn emit_instruction(
        &self,
        instruction: &X86Instr,
        alloc: &Allocation<X86_64>,
        out: &mut String,
    ) {
        use lir::target::x86_64::X86Instr as Inst;

        match instruction {
            Inst::Mov { dest, src, bytes } => {
                let suffix = suffix(bytes);
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes);

                mov_or_scratch(out, &src, &dest, suffix, false);
            }

            Inst::MovFloat { dest, src, bytes } => {
                let suffix = float_suffix(bytes);
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes);

                mov_or_scratch(out, &src, &dest, suffix, true);
            }

            Inst::MovFromStack { dest, rbp_offset, bytes } => {
                let suffix = suffix(bytes);
                let dest = alloc.location(dest, bytes);
                emit!(out, "mov{suffix}    {rbp_offset}(%rbp), {dest}");
            }

            Inst::Lea { dest, src } => {
                let dest = alloc.location(dest, &8);
                let src = self.operand(alloc, src, &8);

                emit!(out, "leaq   {src}, {dest}");
            }

            Inst::StackAddr { dest, origin } => {
                let dest = alloc.location(dest, &8);
                let offset = alloc.struct_offset(origin);

                emit!(out, "leaq   {offset}(%rbp), {dest}");
            }

            Inst::Movzx { dest, src, src_bytes, dest_bytes } => {
                let dest_loc = alloc.location(dest, dest_bytes);
                let src_op = self.operand(alloc, src, src_bytes);

                let needs_scratch = dest_loc.contains("(%rbp)");

                if *src_bytes == 4 && *dest_bytes == 8 {
                    match needs_scratch {
                        true => {
                            emit!(out, "movl    {src_op}, %r11d");
                            emit!(out, "movq    %r11, {dest_loc}");
                        }
                        false => {
                            let dest_32 = alloc.location(dest, &4);
                            emit!(out, "movl    {src_op}, {dest_32}");
                        }
                    }
                } else {
                    let name = zx_instr_name(*src_bytes, *dest_bytes);
                    match needs_scratch {
                        true => {
                            let scratch = scratch_gpr(suffix(dest_bytes));
                            emit!(out, "{name}   {src_op}, {scratch}");
                            let dest_suffix = suffix(dest_bytes);
                            emit!(out, "mov{dest_suffix}    {scratch}, {dest_loc}");
                        }
                        _ => emit!(out, "{name}   {src_op}, {dest_loc}"),
                    }
                }
            }

            Inst::Movsx { dest, src, src_bytes, dest_bytes } => {
                let dest_loc = alloc.location(dest, dest_bytes);
                let src_op = self.operand(alloc, src, src_bytes);

                let name = sx_instr_name(*src_bytes, *dest_bytes);
                if dest_loc.contains("(%rbp)") {
                    let scratch = scratch_gpr(suffix(dest_bytes));
                    emit!(out, "{name}   {src_op}, {scratch}");
                    let dest_suffix = suffix(dest_bytes);
                    emit!(out, "mov{dest_suffix}    {scratch}, {dest_loc}");
                } else {
                    emit!(out, "{name}   {src_op}, {dest_loc}");
                }
            }

            Inst::FieldLoad { dest, origin, offset, bytes, is_float } => {
                let origin_offset = alloc.struct_offset(origin);
                let offset = origin_offset + offset;
                let dest = alloc.location(dest, bytes);

                let suffix = typed_suffix(bytes, *is_float);

                emit!(out, "mov{suffix}    {offset}(%rbp), {dest}");
            }

            Inst::FieldStore { origin, src, offset, bytes, is_float } => {
                let origin_offset = alloc.struct_offset(origin);
                let offset = origin_offset + offset;
                let src = self.operand(alloc, src, bytes);
                let suffix = typed_suffix(bytes, *is_float);

                let need_scratch = src.contains("(%rbp)");

                match is_float {
                    true => match need_scratch {
                        true => {
                            emit!(out, "mov{suffix}    {src}, %xmm15");
                            emit!(out, "mov{suffix}    %xmm15, {offset}(%rbp)");
                        }
                        false => emit!(out, "mov{suffix}   {src}, {offset}(%rbp)"),
                    },

                    false => match need_scratch {
                        true => {
                            let scratch = if *bytes == 8 {
                                "%r11"
                            } else {
                                "%r11d"
                            };
                            emit!(out, "mov{suffix}    {src}, {scratch}");
                            emit!(out, "mov{suffix}    {scratch}, {offset}(%rbp)");
                        }

                        false => emit!(out, "mov{suffix}    {src}, {offset}(%rbp)"),
                    },
                }
            }

            Inst::PtrLoad { dest, ptr, offset, bytes, is_float } => {
                let ptr = alloc.location(ptr, &8);
                let dest = alloc.location(dest, bytes);
                let suffix = typed_suffix(bytes, *is_float);

                match ptr.contains("(%rbp)") {
                    true => {
                        emit!(out, "movq    {ptr}, %r11");
                        emit!(out, "mov{suffix}    {offset}(%r11), {dest}");
                    }
                    false => emit!(out, "mov{suffix}    {offset}({ptr}), {dest}"),
                }
            }

            Inst::PtrStore { ptr, src, offset, bytes, is_float } => {
                let ptr = alloc.location(ptr, &8);
                let src = self.operand(alloc, src, bytes);
                let suffix = typed_suffix(bytes, *is_float);

                // x86_64 can't do memory-to-memory moves
                // so we need to check if the operands are on the stack and
                // move them to scratch register :/
                let ptr_is_spilled = ptr.contains("(%rbp)");
                let src_is_spilled = src.contains("(%rbp)");

                match (ptr_is_spilled, src_is_spilled, *is_float) {
                    (true, true, true) => {
                        emit!(out, "movq    {ptr}, %r11");
                        emit!(out, "mov{suffix}    {src}, %xmm15");
                        emit!(out, "mov{suffix}    %xmm15, {offset}(%r11)");
                    }
                    (true, true, false) => {
                        emit!(out, "movq    {ptr}, %r10");
                        let scratch = if *bytes == 8 {
                            "%r11"
                        } else {
                            "%r11d"
                        };
                        emit!(out, "mov{suffix}    {src}, {scratch}");
                        emit!(out, "mov{suffix}    {scratch}, {offset}(%r10)");
                    }
                    (true, false, _) => {
                        emit!(out, "movq    {ptr}, %r11");
                        emit!(out, "mov{suffix}    {src}, {offset}(%r11)");
                    }
                    (false, true, true) => {
                        emit!(out, "mov{suffix}    {src}, %xmm15");
                        emit!(out, "mov{suffix}    %xmm15, {offset}({ptr})");
                    }
                    (false, true, false) => {
                        let scratch = if *bytes == 8 {
                            "%r11"
                        } else {
                            "%r11d"
                        };
                        emit!(out, "mov{suffix}    {src}, {scratch}");
                        emit!(out, "mov{suffix}    {scratch}, {offset}({ptr})");
                    }
                    (false, false, _) => emit!(out, "mov{suffix}    {src}, {offset}({ptr})"),
                }
            }

            Inst::Add { dest, src, bytes }
            | Inst::Sub { dest, src, bytes }
            | Inst::Imul { dest, src, bytes }
            | Inst::AddFloat { dest, src, bytes }
            | Inst::SubFloat { dest, src, bytes }
            | Inst::MulFloat { dest, src, bytes }
            | Inst::DivFloat { dest, src, bytes }
            | Inst::And { dest, src, bytes }
            | Inst::Or { dest, src, bytes }
            | Inst::Xor { dest, src, bytes } => {
                let suffix = typed_suffix(bytes, self.is_float(dest));
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes);

                match instruction {
                    Inst::Add { .. } | Inst::AddFloat { .. } => {
                        emit!(out, "add{suffix}    {src}, {dest}")
                    }
                    Inst::Sub { .. } | Inst::SubFloat { .. } => {
                        emit!(out, "sub{suffix}    {src}, {dest}")
                    }
                    Inst::Imul { .. } => emit!(out, "imul{suffix}    {src}, {dest}"),
                    Inst::MulFloat { .. } => emit!(out, "mul{suffix}    {src}, {dest}"),
                    Inst::DivFloat { .. } => emit!(out, "div{suffix}    {src}, {dest}"),
                    Inst::And { .. } => emit!(out, "and{suffix}    {src}, {dest}"),
                    Inst::Or { .. } => emit!(out, "or{suffix}    {src}, {dest}"),
                    Inst::Xor { .. } => emit!(out, "xor{suffix}    {src}, {dest}"),

                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            Inst::Neg { dest, bytes } => {
                let suffix = suffix(bytes);
                let dest = alloc.location(dest, bytes);

                emit!(out, "neg{suffix}    {dest}");
            }

            Inst::Not { dest, bytes } => {
                let suffix = suffix(bytes);
                let dest = alloc.location(dest, bytes);

                emit!(out, "not{suffix}    {dest}");
            }

            #[rustfmt::skip]
            Inst::Shl { dest, src, bytes, .. }
            | Inst::Shr { dest, src, bytes, .. }
            | Inst::Sar { dest, src, bytes, .. } => {
                let suffix = suffix(bytes);
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, &1);

                match instruction {
                    Inst::Shl { .. } => emit!(out, "shl{suffix}    {src}, {dest}"),
                    Inst::Shr { .. } => emit!(out, "shr{suffix}    {src}, {dest}"),
                    Inst::Sar { .. } => emit!(out, "sar{suffix}    {src}, {dest}"),
                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }

            Inst::IDiv { result, dividend, divisor, bytes, .. } => {
                let suffix = suffix(bytes);
                let rax = format!("%{}", X86Reg::Rax.name(*bytes));
                let extend = match bytes {
                    1 => "cbtw",
                    2 => "cwtd",
                    4 => "cltd",
                    8 => "cqto",
                    _ => panic!("invalid idiv size: {bytes}"),
                };
                let dividend = alloc.location(dividend, bytes);
                let result = alloc.location(result, bytes);

                if dividend != rax {
                    emit!(out, "mov{suffix}    {dividend}, {rax}");
                }
                emit!(out, "{extend}");

                match divisor {
                    X86Operand::Imm(_) => {
                        let div = self.operand(alloc, divisor, bytes);
                        emit!(out, "subq    $8, %rsp");
                        emit!(out, "mov{suffix}    {div}, (%rsp)");
                        emit!(out, "idiv{suffix}    (%rsp)");
                        emit!(out, "addq    $8, %rsp");
                    }

                    _ => {
                        let div = self.operand(alloc, divisor, bytes);
                        emit!(out, "idiv{suffix}    {div}");
                    }
                }

                if result != rax {
                    emit!(out, "mov{suffix}    {rax}, {result}");
                }
            }

            Inst::XorFloat { dest, src, bytes } => {
                let operand = if *bytes == 4 {
                    "xorps"
                } else {
                    "xorpd"
                };
                let dest = alloc.location(dest, bytes);
                let src = self.operand(alloc, src, bytes);

                emit!(out, "{operand}   {src}, {dest}");
            }

            Inst::Ucomis { lhs, rhs, bytes, .. } => {
                let suffix = float_suffix(bytes);
                let lhs = alloc.location(lhs, bytes);
                let rhs = self.operand(alloc, rhs, bytes);

                // xmm15 is reserved scratch; safe to clobber here
                emit!(out, "mov{suffix}    {lhs}, %xmm15");
                emit!(out, "ucomi{suffix}  {rhs}, %xmm15");
            }

            Inst::Setcc { dest, condition } => {
                let dest = alloc.location(dest, &1);

                emit!(out, "set{}  {dest}", condition.as_str())
            }

            Inst::Test { lhs, rhs, bytes, .. } | Inst::Cmp { lhs, rhs, bytes, .. } => {
                let suffix = suffix(bytes);
                let lhs = alloc.location(lhs, bytes);
                let rhs = self.operand(alloc, rhs, bytes);

                match matches!(instruction, Inst::Cmp { .. }) {
                    true => emit!(out, "cmp{suffix}    {rhs}, {lhs}"),
                    _ => emit!(out, "test{suffix}    {rhs}, {lhs}"),
                }
            }

            Inst::Call { target, moves, ret, stack_args, .. } => {
                let n_stack = stack_args.len();

                if n_stack > 0 {
                    for (i, (operand, mt)) in stack_args.iter().enumerate() {
                        let bytes = mt.bytes();
                        let is_float = matches!(mt, MachineType::Float { .. });
                        let offset = i * 8;
                        let dest = format!("{offset}(%rsp)");

                        match operand {
                            X86Operand::Imm(n) => {
                                match *n >= i32::MIN as i64 && *n <= i32::MAX as i64 {
                                    true => emit!(out, "movq    ${n}, {dest}"),
                                    _ => {
                                        emit!(out, "movabsq ${n}, %r11");
                                        emit!(out, "movq    %r11, {dest}");
                                    }
                                }
                            }
                            X86Operand::RipRel(label) => match is_float {
                                true => {
                                    let suffix = float_suffix(&bytes);
                                    emit!(out, "mov{suffix}    {label}, %xmm15");
                                    emit!(out, "mov{suffix}    %xmm15, {dest}");
                                }
                                false => {
                                    emit!(out, "leaq    {label}, %r11");
                                    emit!(out, "movq    %r11, {dest}");
                                }
                            },
                            X86Operand::VReg(vreg) => {
                                let src = alloc.location(vreg, &bytes);

                                match is_float {
                                    true => {
                                        let suffix = float_suffix(&bytes);
                                        match src.contains("(%rbp)") {
                                            true => {
                                                emit!(out, "mov{suffix}    {src}, %xmm15");
                                                emit!(out, "mov{suffix}    %xmm15, {dest}");
                                            }

                                            _ => emit!(out, "mov{suffix}    {src}, {dest}"),
                                        }
                                    }
                                    false => {
                                        let suffix = suffix(&bytes);
                                        match bytes < 8 {
                                            true => {
                                                emit!(out, "movs{suffix}q   {src}, %r11");
                                                emit!(out, "movq    %r11, {dest}");
                                            }
                                            _ => match src.contains("(%rbp)") {
                                                true => {
                                                    emit!(out, "movq    {src}, %r11");
                                                    emit!(out, "movq    %r11, {dest}");
                                                }
                                                _ => {
                                                    emit!(out, "movq    {src}, {dest}");
                                                }
                                            },
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                #[rustfmt::skip]
                let arg_moves: Vec<_> = moves
                    .iter()
                    .map(|(vreg, reg)| {
                        let bytes = self.reg_bytes(vreg);
                        let is_float = self.is_float(vreg);
                        let src = alloc.location(vreg, &bytes);
                        let src_reg = alloc.reg(vreg);
                        let dest = format!("%{}", reg.name(bytes));

                        ParallelMove { src, src_reg, dest, dest_reg: *reg, bytes, is_float }
                    })
                    .collect();

                resolve_parallel_moves(
                    arg_moves,
                    out,
                    |out, m| {
                        let suffix = typed_suffix(&m.bytes, m.is_float);
                        mov_or_scratch(out, &m.src, &m.dest, suffix, m.is_float);
                    },
                    |out, m| {
                        let suffix = typed_suffix(&m.bytes, m.is_float);
                        let scratch = match m.is_float {
                            true => "%xmm15",
                            false => scratch_gpr(suffix),
                        };

                        emit!(out, "mov{suffix}    {}, {scratch}", m.src);
                        m.src = scratch.to_string();
                        m.src_reg = None;
                    },
                );

                emit!(out, "call    {target}");

                if let Some(ret) = ret {
                    let bytes = self.reg_bytes(ret);
                    let is_float = self.is_float(ret);
                    let class = match is_float {
                        true => RegClass::Float,
                        _ => RegClass::Int,
                    };

                    if let Some(abi_ret) = X86_64::ret(class) {
                        let suffix = typed_suffix(&bytes, is_float);
                        let src = format!("%{}", abi_ret.name(bytes));
                        let dest = alloc.location(ret, &bytes);

                        mov_or_scratch(out, &src, &dest, suffix, is_float);
                    }
                }
            }

            Inst::Syscall { id: syscall_id, moves, ret, .. } => {
                for (operand, reg, bytes) in moves {
                    let dest = format!("%{}", reg.name(*bytes));

                    match operand {
                        X86Operand::RipRel(src) => emit!(out, "leaq   {src}, {dest}"),
                        _ => {
                            let src = self.operand(alloc, operand, bytes);
                            let suffix = suffix(bytes);
                            mov_or_scratch(out, &src, &dest, suffix, false);
                        }
                    }
                }

                emit!(out, "movl    ${syscall_id}, %eax");
                emit!(out, "syscall");

                if let Some(ret) = ret {
                    let bytes = self.reg_bytes(ret);
                    let is_float = self.is_float(ret);
                    let suffix = typed_suffix(&bytes, is_float);
                    let src = format!("%{}", X86Reg::Rax.name(bytes));
                    let dest = alloc.location(ret, &bytes);

                    mov_or_scratch(out, &src, &dest, suffix, is_float);
                }
            }
        }
    }

    fn emit_terminator(
        &self,
        alloc: &Allocation<X86_64>,
        term: &Term,
        name: &str,
        epilogue: &str,
        is_last: bool,
        out: &mut String,
    ) {
        match term {
            Term::Return(None) if !is_last => emit!(out, "jmp       {epilogue}"),
            Term::Return(None) => {}
            Term::Jump(block) => emit!(out, "jmp      .L_block_{name}_{}", block.0),

            Term::Branch { cond, then_block, else_block } => {
                let bytes = self.reg_bytes(cond);
                let condition = alloc.location(cond, &bytes);
                let suffix = suffix(&bytes);

                match condition.contains("(%rbp)") {
                    true => {
                        let scratch = match bytes {
                            8 => "%r11",
                            4 => "%r11d",
                            2 => "%r11w",
                            _ => "%r11b",
                        };
                        emit!(out, "mov{suffix}    {condition}, {scratch}");
                        emit!(out, "test{suffix}    {scratch}, {scratch}");
                    }
                    _ => emit!(out, "test{suffix}    {condition}, {condition}"),
                }
                emit!(out, "jne         .L_block_{name}_{}", then_block.0);
                emit!(out, "jmp         .L_block_{name}_{}", else_block.0);
            }

            Term::Return(Some(vreg)) => {
                let bytes = self.reg_bytes(vreg);
                let is_float = self.is_float(vreg);
                let class = match is_float {
                    true => RegClass::Float,
                    _ => RegClass::Int,
                };

                if let Some(ret_reg) = X86_64::ret(class) {
                    let suffix = typed_suffix(&bytes, is_float);
                    let src = alloc.location(vreg, &bytes);
                    let dest = format!("%{}", ret_reg.name(bytes));

                    mov_or_scratch(out, &src, &dest, suffix, is_float);
                }

                if !is_last {
                    emit!(out, "jmp        {epilogue}");
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
        matches!(self.vreg_types.get(vreg.0 as usize), Some(MachineType::Float { .. }))
    }

    #[inline(always)]
    fn operand<'s>(
        &self,
        alloc: &Allocation<X86_64>,
        operand: &'s X86Operand,
        bytes: &u8,
    ) -> Cow<'s, str> {
        match operand {
            X86Operand::VReg(vreg) => Cow::Owned(alloc.location(vreg, bytes)),
            X86Operand::Imm(n) => Cow::Owned(format!("${n}")),
            X86Operand::RipRel(s) => Cow::Borrowed(s.as_str()),
        }
    }
}

impl Allocation<X86_64> {
    #[inline(always)]
    fn location(&self, vreg: &VReg, bytes: &u8) -> String {
        match self.location_of(vreg) {
            Location::Reg(reg) => format!("%{}", reg.name(*bytes)),
            Location::Stack(offset) => {
                format!("{}(%rbp)", offset - (self.used_callee_saved.len() as i32 * 8))
            }
        }
    }

    fn struct_offset(&self, vreg: &VReg) -> i32 {
        match self.location_of(vreg) {
            Location::Stack(offset) => offset - (self.used_callee_saved.len() as i32 * 8),
            _ => panic!("struct VReg unexpectedly allocated to a register"),
        }
    }

    #[inline(always)]
    fn reg(&self, vreg: &VReg) -> Option<X86Reg> {
        match self.location_of(vreg) {
            Location::Reg(reg) => Some(reg),
            Location::Stack(_) => None,
        }
    }
}

#[inline(always)]
const fn typed_suffix<'s>(bytes: &u8, is_float: bool) -> &'s str {
    match is_float {
        true => float_suffix(bytes),
        false => suffix(bytes),
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
                let scratch = scratch_gpr(suffix);
                emit!(out, "mov{suffix}    {src}, {scratch}");
                emit!(out, "mov{suffix}    {scratch}, {dest}");
            }
        },

        false => emit!(out, "mov{suffix}    {src}, {dest}"),
    }
}

#[inline(always)]
fn scratch_gpr<'s>(suffix: &str) -> &'s str {
    match suffix {
        "q" => "%r11",
        "l" => "%r11d",
        "w" => "%r11w",
        "b" => "%r11b",
        _ => panic!("invalid integer register suffix: {suffix}"),
    }
}

#[inline(always)]
fn zx_instr_name<'s>(src_bytes: u8, dest_bytes: u8) -> &'s str {
    match (src_bytes, dest_bytes) {
        (1, 2) => "movzbw",
        (1, 4) => "movzbl",
        (1, 8) => "movzbq",
        (2, 4) => "movzwl",
        (2, 8) => "movzwq",
        _ => panic!("invalid zero extension: {src_bytes} to {dest_bytes}"),
    }
}

#[inline(always)]
fn sx_instr_name<'s>(src_bytes: u8, dest_bytes: u8) -> &'s str {
    match (src_bytes, dest_bytes) {
        (1, 2) => "movsbw",
        (1, 4) => "movsbl",
        (1, 8) => "movsbq",
        (2, 4) => "movswl",
        (2, 8) => "movswq",
        (4, 8) => "movslq",
        _ => panic!("invalid sign extension: {src_bytes} to {dest_bytes}"),
    }
}
