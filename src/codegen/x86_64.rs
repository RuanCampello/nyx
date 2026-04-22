//! x86_64 code emission (AT&T syntax)
//!
//! ## Conventions
//! - Syntax: AT&T (GAS) — `mov src, dest`, `%` registers, `$` immediates
//! - Sizes: `l` suffix for 32-bit (i32/bool), `q` for 64-bit (i64/pointers)
//! - Frame: System V AMD64 — `push %rbp; mov %rsp, %rbp; sub $N, %rsp`
//! - Args: First 6 int args in `%rdi %rsi %rdx %rcx %r8 %r9`
//!         After that, the next arguments are pushed onto stack in reverse order
//! - Return: `%rax` (32-bit result in `%eax`)
//! - Spills: `-N(%rbp)` stack slots

use crate::{
    hir::Type,
    mir::{Const, Function, Instruction, Mir, Operand, Place, Terminator},
    regalloc::{Allocation, Location, Reg},
};
use std::fmt::{Display, Write};

struct FunctionEmitter<'e> {
    out: &'e mut String,
    allocation: &'e Allocation,
    function: &'e Function,
    /// all functions in the program, used to resolve the callee name
    all_functions: &'e [Function],
    saved_regs: Vec<Reg>,
    symbols: &'e [String],
}

macro_rules! emit {
    ($dst:expr, $($arg:tt)*) => {
        writeln!($dst, "    {}", format_args!($($arg)*)).unwrap();
    };
}

macro_rules! label {
    ($dst:expr, $($arg:tt)*) => {
        writeln!($dst, "{}", format_args!($($arg)*)).unwrap();
    }}

