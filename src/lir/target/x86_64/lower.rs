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

use crate::hir::{self, Type};
use crate::lir::target::x86_64::{Condition, X86_64, X86Instr, X86Operand, X86Reg};
use crate::lir::target::{Lowerable, MemOps, RegClass, Target, aggregate_copy};
use crate::lir::{self, BlockId, MachineType, Term, VReg};
use crate::mir::{self, Const, Function, Layout, Operand, ValueId};

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
            .map(|n| format!("nyx_{n}"))
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

        let dest = self.vreg(instruction.dest.id);
        let typ = instruction.dest.typ;
        let bytes = match typ {
            Type::Unit => 0,
            _ => typ.machine_type(self.layouts).bytes(),
        };
        let is_float = typ.is_float();

        match &instruction.kind {
            InstructionKind::Assign(op) => {
                if let Type::Struct(sid) = typ {
                    let Operand::Place(src) = op else {
                        unreachable!("aggregate copy source must be a place");
                    };
                    let size = self.struct_size(sid);
                    let src_vreg = self.vreg(src.id);

                    #[rustfmt::skip]
                    return aggregate_copy(&mut self.lir, id, false, false, src_vreg, dest, 0, 0, size);
                }

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

            InstructionKind::Binary {
                operation,
                rhs,
                lhs,
            } => {
                use crate::parser::expression::BinaryOperator as B;

                let bytes = lhs.typ().machine_type(self.layouts).bytes();
                let lhs_type = lhs.typ();
                let is_float = lhs_type.is_float();
                let lhs = self.lower_operand(&lhs);
                let rhs = self.lower_operand(&rhs);

                match operation {
                    B::Div if is_float => {
                        self.lir.push_instr(
                            id,
                            X86Instr::MovFloat {
                                dest,
                                src: lhs,
                                bytes,
                            },
                        );
                        self.lir.push_instr(
                            id,
                            X86Instr::DivFloat {
                                dest,
                                src: rhs,
                                bytes,
                            },
                        );
                    }
                    B::Div => {
                        let dividend = self.lir.new_vreg(lhs_type.machine_type(self.layouts));
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
                            true => X86Instr::MovFloat {
                                dest,
                                bytes,
                                src: lhs,
                            },
                            _ => X86Instr::Mov {
                                dest,
                                src: lhs,
                                bytes,
                            },
                        };
                        self.lir.push_instr(id, copy);

                        let arith = match operation {
                            B::Add => match is_float {
                                true => X86Instr::AddFloat {
                                    dest,
                                    src: rhs,
                                    bytes,
                                },
                                _ => X86Instr::Add {
                                    dest,
                                    src: rhs,
                                    bytes,
                                },
                            },

                            B::Sub => match is_float {
                                true => X86Instr::SubFloat {
                                    dest,
                                    src: rhs,
                                    bytes,
                                },
                                _ => X86Instr::Sub {
                                    dest,
                                    src: rhs,
                                    bytes,
                                },
                            },

                            B::Mul => match is_float {
                                true => X86Instr::MulFloat {
                                    dest,
                                    src: rhs,
                                    bytes,
                                },
                                _ => X86Instr::Imul {
                                    dest,
                                    src: rhs,
                                    bytes,
                                },
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

            InstructionKind::FieldLoad { src, offset, typ } => {
                if let Type::Struct(sid) = typ {
                    let origin = match src {
                        Operand::Place(p) => self.vreg(p.id),
                        Operand::Const(_) => unreachable!("struct constant in field access"),
                    };
                    let size = self.struct_size(*sid);
                    let is_src_ref = matches!(src.typ(), Type::Ref { .. });

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

                let bytes = typ.machine_type(self.layouts).bytes();
                let is_float = typ.is_float();
                match src {
                    Operand::Place(place) => {
                        let origin = self.vreg(place.id);
                        let instruction = X86_64::scalar_load(
                            matches!(place.typ, Type::Ref { .. }),
                            dest,
                            origin,
                            *offset as i32,
                            bytes,
                            is_float,
                        );
                        self.lir.push_instr(id, instruction);
                    }
                    Operand::Const(_) => unreachable!("struct constant in field access"),
                }
            }

            InstructionKind::FieldStore { value, offset } => {
                if let Type::Struct(sid) = value.typ() {
                    let Operand::Place(src) = value else {
                        unreachable!("aggregate field store source must be a place");
                    };
                    let size = self.struct_size(sid);
                    let src_vreg = self.vreg(src.id);

                    return aggregate_copy(
                        &mut self.lir,
                        id,
                        false,
                        matches!(typ, Type::Ref { .. }),
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
                    matches!(typ, Type::Ref { .. }),
                    dest,
                    src,
                    *offset as i32,
                    bytes,
                    is_float,
                );
                self.lir.push_instr(id, instruction);
            }

            InstructionKind::AddressOf { src, offset } => {
                let origin = self.vreg(src.id);
                match src.typ {
                    #[rustfmt::skip]
                    Type::Ref { .. } => self.lir.push_instr(id, X86Instr::Mov { dest, src: X86Operand::VReg(origin), bytes: 8 }),
                    _ => self.lir.push_instr(id, X86Instr::StackAddr { dest, origin }),
                }

                if *offset != 0 {
                    self.lir.push_instr(
                        id,
                        X86Instr::Add {
                            dest,
                            src: X86Operand::Imm(*offset as i64),
                            bytes: 8,
                        },
                    );
                }
            }

            InstructionKind::Call { callee, args } => {
                let callee_id = *callee;
                let callee = self
                    .symbols
                    .get(self.all_functions[callee_id.0 as usize].name_symbol)
                    .map(|n| format!("nyx_{n}"))
                    .unwrap_or_else(|| format!("nyx_func_{}", callee_id.0));

                let mut moves = Vec::with_capacity(args.len());
                let mut stack_args = Vec::new();
                let mut int_idx = 0;
                let mut float_idx = 0;

                let return_type = self.all_functions[callee_id.0 as usize].return_type;
                let aggregate_ret = match return_type {
                    Type::Struct(sid) => self.small_integer_return(sid).unwrap_or_default(),
                    _ => Vec::new(),
                };

                if matches!(return_type, Type::Struct(_)) && aggregate_ret.is_empty() {
                    let ptr = self.stack_addr(id, dest);
                    let abi_reg = X86_64::param(int_idx, RegClass::Int)
                        .expect("sret pointer must fit in the first integer argument register");
                    moves.push((ptr, abi_reg));
                    int_idx += 1;
                }

                for arg in args {
                    if let Type::Struct(_) = arg.typ() {
                        let Operand::Place(place) = arg else {
                            unreachable!("aggregate argument source must be a place");
                        };
                        let ptr = self.stack_addr(id, self.vreg(place.id));

                        match X86_64::param(int_idx, RegClass::Int) {
                            Some(abi_reg) => moves.push((ptr, abi_reg)),
                            None => stack_args
                                .push((X86Operand::VReg(ptr), MachineType::Int { bytes: 8 })),
                        }

                        int_idx += 1;
                        continue;
                    }

                    let mt = arg.typ().machine_type(self.layouts);
                    let class = mt.class();

                    match class {
                        RegClass::Int => {
                            match X86_64::param(int_idx, RegClass::Int) {
                                Some(abi_reg) => {
                                    let vreg = self.operand(&arg, id);
                                    moves.push((vreg, abi_reg));
                                }

                                None => {
                                    let operand = self.lower_operand(arg);
                                    stack_args.push((operand, mt));
                                }
                            }

                            int_idx += 1;
                        }

                        RegClass::Float => {
                            match X86_64::param(float_idx, RegClass::Float) {
                                Some(abi_reg) => {
                                    let vreg = self.operand(&arg, id);
                                    moves.push((vreg, abi_reg));
                                }

                                None => {
                                    let operand = self.lower_operand(arg);
                                    stack_args.push((operand, mt));
                                }
                            }

                            float_idx += 1;
                        }
                    }
                }

                let ret = (return_type != Type::Unit && !matches!(return_type, Type::Struct(_)))
                    .then_some(dest);
                let mut ret_vregs = Vec::with_capacity(aggregate_ret.len());
                for &(_, bytes, reg) in &aggregate_ret {
                    let vreg = self.lir.new_vreg(MachineType::Int { bytes });
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
            }

            InstructionKind::Syscall {
                code,
                args,
                returns,
            } => {
                let mut moves = Vec::with_capacity(args.len());
                let mut uses = Vec::with_capacity(args.len());

                for (i, arg) in args.iter().enumerate() {
                    let abi_reg = X86_64::syscall_param(i).expect("too many syscall arguments");
                    let operand = self.lower_operand(arg);
                    let bytes = arg.typ().machine_type(self.layouts).bytes();

                    if let X86Operand::VReg(vreg) = &operand {
                        uses.push(*vreg);
                    }

                    moves.push((operand, abi_reg, bytes));
                }

                let ret = (*returns && typ != Type::Unit).then_some(dest);
                self.lir.push_instr(
                    id,
                    X86Instr::Syscall {
                        id: X86_64::syscall_code(*code) as u32,
                        moves,
                        uses,
                        ret,
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
                        self.lir.push_instr(
                            id,
                            X86Instr::Mov {
                                dest,
                                src: lhs,
                                bytes,
                            },
                        );

                        dest
                    }
                };

                self.lir.push_instr(id, X86Instr::cmp::<0>(lhs, rhs, bytes));
            }
        }

        self.lir.push_instr(
            id,
            X86Instr::Setcc {
                dest: flag,
                condition,
            },
        );
        self.lir.push_instr(id, X86Instr::Movzx { dest, src: flag });

        // movzx widens 1-byte setcc result to i32, so we need to update dest's type
        self.lir.set_vreg_type(dest, MachineType::Int { bytes: 4 });
    }

    fn lower_terminator(&mut self, id: &BlockId, terminator: mir::Terminator) {
        use crate::mir::Terminator as T;

        let terminator = match terminator {
            T::Return(None) => Term::Return(None),
            T::Return(Some(operand)) if matches!(operand.typ(), Type::Struct(_)) => {
                let Type::Struct(sid) = operand.typ() else {
                    unreachable!("checked above");
                };
                let Operand::Place(place) = operand else {
                    unreachable!("aggregate return source must be a place");
                };

                let src_vreg = self.vreg(place.id);

                match self.small_integer_return(sid) {
                    Some(chunks) => {
                        for (offset, bytes, reg) in chunks {
                            let ret = self.lir.new_vreg(MachineType::Int { bytes });
                            #[rustfmt::skip]
                            let load = X86Instr::FieldLoad { dest: ret, origin: src_vreg, offset, bytes, is_float: false };
                            self.lir.add_precolour(ret, reg);
                            self.lir.push_instr(id, load);
                        }
                    }

                    None => {
                        let sret_ptr = self
                            .sret_ptr
                            .expect("struct-returning function must have an sret pointer");
                        let size = self.struct_size(sid);

                        #[rustfmt::skip]
                        aggregate_copy(&mut self.lir, id, false, true, src_vreg, sret_ptr, 0, 0, size);
                    }
                }

                Term::Return(None)
            }
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

    /// Copy physical ABI (and stack slots) registers into VRegs
    /// each parameter arrives in a physical ABI register or in the caller's stack frame
    fn lower_param_moves(&mut self) {
        let entry = BlockId(0);
        let mut int_idx = 0;
        let mut float_idx = 0;
        let mut int_stack_idx = 0;
        let mut float_stack_idx = 0;

        if let Type::Struct(sid) = self.function.return_type {
            if self.small_integer_return(sid).is_some() {
                // Small integer aggregates are returned directly in RAX/RDX.
            } else {
                let ptr = self.lir.new_vreg(MachineType::Int { bytes: 8 });
                let reg = X86_64::param(int_idx, RegClass::Int)
                    .expect("sret pointer must fit in the first integer argument register");
                self.lir.add_precolour(ptr, reg);
                self.sret_ptr = Some(ptr);
                int_idx += 1;
            }
        }

        for (vid, typ) in &self.function.params {
            if let Type::Struct(sid) = typ {
                let ptr = self.lir.new_vreg(MachineType::Int { bytes: 8 });

                match X86_64::param(int_idx, RegClass::Int) {
                    Some(reg) => self.lir.add_precolour(ptr, reg),
                    None => {
                        let offset = X86_64::param_stack_offset(int_stack_idx, RegClass::Int)
                            .expect("param_stack_offset must be defined when param() returns None");
                        self.lir.push_instr(
                            &entry,
                            X86Instr::MovFromStack {
                                dest: ptr,
                                rbp_offset: offset,
                                bytes: 8,
                            },
                        );
                        int_stack_idx += 1;
                    }
                }

                let size = self.struct_size(*sid);
                let dest = self.vreg(*vid);
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
                            let dest = self.vreg(*vid);
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
                        }

                        None => {
                            let offset = X86_64::param_stack_offset(int_stack_idx, RegClass::Int)
                                .expect(
                                    "param_stack_offset must be defined when param() returns None",
                                );

                            let dest = self.vreg(*vid);
                            self.lir.push_instr(
                                &entry,
                                X86Instr::MovFromStack {
                                    dest,
                                    rbp_offset: offset,
                                    bytes: mt.bytes(),
                                },
                            );
                            int_stack_idx += 1;
                        }
                    }

                    int_idx += 1;
                }

                RegClass::Float => {
                    match X86_64::param(float_idx, RegClass::Float) {
                        Some(reg) => {
                            let dest = self.vreg(*vid);
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
                        }

                        None => {
                            let offset = X86_64::param_stack_offset(
                                float_stack_idx,
                                RegClass::Float,
                            )
                            .expect("param_stack_offset must be defined when param() returns None");

                            let dest = self.vreg(*vid);
                            self.lir.push_instr(
                                &entry,
                                X86Instr::MovFromStack {
                                    dest,
                                    rbp_offset: offset,
                                    bytes: mt.bytes(),
                                },
                            );
                            float_stack_idx += 1;
                        }
                    }

                    float_idx += 1;
                }
            }
        }
    }

    fn vreg(&self, id: ValueId) -> VReg {
        self.value[id.0 as usize]
    }

    fn stack_addr(&mut self, block: &BlockId, origin: VReg) -> VReg {
        let dest = self.lir.new_vreg(MachineType::Int { bytes: 8 });
        self.lir.push_instr(block, X86Instr::StackAddr { dest, origin });
        dest
    }

    fn small_integer_return(&self, sid: hir::StructId) -> Option<Vec<(i32, u8, X86Reg)>> {
        let layout = self.layouts[sid.0 as usize];
        let (size, _) = layout.into();
        if size == 0 || size > 16 || layout.contains_float() {
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

    fn struct_size(&self, sid: crate::hir::StructId) -> u32 {
        let (size, _) = self.layouts[sid.0 as usize].into();
        size
    }

    /// materialise a `MIR` operand into a new VReg if it's a constant
    /// otherwise return it's VReg directly
    fn operand(&mut self, op: &Operand, block: &BlockId) -> VReg {
        match op {
            Operand::Place(p) => self.vreg(p.id),
            Operand::Const(c) => {
                let vreg = self.lir.new_vreg(c.typ().machine_type(self.layouts));
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
            Operand::Const(Const::Str { id, .. }) => {
                X86Operand::RipRel(format!(".L_str_{id}(%rip)"))
            }
            Operand::Const(Const::Unit) => unreachable!("unit operand"),
        }
    }

    fn constant_mov(&mut self, dest: VReg, c: &Const) -> X86Instr {
        let bytes = c.typ().machine_type(self.layouts).bytes();
        let src = self.lower_operand(&Operand::Const(*c));

        match c {
            Const::Int(_, _) => X86Instr::Mov { dest, src, bytes },
            Const::Bool(_) => X86Instr::Mov {
                dest,
                src,
                bytes: 4,
            },
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
