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

use crate::hir::{self, TypeKind};
use crate::lir::{
    self, BlockId, MachineType, Term, VReg, assembly_label,
    target::{
        self, Lowerable, MemOps, RegClass, Target, aggregate_copy,
        x86_64::{Condition, X86_64, X86Instr, X86Operand, X86Reg},
    },
};
use crate::mir::{self, Function, Layout, Operand};

struct Lower<'f> {
    function: &'f Function,
    lir: lir::Function<X86_64>,
    value: Vec<VReg>,
    symbols: &'f [String],
    all_functions: &'f [Function],
    layouts: &'f [Layout],
    sret_ptr: Option<VReg>,
}

impl Lowerable for X86_64 {
    fn lower(
        function: &Function,
        symbols: &[String],
        all_functions: &[Function],
        layouts: &[Layout],
    ) -> lir::Function<Self> {
        let name = symbols
            .get(function.name_symbol)
            .map(|n| assembly_label(n))
            .unwrap_or_else(|| format!("nyx_func_{}", function.name_symbol));

        let mut lir = lir::Function::<X86_64>::new(name);

        let value: Vec<VReg> = function
            .locals
            .iter()
            .map(|(_, typ)| lir.new_vreg(typ.machine_type(layouts)))
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
            layouts,
            sret_ptr: None,
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
                    |lir, op, _| target::lower_operand(lir, op, |vid| value[vid], layouts),
                ) {
                    self.lir.push_instr(id, instr);
                }
            },

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
                let lhs = self.lower_operand(&lhs);
                let rhs = self.lower_operand(&rhs);
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
                        Condition::new(&comp, is_float),
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
                        is_src_ref,
                        false,
                        origin,
                        dest,
                        *offset as i32,
                        0,
                        size,
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
                        false,
                        matches!(typ.kind(), TypeKind::Ref { .. }),
                        src_vreg,
                        dest,
                        0,
                        *offset as i32,
                        size,
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

                let callee = self
                    .symbols
                    .get(callee_fn.name_symbol)
                    .map(|n| assembly_label(n))
                    .unwrap_or_else(|| format!("nyx_func_{}", callee_id.0));

                let mut int_idx = 0;
                let return_type = callee_fn.return_type;
                let aggregate_ret = match return_type.is_aggregate() {
                    true => self.small_integer_return(return_type).unwrap_or_default(),
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
                    |lir, op, _| target::lower_operand(lir, op, |vid| value[vid], layouts),
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
                        target::lower_operand(lir, op, |vid| value[vid], layouts)
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

                match self.small_integer_return(typ) {
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

                        #[rustfmt::skip]
                        aggregate_copy(&mut self.lir, id, false, true, src_vreg, sret_ptr, 0, 0, size);
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
                let targets = targets.iter().map(|(val, target)| (*val, (*target).into())).collect();
                Term::Switch { cond, targets, default: default.into() }
            }
        };

        self.lir.set_term(id, terminator);
    }

    fn lower_block(&mut self, id: &BlockId, block: &mir::Block) {
        for instruction in &block.instructions {
            self.lower_instruction(id, instruction);
        }

        self.lower_terminator(id, block.terminator.clone());
    }

    /// Copy physical ABI (and stack slots) registers into VRegs
    /// each parameter arrives in a physical ABI register or in the caller's stack frame
    fn lower_param_moves(&mut self) {
        let entry = BlockId(0);
        let mut int_idx = 0;
        let mut float_idx = 0;
        let mut int_stack_idx = 0;
        let mut float_stack_idx = 0;

        let return_type = self.function.return_type;
        if return_type.is_aggregate() {
            if self.small_integer_return(return_type).is_some() {
                // Small integer aggregates are returned directly in RAX/RDX.
            } else {
                let ptr = self.lir.new_vreg(MachineType::Int { bytes: 8, signed: false });
                let reg = X86_64::param(int_idx, RegClass::Int)
                    .expect("sret pointer must fit in the first integer argument register");
                self.lir.add_precolour(ptr, reg);
                self.sret_ptr = Some(ptr);
                int_idx += 1;
            }
        }

        for (vid, typ) in &self.function.params {
            if typ.is_aggregate() {
                let ptr = self.lir.new_vreg(MachineType::Int { bytes: 8, signed: false });

                match X86_64::param(int_idx, RegClass::Int) {
                    Some(reg) => self.lir.add_precolour(ptr, reg),
                    None => {
                        let offset = X86_64::param_stack_offset(int_stack_idx, RegClass::Int)
                            .expect("param_stack_offset must be defined when param() returns None");
                        self.lir.push_instr(
                            &entry,
                            X86Instr::MovFromStack { dest: ptr, rbp_offset: offset, bytes: 8 },
                        );
                        int_stack_idx += 1;
                    },
                }

                let size = typ.machine_type(self.layouts).stack_size() as u32;
                let dest = self.value[*vid];
                aggregate_copy(&mut self.lir, &entry, true, false, ptr, dest, 0, 0, size);
                int_idx += 1;
                continue;
            }

            let mt = typ.machine_type(self.layouts);
            let class = mt.class();

            match class {
                RegClass::Int => {
                    match X86_64::param(int_idx, RegClass::Int) {
                        Some(reg) => {
                            let dest = self.value[*vid];
                            let abi_vreg = self.lir.new_vreg(mt);
                            self.lir.add_precolour(abi_vreg, reg);

                            self.lir.push_instr(
                                &entry,
                                X86Instr::Mov {
                                    dest,
                                    src: X86Operand::VReg(abi_vreg),
                                    bytes: mt.bytes(),
                                },
                            );
                        },

                        None => {
                            let offset = X86_64::param_stack_offset(int_stack_idx, RegClass::Int)
                                .expect(
                                    "param_stack_offset must be defined when param() returns None",
                                );

                            let dest = self.value[*vid];
                            self.lir.push_instr(
                                &entry,
                                X86Instr::MovFromStack {
                                    dest,
                                    rbp_offset: offset,
                                    bytes: mt.bytes(),
                                },
                            );
                            int_stack_idx += 1;
                        },
                    }

                    int_idx += 1;
                },

                RegClass::Float => {
                    match X86_64::param(float_idx, RegClass::Float) {
                        Some(reg) => {
                            let dest = self.value[*vid];
                            let abi_vreg = self.lir.new_vreg(mt);
                            self.lir.add_precolour(abi_vreg, reg);

                            self.lir.push_instr(
                                &entry,
                                X86Instr::MovFloat {
                                    dest,
                                    src: X86Operand::VReg(abi_vreg),
                                    bytes: mt.bytes(),
                                },
                            );
                        },

                        None => {
                            let offset = X86_64::param_stack_offset(
                                float_stack_idx,
                                RegClass::Float,
                            )
                            .expect("param_stack_offset must be defined when param() returns None");

                            let dest = self.value[*vid];
                            self.lir.push_instr(
                                &entry,
                                X86Instr::MovFromStack {
                                    dest,
                                    rbp_offset: offset,
                                    bytes: mt.bytes(),
                                },
                            );
                            float_stack_idx += 1;
                        },
                    }

                    float_idx += 1;
                },
            }
        }
    }

    fn stack_addr(&mut self, block: &BlockId, origin: VReg) -> VReg {
        let dest = self.lir.new_vreg(MachineType::Int { bytes: 8, signed: false });
        self.lir.push_instr(block, X86Instr::StackAddr { dest, origin });
        dest
    }

    fn small_integer_return(&self, typ: hir::Type) -> Option<Vec<(i32, u8, X86Reg)>> {
        let size = typ.machine_type(self.layouts).stack_size() as u32;
        let contains_float = match typ.kind() {
            TypeKind::Struct(sid) => self.layouts[sid.0 as usize].contains_float(),
            _ => false,
        };
        if size == 0 || size > 16 || contains_float {
            return None;
        }

        let regs = [X86Reg::Rax, X86Reg::Rdx];
        Some(
            lir::aggregate_chunks(size)
                .zip(regs)
                .map(|((offset, bytes), reg)| (offset, bytes, reg))
                .collect(),
        )
    }

    /// materialise a `MIR` operand into a new VReg if it's a constant
    /// otherwise return it's VReg directly
    fn operand(&mut self, op: &Operand, block: &BlockId) -> VReg {
        let layouts = self.layouts;
        target::operand(&mut self.lir, op, block, layouts, |vid| self.value[vid])
    }

    // transforms a MIR operand into a x86_64 LIR operand
    fn lower_operand(&mut self, op: &Operand) -> X86Operand {
        let layouts = self.layouts;
        target::lower_operand(&mut self.lir, op, |vid| self.value[vid], layouts)
    }
}
