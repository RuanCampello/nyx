//! HIR -> MIR lowering

use crate::{
    hir::{
        self, Expression, ExpressionKind, FunctionId, Hir, LocalId, RefTargetKind, Statement, Type,
        TypeKind, index_vec::IndexVec,
    },
    mir::{
        self, Block, BlockId, Const, Function, Instruction, InstructionKind, Mir, Operand, Place,
        Terminator, ValueId, error::MirError, layout::LayoutTable,
    },
    optimisation,
    parser::expression::{BinaryOperator, TypeIntrinsicKind, UnaryOperator},
};
use lasso::Key;
use std::collections::HashMap;

struct PartialBlock {
    id: BlockId,
    instructions: Vec<Instruction>,
    terminator: Option<Terminator>,
}

struct FunctionLower<'a, 'hir> {
    blocks: Vec<PartialBlock>,
    current: usize,
    next: u32,
    local_map: IndexVec<LocalId, ValueId>,
    locals: Vec<(ValueId, Type)>,
    symbols: &'a [String],
    strings: &'a mut Vec<String>,
    layouts: &'a LayoutTable,
    enums: &'a IndexVec<hir::EnumId, hir::Enum>,
    typeck: &'a hir::TypeckResults,
    local_symbols: IndexVec<LocalId, usize>,
    constant_locals: IndexVec<LocalId, Option<String>>,
    runtime_local_uses: IndexVec<LocalId, bool>,
    functions_map: &'a HashMap<FunctionId, &'a hir::Function<'hir>>,
    runtime_uses_map: &'a HashMap<FunctionId, IndexVec<LocalId, bool>>,
    inlined_return_target: Option<(BlockId, Option<Place>)>,
}

struct InlineContext<'a> {
    local_map: IndexVec<LocalId, ValueId>,
    constant_locals: IndexVec<LocalId, Option<String>>,
    runtime_local_uses: IndexVec<LocalId, bool>,
    local_symbols: IndexVec<LocalId, usize>,
    inlined_return_target: Option<(BlockId, Option<Place>)>,
    typeck: &'a hir::TypeckResults,
}

pub fn lower<'hir>(hir: Hir<'hir>) -> Result<Mir, MirError> {
    debug_assert!(
        !hir.functions.iter().any(has_open_generic),
        r#"MIR lowering received HIR containing unresolved GenericParam
        monomorphisation should have produced fully concrete signatures"#
    );

    let mut functions = Vec::with_capacity(hir.functions.len());
    let mut strings = Vec::new();
    let layouts = LayoutTable::build(&hir.structs, &hir.enums);
    let enums = &hir.enums;
    let symbols = hir.symbols;

    let functions_map = hir.functions.iter().map(|f| (f.id, f)).collect();

    let mut runtime_uses_map = HashMap::new();
    for f in &hir.functions {
        runtime_uses_map.insert(f.id, collect_runtime_local_uses(f));
    }

    for function in &hir.functions {
        functions.push(FunctionLower::run(
            function,
            &symbols,
            &layouts,
            enums,
            &mut strings,
            &functions_map,
            &runtime_uses_map,
        )?);
    }

    Ok(Mir {
        functions,
        symbols,
        strings,
        struct_layouts: layouts.struct_summaries(),
        enum_layouts: layouts.enum_summaries(),
    })
}

const fn temp_value_type(typ: Type) -> Type {
    match typ.kind() {
        TypeKind::Unit | TypeKind::Never => Type::new(TypeKind::I32),
        _ => typ,
    }
}

impl<'a, 'hir> FunctionLower<'a, 'hir> {
    fn run(
        function: &hir::Function<'hir>,
        symbols: &'a [String],
        layouts: &'a LayoutTable,
        enums: &'a IndexVec<hir::EnumId, hir::Enum>,
        strings: &'a mut Vec<String>,
        functions_map: &'a HashMap<FunctionId, &'a hir::Function<'hir>>,
        runtime_uses_map: &'a HashMap<FunctionId, IndexVec<LocalId, bool>>,
    ) -> Result<mir::Function, MirError> {
        let id = function.id;
        let intrinsic = function.kind.intrinsic();
        let name_symbol = function.name.0.into_usize();
        let return_type = function.return_type;
        let n_hir_locals = function.locals.len();

        let mut local_map = IndexVec::from_elem(ValueId(0), n_hir_locals);
        let mut local_symbols = IndexVec::from_elem(0usize, n_hir_locals);
        let mut locals = Vec::with_capacity(n_hir_locals);

        for local in &function.locals {
            let value_id = ValueId(locals.len() as u32);
            local_map[local.id] = value_id;
            local_symbols[local.id] = local.name.0.into_usize();
            locals.push((value_id, local.typ))
        }

        let params = function.params.iter().map(|param| (local_map[param.id], param.typ)).collect();

        let next = locals.len() as u32;

        let mut builder = FunctionLower {
            blocks: Vec::new(),
            current: 0,
            local_map,
            locals,
            next,
            symbols,
            strings,
            layouts,
            enums,
            typeck: &function.typeck,
            local_symbols,
            constant_locals: IndexVec::from_elem(None, n_hir_locals),
            runtime_local_uses: runtime_uses_map.get(&id).cloned().unwrap(),
            functions_map,
            runtime_uses_map,
            inlined_return_target: None,
        };

        builder.new_block();
        builder.lower_block(&function.body)?;

        if !builder.blocks[builder.current].is_terminated() {
            builder.terminate(Terminator::Return(None));
        }

        let blocks = builder.blocks.into_iter().map(PartialBlock::finalise).collect();

        Ok(Function {
            id,
            intrinsic,
            blocks,
            return_type,
            params,
            name_symbol,
            locals: builder.locals,
        })
    }

