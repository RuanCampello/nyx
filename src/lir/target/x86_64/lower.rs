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

use crate::hir::{SymbolTable, TypeKind};
use crate::lir::{
    self, BlockId, MachineType, VReg,
    target::{
        self, AggregateCopy, Lower, Lowerable, MemOps, Target, aggregate_copy,
        x86_64::{Condition, X86_64, X86Instr, X86Operand, X86Reg},
    },
};
use crate::mir::{self, Function, Operand};

impl Lowerable for X86_64 {
    fn lower(
        function: &Function,
        symbols: &SymbolTable,
        all_functions: &[Function],
        struct_layouts: &[mir::Layout],
        enum_layouts: &[mir::Layout],
        array_layouts: &[mir::Layout],
    ) -> lir::Function<Self> {
        let mut lower = Lower::<X86_64>::new(
            function,
            symbols,
            all_functions,
            struct_layouts,
            enum_layouts,
            array_layouts,
        );

        lower.lower_param_moves();

        for (idx, block) in function.blocks.iter().enumerate() {
            lower.lower_block(&BlockId(idx as u32), block);
        }

        lower.lir
    }
}

impl<'f> Lower<'f, X86_64> {
    fn lower_instruction(&mut self, id: &BlockId, instruction: &mir::Instruction) {
        use crate::mir::InstructionKind;

        let dest = self.value[instruction.dest.id];
        let typ = instruction.dest.typ;
        let bytes = match typ.kind() {
            TypeKind::Unit => 0,
            _ => typ.machine_type(self.layouts).bytes(),
        };
        let is_float = typ.is_float();

        match &instruction.kind {
            InstructionKind::Assign(op) => {
                let value = &self.value;
                let layouts = self.layouts;

                if let Some(instr) = target::lower_assign(
                    &mut self.lir,
                    id,
                    dest,
                    typ,
                    op,
                    layouts,
                    |vid| value[vid],
                    |lir, op, _| target::lower_operand(lir, op, |vid| value[vid]),
                ) {
                    self.lir.push_instr(id, instr);
                }
            },

            InstructionKind::Unary { operation, rhs } => {
                use crate::parser::expression::UnaryOperator as U;

                const NEG_ZERO: u64 = (-0.0f64).to_bits();
                const NEG_ZERO_32: u64 = (-0.0f32).to_bits() as u64;

                let src = self.lower_operand(rhs);

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
                    },
                    U::Neg => self.lir.push_instr(id, X86Instr::Neg { dest, bytes }),
                    U::Not => match typ.kind() == TypeKind::Bool {
                        true => self.lir.push_instr(
                            id,
                            X86Instr::Xor { dest, src: X86Operand::Imm(1), bytes: 4 },
                        ),
                        _ => self.lir.push_instr(id, X86Instr::Not { dest, bytes }),
                    },
                    U::Deref => unreachable!(),
                    U::Ref | U::RefMut => unreachable!(
                        "UnaryOperator::Ref is lowered to InstructionKind::AddressOf in MIR and never reaches LIR Unary lowering"
                    ),
                }
            },

