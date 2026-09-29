//! MIR -> x86_64 LIR lowering
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

use crate::hir::{EnumRepr, SymbolTable, TypeKind};
use crate::lir::{
    self, BlockId, MachineType, TypeExt, VReg,
    target::{
        self, AggregateCopy, Lower, Lowerable, MemOps, Target, TargetOps, aggregate_copy,
        x86_64::{
            AluOp, Condition, FloatOp, ShiftOp, UnaryOp, X86_64, X86Instr, X86Operand, X86Reg,
        },
    },
};
use crate::mir::{self, Function, Operand};
use crate::parser::expression::BinaryOperator;

impl Lowerable for X86_64 {
    fn lower(
        function: &Function,
        symbols: &SymbolTable,
        all_functions: &[Function],
        strings: &mir::StringPool,
        layouts: &mir::Layouts<'_>,
        reprs: &[Option<EnumRepr>],
        array_layouts: &[mir::Layout],
    ) -> lir::Function<Self> {
        let mut lower = Lower::<X86_64>::new(
            function,
            symbols,
            all_functions,
            strings,
            layouts,
            reprs,
            array_layouts,
        );

        lower.lower_param_moves();

        for (idx, block) in function.blocks.iter().enumerate() {
            lower.lower_block(&BlockId(idx as u32), block);
        }

        lower.lir
    }
}