/// Emit full assembly program.
pub fn emit(mir: &Mir) -> String {
    const DEFAULT_SIZE: usize = 1 << 8;
    let mut out = String::with_capacity(DEFAULT_SIZE);

    label!(out, ".text");

    for function in &mir.functions {
        function.emit(&mut out, &mir.symbols, &mir.functions);
    }

    // emit a `_start` trampoline if the program defines `fn main`
    //
    // this allows the binary to be linked with `ld` directly (no libc)
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

impl<'e> FunctionEmitter<'e> {
    // System V AMD64 calling convention for function calls

    const ARG_REGS_32: &'e [&'e str] = &["%edi", "%esi", "%edx", "%ecx", "%r8d", "%r9d"];
    const ARG_REGS_64: &'e [&'e str] = &["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

    fn new(
        out: &'e mut String,
        alloc: &'e Allocation,
        function: &'e Function,
        symbols: &'e [String],
        all_functions: &'e [Function],
    ) -> Self {
        let saved_regs = Reg::CALLEE_SAVED
            .iter()
            .copied()
            .filter(|reg| {
                alloc
                    .locations
                    .values()
                    .any(|loc| *loc == Location::Register(*reg))
            })
            .collect();

        Self {
            out,
            allocation: alloc,
            function,
            symbols,
            all_functions,
            saved_regs,
        }
    }

    #[inline(always)]
    fn emit_body(&mut self, fn_name: &str, label: &str) {
        let n = self.function.blocks.len();

        for (idx, block) in self.function.blocks.iter().enumerate() {
            // emit block label (skip for entry block), scoped to the function
            // to avoid collisions when multiple functions have the same block index
            if idx > 0 {
                label!(self.out, ".L_block_{fn_name}_{idx}:");
            }

            // emit all instructions in the block
            for instr in &block.instructions {
                self.emit_instruction(instr);
            }

            // emit the block terminator
            self.emit_terminator(&block.terminator, fn_name, label, idx == n - 1);
        }
    }

    /// Prologue: label and frame setup
    #[inline(always)]
    fn emit_prologue(&mut self, name: &str, frame_size: u32) {
        // .globl directive makes function visible to linker
        label!(self.out, ".globl {}", name);
        label!(self.out, "{}:", name);

        emit!(self.out, "push    %rbp");
        emit!(self.out, "mov     %rsp, %rbp");

        // save callee-saved registers this function uses
        //
        // ABI requires them to be preserved across calls (callers depend on this)
        for reg in &self.saved_regs {
            emit!(self.out, "push    %{}", reg.as_str_64());
        }

        if frame_size > 0 {
            emit!(self.out, "sub     ${}, %rsp", frame_size);
        }

        // move the arguments from registers to their allocated locations
        self.emit_argument_moves();
    }

    /// Epilogue: clean-up and return
    #[inline(always)]
    fn emit_epilogue(&mut self, label: &str, frame_size: u32) {
        label!(self.out, "{label}:");

        if frame_size > 0 {
            emit!(self.out, "add     ${}, %rsp", frame_size);
        }

        // restore callee-saved registers in reverse push order
        for reg in self.saved_regs.iter().copied().rev() {
            emit!(self.out, "pop     %{}", reg.as_str_64());
        }

        emit!(self.out, "pop     %rbp");
        emit!(self.out, "ret");
    }

    fn emit_instruction(&mut self, instruction: &Instruction) {
        use crate::mir::InstructionKind;
        use crate::parser::expression::{BinaryOperator, UnaryOperator};

        let dest = place_str(&instruction.dest, self.allocation, &self.saved_regs);
        let suffix = instruction.dest.typ.size_suffix();

        match &instruction.kind {
            InstructionKind::Assign(operand) => {
                // dest = operand
                let src = operand_str(operand, self.allocation, &self.saved_regs);
                // this arise after register allocation when two values coalesced to the same location
                if src != dest {
                    emit!(self.out, "mov{suffix}    {src}, {dest}");
                }
            }

            InstructionKind::Unary { operation, rhs } => {
                let src = operand_str(rhs, self.allocation, &self.saved_regs);
                let moved = src != dest;

                match operation {
                    UnaryOperator::Neg => {
                        // dest = -rhs
                        // strategy: mov rhs to dest, then neg dest
                        if moved {
                            emit!(self.out, "mov{suffix}    {src}, {dest}");
                        }

                        emit!(self.out, "neg{suffix}    {dest}");
                    }

                    UnaryOperator::Not => {
                        // dest = !rhs (logical not for bool)
                        // strategy: xor with 1 (0 -> 1, 1 -> 0)
                        if moved {
                            emit!(self.out, "mov{suffix}    {src}, {dest}");
                        }
                        emit!(self.out, "xor{suffix}    $1, {dest}");
                    }
                }
            }

            InstructionKind::Binary {
                operation,
                rhs,
                lhs,
            } => {
                let is_rhs_const = matches!(rhs, Operand::Const(_));

                let lhs = operand_str(lhs, self.allocation, &self.saved_regs);
                let rhs = operand_str(rhs, self.allocation, &self.saved_regs);

                let moved = lhs != dest;

                match operation {
                    BinaryOperator::Add => {
                        // dest = lhs + rhs
                        if moved {
                            emit!(self.out, "mov{suffix}    {lhs}, {dest}");
                        }
                        emit!(self.out, "add{suffix}    {rhs}, {dest}");
                    }

                    BinaryOperator::Sub => {
                        // dest = lhs - rhs
                        if moved {
                            emit!(self.out, "mov{suffix}    {lhs}, {dest}");
                        }
                        emit!(self.out, "sub{suffix}    {rhs}, {dest}");
                    }

                    BinaryOperator::Mul => {
                        // dest = lhs * rhs
                        // imul can do 2-operand form: imul src, dest (dest *= src)
                        if moved {
                            emit!(self.out, "mov{suffix}    {lhs}, {dest}");
                        }
                        emit!(self.out, "imul{suffix}   {rhs}, {dest}");
                    }

                    BinaryOperator::Div => {
                        // integer division is complex: requires %rax/%rdx setup
                        //
                        // requires: dividend in %eax (32-bit) or %rax (64-bit)
                        //           %edx/%rdx must be sign-extended from %eax/%rax
                        // result: quotient in %eax/%rax, remainder in %edx/%rdx

                        let (rax, extend_instr) = match instruction.dest.typ {
                            Type::I32 => ("%eax", "cltd"), // sign-extend eax -> edx:eax
                            Type::I64 => ("%rax", "cqto"), // sign-extend rax -> rdx:rax
                            _ => unreachable!(),
                        };

                        if moved {
                            // move dividend (lhs) to %rax/%eax
                            emit!(self.out, "mov{suffix}    {lhs}, {rax}");
                        }
                        // sign-extend to rdx:rax or edx:eax
                        emit!(self.out, "{extend_instr}");

                        // PERFORMANCE: maybe we could optimise this out before it reaches here
                        match is_rhs_const {
                            true => {
                                // `idiv` doesn't accept immediates, so materialise constants on stack
                                emit!(self.out, "sub     $8, %rsp");
                                emit!(self.out, "mov{suffix}    {rhs}, (%rsp)");
                                emit!(self.out, "idiv{suffix}   (%rsp)");
                                emit!(self.out, "add     $8, %rsp");
                            }
                            false => {
                                emit!(self.out, "idiv{suffix}    {rhs}");
                            }
                        };

                        if moved {
                            emit!(self.out, "mov{suffix}    {rax}, {dest}");
                        }
                    }

                    // comparison operators: set dest to 0 or 1
                    BinaryOperator::Eq => self.emit_comp("sete", lhs, rhs, dest, suffix),
                    BinaryOperator::Ne => self.emit_comp("setne", lhs, rhs, dest, suffix),
                    BinaryOperator::Lt => self.emit_comp("setl", lhs, rhs, dest, suffix),
                    BinaryOperator::LtEq => self.emit_comp("setle", lhs, rhs, dest, suffix),
                    BinaryOperator::Gt => self.emit_comp("setg", lhs, rhs, dest, suffix),
                    BinaryOperator::GtEq => self.emit_comp("setge", lhs, rhs, dest, suffix),

                    BinaryOperator::And => {
                        // logical and: both operands are bool (0 or 1)
                        if moved {
                            emit!(self.out, "mov{suffix}    {lhs}, {dest}");
                        }
                        emit!(self.out, "and{suffix}    {rhs}, {dest}");
                    }

                    BinaryOperator::Or => {
                        if moved {
                            emit!(self.out, "mov{suffix}    {lhs}, {dest}");
                        }
                        emit!(self.out, "or{suffix}     {rhs}, {dest}");
                    }
                }
            }

            InstructionKind::Call { callee, args } => {
                // stack args are pushed in reverse order (right-to-left)
                let stack_args = args.iter().skip(6).rev().collect::<Vec<_>>();

                for arg in stack_args {
                    let src = operand_str(arg, self.allocation, &self.saved_regs);
                    let typ = arg.typ();
                    let suffix = typ.size_suffix();

                    emit!(self.out, "push{suffix}   {src}");
                }

                // move the first 6 args to registers
                for (idx, arg) in args.iter().take(6).enumerate() {
                    let src = operand_str(arg, self.allocation, &self.saved_regs);
                    let typ = arg.typ();
                    let suffix = typ.size_suffix();

                    let dest_reg = match typ {
                        Type::I32 | Type::Bool => Self::ARG_REGS_32[idx],
                        Type::I64 | Type::String => Self::ARG_REGS_64[idx],
                        _ => unimplemented!("unsupported argument type"),
                    };

                    if src != dest_reg {
                        emit!(self.out, "mov{suffix}    {src}, {dest_reg}");
                    }
                }

                // emit function call
                let callee_name = function_label(
                    self.all_functions[callee.0 as usize].name_symbol,
                    self.symbols,
                );
                emit!(self.out, "call     {callee_name}");

                // clean up stack arguments
                let stack_bytes = args.len().saturating_sub(6) * 8;
                if stack_bytes > 0 {
                    emit!(self.out, "add     ${stack_bytes}, %rsp");
                }

                // move return value from %rax/%eax to destination
                let ret_type = instruction.dest.typ;
                let suffix = ret_type.size_suffix();
                let src_reg = match ret_type {
                    Type::I32 | Type::Bool => "%eax",
                    Type::I64 | Type::String => "%rax",
                    Type::Unit => return,
                    _ => unimplemented!(),
                };

                if dest != src_reg {
                    emit!(self.out, "mov{suffix}    {src_reg}, {dest}");
                }
            }
        }
    }

    /// Emit a comparison operation that sets dest to 0 or 1
    #[inline(always)]
    fn emit_comp(&mut self, set_instr: &str, lhs: String, rhs: String, dest: String, suffix: &str) {
        // strategy:
        // 1. cmp rhs, lhs  (sets flags based on lhs - rhs)
        // 2. set<cc> %al   (sets low byte of %rax to 0 or 1)
        // 3. movzbl %al, dest (zero-extend byte to 32-bit)

        let preserves_rax = !matches!(dest.as_str(), "%al" | "%ax" | "%eax" | "%rax");
        if preserves_rax {
            emit!(self.out, "push    %rax");
        }

        emit!(self.out, "cmp{suffix}    {rhs}, {lhs}",);
        emit!(self.out, "{set_instr}     %al");
        emit!(self.out, "movzbl   %al, {dest}");

        if preserves_rax {
            emit!(self.out, "pop     %rax");
        }
    }

    /// Emit a block terminator
    fn emit_terminator(&mut self, term: &Terminator, fn_name: &str, label: &str, is_last: bool) {
        match term {
            Terminator::Return(None) if !is_last => {
                emit!(self.out, "jmp      {label}");
            }
            Terminator::Return(Some(operand)) => {
                // move return value to %rax/%eax
                let src = operand_str(operand, self.allocation, &self.saved_regs);
                let suffix = operand.typ().size_suffix();
                let ret_reg = match operand.typ() {
                    Type::I32 | Type::Bool => "%eax",
                    Type::I64 | Type::String => "%rax",
                    _ => unimplemented!(),
                };

                if src != ret_reg {
                    emit!(self.out, "mov{suffix}    {src}, {ret_reg}");
                }
                if !is_last {
                    emit!(self.out, "jmp      {label}");
                }
            }

            Terminator::Jump(target) => {
                emit!(self.out, "jmp      .L_block_{fn_name}_{}", target.0);
            }

            Terminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let cond = operand_str(condition, self.allocation, &self.saved_regs);

                // test if condition is non-zero (true)
                emit!(self.out, "testl    {cond}, {cond}");
                emit!(self.out, "jne      .L_block_{fn_name}_{}", then_block.0);
                emit!(self.out, "jmp      .L_block_{fn_name}_{}", else_block.0);
            }

            _ => {}
        }
    }

    fn emit_argument_moves(&mut self) {
        for (idx, (param_id, param_type)) in self.function.params.iter().enumerate() {
            if idx >= 6 {
                break;
            }

            let dest = self.allocation.location_of(*param_id);
            let suffix = param_type.size_suffix();

            let src_reg = match param_type {
                Type::I32 | Type::Bool => Self::ARG_REGS_32[idx],
                Type::I64 | Type::String => Self::ARG_REGS_64[idx],
                _ => continue,
            };

            let dest_str = match dest {
                Location::Register(reg) => format!("%{}", reg_name(reg, *param_type)),
                Location::Stack(offset) => {
                    let offset = adjust_stack_offset(offset, &self.saved_regs);
                    format!("{offset}(%rbp)")
                }
            };

            // only emit move if source and destination are different
            if src_reg != dest_str {
                emit!(self.out, "mov{suffix}    {src_reg}, {dest_str}");
            }
        }
    }

    #[inline(always)]
    const fn frame_size_for_calls(&self) -> u32 {
        // keep %rsp 16-byte aligned at call sites
        let alignment_pad = if self.saved_regs.len() % 2 == 1 { 8 } else { 0 };
        self.allocation.frame_size + alignment_pad
    }
}

