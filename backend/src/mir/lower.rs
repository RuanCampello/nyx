//! HIR -> MIR lowering

use crate::{
    Span,
    hir::{
        self, Expression, ExpressionKind, FunctionId, Hir, LocalId, Statement, SymbolId,
        SymbolTable, TyInterner, Type, TypeKind, ids::IndexVec,
    },
    mir::{
        self, Block, BlockId, Const, Function, Instruction, InstructionKind, Mir, Operand,
        OverflowMode, Place, StringPool, Terminator, ValueId, error::MirError,
    },
    optimisation,
    parser::expression::{BinaryOperator, TypeIntrinsicKind, UnaryOperator},
};

use builtin::BuiltinLowering;
use constants::ConstantValues;
use layout::LayoutCollector;

mod builtin;
mod constants;
mod layout;
mod runtime_uses;

struct FunctionLower<'a, 'hir> {
    context: LoweringContext<'a, 'hir>,
    build: FunctionBuildState<'hir>,
    strings: &'a mut StringPool,
    local_map: IndexVec<LocalId, ValueId>,
    typeck: &'a hir::TypeckResults<'hir>,
    local_symbols: IndexVec<LocalId, SymbolId>,
    constants: ConstantValues<'a>,
    runtime_local_uses: IndexVec<LocalId, bool>,
    inlined_return_target: Option<(BlockId, Option<Place<'hir>>)>,
}

#[derive(Clone, Copy)]
struct LoweringContext<'a, 'hir> {
    types: &'a TyInterner<'hir>,
    symbols: &'a SymbolTable,
    adts: &'a IndexVec<hir::AdtId, hir::AdtDef<'hir>>,
    arrays: &'a hir::ArrayTable<'hir>,
    functions: &'a IndexVec<FunctionId, hir::Function<'hir>>,
    runtime_uses: &'a IndexVec<FunctionId, IndexVec<LocalId, bool>>,
}

struct FunctionBuildState<'hir> {
    blocks: IndexVec<BlockId, PartialBlock<'hir>>,
    current: BlockId,
    next: u32,
    locals: Vec<(ValueId, Type<'hir>)>,
    loop_targets: Vec<LoopTargets>,
    span: Span,
}

struct InlineContext<'a, 'hir> {
    local_map: IndexVec<LocalId, ValueId>,
    constants: ConstantValues<'a>,
    runtime_local_uses: IndexVec<LocalId, bool>,
    local_symbols: IndexVec<LocalId, SymbolId>,
    inlined_return_target: Option<(BlockId, Option<Place<'hir>>)>,
    typeck: &'a hir::TypeckResults<'hir>,
}

struct PartialBlock<'hir> {
    instructions: Vec<Instruction<'hir>>,
    terminator: Option<Terminator<'hir>>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    break_target: BlockId,
    continue_target: BlockId,
}

pub fn lower<'hir>(hir: Hir<'hir>) -> Result<Mir<'hir>, MirError> {
    debug_assert!(
        !hir.functions.iter().any(has_open_generic),
        r#"MIR lowering received HIR containing unresolved GenericParam
        monomorphisation should have produced fully concrete signatures"#
    );

    let mut functions = Vec::with_capacity(hir.functions.len());
    let mut strings = StringPool::default();
    let adts = &hir.adts;
    let arrays = &hir.arrays;
    let symbols = hir.symbols;
    let types = &hir.types;

    let mut runtime_uses = IndexVec::with_capacity(hir.functions.len());
    for function in &hir.functions {
        let id = runtime_uses.push(runtime_uses::collect(function));
        debug_assert_eq!(id, function.id);
    }

    let context = LoweringContext {
        types,
        symbols: &symbols,
        adts,
        arrays,
        functions: &hir.functions,
        runtime_uses: &runtime_uses,
    };

    for function in &hir.functions {
        functions.push(FunctionLower::run(function, context, &mut strings)?);
    }

    let layouts = LayoutCollector::new(types, adts, arrays).collect(&functions, &hir.statics);

    Ok(Mir {
        types: hir.types,
        functions,
        symbols,
        strings,
        statics: hir.statics.iter().copied().collect(),
        layouts: layouts.adts,
        reprs: hir
            .adts
            .iter()
            .map(|adt| match adt.kind {
                hir::AdtKind::Enum { repr, .. } => Some(repr),
                hir::AdtKind::Struct { .. } => None,
            })
            .collect(),
        array_layouts: layouts.arrays,
    })
}

fn temp_value_type<'hir>(typ: Type<'hir>) -> Type<'hir> {
    match typ.kind() {
        TypeKind::Unit | TypeKind::Never => Type::from(TypeKind::I32),
        _ => typ,
    }
}

impl<'a, 'hir> FunctionLower<'a, 'hir> {
    fn run(
        function: &hir::Function<'hir>,
        context: LoweringContext<'a, 'hir>,
        strings: &'a mut StringPool,
    ) -> Result<mir::Function<'hir>, MirError> {
        let id = function.id;
        let intrinsic = function.kind.intrinsic();
        let name_symbol = function.name;
        let return_type = function.return_type;
        let n_hir_locals = function.locals.len();

        let mut local_map = IndexVec::from_elem(ValueId(0), n_hir_locals);
        let local_symbols = function.locals.iter().map(|l| l.name).collect();
        let mut locals = Vec::with_capacity(n_hir_locals);

        for local in &function.locals {
            let value_id = ValueId(locals.len() as u32);
            local_map[local.id] = value_id;
            locals.push((value_id, local.typ))
        }

        let params = function.params.iter().map(|param| (local_map[param.id], param.typ)).collect();

        let next = locals.len() as u32;

        let build = FunctionBuildState {
            blocks: IndexVec::new(),
            current: BlockId::ENTRY,
            next,
            locals,
            loop_targets: Vec::new(),
            span: function.decl_span,
        };

        let mut lower = FunctionLower {
            context,
            build,
            strings,
            local_map,
            typeck: &function.typeck,
            local_symbols,
            constants: ConstantValues::new(context.symbols, n_hir_locals),
            runtime_local_uses: context.runtime_uses[id].clone(),
            inlined_return_target: None,
        };

        lower.build.new_block();
        lower.lower_block(&function.body)?;

        if !lower.build.is_terminated() {
            lower.build.terminate(Terminator::Return(None));
        }

        let (blocks, locals) = lower.build.finish();

