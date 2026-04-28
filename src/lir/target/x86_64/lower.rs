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
use crate::lir::{self, BlockId, MachineType, VReg};
use crate::mir::{Const, Function, Operand, ValueId};

struct Lower<'f> {
    function: &'f Function,
    lir: lir::Function<X86_64>,
    value: Vec<VReg>,
    symbols: &'f [String],
    all_functions: &'f [Function],
}

impl<'f> Lower<'f> {
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
                self.lir.push_instruction(block, instruction);

                vreg
            }
        }
    }

    fn constant_mov(&mut self, dest: VReg, c: &Const) -> X86Instr {
        let bytes = c.typ().machine_type().bytes();

        match c {
            Const::Int(num, _) => X86Instr::Mov {
                dest,
                src: X86Operand::Imm(*num),
                bytes,
            },
            Const::Bool(b) => X86Instr::Mov {
                dest,
                src: X86Operand::Imm(if *b { 1 } else { 0 }),
                bytes: 4,
            },

            Const::Float(f, typ) => {
                let is_32 = *typ == Type::F32;
                let bits = match is_32 {
                    true => (*f as f32).to_bits() as u64,
                    _ => f.to_bits() as u64,
                };
                let label = self.lir.new_float(bits, is_32);

                X86Instr::MovFloat {
                    dest,
                    src: X86Operand::RipRel(format!("{label}(%rip)")),
                    bytes: bytes,
                }
            }
            Const::Unit => unreachable!(),
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
            Type::I64 | Type::U64 | Type::Iptr | Type::Uptr | Type::Str | Type::String => {
                MachineType::Int { bytes: 8 }
            }
            Type::F32 => MachineType::Float { bytes: 4 },
            Type::F64 => MachineType::Float { bytes: 8 },
            Type::Unit => unreachable!("unit doesn't have a machine type"),
        }
    }
}