impl Function {
    fn emit(&self, out: &mut String, symbols: &[String], all_functions: &[Function]) {
        let alloc = Allocation::allocate(self);
        let name = function_label(self.name_symbol, symbols);
        let mut emitter = FunctionEmitter::new(out, &alloc, self, symbols, all_functions);
        let label = format!(".L_{name}_epilogue");

        let frame_size = emitter.frame_size_for_calls();

        emitter.emit_prologue(&name, frame_size);
        emitter.emit_body(&name, &label);
        emitter.emit_epilogue(&label, frame_size);
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

#[inline(always)]
fn function_label(symbol: usize, symbols: &[String]) -> String {
    symbols
        .get(symbol)
        .map(|name| format!("nyx_{name}"))
        .unwrap_or_else(|| format!("nyx_func_{symbol}"))
}

/// Format an operand as AT&T assembly source
#[inline(always)]
fn operand_str(op: &Operand, alloc: &Allocation, saved_regs: &[Reg]) -> String {
    match op {
        Operand::Place(place) => place_str(place, alloc, saved_regs),
        Operand::Const(c) => c.to_string(),
    }
}

impl Display for Const {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Const::Int(n, _) => write!(f, "${n}"),
            Const::Bool(b) => write!(f, "${}", if *b { 1 } else { 0 }),
            Const::Float(_, _) => unimplemented!("float constants not yet supported"),
            Const::Unit => unreachable!("Unit constant has no runtime representation"),
        }
    }
}