            #[rustfmt::skip]
            InstructionKind::Binary {
                operation,
                rhs,
                lhs,
                checked,
            } => {
                use crate::parser::expression::BinaryOperator as B;

                let lhs_mt = lhs.typ().machine_type(self.layouts);
                let bytes = lhs_mt.bytes();
                let is_signed = lhs_mt.is_signed();
                let lhs_type = lhs.typ();
                let is_float = lhs_type.is_float();
                let lhs = self.lower_operand(lhs);
                let rhs = self.lower_operand(rhs);
                let checked = *checked;

                match operation {
                    B::Div if is_float => {
                        self.lir.push_instr(id, X86Instr::MovFloat { dest, src: lhs, bytes });
                        self.lir.push_instr(id, X86Instr::DivFloat { dest, src: rhs, bytes });
                    }
                    B::Div => {
                        let dividend = self.lir.new_vreg(lhs_type.machine_type(self.layouts));
                        self.lir.push_instr(id, X86Instr::Mov { dest: dividend, src: lhs, bytes });
                        self.lir.push_instr(id, X86Instr::idiv(dest, dividend, rhs, bytes));
                    }

                    comp @ (B::Lt | B::LtEq | B::Gt | B::GtEq | B::Eq | B::Ne) => self.lower_cmp(
                        id,
                        dest,
                        lhs,
                        rhs,
                        bytes,
                        is_float,
                        Condition::new(comp, is_float),
                    ),

                    _ => {
                        let copy = match is_float {
                            true => X86Instr::MovFloat { dest, bytes, src: lhs },
                            _ => X86Instr::Mov { dest, src: lhs, bytes },
                        };
                        self.lir.push_instr(id, copy);

                        let arith = match operation {
                            B::Add => match is_float {
                                true => X86Instr::AddFloat { dest, src: rhs, bytes },
                                _ => X86Instr::Add { dest, src: rhs, bytes, checked },
                            },

                            B::Sub => match is_float {
                                true => X86Instr::SubFloat { dest, src: rhs, bytes },
                                _ => X86Instr::Sub { dest, src: rhs, bytes, checked },
                            },

                            B::Mul => match is_float {
                                true => X86Instr::MulFloat { dest, src: rhs, bytes },
                                _ => X86Instr::Imul { dest, src: rhs, bytes, checked },
                            },

                            B::And => X86Instr::And { dest, src: rhs, bytes },
                            B::Or => X86Instr::Or { dest, src: rhs, bytes },
                            B::BitAnd => X86Instr::And { dest, src: rhs, bytes },
                            B::BitOr => X86Instr::Or { dest, src: rhs, bytes },
                            B::BitXor => X86Instr::Xor { dest, src: rhs, bytes },
                            B::Shl | B::Shr => {
                                let (src, precoloured_uses) = match rhs {
                                    X86Operand::Imm(i) => (X86Operand::Imm(i), Vec::new()),
                                    X86Operand::VReg(v) => {
                                        let cl_vreg = self.lir.new_vreg(MachineType::Int {
                                            bytes: 1,
                                            signed: false,
                                        });

                                        self.lir.add_precolour(cl_vreg, X86Reg::Rcx);
                                        let instr = X86Instr::Mov { dest: cl_vreg, src: X86Operand::VReg(v), bytes: 1 };
                                        self.lir.push_instr( id, instr);

                                        (X86Operand::VReg(cl_vreg), vec![(cl_vreg, X86Reg::Rcx)])
                                    }
                                    _ => unsafe { std::hint::unreachable_unchecked() },
                                };

                                match operation {
                                    B::Shl => X86Instr::Shl { dest, src, bytes, precoloured_uses },
                                    B::Shr => match is_signed {
                                        true => X86Instr::Sar { dest, src, bytes, precoloured_uses },
                                        _ => X86Instr::Shr { dest, src, bytes, precoloured_uses },
                                    }
                                    _ => unsafe { std::hint::unreachable_unchecked() },
                                }
                            }

                            _ => unsafe { std::hint::unreachable_unchecked() },
                        };

                        self.lir.push_instr(id, arith);
                    }
                };
            },

