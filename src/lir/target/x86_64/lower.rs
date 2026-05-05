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

use crate::lir::target::x86_64::{Condition, X86_64, X86Instr, X86Operand, X86Reg};
use crate::lir::target::{Lowerable, RegClass, Target};
use crate::lir::{self, BlockId, MachineType, Term, VReg};
use crate::mir::{self, Const, Function, Operand, ValueId};
use crate::parser::statement::Type;

struct Lower<'f> {
    function: &'f Function,
    lir: lir::Function<X86_64>,
    value: Vec<VReg>,
    symbols: &'f [String],
    all_functions: &'f [Function],
}

impl Lowerable for X86_64 {
    fn lower(function: &Function, symbols: &[String], all_functions: &[Function]) -> lir::Function<Self> {
        let name = symbols
            .get(function.name_symbol)
            .map(|n| format!("nyx_{n}"))
            .unwrap_or_else(|| format!("nyx_func_{}", function.name_symbol));

        let mut lir = lir::Function::<X86_64>::new(name);

        let value: Vec<VReg> = function
            .locals
            .iter()
            .map(|(_, typ)| lir.new_vreg(typ.machine_type()))
            .collect();

        for _ in &function.blocks {
            lir.new_block();
        }

        let mut lower = Lower {
            function,
            lir,
            value,
            symbols,
            all_functions,
        };

        lower.lower_param_moves();

        for (idx, block) in function.blocks.iter().enumerate() {
            lower.lower_block(&BlockId(idx as u32), block);
        }

        lower.lir
    }
}

impl<'f> Lower<'f> {
    fn lower_instruction(&mut self, id: &BlockId, instruction: &mir::Instruction) {
        use crate::mir::InstructionKind;

        let dest = self.vreg(instruction.dest.id);
        let typ = instruction.dest.typ;
        let bytes = typ.machine_type().bytes();
        let is_float = typ.is_float();

        match &instruction.kind {
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
                }
            }

            InstructionKind::Binary { operation, rhs, lhs } => {
                use crate::parser::expression::BinaryOperator as B;

                let bytes = lhs.typ().machine_type().bytes();
                let lhs_type = lhs.typ();
                let is_float = lhs_type.is_float();
                let lhs = self.lower_operand(&lhs);
                let rhs = self.lower_operand(&rhs);

                match operation {
                    B::Div if is_float => {
                        self.lir.push_instr(id, X86Instr::MovFloat { dest, src: lhs, bytes });
                        self.lir.push_instr(id, X86Instr::DivFloat { dest, src: rhs, bytes });
                    }
                    B::Div => {
                        let dividend = self.lir.new_vreg(lhs_type.machine_type());
                        self.lir.push_instr(
                            id,
                            X86Instr::Mov {
                                dest: dividend,
                                src: lhs,
                                bytes,
                            },
                        );
                        self.lir.push_instr(id, X86Instr::idiv(dest, dividend, rhs, bytes));
                    }

                    comp @ (B::Lt | B::LtEq | B::Gt | B::GtEq | B::Eq | B::Ne) => {
                        self.lower_cmp(id, dest, lhs, rhs, bytes, is_float, Condition::new(&comp, is_float))
                    }

                    _ => {
                        let copy = match is_float {
                            true => X86Instr::MovFloat { dest, bytes, src: lhs },
                            _ => X86Instr::Mov { dest, src: lhs, bytes },
                        };
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
                let ret_class = (typ != Type::Unit).then(|| typ.machine_type().class());
                self.lir.push_instr(id, X86Instr::call(callee, moves, ret, ret_class));
            }

            InstructionKind::Syscall { code, args } => {
                let mut moves = Vec::with_capacity(args.len());
                let mut uses = Vec::with_capacity(args.len());

                for (i, arg) in args.iter().enumerate() {
                    let abi_reg = X86_64::syscall_param(i).expect("too many syscall arguments");
                    let vreg = self.operand(arg, id);

                    moves.push((vreg, abi_reg));
                    uses.push(vreg);
                }

                let ret = (typ != Type::Unit).then_some(dest);
                self.lir.push_instr(
                    id,
                    X86Instr::Syscall {
                        id: *code as u32,
                        moves,
                        uses,
                        ret,
                        precoloured_def: ret.map(|v| (v, X86Reg::Rax)),
                    },
                );
            }
        }
    }