impl<'f, 'hir> Lower<'f, 'hir, X86_64> {
    fn lower_instruction(&mut self, id: &BlockId, instruction: &mir::Instruction) {
        use crate::mir::InstructionKind as Inst;

        let dest = self.value[instruction.dest.id];
        let typ = instruction.dest.typ;
        let bytes = match typ.kind() {
            TypeKind::Unit => 0,
            _ => typ.machine_type(self.layouts).bytes(),
        };
        let is_float = typ.is_float();

        match &instruction.kind {
            Inst::Assign(op) => self.lower_assign(id, dest, typ, op),
            Inst::Unary { operation, rhs } => {
                use crate::parser::expression::UnaryOperator as U;

                let src = self.lower_operand(rhs, id);

                // 2-address: copy rhs into dest first to mutate it in-place
                let copy = match is_float {
                    true => X86Instr::MovFloat { dest, src, bytes },
                    _ => X86Instr::Mov { dest, src, bytes },
                };

                self.lir.push_instr(id, copy);

                match operation {
                    U::Neg if is_float => {
                        let bits = match typ.is_32_bit() {
                            true => u64::from((-0.0f32).to_bits()),
                            _ => (-0.0f64).to_bits(),
                        };
                        let float = lir::FloatConstant { bits, bytes, packed: true };
                        let label = self.lir.new_float(float);
                        let src = X86Operand::RipRel(format!("{label}(%rip)"));
                        let instr = X86Instr::AluFloat { op: FloatOp::Xor, dest, src, bytes };

                        self.lir.push_instr(id, instr);
                    },
                    U::Neg => {
                        let instr = X86Instr::Unary { op: UnaryOp::Neg, dest, bytes };
                        self.lir.push_instr(id, instr)
                    },
                    U::Not => match typ.kind() == TypeKind::Bool {
                        true => {
                            let (src, op) = (X86Operand::Imm(1), AluOp::Xor);
                            let instr = X86Instr::Alu { op, dest, src, bytes: 4, checked: false };
                            self.lir.push_instr(id, instr);
                        },
                        _ => self
                            .lir
                            .push_instr(id, X86Instr::Unary { op: UnaryOp::Not, dest, bytes }),
                    },
                    U::Deref => unreachable!(),
                    U::Ref | U::RefMut => unreachable!(
                        "UnaryOperator::Ref is lowered to InstructionKind::AddressOf in MIR and never reaches LIR Unary lowering"
                    ),
                }
            },

            #[rustfmt::skip]
            Inst::Binary {
                operation,
                rhs,
                lhs,
                overflow,
            } => {
                use crate::parser::expression::BinaryOperator as B;

                let lhs_mt = lhs.typ().machine_type(self.layouts);
                let bytes = lhs_mt.bytes();
                let is_signed = lhs_mt.is_signed();
                let lhs_type = lhs.typ();
                let is_float = lhs_type.is_float();
                let rhs_bytes = rhs.typ().machine_type(self.layouts).bytes();
                let lhs = self.lower_operand(lhs, id);
                let rhs = self.lower_operand(rhs, id);
                let checked = overflow.is_checked();

                match operation {
                    B::Div if is_float => {
                        self.lir.push_instr(id, X86Instr::MovFloat { dest, src: lhs, bytes });
                        self.lir.push_instr(id, X86Instr::AluFloat { op: FloatOp::Div, dest, src: rhs, bytes });
                    }
                    B::Div | B::Rem if !is_float => {
                        let mt = lhs_type.machine_type(self.layouts);
                        let dividend = self.lir.new_vreg(mt);
                        let remainder = matches!(operation, B::Rem);

                        if let Some(divisor) = self.divisor_check(id, &rhs, mt, 0) {
                            self.lir.push_instr(id, X86Instr::ZeroCheck { divisor, bytes });
                        }

                        self.lir.push_instr(id, X86Instr::Mov { dest: dividend, src: lhs, bytes });

                        if is_signed && let Some(divisor) = self.divisor_check(id, &rhs, mt, -1) {
                            self.lir.push_instr(id, X86Instr::DivOverflowCheck { dividend, divisor, bytes });
                        }
                        self.lir.push_instr(id, X86Instr::idiv(dest, dividend, rhs, bytes, is_signed, remainder));
                    }

                    B::Rem => {
                        let lhs = self.float_reg(id, lhs, bytes);
                        let rhs = self.float_reg(id, rhs, bytes);
                        self.lower_float_remainder(id, dest, lhs, rhs, bytes);
                    }

                    // two-operand 'imul' has no one-byte form at all, and its overflow
                    // flag is a signed test that rejects legal unsigned results
                    B::Mul if !is_float && (bytes == 1 || (checked && !is_signed)) => {
                        let factor = self.lir.new_vreg(lhs_type.machine_type(self.layouts));
                        self.lir.push_instr(id, X86Instr::Mov { dest: factor, src: lhs, bytes });
                        self.lir.push_instr(id, X86Instr::wide_mul(dest, factor, rhs, bytes, is_signed, checked));
                    }

                    comp @ (B::Lt | B::LtEq | B::Gt | B::GtEq | B::Eq | B::Ne) => match is_float {
                        true => self.lower_float_cmp(id, dest, lhs, rhs, bytes, comp),
                        _ => self.lower_cmp(id, dest, lhs, rhs, bytes, Condition::new(comp, !is_signed)),
                    },
                    _ => {
                        let copy = match is_float {
                            true => X86Instr::MovFloat { dest, bytes, src: lhs },
                            _ => X86Instr::Mov { dest, src: lhs, bytes },
                        };
                        self.lir.push_instr(id, copy);

                        let arith = match operation {
                            B::Add => match is_float {
                                true => X86Instr::AluFloat { op: FloatOp::Add, dest, src: rhs, bytes },
                                _ => X86Instr::Alu { op: AluOp::Add, dest, src: rhs, bytes, checked },
                            },

                            B::Sub => match is_float {
                                true => X86Instr::AluFloat { op: FloatOp::Sub, dest, src: rhs, bytes },
                                _ => X86Instr::Alu { op: AluOp::Sub, dest, src: rhs, bytes, checked },
                            },

                            B::Mul => match is_float {
                                true => X86Instr::AluFloat { op: FloatOp::Mul, dest, src: rhs, bytes },
                                _ => X86Instr::Alu { op: AluOp::Mul, dest, src: rhs, bytes, checked },
                            },

                            B::And => X86Instr::Alu { op: AluOp::And, dest, src: rhs, bytes, checked: false },
                            B::Or => X86Instr::Alu { op: AluOp::Or, dest, src: rhs, bytes, checked: false },
                            B::BitAnd => X86Instr::Alu { op: AluOp::And, dest, src: rhs, bytes, checked: false },
                            B::BitOr => X86Instr::Alu { op: AluOp::Or, dest, src: rhs, bytes, checked: false },
                            B::BitXor => X86Instr::Alu { op: AluOp::Xor, dest, src: rhs, bytes, checked: false },
                            B::Shl | B::Shr => {
                                let bits = bytes * 8;
                                let (src, precoloured_uses) = match rhs {
                                    X86Operand::Imm(n) if !checked || (0..i64::from(bits)).contains(&n) => {
                                        (X86Operand::Imm(n.rem_euclid(i64::from(bits))), Vec::new())
                                    },
                                    amount => {
                                        let amount = self.ensure_reg(id, amount, rhs_bytes);
                                        if checked {
                                            self.lir.push_instr(id, X86Instr::ShiftCheck { amount, bytes: rhs_bytes, bits });
                                        }

                                        let count = self.lir.new_vreg(MachineType::Int { bytes: 1, signed: false });
                                        self.lir.add_precolour(count, X86Reg::Rcx);
                                        self.lir.push_instr(id, X86Instr::Mov { dest: count, src: X86Operand::VReg(amount), bytes: 1 });

                                        if !checked && bits < 32 {
                                            let mask = X86Operand::Imm(i64::from(bits) - 1);
                                            let instr = X86Instr::Alu { op: AluOp::And, dest: count, src: mask, bytes: 1, checked: false };
                                            self.lir.push_instr(id, instr);
                                        }

                                        (X86Operand::VReg(count), vec![(count, X86Reg::Rcx)])
                                    },
                                };

                                match operation {
                                    B::Shl => X86Instr::Shift { op: ShiftOp::Left, dest, src, bytes, precoloured_uses },
                                    B::Shr => match is_signed {
                                        true => X86Instr::Shift { op: ShiftOp::ArithmeticRight, dest, src, bytes, precoloured_uses },
                                        _ => X86Instr::Shift { op: ShiftOp::Right, dest, src, bytes, precoloured_uses },
                                    }
                                    _ => unreachable!("the enclosing arm only admits `<<` and `>>`"),
                                }
                            }

                            _ => unreachable!("every other integer operator is lowered by an earlier arm"),
                        };

                        self.lir.push_instr(id, arith);
                    }
                };
            },

            Inst::FieldLoad { src, offset, typ } => {
                if typ.is_aggregate_lir(self.layouts) {
                    let origin = match src {
                        Operand::Place(p) => self.value[p.id],
                        Operand::Const(_) => unreachable!("struct constant in field access"),
                    };
                    let size = typ.machine_type(self.layouts).stack_size() as u32;
                    let is_src_ref = src.typ().is_pointer();

                    return aggregate_copy(
                        &mut self.lir,
                        id,
                        AggregateCopy {
                            src: origin,
                            dest,
                            src_ref: is_src_ref,
                            dest_ref: false,
                            src_base: *offset as i32,
                            dest_base: 0,
                            size,
                        },
                    );
                }

                let mt = typ.machine_type(self.layouts);
                let bytes = mt.bytes();
                let (is_float, signed) = (typ.is_float(), mt.is_signed());
                match src {
                    Operand::Place(place) => {
                        let origin = self.value[place.id];
                        let instruction = X86_64::scalar_load(
                            place.typ.is_pointer(),
                            dest,
                            origin,
                            *offset as i32,
                            bytes,
                            is_float,
                            signed,
                        );
                        self.lir.push_instr(id, instruction);
                    },
                    Operand::Const(_) => unreachable!("struct constant in field access"),
                }
            },

            Inst::FieldStore { value, offset } => {
                if value.typ().is_aggregate_lir(self.layouts) {
                    let (dest_ref, dest_base) = (typ.is_pointer(), *offset as i32);
                    return self.lower_aggregate_store(id, dest, dest_ref, dest_base, value);
                }

                let mt = value.typ().machine_type(self.layouts);
                let (bytes, is_float) = (mt.bytes(), value.typ().is_float());
                let src = self.lower_operand(value, id);

                let instruction = X86_64::scalar_store(
                    typ.is_pointer(),
                    dest,
                    src,
                    *offset as i32,
                    bytes,
                    is_float,
                );
                self.lir.push_instr(id, instruction);
            },

            Inst::ElementLoad { base, index, bound, stride, typ } => {
                self.lower_element_load(id, dest, base, index, bound, *stride, *typ)
            },

            Inst::ElementStore { index, bound, value, stride } => {
                self.lower_element_store(id, dest, typ, index, bound, value, *stride)
            },

            Inst::ElementAddr { base, index, bound, stride } => {
                self.lower_element_addr(id, dest, base, index, bound, *stride)
            },

            Inst::StaticAddr { id: static_id } => {
                let label = lir::static_label(*static_id);
                let load = X86_64::load_label(dest, label, false, 8);

                self.lir.push_instr(id, load);
            },

            Inst::AddressOf { src, offset } => self.lower_address_of(id, dest, src, *offset),
            Inst::Call { callee, args } => self.lower_call(id, dest, *callee, args),
            Inst::Syscall { code, args, returns } => {
                let value = &self.value;
                let layouts = self.layouts;
                let (moves, uses) = target::prepare_syscall_args(
                    &mut self.lir,
                    id,
                    args,
                    layouts,
                    |lir, op, block| target::lower_operand(lir, op, block, |vid| value[vid]),
                );

                let ret = (*returns && typ.kind() != TypeKind::Unit).then_some(dest);
                let code = X86_64::syscall_code(*code);
                let instr = X86Instr::Syscall { id: code as u32, moves, uses, ret };
                self.lir.push_instr(id, instr);
            },

            Inst::Select { condition, then_value, else_value } => {
                let src = self.lower_operand(else_value, id);
                self.lir.push_instr(id, X86Instr::Mov { dest, src, bytes });

                let then_reg = self.operand(then_value, id);
                let condition_bytes = condition.typ().machine_type(self.layouts).bytes();
                let lhs = self.operand(condition, id);

                let instr = X86Instr::Cmp { lhs, rhs: X86Operand::Imm(0), bytes: condition_bytes };
                self.lir.push_instr(id, instr);

                let instr = X86Instr::Cmov { dest, src: then_reg, condition: Condition::Ne, bytes };
                self.lir.push_instr(id, instr);
            },

            Inst::Cast { src, typ } => {
                let src_mt = src.typ().machine_type(self.layouts);
                let src_bytes = src_mt.bytes();
                let src_signed = src_mt.is_signed();

                let dest_mt = typ.machine_type(self.layouts);
                let dest_bytes = dest_mt.bytes();

                let src_op = self.lower_operand(src, id);

                let is_downcast_or_equal = src_bytes >= dest_bytes;
                let is_immediate = matches!(src_op, X86Operand::Imm(_));

                #[rustfmt::skip]
                let instr = match (is_downcast_or_equal, is_immediate) {
                    // standard move for downcasts, equal sizes, or immediate upcasts
                    (true, _) | (false, true) => X86Instr::Mov { dest, src: src_op, bytes: dest_bytes },
                    // upcasting a register/label requires extension based on signedness
                    _ => X86Instr::Extend { dest, src: src_op, src_bytes, dest_bytes, signed: src_signed },
                };

                self.lir.push_instr(id, instr);
            },
        }
    }

