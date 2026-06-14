//! MIR -> AArch64 LIR lowering
//!
//! AArch64 is a naturally 3-address ISA, which makes instruction selection
//! considerably cleaner than `x86_64`:
//!
//!   MIR: t2 = t0 + t1
//!   LIR: ADD Xd, Xn, Xm — direct translation, no copy needed
//!
//! The only departure from this ideal is comparisons: we emit CMP (sets NZCV)
//! followed by CSET (materialises the flag into a GP register as 0 or 1).
//!
//! Division is also cleaner: `SDIV Xd, Xn, Xm` is 3-address with no implicit
//! registers: unlike x86_64's `idiv` which clobbers `rax`/`rdx`.

use crate::{
    hir::{self, SymbolTable, Type, TypeKind},
    lir::{
        self, BlockId, MachineType, Term, VReg, assembly_label,
        target::{
            self, AggregateCopy, Lower, Lowerable, MemOps, RegClass, Target,
            aarch64::{A64Cond, A64Instr, A64Operand, AArch64},
            aggregate_copy,
        },
    },
    mir::{self, Function, Operand},
};

impl Lowerable for AArch64 {
    fn lower(
        function: &Function,
        symbols: &SymbolTable,
        all_functions: &[Function],
        struct_layouts: &[mir::Layout],
        enum_layouts: &[mir::Layout],
    ) -> lir::Function<Self> {
        let mut lower =
            Lower::<AArch64>::new(function, symbols, all_functions, struct_layouts, enum_layouts);

        lower.lower_param_moves();

        for (idx, block) in function.blocks.iter().enumerate() {
            lower.lower_block(&BlockId(idx as u32), block);
        }

        lower.lir
    }
}

impl<'f> Lower<'f, AArch64> {
    fn lower_block(&mut self, id: &BlockId, block: &mir::Block) {
        for instruction in &block.instructions {
            self.lower_instruction(id, instruction);
        }

        self.lower_terminator(id, block.terminator.clone());
    }

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
            InstructionKind::Assign(operand) => {
                let value = &self.value;
                let layouts = self.layouts;
                if let Some(instr) = target::lower_assign(
                    &mut self.lir,
                    id,
                    dest,
                    typ,
                    operand,
                    layouts,
                    |vid| value[vid],
                    |lir, op, _| target::lower_operand(lir, op, |vid| value[vid]),
                ) {
                    self.lir.push_instr(id, instr);
                }
            },

            InstructionKind::Unary { operation, rhs } => {
                use crate::parser::expression::UnaryOperator as U;

                let src = self.operand(rhs, id);

                match operation {
                    U::Neg => match is_float {
                        true => self.lir.push_instr(id, A64Instr::FNeg { dest, src, bytes }),
                        false => self.lir.push_instr(id, A64Instr::Neg { dest, src, bytes }),
                    },
                    // boolean NOT: XOR with 1
                    // integer NOT: MVN instruction
                    U::Not => match typ.kind() == TypeKind::Bool {
                        true => {
                            #[rustfmt::skip]
                            let instr = A64Instr::Eor { dest, lhs: src, rhs: A64Operand::Imm(1), bytes: 4 };
                            self.lir.push_instr(id, instr);
                        },
                        _ => self.lir.push_instr(id, A64Instr::Mvn { dest, src, bytes }),
                    },
                    U::Deref => unreachable!(),
                    U::Ref => unreachable!(
                        "UnaryOperator::Ref is lowered to InstructionKind::AddressOf in MIR and never reaches LIR Unary lowering"
                    ),
                }
            },