        Ok(Function {
            id,
            intrinsic,
            is_const: function.is_const,
            blocks,
            return_type,
            params,
            name_symbol,
            locals,
        })
    }

    fn lower_block(&mut self, block: &hir::Block<'hir>) -> Result<(), MirError> {
        for statement in block.statements {
            if self.build.is_terminated() {
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
                let constant = self.constants.capture(init);
                self.constants.record(*id, constant);

                if !self.runtime_local_uses(*id) && self.constants.get(*id).is_some() {
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

            Stmt::Loop { kind, body } => self.lower_loop(*kind, body)?,
            Stmt::Break => {
                let target =
                    self.build.loop_targets.last().expect("break without an enclosing loop");
                self.terminate(Terminator::Jump(target.break_target));
            },
            Stmt::Continue => {
                let target =
                    self.build.loop_targets.last().expect("continue without an enclosing loop");
                self.terminate(Terminator::Jump(target.continue_target));
            },
        }

        Ok(())
    }

    fn lower_loop(
        &mut self,
        kind: hir::LoopKind<'hir>,
        body: &hir::Block<'hir>,
    ) -> Result<(), MirError> {
        match kind {
            hir::LoopKind::Infinite => self.lower_infinite_loop(body),
            hir::LoopKind::Range { binding, start, end, inclusive } => {
                self.lower_range_loop(binding, start, end, inclusive, body)
            },
            hir::LoopKind::Iterable { binding, iterable } => {
                self.lower_iterable_loop(binding, iterable, body)
            },
        }
    }

    fn lower_infinite_loop(&mut self, body: &hir::Block<'hir>) -> Result<(), MirError> {
        let header = self.new_block();
        let body_block = self.new_block();
        let exit = self.new_block();

        self.terminate(Terminator::Jump(header));
        self.switch_to(header);
        self.terminate(Terminator::Jump(body_block));

        self.switch_to(body_block);
        self.build
            .loop_targets
            .push(LoopTargets { break_target: exit, continue_target: header });
        self.lower_block(body)?;
        self.build.loop_targets.pop();
        if !self.build.is_terminated() {
            self.terminate(Terminator::Jump(header));
        }

        self.switch_to(exit);
        Ok(())
    }

    fn lower_range_loop(
        &mut self,
        binding: Option<LocalId>,
        start: &'hir Expression<'hir>,
        end: &'hir Expression<'hir>,
        inclusive: bool,
        body: &hir::Block<'hir>,
    ) -> Result<(), MirError> {
        let typ = self.typeck.type_of(start.id);
        let counter = self.fresh_temporary(typ);
        let start = self.lower_expr(start)?;
        self.emit(counter, InstructionKind::Assign(start));

        let end_place = self.fresh_temporary(typ);
        let end = self.lower_expr(end)?;
        self.emit(end_place, InstructionKind::Assign(end));

        let ascending = self.fresh_temporary(TypeKind::Bool.into());
        let direction = match inclusive {
            true => BinaryOperator::LtEq,
            false => BinaryOperator::Lt,
        };
        self.emit_binary(ascending, direction, Operand::Place(counter), Operand::Place(end_place));

        let header = self.new_block();
        let ascending_check = self.new_block();
        let descending_check = self.new_block();
        let body_block = self.new_block();
        let step = self.new_block();
        let ascending_step = self.new_block();
        let descending_step = self.new_block();
        let exit = self.new_block();

        self.terminate(Terminator::Jump(header));

        self.switch_to(header);
        self.terminate(Terminator::Branch {
            condition: Operand::Place(ascending),
            then_block: ascending_check,
            else_block: descending_check,
        });

        self.switch_to(ascending_check);
        let comparison = match inclusive {
            true => BinaryOperator::LtEq,
            false => BinaryOperator::Lt,
        };
        let condition =
            self.binary_temporary(comparison, Operand::Place(counter), Operand::Place(end_place));
        self.terminate(Terminator::Branch { condition, then_block: body_block, else_block: exit });

        self.switch_to(descending_check);
        let comparison = match inclusive {
            true => BinaryOperator::GtEq,
            false => BinaryOperator::Gt,
        };
        let condition =
            self.binary_temporary(comparison, Operand::Place(counter), Operand::Place(end_place));
        self.terminate(Terminator::Branch { condition, then_block: body_block, else_block: exit });

        self.switch_to(body_block);
        if let Some(binding) = binding {
            let destination = self.place_for_local(binding, typ);
            self.emit(destination, InstructionKind::Assign(Operand::Place(counter)));
        }
        self.build
            .loop_targets
            .push(LoopTargets { break_target: exit, continue_target: step });
        self.lower_block(body)?;
        self.build.loop_targets.pop();
        if !self.build.is_terminated() {
            self.terminate(Terminator::Jump(step));
        }

        self.switch_to(step);
        self.terminate(Terminator::Branch {
            condition: Operand::Place(ascending),
            then_block: ascending_step,
            else_block: descending_step,
        });

        self.switch_to(ascending_step);
        self.lower_range_step(
            counter,
            end_place,
            typ,
            inclusive,
            BinaryOperator::Add,
            header,
            exit,
        );

        self.switch_to(descending_step);
        self.lower_range_step(
            counter,
            end_place,
            typ,
            inclusive,
            BinaryOperator::Sub,
            header,
            exit,
        );

        self.switch_to(exit);
        Ok(())
    }

    fn lower_range_step(
        &mut self,
        counter: Place<'hir>,
        end: Place<'hir>,
        typ: Type<'hir>,
        inclusive: bool,
        operation: BinaryOperator,
        header: BlockId,
        exit: BlockId,
    ) {
        if inclusive {
            let update = self.new_block();
            let condition = self.binary_temporary(
                BinaryOperator::Eq,
                Operand::Place(counter),
                Operand::Place(end),
            );
            self.terminate(Terminator::Branch { condition, then_block: exit, else_block: update });
            self.switch_to(update);
        }

        self.emit_binary(
            counter,
            operation,
            Operand::Place(counter),
            Operand::Const(Const::Int(1, typ)),
        );
        self.terminate(Terminator::Jump(header));
    }

    fn lower_iterable_loop(
        &mut self,
        binding: LocalId,
        iterable: &'hir Expression<'hir>,
        body: &hir::Block<'hir>,
    ) -> Result<(), MirError> {
        let iterable_type = self.typeck.type_of(iterable.id);
        let (base, bound, element, stride) = self.index_operands(iterable, iterable_type)?;
        let index_type = TypeKind::Uptr.into();
        let index = self.fresh_temporary(index_type);
        self.emit(index, InstructionKind::Assign(Operand::Const(Const::Int(0, index_type))));

        let header = self.new_block();
        let body_block = self.new_block();
        let step = self.new_block();
        let exit = self.new_block();
        self.terminate(Terminator::Jump(header));

        self.switch_to(header);
        let condition = self.binary_temporary(BinaryOperator::Lt, Operand::Place(index), bound);
        self.terminate(Terminator::Branch { condition, then_block: body_block, else_block: exit });

        self.switch_to(body_block);
        let destination = self.place_for_local(binding, element);
        self.emit(
            destination,
            InstructionKind::ElementLoad {
                base,
                index: Operand::Place(index),
                bound,
                stride,
                typ: element,
            },
        );
        self.build
            .loop_targets
            .push(LoopTargets { break_target: exit, continue_target: step });
        self.lower_block(body)?;
        self.build.loop_targets.pop();
        if !self.build.is_terminated() {
            self.terminate(Terminator::Jump(step));
        }

        self.switch_to(step);
        self.emit_binary(
            index,
            BinaryOperator::Add,
            Operand::Place(index),
            Operand::Const(Const::Int(1, index_type)),
        );
        self.terminate(Terminator::Jump(header));

        self.switch_to(exit);
        Ok(())
    }

    #[inline]
    fn binary_temporary(
        &mut self,
        operation: BinaryOperator,
        lhs: Operand<'hir>,
        rhs: Operand<'hir>,
    ) -> Operand<'hir> {
        let destination = self.fresh_temporary(TypeKind::Bool.into());
        self.emit_binary(destination, operation, lhs, rhs);
        Operand::Place(destination)
    }

    #[inline]
    fn emit_binary(
        &mut self,
        dest: Place<'hir>,
        operation: BinaryOperator,
        lhs: Operand<'hir>,
        rhs: Operand<'hir>,
    ) {
        self.emit(
            dest,
            InstructionKind::Binary { operation, lhs, rhs, overflow: OverflowMode::Unchecked },
        );
    }

    fn lower_expr(&mut self, expr: &'hir Expression<'hir>) -> Result<Operand<'hir>, MirError> {
        let outer = std::mem::replace(&mut self.build.span, expr.span);
        let lowered = self.lower_expr_inner(expr);
        self.build.span = outer;

        lowered
    }

    fn lower_expr_inner(
        &mut self,
        expr: &'hir Expression<'hir>,
    ) -> Result<Operand<'hir>, MirError> {
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
                        let s = self.context.symbols.get(*sym);
                        let id = self.strings.intern(s);
                        Operand::Const(Const::Str(id))
                    },
                })
            },

            ExpressionKind::Local(local_id) => {
                Ok(Operand::Place(self.place_for_local(*local_id, typ)))
            },

            // the constant's value tree lives in its own ExprId space, so its
            // typeck is swapped in for the duration of the subtree
            ExpressionKind::Const(constant) => {
                let outer = std::mem::replace(&mut self.typeck, &constant.typeck);
                let value = self.lower_expr(constant.value);
                self.typeck = outer;
                value
            },

            ExpressionKind::ParamConst { .. } => {
                unreachable!("generic associated constant must be resolved before MIR lowering")
            },

            ExpressionKind::Static(id) => {
                let address = self.static_address(*id, typ);
                let dest = self.fresh_temporary(typ);

                self.emit(dest, Kind::FieldLoad { src: Operand::Place(address), offset: 0, typ });

                Ok(Operand::Place(dest))
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

                let is_ref = matches!(operator, UnaryOperator::Ref | UnaryOperator::RefMut);

                // `&base[i]` / `&mut base[i]` takes the element's address rather than
                // loading its value, so it never goes through the value-producing path
                if is_ref && let ExpressionKind::Index { base, index } = &inner.kind {
                    let base_type = self.typeck.type_of(base.id);
                    let (base, bound, _, stride) = self.index_operands(base, base_type)?;
                    let index = self.lower_expr(index)?;
                    let dest = self.fresh_temporary(typ);
                    self.emit(dest, Kind::ElementAddr { base, index, bound, stride });

                    return Ok(Operand::Place(dest));
                }

                // `&*p` / `&mut *p` is the address `p` already holds, so it reborrows rather
                // than loads: materialising the pointee into a temporary and addressing that
                // would hand back a pointer into the current frame
                let deref_pointee = match &inner.kind {
                    ExpressionKind::Unary { operator: UnaryOperator::Deref, expr } => Some(expr),
                    _ => None,
                };

                if is_ref && let Some(pointee) = deref_pointee {
                    return self.lower_expr(pointee);
                }

                // loading the place into a temporary first would address that copy, which dies with the frame
                if is_ref
                    && !typ.is_slice()
                    && let Some(place) = self.place_address(inner)?
                {
                    return Ok(Operand::Place(place));
                }

                if let (UnaryOperator::Neg, ExpressionKind::Literal(hir::Literal::Int(value))) =
                    (operator, &inner.kind)
                {
                    return Ok(Operand::Const(Const::Int(value.wrapping_neg(), typ)));
                }

                let rhs = self.lower_expr(inner)?;
                let dest = self.fresh_temporary(temp_value_type(typ));

                match operator {
                    UnaryOperator::Deref => {
                        self.emit(dest, Kind::FieldLoad { src: rhs, offset: 0, typ })
                    },
                    UnaryOperator::Ref | UnaryOperator::RefMut => {
                        let src = match rhs {
                            Operand::Place(place) => place,
                            Operand::Const(_) => unreachable!("cannot take address of constant"),
                        };

                        match typ.is_slice() {
                            // `&array` builds a (ptr, len) fat pointer
                            true => {
                                let (_, _, len) = self.array_info(self.typeck.type_of(inner.id));
                                let pointer =
                                    self.context.types.refer(self.context.types.common.u8, false);
                                let ptr = self.fresh_temporary(pointer);
                                self.emit(ptr, Kind::AddressOf { src, offset: 0 });

                                self.emit(
                                    dest,
                                    Kind::FieldStore { value: Operand::Place(ptr), offset: 0 },
                                );
                                self.emit(
                                    dest,
                                    Kind::FieldStore {
                                        value: Operand::Const(Const::Int(
                                            len as i64,
                                            TypeKind::Uptr.into(),
                                        )),
                                        offset: 8,
                                    },
                                );
                            },
                            false => self.emit(dest, Kind::AddressOf { src, offset: 0 }),
                        }
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

                    let lhs = self.lower_call_argument(left, self_typ)?;
                    let rhs = self.lower_call_argument(right, other_typ)?;

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
                let overflow = match is_integer && is_arithmetic && optimisation::is_debug() {
                    true => OverflowMode::Checked,
                    _ => OverflowMode::Unchecked,
                };

                self.emit(dest, Kind::Binary { operation: *operator, lhs, rhs, overflow });

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Assign { target, value } => {
                let target = *target;
                let value_expr = *value;

                if let ExpressionKind::Index { base, index } = &target.kind {
                    let base_type = self.typeck.type_of(base.id);
                    let (base, bound, _, stride) = self.index_operands(base, base_type)?;
                    let index = self.lower_expr(index)?;
                    let value = self.lower_expr(value_expr)?;
                    let base = match base {
                        Operand::Place(place) => place,
                        Operand::Const(_) => unreachable!("indexing a constant aggregate"),
                    };

                    self.emit(base, Kind::ElementStore { index, bound, value, stride });

                    return Ok(value);
                }

                if let ExpressionKind::Static(id) = &target.kind {
                    let target_type = self.typeck.type_of(target.id);
                    let address = self.static_address(*id, target_type);
                    let value = self.lower_expr(value_expr)?;

                    self.emit(address, Kind::FieldStore { value, offset: 0 });

                    return Ok(value);
                }

                if let ExpressionKind::Unary { operator: UnaryOperator::Deref, expr } = &target.kind
                {
                    let pointer = self.lower_expr(expr)?;
                    let value = self.lower_expr(value_expr)?;
                    let Operand::Place(dest) = pointer else {
                        unreachable!("dereferencing a constant");
                    };

                    self.emit(dest, Kind::FieldStore { value, offset: 0 });

                    return Ok(value);
                }

                if let ExpressionKind::Local(local) = &target.kind {
                    let constant = self.constants.capture(value_expr);
                    self.constants.record(*local, constant);
                }

                let src = self.lower_expr(value_expr)?;
                let (dest, offset, _) = self.place_parts(target)?;

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

            ExpressionKind::CompoundAssign { target, operator, value } => {
                let (target, value_expr, operator) = (*target, *value, *operator);
                let target_type = self.typeck.type_of(target.id);

                if let ExpressionKind::Index { base, index } = &target.kind {
                    let base_type = self.typeck.type_of(base.id);
                    let (base, bound, _, stride) = self.index_operands(base, base_type)?;
                    let index = self.lower_expr(index)?;
                    let Operand::Place(base_place) = base else {
                        unreachable!("indexing a constant aggregate");
                    };

                    let old = self.fresh_temporary(temp_value_type(target_type));
                    let instr = Kind::ElementLoad { base, index, bound, stride, typ: target_type };
                    self.emit(old, instr);

                    let updated = self.combine(operator, Operand::Place(old), value_expr, typ)?;
                    let instr = Kind::ElementStore { index, bound, value: updated, stride };
                    self.emit(base_place, instr);

                    return Ok(updated);
                }

                if let ExpressionKind::Static(id) = &target.kind {
                    let address = self.static_address(*id, target_type);

                    let old = self.fresh_temporary(temp_value_type(target_type));
                    let src = Operand::Place(address);
                    let instr = Kind::FieldLoad { src, offset: 0, typ: target_type };
                    self.emit(old, instr);

                    let updated = self.combine(operator, Operand::Place(old), value_expr, typ)?;
                    self.emit(address, Kind::FieldStore { value: updated, offset: 0 });

                    return Ok(updated);
                }

                if let ExpressionKind::Unary { operator: UnaryOperator::Deref, expr } = &target.kind
                {
                    let Operand::Place(pointer) = self.lower_expr(expr)? else {
                        unreachable!("dereferencing a constant");
                    };

                    let old = self.fresh_temporary(temp_value_type(target_type));
                    let src = Operand::Place(pointer);
                    let instr = Kind::FieldLoad { src, offset: 0, typ: target_type };
                    self.emit(old, instr);

                    let updated = self.combine(operator, Operand::Place(old), value_expr, typ)?;
                    self.emit(pointer, Kind::FieldStore { value: updated, offset: 0 });

                    return Ok(updated);
                }

                let (dest, offset, _) = self.place_parts(target)?;
                match &target.kind {
                    ExpressionKind::Local(local) => {
                        // a local that never escapes to the runtime was folded away,
                        // so its updated value is handed straight back
                        let updated =
                            self.combine(operator, Operand::Place(dest), value_expr, typ)?;
                        self.constants.clear(*local);

                        match self.runtime_local_uses(*local) {
                            true => {
                                self.emit(dest, Kind::Assign(updated));
                                Ok(Operand::Place(dest))
                            },
                            false => Ok(updated),
                        }
                    },
                    ExpressionKind::Field { .. } => {
                        let old = self.fresh_temporary(temp_value_type(target_type));
                        let instr =
                            Kind::FieldLoad { src: Operand::Place(dest), offset, typ: target_type };
                        self.emit(old, instr);

                        let updated =
                            self.combine(operator, Operand::Place(old), value_expr, typ)?;
                        self.emit(dest, Kind::FieldStore { value: updated, offset });

                        Ok(updated)
                    },
                    _ => unreachable!("invalid compound assignment target in MIR lowering"),
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
                    hir::Res::Intrinsic(intrinsic) => {
                        use crate::hir::Intrinsic as I;

                        match intrinsic {
                            I::PrintLn | I::Print => {
                                let mut pending = String::new();

                                for arg in *args {
                                    match self.constant_text(arg) {
                                        Some(text) => pending.push_str(&text),
                                        _ => {
                                            self.builtin().flush_text(&mut pending);
                                            self.emit_write_value(arg)?;
                                        },
                                    }
                                }

                                if intrinsic == I::PrintLn {
                                    pending.push('\n');
                                }
                                self.builtin().flush_text(&mut pending);

                                Ok(Operand::Const(Const::Unit))
                            },
                            I::Syscall => unreachable!("syscall must carry Res::Syscall"),
                            I::Len => match self.lower_expr(args[0])? {
                                Operand::Const(Const::Str(id)) => Ok(Operand::Const(Const::Int(
                                    self.strings.len_of(id) as i64,
                                    typ,
                                ))),
                                Operand::Place(place) => {
                                    let dest = self.fresh_temporary(typ);
                                    let instr = Kind::FieldLoad {
                                        src: Operand::Place(place),
                                        offset: 8,
                                        typ,
                                    };
                                    self.emit(dest, instr);
                                    Ok(Operand::Place(dest))
                                },
                                other => unreachable!("str length of a non-str operand: {other:?}"),
                            },
                            I::WrappingAdd | I::WrappingSub | I::WrappingMul => {
                                let operation = intrinsic
                                    .binary_operator()
                                    .expect("wrapping intrinsic must map to a binary operator");
                                let (lhs, rhs) =
                                    (self.lower_expr(args[0])?, self.lower_expr(args[1])?);
                                let dest = self.fresh_temporary(typ);
                                let overflow = OverflowMode::Wrapping;

                                let op = Kind::Binary { operation, lhs, rhs, overflow };
                                self.emit(dest, op);
                                Ok(Operand::Place(dest))
                            },
                        }
                    },
                    hir::Res::Syscall(code) => {
                        let lowered_args = args
                            .iter()
                            .map(|arg| self.lower_expr(arg))
                            .collect::<Result<Vec<_>, _>>()?;
                        let dest = self.fresh_temporary(typ);
                        self.emit(dest, Kind::Syscall { code, args: lowered_args, returns: true });
                        Ok(Operand::Place(dest))
                    },
                    hir::Res::ParamMethod { .. } | hir::Res::ParamFunction { .. } => unreachable!(
                        "generic interface dispatch must be resolved before MIR lowering"
                    ),
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

                let place = self.lower_call_argument(receiver, receiver_typ)?;

                let mut lowered_args = Vec::with_capacity(args.len() + 1);
                lowered_args.push(Operand::Place(place));
                for arg in *args {
                    let operand = self.lower_expr(arg)?;
                    lowered_args.push(operand);
                }

                self.emit_call(function, lowered_args, typ)
            },

            ExpressionKind::TypeIntrinsic { kind, typ: target } => {
                let LoweringContext { types, adts, arrays, .. } = self.context;
                let (size, align) = hir::type_layout(*target, types, adts, arrays);
                let value = match kind {
                    TypeIntrinsicKind::SizeOf => size as i64,
                    TypeIntrinsicKind::AlignOf => align as i64,
                };

                Ok(Operand::Const(Const::Int(value, typ)))
            },

            ExpressionKind::Struct { fields, .. } => {
                let dest = self.fresh_temporary(typ);

                for (sym, value) in *fields {
                    let LoweringContext { types, adts, arrays, .. } = self.context;
                    let layout = hir::struct_field(typ, *sym, types, adts, arrays);
                    let value_operand = self.lower_expr(value)?;

                    let instr = Kind::FieldStore { value: value_operand, offset: layout.offset };
                    self.emit(dest, instr);
                }

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Field { .. } => {
                let (origin, offset, typ) = self.place_parts(expr)?;
                let dest = self.fresh_temporary(typ);
                self.emit(dest, Kind::FieldLoad { src: Operand::Place(origin), offset, typ });
                Ok(Operand::Place(dest))
            },

            ExpressionKind::Array { elements } => {
                let (_, elem_size, _) = self.array_info(typ);
                let dest = self.fresh_temporary(typ);

                for (index, element) in elements.iter().enumerate() {
                    let value = self.lower_expr(element)?;
                    let offset = index as u32 * elem_size;
                    self.emit(dest, Kind::FieldStore { value, offset });
                }

                Ok(Operand::Place(dest))
            },

            ExpressionKind::ArrayRepeat { value, count } => {
                let (_, elem_size, _) = self.array_info(typ);
                let dest = self.fresh_temporary(typ);
                let value = self.lower_expr(value)?;

                for index in 0..*count {
                    self.emit(dest, Kind::FieldStore { value, offset: index * elem_size });
                }

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Index { base, index } => {
                let base_type = self.typeck.type_of(base.id);
                let (base, bound, element, stride) = self.index_operands(base, base_type)?;
                let index = self.lower_expr(index)?;
                let dest = self.fresh_temporary(element);

                self.emit(dest, Kind::ElementLoad { base, index, bound, stride, typ: element });

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Block { statements, tail } => {
                for statement in *statements {
                    if self.build.is_terminated() {
                        break;
                    }

                    self.lower_statement(statement)?;
                }

                match tail {
                    Some(tail) if !self.build.is_terminated() => self.lower_expr(tail),
                    _ => Ok(Operand::Const(Const::Unit)),
                }
            },

            ExpressionKind::If { condition, then_block, else_block } => {
                let condition = self.lower_expr(condition)?;

                let (then, else_id, merge) = (self.new_block(), self.new_block(), self.new_block());
                let result = (typ.kind() != TypeKind::Unit).then(|| self.fresh_temporary(typ));

                self.terminate(Terminator::Branch {
                    condition,
                    then_block: then,
                    else_block: else_id,
                });

                // a branch that leaves the function never reaches the merge, so it contributes no value to unify
                let branch = |this: &mut Self, block: &'hir Expression<'hir>| {
                    let value = this.lower_expr(block)?;
                    Ok::<_, MirError>(if !this.build.is_terminated() {
                        if let Some(place) = result {
                            this.emit(place, Kind::Assign(value));
                        }
                        this.terminate(Terminator::Jump(merge));
                    })
                };

                self.switch_to(then);
                branch(self, then_block)?;

                self.switch_to(else_id);
                match else_block {
                    Some(else_block) => branch(self, else_block)?,
                    _ => self.terminate(Terminator::Jump(merge)),
                }

                self.switch_to(merge);

                Ok(match result {
                    Some(place) => Operand::Place(place),
                    _ => Operand::Const(Const::Unit),
                })
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

                    match arm.body {
                        hir::ArmBody::Expr(body) => {
                            let body_operand = self.lower_expr(body)?;
                            if let Some(res_place) = match_result_place {
                                self.emit(res_place, Kind::Assign(body_operand));
                            }
                            self.terminate(Terminator::Jump(join_block));
                        },

                        // an arm that transfers control never reaches the join, so it terminates its own block and contributes no value
                        hir::ArmBody::Return(value) => {
                            let operand = value.map(|value| self.lower_expr(value)).transpose()?;
                            match self.inlined_return_target {
                                Some((exit_block, return_place)) => {
                                    if let (Some(operand), Some(dest)) = (operand, return_place) {
                                        self.emit(dest, Kind::Assign(operand));
                                    }
                                    self.terminate(Terminator::Jump(exit_block));
                                },
                                _ => self.terminate(Terminator::Return(operand)),
                            }
                        },

                        hir::ArmBody::Break => {
                            let target = self
                                .build
                                .loop_targets
                                .last()
                                .expect("break without an enclosing loop");
                            self.terminate(Terminator::Jump(target.break_target));
                        },

                        hir::ArmBody::Continue => {
                            let target = self
                                .build
                                .loop_targets
                                .last()
                                .expect("continue without an enclosing loop");
                            self.terminate(Terminator::Jump(target.continue_target));
                        },
                    }
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
        left: &'hir Expression<'hir>,
        right: &'hir Expression<'hir>,
        typ: Type<'hir>,
    ) -> Result<Operand<'hir>, MirError> {
        debug_assert_eq!(
            typ,
            TypeKind::Bool.into(),
            "`&&` and `||` should be perfomed only on booleans"
        );

        let result = self.fresh_temporary(TypeKind::Bool.into());
        let left_operand = self.lower_expr(left)?;

        let right_id = self.new_block();
        let short_id = self.new_block();
        let merge_id = self.new_block();

        let (then_block, else_block, short_value) = match operator {
            BinaryOperator::And => (right_id, short_id, false),
            BinaryOperator::Or => (short_id, right_id, true),
            _ => unreachable!("lower_short_circuit called with non-short-circuiting operator"),
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

    fn lower_call_argument(
        &mut self,
        left: &'hir Expression,
        typ: Type<'hir>,
    ) -> Result<Place<'hir>, MirError> {
        let place = self.fresh_temporary(typ);

        if let Some(array_id) = self.array_coerced_to_slice(left, typ) {
            let TypeKind::Slice { mutable, .. } = typ.kind() else {
                unreachable!("array_coerced_to_slice only returns Some for slice targets")
            };
            let (_, _, len) = self.array_info(self.context.types.array(array_id));
            let src = match self.is_place_expr(left) {
                true => {
                    let (origin, offset, _) = self.place_info(left);
                    let ptr = self.fresh_temporary(
                        self.context.types.refer(self.context.types.common.u8, mutable),
                    );
                    self.emit(ptr, InstructionKind::AddressOf { src: origin, offset });
                    ptr
                },
                false => {
                    let lowered = self.lower_expr(left)?;
                    let value = self.fresh_temporary(self.typeck.type_of(left.id));
                    self.emit(value, InstructionKind::Assign(lowered));

                    let ptr = self.fresh_temporary(
                        self.context.types.refer(self.context.types.common.u8, mutable),
                    );
                    self.emit(ptr, InstructionKind::AddressOf { src: value, offset: 0 });
                    ptr
                },
            };

            self.emit(place, InstructionKind::FieldStore { value: Operand::Place(src), offset: 0 });

            let value = Operand::Const(Const::Int(len as i64, TypeKind::Uptr.into()));
            let instr = InstructionKind::FieldStore { value, offset: 8 };
            self.emit(place, instr);

            return Ok(place);
        }

        match self.is_place_expr(left) {
            true => {
                let (origin, offset, typ) = self.place_info(left);
                debug_assert!(typ.kind() != TypeKind::Unit);

                // a slice receiver is the fat pointer itself, passed by value; other
                // aggregates (`&self` on a struct) are passed as a pointer to the storage
                let instr = match (offset, typ.is_ref() || typ.is_slice()) {
                    (0, true) => InstructionKind::Assign(Operand::Place(origin)),
                    (_, true) => {
                        InstructionKind::FieldLoad { src: Operand::Place(origin), offset, typ }
                    },
                    _ => InstructionKind::AddressOf { src: origin, offset },
                };

                self.emit(place, instr)
            },
            _ => {
                if let Some(address) = self.place_address(left)? {
                    self.emit(place, InstructionKind::Assign(Operand::Place(address)));
                    return Ok(place);
                }

                let val_type = self.typeck.type_of(left.id);
                let lowered = self.lower_expr(left)?;

                match val_type.is_ref() || val_type.is_slice() {
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

    #[inline]
    fn array_coerced_to_slice(
        &self,
        expr: &'hir Expression,
        target: Type<'hir>,
    ) -> Option<hir::ArrayId> {
        let TypeKind::Slice { element, .. } = target.kind() else {
            return None;
        };
        let TypeKind::Array(id) = self.typeck.type_of(expr.id).kind() else {
            return None;
        };
        (self.context.arrays.get(id).element == element.into()).then_some(id)
    }

    #[inline(always)]
    fn local_type(&self, id: LocalId) -> Type<'hir> {
        self.build.locals[self.local_map[id].0 as usize].1
    }

    #[inline(always)]
    fn is_place_expr(&self, expr: &Expression<'hir>) -> bool {
        match &expr.kind {
            ExpressionKind::Local(_) => true,
            ExpressionKind::Field { base, .. } => self.is_place_expr(base),
            _ => false,
        }
    }

    /// A temporary holding the address of `id`, typed as a mutable raw pointer
    /// so the LIR picks pointer addressing rather than a frame-slot offset
    fn static_address(&mut self, id: hir::StaticId, typ: Type<'hir>) -> Place<'hir> {
        let dest = self.fresh_temporary(self.context.types.raw(typ, true));

        self.emit(dest, InstructionKind::StaticAddr { id });

        dest
    }

    fn place_info(&self, expr: &Expression<'hir>) -> (Place<'hir>, u32, Type<'hir>) {
        match &expr.kind {
            ExpressionKind::Local(local_id) => {
                let origin = self.place_for_local(*local_id, self.local_type(*local_id));
                (origin, 0, origin.typ)
            },
            ExpressionKind::Field { base, field } => {
                let (origin, base_offset, base_type) = self.place_info(base);
                let LoweringContext { types, adts, arrays, .. } = self.context;
                let layout = hir::struct_field(base_type, *field, types, adts, arrays);
                (origin, base_offset + layout.offset, layout.typ)
            },
            _ => panic!("place_info called on non-place expression: {:?}", expr),
        }
    }

    /// [Self::place_info] over any place, including ones reached dynamically
    fn combine(
        &mut self,
        operator: BinaryOperator,
        old: Operand<'hir>,
        value: &'hir Expression<'hir>,
        typ: Type<'hir>,
    ) -> Result<Operand<'hir>, MirError> {
        let rhs = self.lower_expr(value)?;
        let dest = self.fresh_temporary(temp_value_type(typ));

        let is_arithmetic =
            matches!(operator, BinaryOperator::Add | BinaryOperator::Sub | BinaryOperator::Mul);
        let overflow = match typ.is_integer() && is_arithmetic && optimisation::is_debug() {
            true => OverflowMode::Checked,
            _ => OverflowMode::Unchecked,
        };

        self.emit(dest, InstructionKind::Binary { operation: operator, lhs: old, rhs, overflow });

        Ok(Operand::Place(dest))
    }

    fn place_parts(
        &mut self,
        expr: &'hir Expression<'hir>,
    ) -> Result<(Place<'hir>, u32, Type<'hir>), MirError> {
        match &expr.kind {
            ExpressionKind::Local(_) => Ok(self.place_info(expr)),

            ExpressionKind::Field { base, field } => {
                let (origin, base_offset, base_type) = self.place_parts(base)?;
                let LoweringContext { types, adts, arrays, .. } = self.context;
                let layout = hir::struct_field(base_type, *field, types, adts, arrays);

                Ok((origin, base_offset + layout.offset, layout.typ))
            },

            _ => {
                let address = self
                    .place_address(expr)?
                    .expect("place_parts called on a non-place expression");

                Ok((address, 0, self.typeck.type_of(expr.id)))
            },
        }
    }

    /// `(element, element_size, length)` of a fixed-size array type
    fn array_info(&self, array_type: Type<'hir>) -> (Type<'hir>, u32, u32) {
        match array_type.kind() {
            TypeKind::Array(id) => {
                let context = self.context;
                let array = context.arrays.get(id);
                let (size, _) =
                    hir::type_layout(array.element, context.types, context.adts, context.arrays);
                (array.element, size, array.len)
            },
            _ => unreachable!("array_info on a non-array type"),
        }
    }

    /// `(element, element_size)` of an indexable type (array or slice)
    fn element_info(&self, typ: Type<'hir>) -> (Type<'hir>, u32) {
        let element = match typ.kind() {
            TypeKind::Array(id) => self.context.arrays.get(id).element,
            TypeKind::Slice { element, .. } => element.into(),
            _ => unreachable!("element_info on a non-indexable type: {typ}"),
        };
        let LoweringContext { types, arrays, adts, .. } = self.context;
        let (stride, _) = hir::type_layout(element, types, adts, arrays);
        (element, stride)
    }

    fn place_address(
        &mut self,
        expr: &'hir Expression<'hir>,
    ) -> Result<Option<Place<'hir>>, MirError> {
        let pointer = self.context.types.refer(self.context.types.common.u8, true);

        match &expr.kind {
            ExpressionKind::Local(_) | ExpressionKind::Field { .. } => {
                let (origin, offset, _) = self.place_parts(expr)?;
                let dest = self.fresh_temporary(pointer);
                self.emit(dest, InstructionKind::AddressOf { src: origin, offset });

                Ok(Some(dest))
            },

            ExpressionKind::Index { base, index } => {
                let base_type = self.typeck.type_of(base.id);
                let (base, bound, _, stride) = self.index_operands(base, base_type)?;
                let index = self.lower_expr(index)?;
                let dest = self.fresh_temporary(pointer);
                self.emit(dest, InstructionKind::ElementAddr { base, index, bound, stride });

                Ok(Some(dest))
            },

            ExpressionKind::Unary { operator: UnaryOperator::Deref, expr } => {
                match self.lower_expr(expr)? {
                    Operand::Place(place) => Ok(Some(place)),
                    Operand::Const(_) => Ok(None),
                }
            },

            _ => Ok(None),
        }
    }

    fn array_base_place(
        &mut self,
        base: &'hir Expression<'hir>,
    ) -> Result<Option<Place<'hir>>, MirError> {
        if self.is_place_expr(base) {
            let (origin, offset, _) = self.place_info(base);
            if offset == 0 && !origin.typ.is_pointer() {
                return Ok(Some(origin));
            }
        }

        self.place_address(base)
    }

    fn index_operands(
        &mut self,
        base: &'hir Expression<'hir>,
        base_type: Type<'hir>,
    ) -> Result<(Operand<'hir>, Operand<'hir>, Type<'hir>, u32), MirError> {
        let (element, stride) = self.element_info(base_type);

        match base_type.kind() {
            TypeKind::Slice { .. } => {
                let slice = match self.lower_expr(base)? {
                    Operand::Place(place) => place,
                    Operand::Const(_) => unreachable!("indexing a constant slice"),
                };
                let pointer = self.context.types.refer(self.context.types.common.u8, false);
                let ptr = self.fresh_temporary(pointer);
                let instr = InstructionKind::FieldLoad {
                    src: Operand::Place(slice),
                    offset: 0,
                    typ: pointer,
                };
                self.emit(ptr, instr);

                let len = self.fresh_temporary(TypeKind::Uptr.into());
                let instr = InstructionKind::FieldLoad {
                    src: Operand::Place(slice),
                    offset: 8,
                    typ: TypeKind::Uptr.into(),
                };
                self.emit(len, instr);
                Ok((Operand::Place(ptr), Operand::Place(len), element, stride))
            },
            _ => {
                let (_, _, len) = self.array_info(base_type);
                let bound = Operand::Const(Const::Int(len as i64, TypeKind::Uptr.into()));
                let base = match self.array_base_place(base)? {
                    Some(place) => Operand::Place(place),
                    None => self.lower_expr(base)?,
                };

                Ok((base, bound, element, stride))
            },
        }
    }

    fn terminate(&mut self, term: Terminator<'hir>) {
        self.build.terminate(term);
    }

    #[inline(always)]
    fn place_for_local(&self, local_id: LocalId, typ: Type<'hir>) -> Place<'hir> {
        Place { id: self.local_map[local_id], typ }
    }

    #[inline(always)]
    fn fresh_temporary(&mut self, typ: Type<'hir>) -> Place<'hir> {
        self.build.fresh_temporary(typ)
    }

    #[inline(always)]
    fn switch_to(&mut self, id: BlockId) {
        self.build.switch_to(id);
    }

    fn lower_pattern_match(
        &mut self,
        place: Place<'hir>,
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
            PatternKind::Bind { local, sub } => {
                let local_typ = self.local_type(*local);
                let dest_place = self.place_for_local(*local, local_typ);
                self.emit(dest_place, Kind::Assign(Operand::Place(place)));
                self.lower_pattern_match(place, sub, success_block, fail_block)
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
                self.emit_eq_branch(Operand::Place(place), rhs, success_block, fail_block);
                Ok(())
            },
            PatternKind::Range { start, end, inclusive } => {
                use hir::Literal as L;
                let place_typ = place.typ;
                let as_const = |lit: &L| match lit {
                    L::Int(n) => Const::Int(*n, place_typ),
                    L::Char(c) => Const::Int(*c as i64, place_typ),
                    _ => unreachable!("HIR only lowers integer and char range endpoints"),
                };
                let (start, end) = (as_const(start), as_const(end));

                let upper_check = self.new_block();
                self.emit_cmp_branch(
                    BinaryOperator::GtEq,
                    Operand::Place(place),
                    start,
                    upper_check,
                    fail_block,
                );

                self.switch_to(upper_check);
                let upper_op = match inclusive {
                    true => BinaryOperator::LtEq,
                    false => BinaryOperator::Lt,
                };
                self.emit_cmp_branch(
                    upper_op,
                    Operand::Place(place),
                    end,
                    success_block,
                    fail_block,
                );
                Ok(())
            },
            PatternKind::Struct { fields, .. } => {
                let n = fields.len();
                if n == 0 {
                    self.terminate(Terminator::Jump(success_block));
                    return Ok(());
                }

                for (i, (field, sub)) in fields.iter().enumerate() {
                    let LoweringContext { types, adts, arrays, .. } = self.context;
                    let layout = hir::struct_field(place.typ, *field, types, adts, arrays);
                    let (offset, typ) = (layout.offset, layout.typ);

                    let field_place = self.fresh_temporary(typ);
                    self.emit(
                        field_place,
                        Kind::FieldLoad { src: Operand::Place(place), offset, typ },
                    );

                    let next = match i + 1 < n {
                        true => self.new_block(),
                        false => success_block,
                    };
                    self.lower_pattern_match(field_place, sub, next, fail_block)?;
                    if i + 1 < n {
                        self.switch_to(next);
                    }
                }
                Ok(())
            },
            PatternKind::Or(alternatives) => {
                let mut check = self.build.current_block_id();
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
                let enum_def = &self.context.adts[*enum_id];
                let variant = enum_def.variants()[*variant_idx];
                let tag_val = variant.value;
                let tag_ty = enum_def.enum_repr().typ();

                // load discriminant tag from offset 0
                let tag_place = self.fresh_temporary(tag_ty);
                let instr = Kind::FieldLoad { src: Operand::Place(place), offset: 0, typ: tag_ty };
                self.emit(tag_place, instr);
                let tag_const = Const::Int(tag_val, tag_ty);
                let tag_place = Operand::Place(tag_place);

                match sub {
                    // first branch on the tag: a match continues into `sub_block`,
                    // a mismatch falls through to `fail_block`
                    Some(sub_pat) => {
                        let sub_block = self.new_block();
                        self.emit_eq_branch(tag_place, tag_const, sub_block, fail_block);
                        self.switch_to(sub_block);

                        let LoweringContext { types, adts, arrays, .. } = self.context;
                        let offset = hir::enum_payload_offset(place.typ, types, adts, arrays);

                        let typ = variant
                            .payload
                            .expect("variant must have payload type since it has subpattern");
                        let args = match place.typ.kind() {
                            TypeKind::Adt(_, args) => args,
                            TypeKind::Ref { to, .. } => match to.kind() {
                                TypeKind::Adt(_, args) => args,
                                _ => unreachable!("variant pattern place must be an enum"),
                            },
                            _ => unreachable!("variant pattern place must be an enum"),
                        };
                        let typ = typ.subst(self.context.types, self.context.arrays, args);
                        let payload_place = self.fresh_temporary(typ);
                        let instr = Kind::FieldLoad { src: Operand::Place(place), offset, typ };
                        self.emit(payload_place, instr);
                        self.lower_pattern_match(
                            payload_place,
                            sub_pat,
                            success_block,
                            fail_block,
                        )?;
                    },
                    // just check the tag
                    None => self.emit_eq_branch(tag_place, tag_const, success_block, fail_block),
                }
                Ok(())
            },
        }
    }

    /// emit `cond = lhs == rhs`, then branch to `then_block` if `cond` is true, otherwise to `else_block`
    #[inline]
    fn emit_eq_branch(
        &mut self,
        lhs: Operand<'hir>,
        rhs: Const<'hir>,
        then_block: BlockId,
        else_block: BlockId,
    ) {
        self.emit_cmp_branch(BinaryOperator::Eq, lhs, rhs, then_block, else_block);
    }

    /// emit `cond = lhs <op> rhs`, then branch to `then_block` if `cond` is true, otherwise to `else_block`
    fn emit_cmp_branch(
        &mut self,
        operation: BinaryOperator,
        lhs: Operand<'hir>,
        rhs: Const<'hir>,
        then_block: BlockId,
        else_block: BlockId,
    ) {
        let cond = self.fresh_temporary(TypeKind::Bool.into());
        let rhs = Operand::Const(rhs);
        let overflow = OverflowMode::Unchecked;
        let instr = InstructionKind::Binary { operation, lhs, rhs, overflow };

        self.emit(cond, instr);
        let condition = Operand::Place(cond);
        self.terminate(Terminator::Branch { condition, then_block, else_block });
    }

    #[inline(always)]
    fn emit(&mut self, dest: Place<'hir>, kind: InstructionKind<'hir>) {
        self.build.emit(dest, kind);
    }

    fn builtin(&mut self) -> BuiltinLowering<'_, 'a, 'hir> {
        BuiltinLowering::new(self.context, &mut self.build, self.strings)
    }

    fn emit_write_value(&mut self, expr: &'hir Expression<'hir>) -> Result<(), MirError> {
        let typ = self.typeck.type_of(expr.id);
        let kind = hir::lang::print_kind(typ).expect("HIR rejects an interpolated value");
        let operand = self.lower_expr(expr)?;

        self.builtin().emit_value(kind, operand);

        Ok(())
    }
    #[inline]
    fn constant_text(&self, expr: &Expression<'hir>) -> Option<String> {
        hir::lang::print_kind(self.typeck.type_of(expr.id))?;
        self.constants.capture(expr)
    }

    #[inline]
    fn get_fn_unchecked(&self, id: &FunctionId) -> &'a hir::Function<'hir> {
        self.context
            .functions
            .get(*id)
            .unwrap_or_else(|| panic!("callee function {:?} not found", id))
    }

    fn emit_variant(
        &mut self,
        id: hir::AdtId,
        index: usize,
        payload: Option<&'hir Expression<'hir>>,
        typ: Type<'hir>,
    ) -> Result<Operand<'hir>, MirError> {
        let dest = self.fresh_temporary(typ);

        let tag_ty = self.context.adts[id].enum_repr().typ();
        let tag = self.context.adts[id].variants()[index].value;
        self.emit(
            dest,
            InstructionKind::FieldStore {
                value: Operand::Const(Const::Int(tag, tag_ty)),
                offset: 0,
            },
        );

        if let Some(payload) = payload {
            let context = self.context;
            let offset = hir::enum_payload_offset(typ, context.types, context.adts, context.arrays);
            let value = self.lower_expr(payload)?;
            self.emit(dest, InstructionKind::FieldStore { value, offset });
        }

        Ok(Operand::Place(dest))
    }

    fn emit_call(
        &mut self,
        callee_id: FunctionId,
        lowered_args: Vec<Operand<'hir>>,
        return_type: Type<'hir>,
    ) -> Result<Operand<'hir>, MirError> {
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
        lowered_args: Vec<Operand<'hir>>,
    ) -> Result<Operand<'hir>, MirError> {
        let callee = &self.context.functions[callee_id];

        let inline_ret_place = match callee.return_type.kind() != TypeKind::Unit {
            true => Some(self.fresh_temporary(callee.return_type)),
            _ => None,
        };
        let exit_block_id = self.new_block();

        let callee_n_locals = callee.locals.len();
        let mut callee_local_map = IndexVec::from_elem(ValueId(0), callee_n_locals);
        for local in &callee.locals {
            assert!(
                local.typ.kind() != TypeKind::Unit,
                "internal error: inline callee {} has unit local {:?}",
                self.context.symbols.get(callee.name),
                local.id
            );
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

        if !self.build.is_terminated() {
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
        return_place: Option<Place<'hir>>,
    ) -> InlineContext<'a, 'hir> {
        use std::mem::replace;

        InlineContext {
            local_map: replace(&mut self.local_map, local_map),
            constants: replace(
                &mut self.constants,
                ConstantValues::new(self.context.symbols, callee.locals.len()),
            ),
            runtime_local_uses: replace(
                &mut self.runtime_local_uses,
                self.context.runtime_uses[callee.id].clone(),
            ),
            local_symbols: replace(
                &mut self.local_symbols,
                callee.locals.iter().map(|l| l.name).collect(),
            ),
            inlined_return_target: self
                .inlined_return_target
                .replace((exit_block_id, return_place)),
            typeck: replace(&mut self.typeck, &callee.typeck),
        }
    }

    fn restore_inline_context(&mut self, context: InlineContext<'a, 'hir>) {
        self.local_map = context.local_map;
        self.constants = context.constants;
        self.runtime_local_uses = context.runtime_local_uses;
        self.local_symbols = context.local_symbols;
        self.inlined_return_target = context.inlined_return_target;
        self.typeck = context.typeck;
    }

    #[inline(always)]
    fn runtime_local_uses(&self, id: LocalId) -> bool {
        self.runtime_local_uses.get(id).copied().unwrap_or(false)
    }

    #[inline(always)]
    fn new_block(&mut self) -> BlockId {
        self.build.new_block()
    }
}

impl<'hir> FunctionBuildState<'hir> {
    #[inline(always)]
    fn new_block(&mut self) -> BlockId {
        self.blocks.push(PartialBlock::new())
    }

    fn terminate(&mut self, terminator: Terminator<'hir>) {
        assert!(!self.is_terminated(), "double-termination of block {:?}", self.current);
        self.blocks[self.current].terminator = Some(terminator);
    }

    #[inline(always)]
    fn fresh_temporary(&mut self, typ: Type<'hir>) -> Place<'hir> {
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
        self.current = id;
    }

    #[inline(always)]
    const fn current_block_id(&self) -> BlockId {
        self.current
    }

    #[inline(always)]
    fn emit(&mut self, dest: Place<'hir>, kind: InstructionKind<'hir>) {
        self.blocks[self.current]
            .instructions
            .push(Instruction { dest, kind, span: self.span });
    }

    fn finish(self) -> (IndexVec<BlockId, Block<'hir>>, Vec<(ValueId, Type<'hir>)>) {
        let blocks = self.blocks.into_iter().map(PartialBlock::finalise).collect();
        (blocks, self.locals)
    }
}

impl<'hir> PartialBlock<'hir> {
    fn new() -> Self {
        Self { instructions: Vec::new(), terminator: None }
    }

    #[inline(always)]
    const fn is_terminated(&self) -> bool {
        self.terminator.is_some()
    }

    fn finalise(self) -> Block<'hir> {
        Block {
            instructions: self.instructions,
            terminator: self.terminator.expect("block missing terminator"),
        }
    }
}

fn has_open_generic(func: &hir::Function<'_>) -> bool {
    fn is_open(t: Type) -> bool {
        match t.kind() {
            TypeKind::GenericParam(_) => true,
            TypeKind::Ref { to, .. } | TypeKind::Raw { to, .. } => {
                matches!(to.kind(), TypeKind::GenericParam(_))
            },
            _ => false,
        }
    }

    is_open(func.return_type) || func.params.iter().any(|p| is_open(p.typ))
}