    fn lower_cmp(
        &mut self,
        id: &BlockId,
        dest: VReg,
        lhs: X86Operand,
        rhs: X86Operand,
        bytes: u8,
        condition: Condition,
    ) {
        let lhs = match lhs {
            X86Operand::VReg(reg) => reg,
            _ => {
                let dest = self.lir.new_vreg(MachineType::Int { bytes, signed: false });
                self.lir.push_instr(id, X86Instr::Mov { dest, src: lhs, bytes });

                dest
            },
        };

        self.lir.push_instr(id, X86Instr::Cmp { lhs, rhs, bytes });

        let flag = self.lir.new_vreg(MachineType::Int { bytes: 1, signed: false });
        self.lir.push_instr(id, X86Instr::Setcc { dest: flag, condition });
        self.widen_flag(id, dest, flag);
    }

    /// IEEE requires every ordered comparison with NaN to be false, and `NaN != NaN` to be true
    fn lower_float_cmp(
        &mut self,
        id: &BlockId,
        dest: VReg,
        lhs: X86Operand,
        rhs: X86Operand,
        bytes: u8,
        operator: &BinaryOperator,
    ) {
        use BinaryOperator as B;

        let (lhs, rhs) = match operator {
            B::Lt | B::LtEq => {
                let swapped = self.float_reg(id, rhs, bytes);
                (swapped, X86Operand::VReg(self.float_reg(id, lhs, bytes)))
            },
            _ => (self.float_reg(id, lhs, bytes), rhs),
        };

        self.lir.push_instr(id, X86Instr::Ucomis { lhs, rhs, bytes });

        let flag = self.lir.new_vreg(MachineType::Int { bytes: 1, signed: false });
        let condition = match operator {
            B::Eq => Condition::E,
            B::Ne => Condition::Ne,
            B::Lt | B::Gt => Condition::A,
            _ => Condition::Ae,
        };

        self.lir.push_instr(id, X86Instr::Setcc { dest: flag, condition });

        if let B::Eq | B::Ne = operator {
            let parity = self.lir.new_vreg(MachineType::Int { bytes: 1, signed: false });
            let src = X86Operand::VReg(parity);

            let (condition, op) = match operator {
                B::Eq => (Condition::Np, AluOp::And),
                _ => (Condition::P, AluOp::Or),
            };

            self.lir.push_instr(id, X86Instr::Setcc { dest: parity, condition });
            let instr = X86Instr::Alu { op, dest: flag, src, bytes: 1, checked: false };
            self.lir.push_instr(id, instr);
        }

        self.widen_flag(id, dest, flag);
    }