            InstructionKind::FieldLoad { src, offset, typ } => {
                if typ.is_aggregate() {
                    let origin = match src {
                        Operand::Place(p) => self.value[p.id],
                        Operand::Const(_) => unreachable!("struct constant in field access"),
                    };
                    let size = typ.machine_type(self.layouts).stack_size() as u32;
                    let is_src_ref = matches!(src.typ().kind(), TypeKind::Ref { .. });

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
                let is_float = typ.is_float();
                let signed = mt.is_signed();
                match src {
                    Operand::Place(place) => {
                        let origin = self.value[place.id];
                        let instruction = X86_64::scalar_load(
                            matches!(place.typ.kind(), TypeKind::Ref { .. }),
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

            InstructionKind::FieldStore { value, offset } => {
                if value.typ().is_aggregate() {
                    let Operand::Place(src) = value else {
                        unreachable!("aggregate field store source must be a place");
                    };
                    let size = value.typ().machine_type(self.layouts).stack_size() as u32;
                    let src_vreg = self.value[src.id];

                    return aggregate_copy(
                        &mut self.lir,
                        id,
                        AggregateCopy {
                            src: src_vreg,
                            dest,
                            src_ref: false,
                            dest_ref: matches!(typ.kind(), TypeKind::Ref { .. }),
                            src_base: 0,
                            dest_base: *offset as i32,
                            size,
                        },
                    );
                }

                let mt = value.typ().machine_type(self.layouts);
                let bytes = mt.bytes();
                let is_float = value.typ().is_float();
                let src = self.lower_operand(value);

                let instruction = X86_64::scalar_store(
                    matches!(typ.kind(), TypeKind::Ref { .. }),
                    dest,
                    src,
                    *offset as i32,
                    bytes,
                    is_float,
                );
                self.lir.push_instr(id, instruction);
            },

            InstructionKind::ElementLoad { base, index, bound, stride, typ } => {
                self.lower_element_load(id, dest, base, index, bound, *stride, *typ)
            },

            InstructionKind::ElementStore { index, bound, value, stride } => {
                self.lower_element_store(id, dest, typ, index, bound, value, *stride)
            },

            InstructionKind::ElementAddr { base, index, bound, stride } => {
                self.lower_element_addr(id, dest, base, index, bound, *stride)
            },

            InstructionKind::AddressOf { src, offset } => {
                self.lower_address_of(id, dest, src, *offset)
            },

            InstructionKind::Call { callee, args } => self.lower_call(id, dest, *callee, args),

            InstructionKind::Syscall { code, args, returns } => {
                let value = &self.value;
                let layouts = self.layouts;
                let (syscall_moves, syscall_uses) =
                    target::prepare_syscall_args(&mut self.lir, id, args, layouts, |lir, op, _| {
                        target::lower_operand(lir, op, |vid| value[vid])
                    });

                let ret = (*returns && typ.kind() != TypeKind::Unit).then_some(dest);
                self.lir.push_instr(
                    id,
                    X86Instr::Syscall {
                        id: X86_64::syscall_code(*code) as u32,
                        moves: syscall_moves,
                        uses: syscall_uses,
                        ret,
                    },
                );
            },

            InstructionKind::Cast { src, typ } => {
                let src_mt = src.typ().machine_type(self.layouts);
                let src_bytes = src_mt.bytes();
                let src_signed = src_mt.is_signed();

                let dest_mt = typ.machine_type(self.layouts);
                let dest_bytes = dest_mt.bytes();

                let src_op = self.lower_operand(src);

                let is_downcast_or_equal = src_bytes >= dest_bytes;
                let is_immediate = matches!(src_op, X86Operand::Imm(_));

                #[rustfmt::skip]
                let instr = match (is_downcast_or_equal, is_immediate) {
                    // standard move for downcasts, equal sizes, or immediate upcasts
                    (true, _) | (false, true) => X86Instr::Mov { dest, src: src_op, bytes: dest_bytes },
                    // upcasting a register/label requires extension based on signedness
                    (false, false) if src_signed => X86Instr::Movsx { dest, src: src_op, src_bytes, dest_bytes},
                    (false, false) => X86Instr::Movzx { dest, src: src_op, src_bytes, dest_bytes },
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
        is_float: bool,
        condition: Condition,
    ) {
        // 1-byte result of setcc, then zero-extended into dest
        let flag = self.lir.new_vreg(MachineType::Int { bytes: 1, signed: false });

        match is_float {
            true => {
                let X86Operand::VReg(lhs) = lhs else {
                    panic!("float lhs must be a virtual register");
                };
                self.lir.push_instr(id, X86Instr::Ucomis { lhs, rhs, bytes });
            },

            _ => {
                let lhs = match lhs {
                    X86Operand::VReg(reg) => reg,
                    _ => {
                        let dest = self.lir.new_vreg(MachineType::Int { bytes, signed: false });
                        self.lir.push_instr(id, X86Instr::Mov { dest, src: lhs, bytes });

                        dest
                    },
                };

                self.lir.push_instr(id, X86Instr::Cmp { lhs, rhs, bytes });
            },
        }

        self.lir.push_instr(id, X86Instr::Setcc { dest: flag, condition });
        self.lir.push_instr(
            id,
            X86Instr::Movzx {
                dest,
                src: X86Operand::VReg(flag),
                src_bytes: 1,
                dest_bytes: 4,
            },
        );

        // movzx widens 1-byte setcc result to i32, so we need to update dest's type
        self.lir.set_vreg_type(dest, MachineType::Int { bytes: 4, signed: false });
    }

    fn lower_block(&mut self, id: &BlockId, block: &mir::Block) {
        for instruction in &block.instructions {
            self.lower_instruction(id, instruction);
        }

        self.lower_terminator(id, block.terminator.clone());
    }
}
