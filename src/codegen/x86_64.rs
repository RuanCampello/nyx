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
    mir::{Const, Function, Instruction, Mir, Operand, Place, Terminator},
    regalloc::{Allocation, Location, Reg},
};
use std::fmt::{Display, Write};

struct FunctionEmitter<'e> {
    out: &'e mut String,
    allocation: &'e Allocation,
    function: &'e Function,
    symbols: &'e [String],
}

/// Emit full assembly program.
pub fn emit(mir: &Mir) -> String {
    const DEFAULT_SIZE: usize = 1 << 8;
    let mut out = String::with_capacity(DEFAULT_SIZE);

    writeln!(out, "    .text").unwrap();

    todo!()
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
    ) -> Self {
        Self {
            out,
            allocation: alloc,
            function,
            symbols,
        }
    }

    #[inline(always)]
    fn emit_body(&mut self) {
        for (idx, block) in self.function.blocks.iter().enumerate() {
            // emit block label (skip for entry block)
            if idx > 0 {
                writeln!(self.out, ".L_block_{}:", idx).unwrap();
            }

            // emit all instructions in the block
            for instr in &block.instructions {
                self.emit_instruction(instr);
            }

            // emit the block terminator
            self.emit_terminator(&block.terminator);
        }
    }

    /// Prologue: label and frame setup
    #[inline(always)]
    fn emit_prologue(&mut self) {
        // TODO: get actual function name for symbols
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

    fn emit_instruction(&mut self, instruction: &Instruction) {
        use crate::mir::InstructionKind;
        use crate::parser::expression::{BinaryOperator, UnaryOperator};

        let dest = place_str(&instruction.dest, self.allocation);
        let suffix = instruction.dest.typ.size_suffix();

        match &instruction.kind {
            InstructionKind::Assign(operand) => {
                // dest = operand
                let src = operand_str(operand, self.allocation);
                writeln!(self.out, "    mov{}    {}, {}", suffix, src, dest).unwrap();
            }

            InstructionKind::Unary { operation, rhs } => {
                let src = operand_str(rhs, self.allocation);

                match operation {
                    UnaryOperator::Neg => {
                        // dest = -rhs
                        // strategy: mov rhs to dest, then neg dest
                        writeln!(self.out, "    mov{}    {}, {}", suffix, src, dest).unwrap();
                        writeln!(self.out, "    neg{}    {}", suffix, dest).unwrap();
                    }

                    UnaryOperator::Not => {
                        // dest = !rhs (logical not for bool)
                        // Strategy: xor with 1 (0 -> 1, 1 -> 0)
                        writeln!(self.out, "    mov{}    {}, {}", suffix, src, dest).unwrap();
                        writeln!(self.out, "    xor{}    $1, {}", suffix, dest).unwrap();
                    }
                }
            }

            InstructionKind::Binary {
                operation,
                rhs,
                lhs,
            } => {
                let lhs = operand_str(lhs, self.allocation);
                let rhs = operand_str(rhs, self.allocation);

                match operation {
                    BinaryOperator::Add => {
                        // dest = lhs + rhs
                        writeln!(self.out, "    mov{}    {}, {}", suffix, lhs, dest).unwrap();
                        writeln!(self.out, "    add{}    {}, {}", suffix, rhs, dest).unwrap();
                    }

                    BinaryOperator::Sub => {
                        // dest = lhs - rhs
                        writeln!(self.out, "    mov{}    {}, {}", suffix, lhs, dest).unwrap();
                        writeln!(self.out, "    sub{}    {}, {}", suffix, rhs, dest).unwrap();
                    }

                    BinaryOperator::Mul => {
                        // dest = lhs * rhs
                        // imul can do 2-operand form: imul src, dest (dest *= src)
                        writeln!(self.out, "    mov{}    {}, {}", suffix, lhs, dest).unwrap();
                        writeln!(self.out, "    imul{}   {}, {}", suffix, rhs, dest).unwrap();
                    }

                    BinaryOperator::Div => {
                        // integer division is complex: requires %rax/%rdx setup
                        // TODO: (assumes values fit in registers)
                        writeln!(self.out, "    # TODO: div requires rax/rdx handling").unwrap();
                        writeln!(self.out, "    mov{}    {}, {}", suffix, lhs, dest).unwrap();
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
                        writeln!(self.out, "    mov{}    {}, {}", suffix, lhs, dest).unwrap();
                        writeln!(self.out, "    and{}    {}, {}", suffix, rhs, dest).unwrap();
                    }

                    BinaryOperator::Or => {
                        writeln!(self.out, "    mov{}    {}, {}", suffix, lhs, dest).unwrap();
                        writeln!(self.out, "    or{}     {}, {}", suffix, rhs, dest).unwrap();
                    }
                }
            }

            InstructionKind::Call { callee, args } => {
                for (idx, arg) in args.iter().enumerate() {
                    if idx >= 6 {
                        unreachable!("stack argument: {idx}");
                    }

                    let src = operand_str(arg, self.allocation);
                    let typ = arg.typ();
                    let suffix = typ.size_suffix();

                    let dest_reg = match typ {
                        Type::I32 | Type::Bool => Self::ARG_REGS_32[idx],
                        Type::I64 | Type::String => Self::ARG_REGS_64[idx],
                        _ => unimplemented!("unsupported argument type"),
                    };

                    writeln!(self.out, "    mov{}    {}, {}", suffix, src, dest_reg).unwrap();
                }

                // emit function call
                // TODO: actually get the function name from symbols
                let callee_name = format!("nyx_func_{}", callee.0);
                writeln!(self.out, "    call     {}", callee_name).unwrap();

                // move return value from %rax/%eax to destination
                let ret_type = instruction.dest.typ;
                let suffix = ret_type.size_suffix();
                let src_reg = match ret_type {
                    Type::I32 | Type::Bool => "%eax",
                    Type::I64 | Type::String => "%rax",
                    Type::Unit => return,
                    _ => unimplemented!(),
                };

                writeln!(self.out, "    mov{}    {}, {}", suffix, src_reg, dest).unwrap();
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

        writeln!(self.out, "    cmp{}    {}, {}", suffix, rhs, lhs).unwrap();
        writeln!(self.out, "    {}     %al", set_instr).unwrap();
        writeln!(self.out, "    movzbl   %al, {}", dest).unwrap();
    }

    /// Emit a block terminator
    fn emit_terminator(&mut self, term: &Terminator) {
        match term {
            Terminator::Return(None) => {} // epilogue handles cleanup
            Terminator::Return(Some(operand)) => {
                // move return value to %rax/%eax
                let src = operand_str(operand, self.allocation);
                let suffix = operand.typ().size_suffix();
                let ret_reg = match operand.typ() {
                    Type::I32 | Type::Bool => "%eax",
                    Type::I64 | Type::String => "%rax",
                    _ => unimplemented!(),
                };

                writeln!(self.out, "    mov{}    {}, {}", suffix, src, ret_reg).unwrap();
            }

            Terminator::Jump(target) => {
                writeln!(self.out, "    jmp      .L_block_{}", target.0).unwrap()
            }

            Terminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let cond = operand_str(condition, self.allocation);

                // test if condition is non-zero (true)
                writeln!(self.out, "    testl    {}, {}", cond, cond).unwrap();
                writeln!(self.out, "    jne      .L_block_{}", then_block.0).unwrap();
                writeln!(self.out, "    jmp      .L_block_{}", else_block.0).unwrap();
            }
        }
    }

    fn emit_argument_moves(&mut self) {
        // TODO: need to get actual parameters from function
        // for now, we infer from first n locals that match calling convention
        let params = &self.function.locals[0..6.min(self.function.locals.len())];

        for (idx, (param_id, param_type)) in params.iter().enumerate() {
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
                Location::Stack(offset) => format!("{}(%rbp)", offset),
            };

            // only emit move if source and destination are different
            if src_reg != dest_str {
                writeln!(self.out, "    mov{}    {}, {}", suffix, src_reg, dest_str).unwrap();
            }
        }
    }
}

impl Function {
    fn emit(&self, out: &mut String, symbols: &[String]) {
        let alloc = Allocation::allocate(self);
        let mut emitter = FunctionEmitter::new(out, &alloc, self, symbols);

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