            InstructionKind::Binary { operation, rhs, lhs, checked } => {
                use crate::parser::expression::BinaryOperator as B;

                let bytes = lhs.typ().machine_type(self.layouts).bytes();
                let lhs_type = lhs.typ();
                let rhs_type = rhs.typ();
                let is_float = lhs_type.is_float();
                let lhs = self.lower_operand(lhs);
                let rhs = self.lower_operand(rhs);
                let checked = *checked;

                match operation {
                    comp @ (B::Lt | B::LtEq | B::Gt | B::GtEq | B::Eq | B::Ne) => {
                        self.lower_cmp(
                            id,
                            dest,
                            lhs,
                            rhs,
                            bytes,
                            is_float,
                            A64Cond::new(comp, is_float),
                        );
                    },

                    _ => {
                        // for register-only instructions (MUL, SDIV, float ops)
                        // we need both operands in registers
                        let lhs = self.ensure_vreg(lhs, lhs_type, id);

                        match operation {
                            B::Add => {
                                let instr = match is_float {
                                    #[rustfmt::skip]
                                    true => A64Instr::FAdd { dest, lhs, rhs: self.ensure_vreg(rhs, lhs_type, id), bytes },
                                    false => A64Instr::Add {
                                        dest,
                                        lhs,
                                        rhs: self.fit_add_sub_operand(rhs, lhs_type, id),
                                        bytes,
                                        checked,
                                    },
                                };
                                self.lir.push_instr(id, instr);
                            },

                            B::Sub => {
                                #[rustfmt::skip]
                                let instr = match is_float {
                                    true => A64Instr::FSub { dest, lhs, rhs: self.ensure_vreg(rhs, lhs_type, id), bytes },
                                    false => A64Instr::Sub {
                                        dest,
                                        lhs,
                                        rhs: self.fit_add_sub_operand(rhs, lhs_type, id),
                                        bytes,
                                        checked,
                                    },
                                };

                                self.lir.push_instr(id, instr);
                            },

                            B::Mul => {
                                let rhs = self.ensure_vreg(rhs, lhs_type, id);
                                let instr = match is_float {
                                    true => A64Instr::FMul { dest, lhs, rhs, bytes },
                                    false => A64Instr::Mul { dest, lhs, rhs, bytes, checked },
                                };
                                self.lir.push_instr(id, instr);
                            },

                            B::Div => {
                                let rhs = self.ensure_vreg(rhs, lhs_type, id);
                                let instr = match is_float {
                                    true => A64Instr::FDiv { dest, lhs, rhs, bytes },
                                    false => A64Instr::SDiv { dest, lhs, rhs, bytes },
                                };
                                self.lir.push_instr(id, instr);
                            },

                            B::And | B::BitAnd => {
                                let rhs = self.fit_logical_operand(rhs, rhs_type, id);
                                self.lir.push_instr(id, A64Instr::And { dest, lhs, rhs, bytes });
                            },
                            B::Or | B::BitOr => {
                                let rhs = self.fit_logical_operand(rhs, rhs_type, id);
                                self.lir.push_instr(id, A64Instr::Or { dest, lhs, rhs, bytes });
                            },
                            B::BitXor => {
                                let rhs = self.fit_logical_operand(rhs, rhs_type, id);
                                self.lir.push_instr(id, A64Instr::Eor { dest, lhs, rhs, bytes });
                            },
                            B::Shl | B::Shr => {
                                let rhs = self.fit_shift_operand(rhs, rhs_type, bytes, id);
                                let instr = match operation {
                                    B::Shl => A64Instr::Lsl { dest, lhs, rhs, bytes },
                                    B::Shr => match lhs_type.machine_type(self.layouts).is_signed()
                                    {
                                        true => A64Instr::Asr { dest, lhs, rhs, bytes },
                                        _ => A64Instr::Lsr { dest, lhs, rhs, bytes },
                                    },
                                    _ => unsafe { std::hint::unreachable_unchecked() },
                                };
                                self.lir.push_instr(id, instr);
                            },

                            _ => unsafe { std::hint::unreachable_unchecked() },
                        }
                    },
                }
            },