    fn lower_cmp(
        &mut self,
        id: &BlockId,
        dest: VReg,
        lhs: X86Operand,
        rhs: X86Operand,
        bytes: u8,
        is_float: bool,
        condition: Condition,
    ) {
        // 1-byte result of setcc, then zero-extended into dest
        let flag = self.lir.new_vreg(MachineType::Int { bytes: 1 });

        match is_float {
            true => {
                let X86Operand::VReg(lhs) = lhs else {
                    panic!("float lhs must be a virtual register");
                };
                self.lir.push_instr(id, X86Instr::cmp::<2>(lhs, rhs, bytes));
            }

            _ => {
                let lhs = match lhs {
                    X86Operand::VReg(reg) => reg,
                    _ => {
                        let dest = self.lir.new_vreg(MachineType::Int { bytes });
                        self.lir.push_instr(id, X86Instr::Mov { dest, src: lhs, bytes });

                        dest
                    }
                };

                self.lir.push_instr(id, X86Instr::cmp::<0>(lhs, rhs, bytes));
            }
        }

        self.lir.push_instr(id, X86Instr::Setcc { dest: flag, condition });
        self.lir.push_instr(id, X86Instr::Movzx { dest, src: flag });

        // movzx widens 1-byte setcc result to i32, so we need to update dest's type
        self.lir.set_vreg_type(dest, MachineType::Int { bytes: 4 });
    }

    #[inline(always)]
    fn lower_terminator(&mut self, id: &BlockId, terminator: mir::Terminator) {
        use crate::mir::Terminator as T;

        let terminator = match terminator {
            T::Return(None) => Term::Return(None),
            T::Return(Some(operand)) => Term::Return(Some(self.operand(&operand, id))),
            T::Jump(block) => Term::Jump(block.into()),
            T::Branch {
                condition,
                then_block,
                else_block,
            } => Term::Branch {
                cond: self.operand(&condition, id),
                then_block: then_block.into(),
                else_block: else_block.into(),
            },
        };

        self.lir.set_term(id, terminator);
    }

    fn lower_block(&mut self, id: &BlockId, block: &mir::Block) {
        for instruction in &block.instructions {
            self.lower_instruction(id, instruction);
        }

        self.lower_terminator(id, block.terminator);
    }

    /// Copy physical ABI registers into VRegs
    ///
    /// each parameter arrives in a physical ABI register
    /// we create a fresh VReg precoloured to that ABI register, then emit a regular `Mov` from
    /// it into the parameter's VReg. this makes the data flow explicit and
    /// prevents the allocator from assigning the ABI register to another
    /// VReg before the parameter has been read
    fn lower_param_moves(&mut self) {
        let entry = BlockId(0);
        let mut int_idx = 0;
        let mut float_idx = 0;

        for (vid, typ) in &self.function.params {
            let mt = typ.machine_type();
            let class = mt.class();

            let abi_reg = match class {
                RegClass::Int => {
                    let r = X86_64::param(int_idx, RegClass::Int);
                    int_idx += 1;
                    r
                }
                RegClass::Float => {
                    let r = X86_64::param(float_idx, RegClass::Float);
                    float_idx += 1;
                    r
                }
            };

            if let Some(reg) = abi_reg {
                let dest = self.vreg(*vid);
                let abi_vreg = self.lir.new_vreg(mt);

                // precolour abi_vreg to physical ABI register
                self.lir.add_precolour(abi_vreg, reg);

                let instr = match class {
                    RegClass::Float => X86Instr::MovFloat {
                        dest,
                        src: X86Operand::VReg(abi_vreg),
                        bytes: mt.bytes(),
                    },
                    RegClass::Int => X86Instr::Mov {
                        dest,
                        src: X86Operand::VReg(abi_vreg),
                        bytes: mt.bytes(),
                    },
                };

                self.lir.push_instr(&entry, instr);
            }
        }
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
            Operand::Const(Const::Str { id, .. }) => X86Operand::RipRel(format!(".L_str_{id}(%rip)")),
            Operand::Const(Const::Unit) => unreachable!("unit operand"),
        }
    }

    fn constant_mov(&mut self, dest: VReg, c: &Const) -> X86Instr {
        let bytes = c.typ().machine_type().bytes();
        let src = self.lower_operand(&Operand::Const(*c));

        match c {
            Const::Int(_, _) => X86Instr::Mov { dest, src, bytes },
            Const::Bool(_) => X86Instr::Mov { dest, src, bytes: 4 },
            Const::Float(_, _) => X86Instr::MovFloat {
                dest,
                src,
                bytes: bytes,
            },
            Const::Str { id, .. } => X86Instr::Lea {
                dest,
                src: X86Operand::RipRel(format!(".L_str_{id}(%rip)")),
            },
            Const::Unit => unreachable!("unit operand"),
        }
    }
}

impl Type {
    // TODO: abstract this later in a `SizedType` trait or something
    // to have arch depedent sizes
    pub(in crate::lir) fn machine_type(&self) -> MachineType {
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