#[inline(always)]
fn place_str(place: &Place, allocation: &Allocation, saved_regs: &[Reg]) -> String {
    let loc = allocation.location_of(place.id);
    match loc {
        Location::Register(reg) => format!("%{}", reg_name(reg, place.typ)),
        Location::Stack(offset) => {
            let offset = adjust_stack_offset(offset, saved_regs);
            format!("{offset}(%rbp)")
        }
    }
}

#[inline(always)]
const fn adjust_stack_offset(offset: i32, saved_regs: &[Reg]) -> i32 {
    offset - (saved_regs.len() as i32 * 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hir, mir, parser::Parser};

    fn compile(src: &str) -> String {
        let statements = Parser::new(src).parse().unwrap();
        let hir = hir::lower(statements).unwrap();
        let mir = mir::lower(hir).unwrap();

        emit(&mir)
    }

    #[test]
    fn function_with_spills_allocates_frame() {
        let src = r#"
            fn pressure(
                a: i32, b: i32, c: i32, d: i32, e: i32,
                f: i32, g: i32, h: i32, i: i32, j: i32,
                k: i32, l: i32, m: i32, n: i32, o: i32, p: i32
            ): i32 {
                a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p
            }
        "#;
        let asm = compile(src);

        // should allocate stack frame for spills
        assert!(asm.contains("sub"));
    }

    #[test]
    fn simple_return_constant() {
        let asm = compile("fn foo(): i32 { 42 }");

        // should move constant to eax and return
        assert!(asm.contains("movl"));
        assert!(asm.contains("$42"));
        assert!(asm.contains("%eax"));
    }

    #[test]
    fn add_operation_emits_add_instruction() {
        let asm = compile("fn add(a: i32, b: i32): i32 { a + b }");

        assert!(asm.contains("addl"));
    }

    #[test]
    fn sub_operation_emits_sub_instruction() {
        let asm = compile("fn sub(a: i32, b: i32): i32 { a - b }");

        assert!(asm.contains("subl"));
    }

    #[test]
    fn mul_operation_emits_imul_instruction() {
        let asm = compile("fn mul(a: i32, b: i32): i32 { a * b }");

        assert!(asm.contains("imul"));
    }

    #[test]
    fn div_operation_emits_idiv_with_setup() {
        let asm = compile("fn div(a: i32, b: i32): i32 { a / b }");

        assert!(asm.contains("cltd")); // sign-extend for 32-bit
        assert!(asm.contains("idivl"));
    }

    #[test]
    fn div_64bit_uses_cqto() {
        let asm = compile("fn div64(a: i64, b: i64): i64 { a / b }");

        assert!(asm.contains("cqto")); // sign-extend for 64-bit
        assert!(asm.contains("idivq"));
    }

    #[test]
    fn negation_emits_neg_instruction() {
        let asm = compile("fn neg(x: i32): i32 { -x }");

        assert!(asm.contains("negl"));
    }

    #[test]
    fn logical_not_emits_xor() {
        let asm = compile("fn not(x: bool): bool { !x }");

        assert!(asm.contains("xorl"));
        assert!(asm.contains("$1"));
    }

    #[test]
    fn comparison_eq_emits_sete() {
        let asm = compile("fn eq(a: i32, b: i32): bool { a == b }");

        assert!(asm.contains("cmpl"));
        assert!(asm.contains("sete"));
        assert!(asm.contains("movzbl"));
    }

    #[test]
    fn comparison_ne_emits_setne() {
        let asm = compile("fn ne(a: i32, b: i32): bool { a != b }");

        assert!(asm.contains("setne"));
    }

    #[test]
    fn comparison_lt_emits_setl() {
        let asm = compile("fn lt(a: i32, b: i32): bool { a < b }");

        assert!(asm.contains("setl"));
    }

    #[test]
    fn comparison_le_emits_setle() {
        let asm = compile("fn le(a: i32, b: i32): bool { a <= b }");

        assert!(asm.contains("setle"));
    }

    #[test]
    fn comparison_gt_emits_setg() {
        let asm = compile("fn gt(a: i32, b: i32): bool { a > b }");

        assert!(asm.contains("setg"));
    }

    #[test]
    fn comparison_ge_emits_setge() {
        let asm = compile("fn ge(a: i32, b: i32): bool { a >= b }");

        assert!(asm.contains("setge"));
    }

    #[test]
    fn logical_and_emits_and_instruction() {
        let asm = compile("fn and(a: bool, b: bool): bool { a && b }");

        assert!(asm.contains("andl"));
    }

    #[test]
    fn logical_or_emits_or_instruction() {
        let asm = compile("fn or(a: bool, b: bool): bool { a || b }");

        assert!(asm.contains("orl"));
    }

    #[test]
    fn if_statement_emits_branch() {
        let src = r#"
            fn test(x: bool): i32 {
                if x { 1 } else { 2 }
            }
        "#;
        let asm = compile(src);

        assert!(asm.contains("testl"));
        assert!(asm.contains("jne"));
        assert!(asm.contains(".L_block_"));
    }

    #[test]
    fn while_loop_emits_jump_back() {
        let src = r#"
            fn loop_test(x: i32): i32 {
                let mut i: i32 = 0;
                while i < x {
                    i = i + 1;
                }
                i
            }
        "#;
        let asm = compile(src);

        // should have conditional branch and backward jump
        assert!(asm.contains("jmp"));
        assert!(asm.contains(".L_block_"));
    }

    #[test]
    fn complex_arithmetic_expression() {
        let asm = compile("fn expr(a: i32, b: i32, c: i32): i32 { (a + b) * c }");

        assert!(asm.contains("addl"));
        assert!(asm.contains("imul"));
    }

    #[test]
    fn nested_comparisons() {
        let src = r#"
            fn complex(a: i32, b: i32, c: i32): bool {
                (a < b) && (b < c)
            }
        "#;
        let asm = compile(src);

        assert!(asm.contains("cmpl"));
        assert!(asm.contains("andl"));
    }
}