            InstructionKind::Call { callee, args } => {
                let callee_id = *callee;
                let callee_fn = self
                    .all_functions
                    .iter()
                    .find(|f| f.id == callee_id)
                    .unwrap_or_else(|| panic!("callee function {callee_id:?} not found"));

                let (bytes, _signed, is_float) = (8, false, false);

                if callee_fn.intrinsic == Some(hir::Intrinsic::Len) {
                    let ptr = self.operand(&args[0], id);
                    return self.lir.push_instr(
                        id,
                        A64Instr::PtrLoad { dest, ptr, offset: 8, bytes, is_float, signed: false },
                    );
                }

                let callee = assembly_label(self.symbols.get(callee_fn.name_symbol));

                let mut moves = Vec::with_capacity(args.len());
                let mut int_idx = 0;

                if typ.is_aggregate() {
                    let ptr = self.stack_addr(id, dest);
                    let abi_reg = AArch64::param(int_idx, RegClass::Int)
                        .expect("sret pointer must fit in the first integer argument register");
                    moves.push((ptr, abi_reg));
                    int_idx += 1;
                }

                let value = &self.value;
                let layouts = self.layouts;
                let (arg_moves, stack_args) = lir::target::prepare_call_args(
                    &mut self.lir,
                    id,
                    args,
                    layouts,
                    |vid| value[vid],
                    |lir, op, block| {
                        lir::target::operand(lir, op, block, layouts, |vid| value[vid])
                    },
                    |lir, op, _block| lir::target::lower_operand(lir, op, |vid| value[vid]),
                    |lir, block, origin| {
                        let dest = lir.new_vreg(MachineType::Int { bytes: 8, signed: false });
                        lir.push_instr(block, A64Instr::StackAddr { dest, origin });
                        dest
                    },
                    int_idx,
                    0,
                );
                moves.extend(arg_moves);

                let return_type = callee_fn.return_type;
                let ret = (return_type.kind() != TypeKind::Unit && !return_type.is_aggregate())
                    .then_some(dest);
                self.lir.push_instr(id, A64Instr::call(callee, moves, stack_args, ret));
            },

            InstructionKind::Syscall { code, args, returns } => {
                let value = &self.value;
                let layouts = self.layouts;
                let (syscall_moves, syscall_uses) = lir::target::prepare_syscall_args(
                    &mut self.lir,
                    id,
                    args,
                    layouts,
                    |lir, op, _block| lir::target::lower_operand(lir, op, |vid| value[vid]),
                );

                let ret = (*returns && typ.kind() != TypeKind::Unit).then_some(dest);
                self.lir.push_instr(
                    id,
                    A64Instr::Syscall {
                        id: AArch64::syscall_code(*code),
                        moves: syscall_moves,
                        uses: syscall_uses,
                        ret,
                    },
                );
            },

            InstructionKind::FieldLoad { src, offset, typ } => {
                if typ.is_aggregate() {
                    let origin = match src {
                        Operand::Place(p) => self.value[p.id],
                        Operand::Const(_) => unreachable!("struct constant in field access"),
                    };

                    let size = typ.machine_type(self.layouts).stack_size() as u32;
                    return aggregate_copy(
                        &mut self.lir,
                        id,
                        AggregateCopy {
                            src: origin,
                            dest,
                            src_ref: matches!(src.typ().kind(), TypeKind::Ref { .. }),
                            dest_ref: false,
                            src_base: *offset as i32,
                            dest_base: 0,
                            size,
                        },
                    );
                }

                let mt = typ.machine_type(self.layouts);
                let bytes = mt.bytes();
                let signed = mt.is_signed();

                match src {
                    Operand::Place(place) => {
                        let origin = self.value[place.id];
                        let instruction = AArch64::scalar_load(
                            matches!(place.typ.kind(), TypeKind::Ref { .. }),
                            dest,
                            origin,
                            *offset as i32,
                            bytes,
                            typ.is_float(),
                            signed,
                        );
                        self.lir.push_instr(id, instruction);
                    },

                    Operand::Const(_) => unreachable!("struct constant in field access"),
                }
            },

            InstructionKind::FieldStore { value, offset } => {
                let offset = *offset as i32;

                if value.typ().is_aggregate() {
                    let Operand::Place(src) = value else {
                        unreachable!("aggregate field store source must be a place");
                    };

                    let src_vreg = self.value[src.id];
                    let size = value.typ().machine_type(self.layouts).stack_size() as u32;
                    return aggregate_copy(
                        &mut self.lir,
                        id,
                        AggregateCopy {
                            src: src_vreg,
                            dest,
                            src_ref: matches!(src.typ.kind(), TypeKind::Ref { .. }),
                            dest_ref: matches!(typ.kind(), TypeKind::Ref { .. }),
                            src_base: 0,
                            dest_base: offset,
                            size,
                        },
                    );
                }

                let is_float = value.typ().is_float();
                let mt = value.typ().machine_type(self.layouts);
                let bytes = mt.bytes();
                let src = self.lower_operand(value);

                let instruction = AArch64::scalar_store(
                    matches!(typ.kind(), TypeKind::Ref { .. }),
                    dest,
                    src,
                    offset,
                    bytes,
                    is_float,
                );
                self.lir.push_instr(id, instruction);
            },

