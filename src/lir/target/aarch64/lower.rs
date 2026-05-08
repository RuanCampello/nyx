//! MIR -> AArch64 LIR lowering
//!
//! AArch64 is naturaly 3-address ISA, which makes instruction selection
//! considerably cleaner than `x86_64`:
//!
//!   MIR: t2 = t0 + t1
//!   LIR: ADD Xd, Xn, Xm - direct translation, no copy needed
//!
//! The only departure from this ideal is comparisons: we emit CMP (sets NZCV)
//! followed by CSET (materialises the flag into a GP register as 0 or 1)

use crate::{
    lir::{
        self, BlockId, VReg,
        target::{
            Lowerable, Target,
            aarch64::{A64Instr, A64Operand, AArch64},
        },
    },
    mir::{self, Function, Operand, ValueId},
    parser::statement::Type,
};

struct Lower<'f> {
    function: &'f Function,
    lir: lir::Function<AArch64>,
    /// maps MIR ValueId -> LIR VReg
    value: Vec<VReg>,
    symbols: &'f [String],
    all_functions: &'f [Function],
}

impl Lowerable for AArch64 {
    fn lower(
        function: &Function,
        symbols: &[String],
        all_functions: &[Function],
    ) -> lir::Function<Self> {
        todo!()
    }
}

impl<'f> Lower<'f> {
    fn lower_block(&mut self, id: &BlockId, block: &mir::Block) {
        for instruction in &block.instructions {
            self.lower_instruction(id, instruction);
        }

        self.lower_terminator(id, block.terminator);
    }

    fn lower_instruction(&mut self, id: &BlockId, instruction: &mir::Instruction) {
        use crate::mir::InstructionKind;

        let dest = self.vreg(instruction.dest.id);
        let typ = instruction.dest.typ;
        let bytes = typ.machine_type().bytes();
        let is_float = typ.is_float();

        match &instruction.kind {
            InstructionKind::Assign(operand) => match self.lower_operand(operand, id) {
                A64Operand::VReg(src) => {
                    let instruction = match is_float {
                        true => A64Instr::FMov { dest, src, bytes },
                        false => A64Instr::Mov { dest, src, bytes },
                    };

                    self.lir.push_instr(id, instruction);
                }

                A64Operand::Imm(imm) => {
                    self.lir.push_instr(id, A64Instr::MovImm { dest, imm, bytes })
                }

                A64Operand::Label(label) => {
                    let instruction = match is_float {
                        true => A64Instr::FLiteral { dest, label, bytes },
                        false => A64Instr::Adr { dest, label },
                    };

                    self.lir.push_instr(id, instruction);
                }
            },

            InstructionKind::Syscall {
                code,
                args,
                returns,
            } => {
                let mut moves = Vec::with_capacity(args.len());
                let mut uses = Vec::with_capacity(args.len());

                for (idx, arg) in args.iter().enumerate() {
                    let abi_reg = AArch64::syscall_param(idx).expect("too many syscall arguments");
                    let operand = self.lower_operand(arg, id);
                    let bytes = arg.typ().machine_type().bytes();

                    if let A64Operand::VReg(vreg) = &operand {
                        uses.push(*vreg);
                    }

                    moves.push((operand, abi_reg, bytes));
                }

                let ret = (*returns && typ != Type::Unit).then_some(dest);
                self.lir.push_instr(
                    id,
                    A64Instr::Syscall {
                        id: *code as u64,
                        moves,
                        uses,
                        ret,
                    },
                );
            }
            _ => todo!("lower_instruction"),
        }
    }

    fn lower_terminator(&mut self, id: &BlockId, terminator: mir::Terminator) {
        todo!("lower_terminator")
    }

    fn lower_operand(&mut self, operand: &Operand, block: &lir::BlockId) -> A64Operand {
        todo!("lower_operand")
    }

    #[inline(always)]
    fn vreg(&self, id: ValueId) -> VReg {
        self.value[id.0 as usize]
    }
}
