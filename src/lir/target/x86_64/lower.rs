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

use crate::hir::{self, SymbolTable, TypeKind};
use crate::lir::{
    self, BlockId, MachineType, Term, VReg, assembly_label,
    target::{
        self, AggregateCopy, Lower, Lowerable, MemOps, RegClass, Target, aggregate_copy,
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
                    U::Ref => unreachable!(
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
                let typ = *typ;
                let int8 = MachineType::Int { bytes: 8, signed: false };

                // materialise the index into a 64-bit register, shared by the
                // bounds check and the address computation
                let index_reg = self.lir.new_vreg(int8);
                let index_src = self.lower_operand(index);
                let instr = X86Instr::Mov { dest: index_reg, src: index_src, bytes: 8 };
                self.lir.push_instr(id, instr);

                let bound = self.lower_operand(bound);
                let instr = X86Instr::BoundsCheck { index: index_reg, bound };
                self.lir.push_instr(id, instr);

                // element address = base address + index * stride
                let Operand::Place(base) = base else {
                    unreachable!("indexing a constant aggregate");
                };
                let origin = self.value[base.id];
                let addr = self.lir.new_vreg(int8);
                match base.typ.kind() {
                    TypeKind::Ref { .. } => self.lir.push_instr(
                        id,
                        X86Instr::Mov { dest: addr, src: X86Operand::VReg(origin), bytes: 8 },
                    ),
                    _ => self.lir.push_instr(id, X86Instr::StackAddr { dest: addr, origin }),
                }

                let offset = self.lir.new_vreg(int8);
                let instr =
                    X86Instr::Mov { dest: offset, src: X86Operand::VReg(index_reg), bytes: 8 };
                self.lir.push_instr(id, instr);

                let src = X86Operand::Imm(*stride as i64);
                let instr = X86Instr::Imul { dest: offset, src, bytes: 8, checked: false };
                self.lir.push_instr(id, instr);

                // the accumulator is freshly copied before the add,
                // so its register stays distinct from the base address
                let element = self.lir.new_vreg(int8);
                let instr = X86Instr::Mov { dest: element, src: X86Operand::VReg(addr), bytes: 8 };
                self.lir.push_instr(id, instr);

                let src = X86Operand::VReg(offset);
                let instr = X86Instr::Add { dest: element, src, bytes: 8, checked: false };
                self.lir.push_instr(id, instr);

                match typ.is_aggregate() {
                    true => {
                        let size = typ.machine_type(self.layouts).stack_size() as u32;
                        let copy = AggregateCopy {
                            src: element,
                            dest,
                            src_ref: true,
                            dest_ref: false,
                            src_base: 0,
                            dest_base: 0,
                            size,
                        };
                        aggregate_copy(&mut self.lir, id, copy);
                    },
                    false => {
                        let mt = typ.machine_type(self.layouts);
                        let instruction = X86_64::scalar_load(
                            true,
                            dest,
                            element,
                            0,
                            mt.bytes(),
                            typ.is_float(),
                            mt.is_signed(),
                        );
                        self.lir.push_instr(id, instruction);
                    },
                }
            },

            #[rustfmt::skip]
            InstructionKind::AddressOf { src, offset } => {
                let origin = self.value[src.id];
                match src.typ.kind() {
                    TypeKind::Ref { .. } => self.lir.push_instr(id, X86Instr::Mov { dest, src: X86Operand::VReg(origin), bytes: 8 }),
                    _ => self.lir.push_instr(id, X86Instr::StackAddr { dest, origin }),
                }

                if *offset != 0 {
                    self.lir.push_instr(
                        id,
                        X86Instr::Add { dest, src: X86Operand::Imm(*offset as i64), bytes: 8, checked: false },
                    );
                }
            },

            InstructionKind::Call { callee, args } => {
                let callee_id = *callee;
                let callee_fn = self
                    .all_functions
                    .iter()
                    .find(|f| f.id == callee_id)
                    .unwrap_or_else(|| panic!("callee function {callee_id:?} not found"));

                let (bytes, signed, is_float) = (8, false, false);

                if callee_fn.intrinsic == Some(hir::Intrinsic::Len) {
                    let ptr = self.operand(&args[0], id);
                    return self.lir.push_instr(
                        id,
                        X86Instr::PtrLoad { dest, ptr, offset: 8, bytes, is_float },
                    );
                }

                let callee = assembly_label(self.symbols.get(callee_fn.name_symbol));

                let mut int_idx = 0;
                let return_type = callee_fn.return_type;
                let aggregate_ret = match return_type.is_aggregate() {
                    true => {
                        super::small_integer_return(return_type, self.layouts).unwrap_or_default()
                    },
                    _ => Vec::new(),
                };

                let mut moves = Vec::new();
                if return_type.is_aggregate() && aggregate_ret.is_empty() {
                    let ptr = self.stack_addr(id, dest);
                    let abi_reg = X86_64::param(int_idx, RegClass::Int)
                        .expect("sret pointer must fit in the first integer argument register");
                    moves.push((ptr, abi_reg));
                    int_idx += 1;
                }

                let value = &self.value;
                let layouts = self.layouts;

                let (arg_moves, stack_args) = target::prepare_call_args(
                    &mut self.lir,
                    id,
                    args,
                    layouts,
                    |vid| value[vid],
                    |lir, op, block| target::operand(lir, op, block, layouts, |vid| value[vid]),
                    |lir, op, _| target::lower_operand(lir, op, |vid| value[vid]),
                    |lir, block, origin| {
                        let dest = lir.new_vreg(MachineType::Int { bytes, signed });
                        lir.push_instr(block, X86Instr::StackAddr { dest, origin });
                        dest
                    },
                    int_idx,
                    0,
                );
                moves.extend(arg_moves);

                let ret = (return_type.kind() != TypeKind::Unit && !return_type.is_aggregate())
                    .then_some(dest);
                let mut ret_vregs = Vec::with_capacity(aggregate_ret.len());
                for &(_, bytes, reg) in &aggregate_ret {
                    let vreg = self.lir.new_vreg(MachineType::Int { bytes, signed: false });
                    self.lir.add_precolour(vreg, reg);
                    ret_vregs.push(vreg);
                }

                self.lir.push_instr(
                    id,
                    X86Instr::call(callee, moves, stack_args, ret, ret_vregs.clone()),
                );

                for ((offset, bytes, _), src) in aggregate_ret.into_iter().zip(ret_vregs) {
                    let src = X86Operand::VReg(src);
                    #[rustfmt::skip]
                    let instr = X86Instr::FieldStore { origin: dest, src, offset, bytes, is_float: false };
                    self.lir.push_instr(id, instr);
                }
            },

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
                self.lir.push_instr(id, X86Instr::cmp::<2>(lhs, rhs, bytes));
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

                self.lir.push_instr(id, X86Instr::cmp::<0>(lhs, rhs, bytes));
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

    fn lower_terminator(&mut self, id: &BlockId, terminator: mir::Terminator) {
        use crate::mir::Terminator as T;

        let terminator = match terminator {
            T::Return(None) => Term::Return(None),
            T::Return(Some(operand)) if operand.typ().is_aggregate() => {
                let typ = operand.typ();
                let Operand::Place(place) = operand else {
                    unreachable!("aggregate return source must be a place");
                };

                let src_vreg = self.value[place.id];

                match super::small_integer_return(typ, self.layouts) {
                    Some(chunks) => {
                        for (offset, bytes, reg) in chunks {
                            let ret = self.lir.new_vreg(MachineType::Int { bytes, signed: false });
                            #[rustfmt::skip]
                            let load = X86Instr::FieldLoad { dest: ret, origin: src_vreg, offset, bytes, is_float: false };
                            self.lir.add_precolour(ret, reg);
                            self.lir.push_instr(id, load);
                        }
                    },

                    None => {
                        let sret_ptr = self
                            .sret_ptr
                            .expect("struct-returning function must have an sret pointer");
                        let size = typ.machine_type(self.layouts).stack_size() as u32;
                        let copy = AggregateCopy::new(src_vreg, sret_ptr, size).with_dest_ref();

                        aggregate_copy(&mut self.lir, id, copy);
                    },
                }

                Term::Return(None)
            },
            T::Return(Some(operand)) => Term::Return(Some(self.operand(&operand, id))),
            T::Jump(block) => Term::Jump(block.into()),
            T::Branch { condition, then_block, else_block } => Term::Branch {
                cond: self.operand(&condition, id),
                then_block: then_block.into(),
                else_block: else_block.into(),
            },
            T::Switch { discriminant, targets, default } => {
                let cond = self.operand(&discriminant, id);
                let targets =
                    targets.iter().map(|(val, target)| (*val, (*target).into())).collect();
                Term::Switch { cond, targets, default: default.into() }
            },
        };

        self.lir.set_term(id, terminator);
    }

    fn lower_block(&mut self, id: &BlockId, block: &mir::Block) {
        for instruction in &block.instructions {
            self.lower_instruction(id, instruction);
        }

        self.lower_terminator(id, block.terminator.clone());
    }
}
