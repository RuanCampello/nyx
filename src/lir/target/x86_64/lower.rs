//! MIR → x86_64 LIR lowering
//!
//! Translates 3-address MIR into 2-address x86_64 LIR.
//!
//! The core pattern for binary arithmetic:
//!
//!   MIR:  t2 = t0 + t1
//!   LIR:  v2 = Mov(v0)    ← copy lhs into dest VReg (2-address form)
//!         Add(v2, v1)     ← dest = dest + src
//!
//! The coalescer eliminates the Mov when v2 and v0 don't interfere

use crate::hir::Type;
use crate::lir::target::x86_64::{X86_64, X86Instr, X86Operand};
use crate::lir::target::{RegClass, Target};
use crate::lir::{self, BlockId, MachineType, VReg};
use crate::mir::{self, Const, Function, Operand, ValueId};

struct Lower<'f> {
    function: &'f Function,
    lir: lir::Function<X86_64>,
    value: Vec<VReg>,
    symbols: &'f [String],
    all_functions: &'f [Function],
}

impl<'f> Lower<'f> {
    fn lower_instruction(&mut self, id: &BlockId, instruction: mir::Instruction) {
        use crate::mir::InstructionKind;

        let dest = self.vreg(instruction.dest.id);
        let typ = instruction.dest.typ;
        let bytes = typ.machine_type().bytes();
        let is_float = typ.is_float();

        match instruction.kind {
            InstructionKind::Assign(op) => {
                let src = self.lower_operand(&op);
                let instruction = match is_float {
                    true => X86Instr::MovFloat { dest, src, bytes },
                    _ => X86Instr::Mov { dest, src, bytes },
                };

                self.lir.push_instr(id, instruction);
            }

            InstructionKind::Unary { operation, rhs } => {
                use crate::parser::expression::UnaryOperator as U;

                const NEG_ZERO: u64 = (-0.0f64).to_bits() as u64;
                const NEG_ZERO_32: u64 = (-0.0f32).to_bits() as u64;

                let src = self.lower_operand(&rhs);

                // 2-address: copy rhs into dest first to mutate it in-place
                let copy = match is_float {
                    true => X86Instr::MovFloat { dest, src, bytes },
                    _ => X86Instr::Mov { dest, src, bytes },
                };

                self.lir.push_instr(id, copy);

                match operation {
                    U::Neg if is_float => {
                        let bits = match typ.is_32_bit() {
                            true => NEG_ZERO_32,
                            _ => NEG_ZERO,
                        };
                        let label = self.lir.new_float(bits, typ.is_32_bit());

                        self.lir.push_instr(
                            id,
                            X86Instr::XorFloat {
                                dest,
                                src: X86Operand::RipRel(format!("{label}(%rip)")),
                                bytes,
                            },
                        );
                    }
                    U::Neg => self.lir.push_instr(id, X86Instr::Neg { dest, bytes }),
                    U::Not => self.lir.push_instr(
                        id,
                        X86Instr::Xor {
                            dest,
                            src: X86Operand::Imm(1),
                            bytes: 4,
                        },
                    ),

                    _ => todo!(),
                }
            }

            InstructionKind::Binary { operation, rhs, lhs } => {
                use crate::parser::expression::BinaryOperator as B;

                let bytes = lhs.typ().machine_type().bytes();
                let is_float = lhs.typ().is_float();
                let lhs = self.lower_operand(&lhs);
                let rhs = self.lower_operand(&rhs);

                let copy = match is_float {
                    true => X86Instr::MovFloat { dest, src: lhs, bytes },
                    false => X86Instr::Mov { dest, src: lhs, bytes },
                };

                match operation {
                    B::Div if is_float => {
                        self.lir.push_instr(id, copy);
                        self.lir.push_instr(id, X86Instr::DivFloat { dest, src: rhs, bytes });
                    }
                    B::Div => unimplemented!(),

                    _ => {
                        self.lir.push_instr(id, copy);

                        let arith = match operation {
                            B::Add => match is_float {
                                true => X86Instr::AddFloat { dest, src: rhs, bytes },
                                _ => X86Instr::Add { dest, src: rhs, bytes },
                            },

                            B::Sub => match is_float {
                                true => X86Instr::SubFloat { dest, src: rhs, bytes },
                                _ => X86Instr::Sub { dest, src: rhs, bytes },
                            },

                            B::Mul => match is_float {
                                true => X86Instr::MulFloat { dest, src: rhs, bytes },
                                _ => X86Instr::Imul { dest, src: rhs, bytes },
                            },

                            B::And => X86Instr::And {
                                dest,
                                src: rhs,
                                bytes: 4,
                            },
                            B::Or => X86Instr::Or {
                                dest,
                                src: rhs,
                                bytes: 4,
                            },

                            _ => unsafe { std::hint::unreachable_unchecked() },
                        };

                        self.lir.push_instr(id, arith);
                    }
                };
            }

            InstructionKind::Call { callee, args } => {
                let callee = self
                    .symbols
                    .get(self.all_functions[callee.0 as usize].name_symbol)
                    .map(|n| format!("nyx_{n}"))
                    .unwrap_or_else(|| format!("nyx_func_{}", callee.0));

                let mut moves = Vec::with_capacity(args.len());
                let mut int = 0;
                let mut float = 0;

                for arg in args {
                    let machine = arg.typ().machine_type();

                    let abi_reg = match machine.class() {
                        RegClass::Int => {
                            let reg = X86_64::param(int, RegClass::Int);
                            int += 1;

                            reg
                        }

                        RegClass::Float => {
                            let reg = X86_64::param(float, RegClass::Float);
                            float += 1;

                            reg
                        }
                    };

                    let Some(abi_reg) = abi_reg else { continue };
                    let vreg = self.operand(&arg, id);
                    moves.push((vreg, abi_reg));
                }

                let ret = (typ != Type::Unit).then_some(dest);
                self.lir.push_instr(id, X86Instr::call(callee, moves, ret));
            }
            _ => unimplemented!(),
        }
    }