            InstructionKind::AddressOf { src, offset } => {
                let origin = self.value[src.id];
                match src.typ.kind() {
                    #[rustfmt::skip]
                    TypeKind::Ref { .. } => self.lir.push_instr( id, A64Instr::Mov { dest, src: origin, bytes: 8 }),
                    _ => self.lir.push_instr(id, A64Instr::StackAddr { dest, origin }),
                }

                if *offset != 0 {
                    self.lir.push_instr(
                        id,
                        A64Instr::Add {
                            dest,
                            lhs: dest,
                            rhs: A64Operand::Imm(*offset as i64),
                            bytes: 8,
                            checked: false,
                        },
                    );
                }
            },

            InstructionKind::Cast { src, typ } => {
                use std::cmp::Ordering;

                let src_mt = src.typ().machine_type(self.layouts);
                let src_bytes = src_mt.bytes();
                let src_signed = src_mt.is_signed();

                let dest_mt = typ.machine_type(self.layouts);
                let dest_bytes = dest_mt.bytes();

                let op = self.lower_operand(src);

                #[rustfmt::skip]
                let instr = match src_bytes.cmp(&dest_bytes) {
                    Ordering::Equal => match op {
                        A64Operand::VReg(src) => A64Instr::Mov { dest, src, bytes: dest_bytes },
                        A64Operand::Imm(imm) => A64Instr::MovImm { dest, imm, bytes: dest_bytes },
                        A64Operand::Label(label) => A64Instr::Adr { dest, label },
                    },

                    // downcasting / truncate
                    Ordering::Greater if dest_bytes < 4 => match op {
                        A64Operand::VReg(src) => A64Instr::Extend { dest, src, src_bytes: dest_bytes, dest_bytes, signed: dest_mt.is_signed() },
                        A64Operand::Imm(imm) => {
                            let mask = if dest_bytes == 1 { 0xff } else { 0xffff };
                            let masked = match dest_mt.is_signed() {
                                true => {
                                    let shift = 64 - (dest_bytes * 8);
                                    (imm << shift) >> shift
                                }
                                _ => imm & mask,
                            };

                            A64Instr::MovImm { dest, imm: masked, bytes: dest_bytes }
                        }
                        A64Operand::Label(label) => A64Instr::Adr { dest, label },
                    },
                    Ordering::Greater => match op {
                        A64Operand::VReg(src) => A64Instr::Mov { dest, src, bytes: dest_bytes },
                        A64Operand::Imm(imm) => A64Instr::MovImm { dest, imm, bytes: dest_bytes },
                        A64Operand::Label(label) => A64Instr::Adr { dest, label },
                    },

                    // upcasting
                    Ordering::Less => match op {
                        A64Operand::Imm(imm) => A64Instr::MovImm { dest, imm, bytes: dest_bytes },
                        A64Operand::VReg(src) => A64Instr::Extend { dest, src, src_bytes, dest_bytes, signed: src_signed },
                        A64Operand::Label(label) => A64Instr::Adr { dest, label },
                    },
                };

                self.lir.push_instr(id, instr);
            },
        }
    }

    /// CMP + CSET: the ARM comparison pattern
    ///
    /// unlike x86_64's `setcc + movzx` triple, CSET directly materialises a 0/1 into a full-width GP register
    fn lower_cmp(
        &mut self,
        id: &BlockId,
        dest: VReg,
        lhs: A64Operand,
        rhs: A64Operand,
        bytes: u8,
        is_float: bool,
        cond: A64Cond,
    ) {
        match is_float {
            true => {
                let lhs = self.ensure_vreg(lhs, TypeKind::F64.into(), id);
                let rhs = self.ensure_vreg(rhs, TypeKind::F64.into(), id);

                self.lir.push_instr(id, A64Instr::FCmp { lhs, rhs, bytes });
            },

            false => {
                let lhs = self.ensure_vreg(lhs, TypeKind::I64.into(), id);
                let rhs = self.fit_add_sub_operand(rhs, TypeKind::I64.into(), id);

                self.lir.push_instr(id, A64Instr::Cmp { lhs, rhs, bytes });
            },
        }

        self.lir.push_instr(id, A64Instr::Cset { dest, cond });
    }

    #[inline(always)]
    fn lower_terminator(&mut self, id: &BlockId, terminator: mir::Terminator) {
        use crate::mir::Terminator as T;

        let terminator = match terminator {
            T::Return(None) => Term::Return(None),
            T::Return(Some(operand)) if operand.typ().is_aggregate() => {
                let typ = operand.typ();
                let Operand::Place(place) = operand else {
                    unreachable!("aggregate return source must be a place");
                };
                let sret_ptr =
                    self.sret_ptr.expect("struct-returning function must have an sret pointer");
                let src_vreg = self.value[place.id];
                let size = typ.machine_type(self.layouts).stack_size() as u32;
                let copy = AggregateCopy::new(src_vreg, sret_ptr, size).with_dest_ref();
                aggregate_copy(&mut self.lir, id, copy);
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

    /// if the operand is already a `VReg`, return it
    /// otherwise materialise into a new `VReg`
    #[inline(always)]
    fn ensure_vreg(&mut self, op: A64Operand, hint_type: Type, block: &BlockId) -> VReg {
        match op {
            A64Operand::VReg(v) => v,
            A64Operand::Imm(n) => {
                let mt = hint_type.machine_type(self.layouts);
                let vreg = self.lir.new_vreg(mt);

                self.lir
                    .push_instr(block, A64Instr::MovImm { dest: vreg, imm: n, bytes: mt.bytes() });

                vreg
            },
            A64Operand::Label(label) => {
                let mt = hint_type.machine_type(self.layouts);
                let vreg = self.lir.new_vreg(mt);

                if hint_type.is_float() {
                    self.lir.push_instr(
                        block,
                        A64Instr::FLiteral { dest: vreg, label, bytes: mt.bytes() },
                    );
                } else {
                    self.lir.push_instr(block, A64Instr::Adr { dest: vreg, label });
                }

                vreg
            },
        }
    }

    /// for ADD/SUB/CMP: accept imm12 (0..=4095) inline, otherwise materialise
    #[inline(always)]
    fn fit_add_sub_operand(
        &mut self,
        op: A64Operand,
        hint_type: Type,
        block: &BlockId,
    ) -> A64Operand {
        match op {
            A64Operand::Imm(n) if fits_imm12(n) => A64Operand::Imm(n),
            A64Operand::VReg(_) => op,
            _ => A64Operand::VReg(self.ensure_vreg(op, hint_type, block)),
        }
    }

    /// for AND/ORR/EOR: logical immediates have complex encoding rules on ARM
    /// for simplicity we only allow small immediates that are valid bitmask patterns
    /// in practice, boolean logic (AND 1, OR 1, EOR 1) always fits
    #[inline(always)]
    fn fit_logical_operand(
        &mut self,
        op: A64Operand,
        hint_type: Type,
        block: &BlockId,
    ) -> A64Operand {
        match op {
            A64Operand::Imm(n) if n > 0 && fits_imm12(n) => A64Operand::Imm(n),
            A64Operand::VReg(_) => op,
            _ => A64Operand::VReg(self.ensure_vreg(op, hint_type, block)),
        }
    }

    /// for LSL/LSR/ASR: shift amount can be immediate (0..31 for 32-bit, 0..63 for 64-bit) or a register
    #[inline(always)]
    fn fit_shift_operand(
        &mut self,
        op: A64Operand,
        hint_type: Type,
        bytes: u8,
        block: &BlockId,
    ) -> A64Operand {
        match op {
            A64Operand::Imm(n) => {
                let max = if bytes == 8 {
                    63
                } else {
                    31
                };
                match n >= 0 && n <= max {
                    true => A64Operand::Imm(n),
                    _ => A64Operand::VReg(self.ensure_vreg(op, hint_type, block)),
                }
            },
            A64Operand::VReg(_) => op,
            _ => A64Operand::VReg(self.ensure_vreg(op, hint_type, block)),
        }
    }
}

/// AArch64 ADD/SUB imm12 can encode unsigned values 0..=4095
#[inline(always)]
const fn fits_imm12(val: i64) -> bool {
    val >= 0 && val <= 4095
}