    #[inline(always)]
    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(PartialBlock::new(id));

        id
    }

    fn lower_block(&mut self, block: &hir::Block<'hir>) -> Result<(), MirError> {
        for statement in block.statements {
            if self.is_terminated() {
                break;
            }

            self.lower_statement(statement)?;
        }

        Ok(())
    }

    fn lower_statement(&mut self, statement: &Statement<'hir>) -> Result<(), MirError> {
        use hir::Statement as Stmt;

        match statement {
            Stmt::LetInit { id, init } => {
                let init = *init;
                self.constant_locals[*id] = self.capture_constant_expr(init);

                if !self.runtime_local_uses(*id) && self.constant_locals[*id].is_some() {
                    return Ok(());
                }

                let typ = self.typeck.type_of(init.id);
                let src = self.lower_expr(init)?;
                let dest = self.place_for_local(*id, typ);

                if self.runtime_local_uses(*id) {
                    self.emit(dest, InstructionKind::Assign(src));
                }
            },
            Stmt::LetUninit { .. } => {},

            Stmt::Expr(expr) => {
                self.lower_expr(expr)?;
            },

            Stmt::Return(value) => {
                let operand = value.as_ref().map(|e| self.lower_expr(e)).transpose()?;
                if let Some((exit_block_id, ret_place)) = self.inlined_return_target {
                    if let (Some(op), Some(dest)) = (operand, ret_place) {
                        self.emit(dest, InstructionKind::Assign(op));
                    }
                    self.terminate(Terminator::Jump(exit_block_id));
                } else {
                    self.terminate(Terminator::Return(operand));
                }
            },

            Stmt::If { condition, then_block, else_block } => {
                let condition = self.lower_expr(condition)?;

                let then_id = self.new_block();
                let else_id = self.new_block();
                let merge_id = self.new_block();

                self.terminate(Terminator::Branch {
                    condition,
                    then_block: then_id,
                    else_block: else_id,
                });

                self.switch_to(then_id);
                self.lower_block(then_block)?;
                if !self.is_terminated() {
                    self.terminate(Terminator::Jump(merge_id));
                }

                self.switch_to(else_id);
                if let Some(else_blk) = else_block {
                    self.lower_block(else_blk)?;
                }

                if !self.is_terminated() {
                    self.terminate(Terminator::Jump(merge_id));
                }

                self.switch_to(merge_id);
            },

            Stmt::While { condition, body } => {
                let header_id = self.new_block();
                let body_id = self.new_block();
                let exit_id = self.new_block();

                self.terminate(Terminator::Jump(header_id));

                self.switch_to(header_id);
                let condition = self.lower_expr(condition)?;

                self.terminate(Terminator::Branch {
                    condition,
                    then_block: body_id,
                    else_block: exit_id,
                });

                self.switch_to(body_id);
                self.lower_block(body)?;
                if !self.is_terminated() {
                    self.terminate(Terminator::Jump(header_id));
                }

                self.switch_to(exit_id);
            },

            Stmt::Block(inner) => {
                self.lower_block(inner)?;
            },
        }

        Ok(())
    }

    fn lower_expr(&mut self, expr: &Expression<'hir>) -> Result<Operand, MirError> {
        use InstructionKind as Kind;

        let typ = self.typeck.type_of(expr.id);

        match &expr.kind {
            ExpressionKind::Literal(lit) => {
                use hir::Literal as L;
                Ok(match lit {
                    L::Unit => Operand::Const(Const::Unit),
                    L::Int(n) => Operand::Const(Const::Int(*n, typ)),
                    L::Float(f) => Operand::Const(Const::Float(*f, typ)),
                    L::Bool(b) => Operand::Const(Const::Bool(*b)),
                    L::Char(c) => Operand::Const(Const::Int(*c as i64, typ)),
                    L::Str(sym) => {
                        let s = &self.symbols[sym.0.into_usize()];
                        let id = self.intern_string(&s.clone());
                        let len = s.len();
                        Operand::Const(Const::Str { id, len })
                    },
                })
            },

            ExpressionKind::Local(local_id) => {
                Ok(Operand::Place(self.place_for_local(*local_id, typ)))
            },

            ExpressionKind::Cast { from, to } => {
                let from = *from;
                let to = *to;
                let src = self.lower_expr(from)?;
                let dest = self.fresh_temporary(to);

                self.emit(dest, Kind::Cast { src, typ: to });

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Unary { operator, expr: inner } => {
                let operator = *operator;
                let inner = *inner;
                let rhs = self.lower_expr(inner)?;
                let dest = self.fresh_temporary(temp_value_type(typ));

                match operator {
                    UnaryOperator::Deref => {
                        self.emit(dest, Kind::FieldLoad { src: rhs, offset: 0, typ })
                    },
                    UnaryOperator::Ref => {
                        let src = match rhs {
                            Operand::Place(place) => place,
                            Operand::Const(_) => unreachable!("cannot take address of constant"),
                        };
                        self.emit(dest, Kind::AddressOf { src, offset: 0 })
                    },
                    _ => self.emit(dest, Kind::Unary { operation: operator, rhs }),
                };

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Binary { operator, left, right } => {
                if let Some(hir::Res::Function(function)) = self.typeck.type_dependent_def(expr.id)
                {
                    let callee = self.get_fn_unchecked(&function);
                    let self_typ = callee
                        .params
                        .first()
                        .map(|p| p.typ)
                        .unwrap_or_else(|| self.typeck.type_of(left.id));
                    let other_typ = callee
                        .params
                        .get(1)
                        .map(|p| p.typ)
                        .unwrap_or_else(|| self.typeck.type_of(right.id));

                    let lhs = self.lower_overloaded(left, self_typ)?;
                    let rhs = self.lower_overloaded(right, other_typ)?;

                    return self.emit_call(
                        function,
                        vec![Operand::Place(lhs), Operand::Place(rhs)],
                        typ,
                    );
                }

                if matches!(operator, BinaryOperator::And | BinaryOperator::Or) {
                    return self.lower_short_circuit(*operator, left, right, typ);
                }

                let lhs = self.lower_expr(left)?;
                let rhs = self.lower_expr(right)?;
                let dest = self.fresh_temporary(temp_value_type(typ));

                let is_integer = typ.is_integer();
                let is_arithmetic = matches!(
                    operator,
                    BinaryOperator::Add | BinaryOperator::Sub | BinaryOperator::Mul
                );
                let is_on_debug = optimisation::Level::Debug == optimisation::get();
                let checked = is_integer && is_arithmetic && is_on_debug;

                self.emit(dest, Kind::Binary { operation: *operator, lhs, rhs, checked });

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Assign { target, value } => {
                let target = *target;
                let value_expr = *value;
                if let ExpressionKind::Local(local) = &target.kind {
                    self.constant_locals[*local] = self.capture_constant_expr(value_expr);
                }

                let src = self.lower_expr(value_expr)?;
                let (dest, offset, _) = self.place_info(target);

                match &target.kind {
                    ExpressionKind::Local(local) if self.runtime_local_uses(*local) => {
                        self.emit(dest, Kind::Assign(src));
                        Ok(Operand::Place(dest))
                    },
                    ExpressionKind::Local(_) => Ok(src),
                    ExpressionKind::Field { .. } => {
                        self.emit(dest, Kind::FieldStore { value: src, offset });
                        Ok(src)
                    },
                    _ => unreachable!("invalid assignment target in MIR lowering"),
                }
            },

            ExpressionKind::Path(_) => unreachable!(
                "a path callee is resolved via the side-tables, never lowered as a value"
            ),
            ExpressionKind::Call { args, .. } => {
                // a call resolves either to a function or, for `Optional::Some(x)`,
                // to an enum variant constructor that builds a tagged-union inline
                match self.typeck.type_dependent_def(expr.id).expect("call target must be resolved")
                {
                    hir::Res::Variant { id, index } => {
                        self.emit_variant(id, index, args.first().copied(), typ)
                    },
                    hir::Res::Function(function) => {
                        let mut lowered_args = Vec::with_capacity(args.len());
                        for arg in *args {
                            let operand = self.lower_expr(arg)?;
                            lowered_args.push(operand);
                        }

                        self.emit_call(function, lowered_args, typ)
                    },
                }
            },

            ExpressionKind::MethodCall { receiver, args, .. } => {
                let function = self
                    .typeck
                    .type_dependent_def(expr.id)
                    .and_then(hir::Res::function)
                    .expect("method target must be resolved");
                let callee_fn = self.get_fn_unchecked(&function);
                let receiver_typ = callee_fn
                    .params
                    .first()
                    .map(|p| p.typ)
                    .unwrap_or_else(|| self.typeck.type_of(receiver.id));

                let place = self.lower_overloaded(receiver, receiver_typ)?;

                let mut lowered_args = Vec::with_capacity(args.len() + 1);
                lowered_args.push(Operand::Place(place));
                for arg in *args {
                    let operand = self.lower_expr(arg)?;
                    lowered_args.push(operand);
                }

                self.emit_call(function, lowered_args, typ)
            },

            ExpressionKind::IntrinsicCall { intrinsic, args } => {
                use crate::hir::Intrinsic;

                // the return value is ignored for those functions
                match intrinsic {
                    Intrinsic::PrintLn | Intrinsic::Print => {
                        let mut output = String::new();
                        for arg in *args {
                            self.push_print_arg(&mut output, arg);
                        }

                        if *intrinsic == Intrinsic::PrintLn {
                            output.push('\n');
                        }

                        if !output.is_empty() {
                            self.emit_write_string(output);
                        }

                        Ok(Operand::Const(Const::Unit))
                    },

                    Intrinsic::Syscall => {
                        unreachable!("syscall intrinsic lowers through ExpressionKind::Syscall")
                    },

                    Intrinsic::Len => match self.lower_expr(args[0])? {
                        Operand::Const(Const::Str { len, .. }) => {
                            Ok(Operand::Const(Const::Int(len as i64, typ)))
                        },
                        Operand::Place(place) => {
                            let dest = self.fresh_temporary(typ);
                            let instr = InstructionKind::FieldLoad {
                                src: Operand::Place(place),
                                offset: 8,
                                typ,
                            };
                            self.emit(dest, instr);
                            Ok(Operand::Place(dest))
                        },
                        other => unreachable!("str length of a non-str operand: {other:?}"),
                    },
                }
            },

            ExpressionKind::TypeIntrinsic { kind, typ: target } => {
                let (size, align) = self.layouts.type_layout(*target);
                let value = match kind {
                    TypeIntrinsicKind::SizeOf => size as i64,
                    TypeIntrinsicKind::AlignOf => align as i64,
                };

                Ok(Operand::Const(Const::Int(value, typ)))
            },

            ExpressionKind::Syscall { code, args } => {
                let code = *code;
                let lowered_args =
                    args.iter().map(|a| self.lower_expr(a)).collect::<Result<Vec<_>, _>>()?;
                let dest = self.fresh_temporary(typ);

                self.emit(dest, Kind::Syscall { code, args: lowered_args, returns: true });

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Struct { id, fields } => {
                let id = *id;
                let dest = self.fresh_temporary(Type::new(TypeKind::Struct(id)));

                for (sym, value) in *fields {
                    let layout = self.layouts.field(Type::new(TypeKind::Struct(id)), *sym);
                    let value_operand = self.lower_expr(value)?;

                    self.emit(
                        dest,
                        Kind::FieldStore { value: value_operand, offset: layout.offset },
                    );
                }

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Field { .. } => {
                let (origin, offset, typ) = self.place_info(expr);
                let dest = self.fresh_temporary(typ);
                self.emit(dest, Kind::FieldLoad { src: Operand::Place(origin), offset, typ });
                Ok(Operand::Place(dest))
            },

            ExpressionKind::Match { scrutinee, arms } => {
                let scrutinee = *scrutinee;
                let arms = *arms;

                // Evaluate scrutinee to a place
                let scrutinee_operand = self.lower_expr(scrutinee)?;
                let scrutinee_place = match scrutinee_operand {
                    Operand::Place(p) => p,
                    Operand::Const(c) => {
                        let p = self.fresh_temporary(self.typeck.type_of(scrutinee.id));
                        self.emit(p, Kind::Assign(Operand::Const(c)));
                        p
                    },
                };

                let join_block = self.new_block();
                let match_result_place =
                    (typ.kind() != TypeKind::Unit).then(|| self.fresh_temporary(typ));

                let mut next_arm_check_block = self.new_block();

                // Jump to the first check block
                self.terminate(Terminator::Jump(next_arm_check_block));

                for arm in arms.iter() {
                    self.switch_to(next_arm_check_block);
                    next_arm_check_block = self.new_block();
                    let body_block = self.new_block();

                    self.lower_pattern_match(
                        scrutinee_place,
                        arm.pattern,
                        body_block,
                        next_arm_check_block,
                    )?;

                    // if present, evaluate in the body block
                    // on false, fall through to next arm
                    self.switch_to(body_block);
                    let exec_block = arm
                        .guard
                        .map(|guard| {
                            let then_block = self.new_block();
                            let condition = self.lower_expr(guard)?;

                            self.terminate(Terminator::Branch {
                                condition,
                                then_block,
                                else_block: next_arm_check_block,
                            });
                            self.switch_to(then_block);

                            Ok::<_, MirError>(then_block)
                        })
                        .transpose()?
                        .unwrap_or(body_block);
                    let _ = exec_block;

                    let body_operand = self.lower_expr(arm.body)?;
                    if let Some(res_place) = match_result_place {
                        self.emit(res_place, Kind::Assign(body_operand));
                    }
                    self.terminate(Terminator::Jump(join_block));
                }

                self.switch_to(next_arm_check_block);
                self.terminate(Terminator::Return(None));

                self.switch_to(join_block);

                match match_result_place {
                    Some(p) => Ok(Operand::Place(p)),
                    None => Ok(Operand::Const(Const::Unit)),
                }
            },
        }
    }

    fn lower_short_circuit(
        &mut self,
        operator: BinaryOperator,
        left: &Expression<'hir>,
        right: &Expression<'hir>,
        typ: Type,
    ) -> Result<Operand, MirError> {
        debug_assert_eq!(
            typ,
            Type::new(TypeKind::Bool),
            "`&&` and `||` should be perfomed only on booleans"
        );

        let result = self.fresh_temporary(Type::new(TypeKind::Bool));
        let left_operand = self.lower_expr(left)?;

        let right_id = self.new_block();
        let short_id = self.new_block();
        let merge_id = self.new_block();

        let (then_block, else_block, short_value) = match operator {
            BinaryOperator::And => (right_id, short_id, false),
            BinaryOperator::Or => (short_id, right_id, true),
            _ => unsafe { std::hint::unreachable_unchecked() },
        };

        self.terminate(Terminator::Branch { condition: left_operand, then_block, else_block });

        self.switch_to(right_id);
        let right_operand = self.lower_expr(right)?;
        self.emit(result, InstructionKind::Assign(right_operand));
        self.terminate(Terminator::Jump(merge_id));

        self.switch_to(short_id);
        self.emit(result, InstructionKind::Assign(Operand::Const(Const::Bool(short_value))));
        self.terminate(Terminator::Jump(merge_id));

        self.switch_to(merge_id);

        Ok(Operand::Place(result))
    }

    fn lower_overloaded(&mut self, left: &'hir Expression, typ: Type) -> Result<Place, MirError> {
        let place = self.fresh_temporary(typ);

        match self.is_place_expr(left) {
            true => {
                let (origin, offset, typ) = self.place_info(left);
                debug_assert!(typ.kind() != TypeKind::Unit);

                let instr = match (offset, typ.is_ref()) {
                    (0, true) => InstructionKind::Assign(Operand::Place(origin)),
                    (_, true) => {
                        InstructionKind::FieldLoad { src: Operand::Place(origin), offset, typ }
                    },
                    _ => InstructionKind::AddressOf { src: origin, offset },
                };

                self.emit(place, instr)
            },
            _ => {
                let val_type = self.typeck.type_of(left.id);
                let lowered = self.lower_expr(left)?;

                match val_type.is_ref() {
                    true => self.emit(place, InstructionKind::Assign(lowered)),
                    _ => {
                        let val_place = self.fresh_temporary(val_type);
                        self.emit(val_place, InstructionKind::Assign(lowered));
                        self.emit(place, InstructionKind::AddressOf { src: val_place, offset: 0 });
                    },
                }
            },
        }

        Ok(place)
    }

    #[inline(always)]
    fn local_type(&self, id: LocalId) -> Type {
        self.locals[self.local_map[id].0 as usize].1
    }

    #[inline(always)]
    fn is_place_expr(&self, expr: &Expression<'hir>) -> bool {
        matches!(&expr.kind, ExpressionKind::Local(_) | ExpressionKind::Field { .. })
    }

    fn place_info(&self, expr: &Expression<'hir>) -> (Place, u32, Type) {
        match &expr.kind {
            ExpressionKind::Local(local_id) => {
                let origin = self.place_for_local(*local_id, self.local_type(*local_id));
                (origin, 0, origin.typ)
            },
            ExpressionKind::Field { base, field } => {
                let (origin, base_offset, base_type) = self.place_info(base);
                let layout = self.layouts.field(base_type, *field);
                (origin, base_offset + layout.offset, layout.typ)
            },
            _ => panic!("place_info called on non-place expression: {:?}", expr),
        }
    }

    fn terminate(&mut self, term: Terminator) {
        debug_assert!(
            !self.blocks[self.current].is_terminated(),
            "double-termination of block {:?}",
            self.blocks[self.current].id
        );

        self.blocks[self.current].terminator = Some(term);
    }

    #[inline(always)]
    fn place_for_local(&self, local_id: LocalId, typ: Type) -> Place {
        Place { id: self.local_map[local_id], typ }
    }

    #[inline(always)]
    fn fresh_temporary(&mut self, typ: Type) -> Place {
        assert!(
            !matches!(typ.kind(), TypeKind::Unit),
            "internal error: unit type temporary created"
        );
        let id = ValueId(self.next);
        self.next += 1;
        self.locals.push((id, typ));
        Place { id, typ }
    }

    #[inline(always)]
    fn is_terminated(&self) -> bool {
        self.blocks[self.current].is_terminated()
    }

    #[inline(always)]
    const fn switch_to(&mut self, id: BlockId) {
        self.current = id.0 as usize;
    }

    #[inline(always)]
    fn current_block_id(&self) -> BlockId {
        BlockId(self.current as u32)
    }

    fn lower_pattern_match(
        &mut self,
        place: Place,
        pattern: &hir::Pattern<'hir>,
        success_block: BlockId,
        fail_block: BlockId,
    ) -> Result<(), MirError> {
        use InstructionKind as Kind;
        use hir::PatternKind;

        match &pattern.kind {
            PatternKind::Wildcard => {
                self.terminate(Terminator::Jump(success_block));
                Ok(())
            },
            PatternKind::Binding(local_id) => {
                let local_typ = self.local_type(*local_id);
                let dest_place = self.place_for_local(*local_id, local_typ);
                self.emit(dest_place, Kind::Assign(Operand::Place(place)));
                self.terminate(Terminator::Jump(success_block));
                Ok(())
            },
            PatternKind::Literal(lit) => {
                use hir::Literal as L;
                let place_typ = place.typ;
                let rhs = match lit {
                    L::Int(n) => Const::Int(*n, place_typ),
                    L::Float(f) => Const::Float(*f, place_typ),
                    L::Bool(b) => Const::Bool(*b),
                    L::Char(c) => Const::Int(*c as i64, place_typ),
                    L::Unit | L::Str(_) => {
                        self.terminate(Terminator::Jump(success_block));
                        return Ok(());
                    },
                };
                let cond = self.fresh_temporary(Type::new(TypeKind::Bool));
                self.emit(
                    cond,
                    Kind::Binary {
                        operation: BinaryOperator::Eq,
                        lhs: Operand::Place(place),
                        rhs: Operand::Const(rhs),
                        checked: false,
                    },
                );
                self.terminate(Terminator::Branch {
                    condition: Operand::Place(cond),
                    then_block: success_block,
                    else_block: fail_block,
                });
                Ok(())
            },
            PatternKind::Or(alternatives) => {
                let mut check = self.current_block_id();
                let n = alternatives.len();
                for (i, alt) in alternatives.iter().enumerate() {
                    self.switch_to(check);
                    let next = match i + 1 < n {
                        true => self.new_block(),
                        _ => fail_block,
                    };
                    self.lower_pattern_match(place, alt, success_block, next)?;
                    check = next;
                }

                Ok(())
            },
            PatternKind::Variant { id: enum_id, variant_idx, sub } => {
                let enum_def = &self.enums[*enum_id];
                let variant = enum_def.variants[*variant_idx];
                let tag_val = variant.value;
                let tag_ty = enum_id.repr().typ();

                // load discriminant tag from offset 0
                let tag_place = self.fresh_temporary(tag_ty);
                self.emit(
                    tag_place,
                    Kind::FieldLoad { src: Operand::Place(place), offset: 0, typ: tag_ty },
                );

                let match_cond_block = self.current_block_id();

                if let Some(sub_pat) = sub {
                    let sub_block = self.new_block();

                    // first branch on the tag: a match continues into `sub_block`,
                    // a mismatch falls through to `fail_block`
                    let _ = match_cond_block;
                    let cond = self.fresh_temporary(Type::new(TypeKind::Bool));
                    self.emit(
                        cond,
                        Kind::Binary {
                            operation: BinaryOperator::Eq,
                            lhs: Operand::Place(tag_place),
                            rhs: Operand::Const(Const::Int(tag_val, tag_ty)),
                            checked: false,
                        },
                    );
                    self.terminate(Terminator::Branch {
                        condition: Operand::Place(cond),
                        then_block: sub_block,
                        else_block: fail_block,
                    });

                    // only inside `sub_block` (tag confirmed) do we read the payload
                    // and recursively match the sub-pattern
                    self.switch_to(sub_block);
                    let offset = self.layouts.enum_layout(*enum_id).payload_offset;
                    let typ = variant
                        .payload
                        .expect("Variant must have payload type since it has subpattern");
                    let payload_place = self.fresh_temporary(typ);
                    self.emit(
                        payload_place,
                        Kind::FieldLoad { src: Operand::Place(place), offset, typ },
                    );
                    self.lower_pattern_match(payload_place, sub_pat, success_block, fail_block)?;
                } else {
                    // Just check tag
                    let cond = self.fresh_temporary(Type::new(TypeKind::Bool));
                    self.emit(
                        cond,
                        Kind::Binary {
                            operation: BinaryOperator::Eq,
                            lhs: Operand::Place(tag_place),
                            rhs: Operand::Const(Const::Int(tag_val, tag_ty)),
                            checked: false,
                        },
                    );
                    self.terminate(Terminator::Branch {
                        condition: Operand::Place(cond),
                        then_block: success_block,
                        else_block: fail_block,
                    });
                }
                Ok(())
            },
        }
    }

    #[inline(always)]
    fn emit(&mut self, dest: Place, kind: InstructionKind) {
        self.blocks[self.current].instructions.push(Instruction { dest, kind });
    }

    fn emit_write_string(&mut self, text: String) {
        let len = text.len();
        let id = self.intern_owned_string(text);
        let dest = self.fresh_temporary(Type::new(TypeKind::I32));

        self.emit(
            dest,
            InstructionKind::Syscall {
                code: crate::hir::SyscallCode::Write,
                args: vec![
                    Operand::Const(Const::Int(1, Type::new(TypeKind::I32))),
                    Operand::Const(Const::Str { id, len }),
                    Operand::Const(Const::Int(len as i64, Type::new(TypeKind::I32))),
                ],
                returns: false,
            },
        );
    }

    #[inline]
    fn push_print_arg(&self, output: &mut String, expr: &Expression<'hir>) {
        if let ExpressionKind::Literal(hir::Literal::Str(sym)) = &expr.kind {
            let text = &self.symbols[sym.0.into_usize()];
            output.push_str(&self.expand_interpolation(text));
        } else if let Some(text) = self.capture_constant_expr(expr) {
            output.push_str(&text);
        }
    }

    #[inline]
    fn capture_constant_expr(&self, expr: &Expression<'hir>) -> Option<String> {
        match &expr.kind {
            ExpressionKind::Literal(lit) => {
                use hir::Literal as L;
                Some(match lit {
                    L::Int(n) => n.to_string(),
                    L::Float(f) => f.to_string(),
                    L::Bool(b) => b.to_string(),
                    L::Char(c) => c.to_string(),
                    L::Str(sym) => self.symbols[sym.0.into_usize()].clone(),
                    L::Unit => String::new(),
                })
            },
            ExpressionKind::Local(id) => self.constant_locals[*id].clone(),
            _ => None,
        }
    }

    fn expand_interpolation(&self, input: &str) -> String {
        let mut output = String::with_capacity(input.len());
        let mut chars = input.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch != '{' {
                output.push(ch);
                continue;
            }

            let mut name = String::new();
            let mut closed = false;

            for ch in chars.by_ref() {
                if ch == '}' {
                    closed = true;
                    break;
                }

                name.push(ch);
            }

            if closed {
                match self.lookup_constant_local(&name) {
                    Some(value) => output.push_str(value),
                    None => {
                        output.push('{');
                        output.push_str(&name);
                        output.push('}');
                    },
                }
            } else {
                output.push('{');
                output.push_str(&name);
            }
        }

        output
    }

    #[inline]
    fn get_fn_unchecked<'f>(&'f self, id: &FunctionId) -> &'f hir::Function<'f> {
        self.functions_map
            .get(id)
            .unwrap_or_else(|| panic!("callee function {:?} not found", id))
    }

    #[inline]
    fn lookup_constant_local(&self, name: &str) -> Option<&str> {
        self.local_symbols
            .iter()
            .enumerate()
            .find(|(_, symbol)| self.symbols.get(**symbol).is_some_and(|local| local == name))
            .and_then(|(idx, _)| self.constant_locals[LocalId(idx as u32)].as_deref())
    }

    #[inline]
    fn intern_string(&mut self, value: &str) -> usize {
        if let Some(id) = self.strings.iter().position(|existing| existing == value) {
            return id;
        }

        let id = self.strings.len();
        self.strings.push(value.to_owned());
        id
    }

    #[inline(always)]
    fn intern_owned_string(&mut self, value: String) -> usize {
        if let Some(id) = self.strings.iter().position(|existing| existing == &value) {
            return id;
        }

        let id = self.strings.len();
        self.strings.push(value);
        id
    }

    fn emit_variant(
        &mut self,
        id: hir::EnumId,
        index: usize,
        payload: Option<&'hir Expression<'hir>>,
        typ: Type,
    ) -> Result<Operand, MirError> {
        let dest = self.fresh_temporary(typ);

        let tag_ty = id.repr().typ();
        let tag = self.enums[id].variants[index].value;
        self.emit(
            dest,
            InstructionKind::FieldStore {
                value: Operand::Const(Const::Int(tag, tag_ty)),
                offset: 0,
            },
        );

        if let Some(payload) = payload {
            let offset = self.layouts.enum_layout(id).payload_offset;
            let value = self.lower_expr(payload)?;
            self.emit(dest, InstructionKind::FieldStore { value, offset });
        }

        Ok(Operand::Place(dest))
    }

    fn emit_call(
        &mut self,
        callee_id: FunctionId,
        lowered_args: Vec<Operand>,
        return_type: Type,
    ) -> Result<Operand, MirError> {
        let callee = self.get_fn_unchecked(&callee_id);
        match callee.inline {
            true => self.inline_call(callee_id, lowered_args),
            _ => {
                let dest = self.fresh_temporary(temp_value_type(return_type));

                self.emit(dest, InstructionKind::Call { callee: callee_id, args: lowered_args });

                Ok(Operand::Place(dest))
            },
        }
    }

    fn inline_call(
        &mut self,
        callee_id: FunctionId,
        lowered_args: Vec<Operand>,
    ) -> Result<Operand, MirError> {
        let callee = self.functions_map.get(&callee_id).unwrap();

        let inline_ret_place = match callee.return_type.kind() != TypeKind::Unit {
            true => Some(self.fresh_temporary(callee.return_type)),
            _ => None,
        };
        let exit_block_id = self.new_block();

        let callee_n_locals = callee.locals.len();
        let mut callee_local_map = IndexVec::from_elem(ValueId(0), callee_n_locals);
        for local in &callee.locals {
            let place = self.fresh_temporary(local.typ);
            callee_local_map[local.id] = place.id;
        }

        // emit assignment of arguments to callee parameters
        for (param, arg_operand) in callee.params.iter().zip(lowered_args) {
            let dest_val_id = callee_local_map[param.id];
            let dest_place = Place { id: dest_val_id, typ: param.typ };
            self.emit(dest_place, InstructionKind::Assign(arg_operand));
        }

        let old_context =
            self.enter_inline_context(callee, callee_local_map, exit_block_id, inline_ret_place);

        self.lower_block(&callee.body)?;

        if !self.is_terminated() {
            self.terminate(Terminator::Jump(exit_block_id));
        }

        self.restore_inline_context(old_context);

        self.switch_to(exit_block_id);

        let result = match inline_ret_place {
            Some(place) => Operand::Place(place),
            None => Operand::Const(Const::Unit),
        };
        Ok(result)
    }

    fn enter_inline_context(
        &mut self,
        callee: &'a hir::Function<'hir>,
        local_map: IndexVec<LocalId, ValueId>,
        exit_block_id: BlockId,
        return_place: Option<Place>,
    ) -> InlineContext<'a> {
        use std::mem::replace;

        InlineContext {
            local_map: replace(&mut self.local_map, local_map),
            constant_locals: replace(
                &mut self.constant_locals,
                IndexVec::from_elem(None, callee.locals.len()),
            ),
            runtime_local_uses: replace(
                &mut self.runtime_local_uses,
                self.runtime_uses_map
                    .get(&callee.id)
                    .expect("runtime use map must contain inlined function")
                    .clone(),
            ),
            local_symbols: replace(
                &mut self.local_symbols,
                callee.locals.iter().map(|l| l.name.0.into_usize()).collect(),
            ),
            inlined_return_target: self
                .inlined_return_target
                .replace((exit_block_id, return_place)),
            typeck: replace(&mut self.typeck, &callee.typeck),
        }
    }

    fn restore_inline_context(&mut self, context: InlineContext<'a>) {
        self.local_map = context.local_map;
        self.constant_locals = context.constant_locals;
        self.runtime_local_uses = context.runtime_local_uses;
        self.local_symbols = context.local_symbols;
        self.inlined_return_target = context.inlined_return_target;
        self.typeck = context.typeck;
    }

    #[inline(always)]
    fn runtime_local_uses(&self, id: LocalId) -> bool {
        self.runtime_local_uses.get(id).copied().unwrap_or(false)
    }
}

impl PartialBlock {
    fn new(id: BlockId) -> Self {
        Self { id, instructions: Vec::new(), terminator: None }
    }

    #[inline(always)]
    const fn is_terminated(&self) -> bool {
        self.terminator.is_some()
    }

    fn finalise(self) -> Block {
        Block {
            id: self.id,
            instructions: self.instructions,
            terminator: self.terminator.expect("block missing terminator"),
        }
    }
}

fn collect_runtime_local_uses(function: &hir::Function<'_>) -> IndexVec<LocalId, bool> {
    let mut uses = IndexVec::from_elem(false, function.locals.len());
    visit_block_runtime_uses(&function.body, &mut uses);

    uses
}

fn visit_block_runtime_uses(block: &hir::Block<'_>, uses: &mut IndexVec<LocalId, bool>) {
    for statement in block.statements {
        match statement {
            Statement::LetInit { init, .. } => visit_expr_runtime_uses(init, uses),
            Statement::LetUninit { .. } => {},
            Statement::Expr(expr) => visit_expr_runtime_uses(expr, uses),
            Statement::Return(Some(expr)) => visit_expr_runtime_uses(expr, uses),
            Statement::Return(None) => {},
            Statement::If { condition, then_block, else_block } => {
                visit_expr_runtime_uses(condition, uses);
                visit_block_runtime_uses(then_block, uses);
                if let Some(else_block) = else_block {
                    visit_block_runtime_uses(else_block, uses);
                }
            },
            hir::Statement::While { condition, body } => {
                visit_expr_runtime_uses(condition, uses);
                visit_block_runtime_uses(body, uses);
            },
            Statement::Block(block) => visit_block_runtime_uses(block, uses),
        }
    }
}

fn visit_expr_runtime_uses(expr: &hir::Expression<'_>, uses: &mut IndexVec<LocalId, bool>) {
    match &expr.kind {
        ExpressionKind::Local(id) => uses[*id] = true,
        ExpressionKind::Unary { expr: inner, .. } => visit_expr_runtime_uses(inner, uses),
        ExpressionKind::Cast { from, .. } => visit_expr_runtime_uses(from, uses),
        ExpressionKind::Binary { left, right, .. } => {
            visit_expr_runtime_uses(left, uses);
            visit_expr_runtime_uses(right, uses);
        },
        ExpressionKind::Assign { target, value } => {
            if !matches!(&target.kind, ExpressionKind::Local(_)) {
                visit_place_runtime_uses(target, uses);
            }
            visit_expr_runtime_uses(value, uses);
        },
        ExpressionKind::Struct { fields, .. } => {
            for &(_, value) in *fields {
                visit_expr_runtime_uses(value, uses);
            }
        },
        ExpressionKind::Call { args, .. }
        | ExpressionKind::IntrinsicCall { args, .. }
        | ExpressionKind::Syscall { args, .. } => {
            for arg in *args {
                visit_expr_runtime_uses(arg, uses);
            }
        },
        ExpressionKind::MethodCall { receiver, args, .. } => {
            let receiver = *receiver;
            let is_place =
                matches!(&receiver.kind, ExpressionKind::Local(_) | ExpressionKind::Field { .. });

            match is_place {
                true => visit_place_runtime_uses(receiver, uses),
                _ => visit_expr_runtime_uses(receiver, uses),
            }

            for arg in *args {
                visit_expr_runtime_uses(arg, uses);
            }
        },
        ExpressionKind::Field { .. } => visit_place_runtime_uses(expr, uses),
        ExpressionKind::TypeIntrinsic { .. }
        | ExpressionKind::Literal(_)
        | ExpressionKind::Path(_) => {},
        ExpressionKind::Match { scrutinee, arms } => {
            visit_expr_runtime_uses(scrutinee, uses);
            for arm in *arms {
                if let Some(guard) = arm.guard {
                    visit_expr_runtime_uses(guard, uses);
                }
                visit_expr_runtime_uses(arm.body, uses);
            }
        },
    }
}

fn visit_place_runtime_uses(expr: &hir::Expression<'_>, uses: &mut IndexVec<LocalId, bool>) {
    if let Some(local_id) = hir::place_base_local(expr) {
        uses[local_id] = true;
    }
}

fn has_open_generic(func: &hir::Function<'_>) -> bool {
    fn is_open(t: Type) -> bool {
        match t.kind() {
            TypeKind::GenericParam(_) => true,
            TypeKind::Ref { to, .. } => matches!(to.kind(), RefTargetKind::GenericParam(_)),
            _ => false,
        }
    }

    is_open(func.return_type) || func.params.iter().any(|p| is_open(p.typ))
}