    fn lower_terminator(&mut self, id: &BlockId, terminator: mir::Terminator) {
        todo!()
    }

    fn lower_block(&mut self, id: &BlockId, block: mir::Block) {
        for instruction in block.instructions {
            self.lower_instruction(id, instruction);
        }

        self.lower_terminator(id, block.terminator);
    }

    fn vreg(&self, id: ValueId) -> VReg {
        self.value[id.0 as usize]
    }

    /// materialise a `MIR` operand into a new VReg if it's a constant
    /// otherwise return it's VReg directly
    fn operand(&mut self, op: &Operand, block: &BlockId) -> VReg {
        match op {
            Operand::Place(p) => self.vreg(p.id),
            Operand::Const(c) => {
                let vreg = self.lir.new_vreg(c.typ().machine_type());
                let instruction = self.constant_mov(vreg, c);
                self.lir.push_instr(block, instruction);

                vreg
            }
        }
    }

    // transforms a MIR operand into a x86_64 LIR operand
    fn lower_operand(&mut self, op: &Operand) -> X86Operand {
        match op {
            Operand::Place(p) => X86Operand::VReg(self.vreg(p.id)),
            Operand::Const(Const::Float(v, typ)) => {
                let is_32 = *typ == Type::F32;
                let bits = match is_32 {
                    true => (*v as f32).to_bits() as u64,
                    _ => v.to_bits(),
                };
                let label = self.lir.new_float(bits, is_32);

                X86Operand::RipRel(format!("{label}(%rip)"))
            }
            Operand::Const(Const::Int(n, _)) => X86Operand::Imm(*n),
            Operand::Const(Const::Bool(b)) => X86Operand::Imm(if *b { 1 } else { 0 }),
            Operand::Const(Const::Unit) => unreachable!("unit operand"),
        }
    }

    fn constant_mov(&mut self, dest: VReg, c: &Const) -> X86Instr {
        let bytes = c.typ().machine_type().bytes();
        let src = self.lower_operand(&Operand::Const(*c));

        match c {
            Const::Int(num, _) => X86Instr::Mov { dest, src, bytes },
            Const::Bool(b) => X86Instr::Mov { dest, src, bytes: 4 },
            Const::Float(f, typ) => X86Instr::MovFloat {
                dest,
                src,
                bytes: bytes,
            },
            Const::Unit => unreachable!("unit operand"),
        }
    }
}

impl Type {
    // TODO: abstract this later in a `SizedType` trait or something
    // to have arch depedent sizes
    fn machine_type(&self) -> MachineType {
        match self {
            Type::I8 | Type::U8 | Type::Bool | Type::Char => MachineType::Int { bytes: 1 },
            Type::I16 | Type::U16 => MachineType::Int { bytes: 2 },
            Type::I32 | Type::U32 => MachineType::Int { bytes: 4 },
            Type::I64 | Type::U64 | Type::Iptr | Type::Uptr | Type::Str | Type::String => MachineType::Int { bytes: 8 },
            Type::F32 => MachineType::Float { bytes: 4 },
            Type::F64 => MachineType::Float { bytes: 8 },
            Type::Unit => unreachable!("unit doesn't have a machine type"),
        }
    }
}