    /// the divisor as a register when it may be `trapping`, the one value that faults
    fn divisor_check(
        &mut self,
        id: &BlockId,
        rhs: &X86Operand,
        mt: MachineType,
        trapping: i64,
    ) -> Option<VReg> {
        match rhs {
            X86Operand::VReg(reg) => Some(*reg),
            X86Operand::Imm(value) if *value == trapping => {
                Some(self.ensure_reg(id, X86Operand::Imm(trapping), mt.bytes()))
            },
            _ => None,
        }
    }

    fn ensure_reg(&mut self, id: &BlockId, operand: X86Operand, bytes: u8) -> VReg {
        match operand {
            X86Operand::VReg(reg) => reg,
            src => {
                let dest = self.lir.new_vreg(MachineType::Int { bytes, signed: true });
                self.lir.push_instr(id, X86Instr::Mov { dest, src, bytes });
                dest
            },
        }
    }

    /// `ucomis` only compares against a register, so a constant operand is loaded first
    fn float_reg(&mut self, id: &BlockId, operand: X86Operand, bytes: u8) -> VReg {
        match operand {
            X86Operand::VReg(reg) => reg,
            src => {
                let dest = self.lir.new_vreg(MachineType::Float { bytes });
                self.lir.push_instr(id, X86Instr::MovFloat { dest, src, bytes });

                dest
            },
        }
    }

    /// zero-extend the 1-byte `setcc` result into the i32 the comparison produces
    fn widen_flag(&mut self, id: &BlockId, dest: VReg, flag: VReg) {
        let src = X86Operand::VReg(flag);
        let instr = X86Instr::Extend { dest, src, src_bytes: 1, dest_bytes: 4, signed: false };

        self.lir.push_instr(id, instr);
        self.lir.set_vreg_type(dest, MachineType::Int { bytes: 4, signed: false });
    }

    fn lower_block(&mut self, id: &BlockId, block: &mir::Block) {
        for instruction in &block.instructions {
            self.lower_instruction(id, instruction);
        }

        self.lower_terminator(id, block.terminator.clone());
    }
}
