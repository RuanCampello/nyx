//! HIR -> MIR lowering

use crate::{
    Span,
    hir::{
        self, Expression, ExpressionKind, FunctionId, Hir, Layout, LocalId, Statement, SymbolId,
        SymbolTable, TyInterner, Type, TypeKind, ids::IndexVec, lang::PrintKind,
    },
    mir::{
        self, Block, BlockId, Const, Function, Instruction, InstructionKind,
        InstructionKind as Kind, Mir, Operand, OverflowMode, Place, StringPool, Terminator,
        ValueId, error::MirError,
    },
    optimisation,
    parser::expression::{BinaryOperator, TypeIntrinsicKind, UnaryOperator},
};
use std::collections::HashMap;

struct FunctionLower<'a, 'hir> {
    blocks: Vec<PartialBlock<'hir>>,
    current: usize,
    next: u32,
    local_map: IndexVec<LocalId, ValueId>,
    locals: Vec<(ValueId, Type<'hir>)>,
    types: &'a TyInterner<'hir>,
    symbols: &'a SymbolTable,
    strings: &'a mut StringPool,
    adts: &'a IndexVec<hir::AdtId, hir::AdtDef<'hir>>,
    arrays: &'a hir::ArrayTable<'hir>,
    typeck: &'a hir::TypeckResults<'hir>,
    local_symbols: IndexVec<LocalId, SymbolId>,
    constant_locals: IndexVec<LocalId, Option<String>>,
    runtime_local_uses: IndexVec<LocalId, bool>,
    functions: &'a IndexVec<FunctionId, hir::Function<'hir>>,
    runtime_uses: &'a IndexVec<FunctionId, IndexVec<LocalId, bool>>,
    inlined_return_target: Option<(BlockId, Option<Place<'hir>>)>,
    loop_targets: Vec<LoopTargets>,
    span: Span,
}

struct InlineContext<'a, 'hir> {
    local_map: IndexVec<LocalId, ValueId>,
    constant_locals: IndexVec<LocalId, Option<String>>,
    runtime_local_uses: IndexVec<LocalId, bool>,
    local_symbols: IndexVec<LocalId, SymbolId>,
    inlined_return_target: Option<(BlockId, Option<Place<'hir>>)>,
    typeck: &'a hir::TypeckResults<'hir>,
}

struct PartialBlock<'hir> {
    id: BlockId,
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
    for f in &hir.functions {
        let id = runtime_uses.push(collect_runtime_local_uses(f));
        debug_assert_eq!(id, f.id);
    }

    for function in &hir.functions {
        functions.push(FunctionLower::run(
            function,
            types,
            &symbols,
            adts,
            arrays,
            &mut strings,
            &hir.functions,
            &runtime_uses,
        )?);
    }

    let mut adt_layouts = HashMap::new();
    for function in &functions {
        collect_adt_layout(function.return_type, types, adts, arrays, &mut adt_layouts);
        for &(_, typ) in &function.locals {
            collect_adt_layout(typ, types, adts, arrays, &mut adt_layouts);
        }
    }
    for item in hir.statics.iter() {
        collect_adt_layout(item.typ, types, adts, arrays, &mut adt_layouts);
    }

    // taken last: substituting a generic array field above may have minted
    // fresh entries in `arrays`, so this snapshot must see the final table
    let array_layouts = arrays
        .snapshot()
        .iter()
        .map(|array| {
            let (size, align) = hir::type_layout(array.element, types, adts, arrays);
            let contains_float = hir::type_contains_float(array.element, types, adts, arrays);
            Layout::new(size * array.len, align, contains_float)
        })
        .collect();

    Ok(Mir {
        types: hir.types,
        functions,
        symbols,
        strings,
        statics: hir.statics.iter().copied().collect(),
        layouts: adt_layouts,
        reprs: hir
            .adts
            .iter()
            .map(|adt| match adt.kind {
                hir::AdtKind::Enum { repr, .. } => Some(repr),
                hir::AdtKind::Struct { .. } => None,
            })
            .collect(),
        array_layouts,
    })
}

fn collect_adt_layout<'hir>(
    typ: Type<'hir>,
    types: &TyInterner<'hir>,
    adts: &IndexVec<hir::AdtId, hir::AdtDef<'hir>>,
    arrays: &hir::ArrayTable<'hir>,
    layouts: &mut HashMap<Type<'hir>, Layout>,
) {
    match typ.kind() {
        TypeKind::Adt(id, args) => {
            if layouts.contains_key(&typ) {
                return;
            }
            let (size, align) = hir::type_layout(typ, types, adts, arrays);
            let contains_float = hir::type_contains_float(typ, types, adts, arrays);
            layouts.insert(typ, Layout::new(size, align, contains_float));
            match &adts[id].kind {
                hir::AdtKind::Struct { fields, .. } => {
                    for field in fields {
                        collect_adt_layout(
                            field.typ.subst(types, arrays, args),
                            types,
                            adts,
                            arrays,
                            layouts,
                        );
                    }
                },
                hir::AdtKind::Enum { variants, .. } => {
                    for payload in variants.iter().filter_map(|variant| variant.payload) {
                        collect_adt_layout(
                            payload.subst(types, arrays, args),
                            types,
                            adts,
                            arrays,
                            layouts,
                        );
                    }
                },
            }
        },
        TypeKind::Array(id) => {
            collect_adt_layout(arrays.get(id).element, types, adts, arrays, layouts);
        },
        TypeKind::Ref { to, .. } | TypeKind::Raw { to, .. } => {
            collect_adt_layout(Type::from(to), types, adts, arrays, layouts);
        },
        TypeKind::Slice { element, .. } => {
            collect_adt_layout(Type::from(element), types, adts, arrays, layouts);
        },
        _ => {},
    }
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
        types: &'a TyInterner<'hir>,
        symbols: &'a SymbolTable,
        adts: &'a IndexVec<hir::AdtId, hir::AdtDef<'hir>>,
        arrays: &'a hir::ArrayTable<'hir>,
        strings: &'a mut StringPool,
        functions: &'a IndexVec<FunctionId, hir::Function<'hir>>,
        runtime_uses: &'a IndexVec<FunctionId, IndexVec<LocalId, bool>>,
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

        let mut builder = FunctionLower {
            blocks: Vec::new(),
            current: 0,
            local_map,
            locals,
            next,
            types,
            symbols,
            strings,
            adts,
            arrays,
            typeck: &function.typeck,
            local_symbols,
            constant_locals: IndexVec::from_elem(None, n_hir_locals),
            runtime_local_uses: runtime_uses[id].clone(),
            functions,
            runtime_uses,
            inlined_return_target: None,
            loop_targets: Vec::new(),
            span: function.decl_span,
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
            is_const: function.is_const,
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

            Stmt::Loop { kind, body } => self.lower_loop(*kind, body)?,
            Stmt::Break => {
                let target = self.loop_targets.last().expect("break without an enclosing loop");
                self.terminate(Terminator::Jump(target.break_target));
            },
            Stmt::Continue => {
                let target = self.loop_targets.last().expect("continue without an enclosing loop");
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
        self.loop_targets
            .push(LoopTargets { break_target: exit, continue_target: header });
        self.lower_block(body)?;
        self.loop_targets.pop();
        if !self.is_terminated() {
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
        self.loop_targets
            .push(LoopTargets { break_target: exit, continue_target: step });
        self.lower_block(body)?;
        self.loop_targets.pop();
        if !self.is_terminated() {
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
        self.loop_targets
            .push(LoopTargets { break_target: exit, continue_target: step });
        self.lower_block(body)?;
        self.loop_targets.pop();
        if !self.is_terminated() {
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
        let outer = std::mem::replace(&mut self.span, expr.span);
        let lowered = self.lower_expr_inner(expr);
        self.span = outer;

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
                        let s = self.symbols.get(*sym);
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
                                let pointer = self.types.refer(self.types.common.u8, false);
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
                    self.constant_locals[*local] = self.capture_constant_expr(value_expr);
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
                        self.constant_locals[*local] = None;

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
                                            self.flush_text(&mut pending);
                                            self.emit_write_value(arg)?;
                                        },
                                    }
                                }

                                if intrinsic == I::PrintLn {
                                    pending.push('\n');
                                }
                                self.flush_text(&mut pending);

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
                let (size, align) = hir::type_layout(*target, self.types, self.adts, self.arrays);
                let value = match kind {
                    TypeIntrinsicKind::SizeOf => size as i64,
                    TypeIntrinsicKind::AlignOf => align as i64,
                };

                Ok(Operand::Const(Const::Int(value, typ)))
            },

            ExpressionKind::Struct { fields, .. } => {
                let dest = self.fresh_temporary(typ);

                for (sym, value) in *fields {
                    let layout = hir::struct_field(typ, *sym, self.types, self.adts, self.arrays);
                    let value_operand = self.lower_expr(value)?;

                    self.emit(
                        dest,
                        Kind::FieldStore { value: value_operand, offset: layout.offset },
                    );
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
                    if self.is_terminated() {
                        break;
                    }

                    self.lower_statement(statement)?;
                }

                match tail {
                    Some(tail) if !self.is_terminated() => self.lower_expr(tail),
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
                    Ok::<_, MirError>(if !this.is_terminated() {
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
                            let target =
                                self.loop_targets.last().expect("break without an enclosing loop");
                            self.terminate(Terminator::Jump(target.break_target));
                        },

                        hir::ArmBody::Continue => {
                            let target = self
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
            let (_, _, len) = self.array_info(self.types.array(array_id));
            let src = match self.is_place_expr(left) {
                true => {
                    let (origin, offset, _) = self.place_info(left);
                    let ptr = self.fresh_temporary(self.types.refer(self.types.common.u8, mutable));
                    self.emit(ptr, InstructionKind::AddressOf { src: origin, offset });
                    ptr
                },
                false => {
                    let lowered = self.lower_expr(left)?;
                    let value = self.fresh_temporary(self.typeck.type_of(left.id));
                    self.emit(value, InstructionKind::Assign(lowered));

                    let ptr = self.fresh_temporary(self.types.refer(self.types.common.u8, mutable));
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
        (self.arrays.get(id).element == element.into()).then_some(id)
    }

    #[inline(always)]
    fn local_type(&self, id: LocalId) -> Type<'hir> {
        self.locals[self.local_map[id].0 as usize].1
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
        let dest = self.fresh_temporary(self.types.raw(typ, true));

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
                let layout =
                    hir::struct_field(base_type, *field, self.types, self.adts, self.arrays);
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
                let layout =
                    hir::struct_field(base_type, *field, self.types, self.adts, self.arrays);

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
                let array = self.arrays.get(id);
                let (size, _) = hir::type_layout(array.element, self.types, self.adts, self.arrays);
                (array.element, size, array.len)
            },
            _ => unreachable!("array_info on a non-array type"),
        }
    }

    /// `(element, element_size)` of an indexable type (array or slice)
    fn element_info(&self, typ: Type<'hir>) -> (Type<'hir>, u32) {
        let element = match typ.kind() {
            TypeKind::Array(id) => self.arrays.get(id).element,
            TypeKind::Slice { element, .. } => element.into(),
            _ => unreachable!("element_info on a non-indexable type: {typ}"),
        };
        let (stride, _) = hir::type_layout(element, self.types, self.adts, self.arrays);
        (element, stride)
    }

    fn place_address(
        &mut self,
        expr: &'hir Expression<'hir>,
    ) -> Result<Option<Place<'hir>>, MirError> {
        let pointer = self.types.refer(self.types.common.u8, true);

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
                let pointer = self.types.refer(self.types.common.u8, false);
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
        debug_assert!(
            !self.blocks[self.current].is_terminated(),
            "double-termination of block {:?}",
            self.blocks[self.current].id
        );

        self.blocks[self.current].terminator = Some(term);
    }

    #[inline(always)]
    fn place_for_local(&self, local_id: LocalId, typ: Type<'hir>) -> Place<'hir> {
        Place { id: self.local_map[local_id], typ }
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
        self.current = id.0 as usize;
    }

    #[inline(always)]
    fn current_block_id(&self) -> BlockId {
        BlockId(self.current as u32)
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
                    let layout =
                        hir::struct_field(place.typ, *field, self.types, self.adts, self.arrays);
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
                let enum_def = &self.adts[*enum_id];
                let variant = enum_def.variants()[*variant_idx];
                let tag_val = variant.value;
                let tag_ty = enum_def.enum_repr().typ();

                // load discriminant tag from offset 0
                let tag_place = self.fresh_temporary(tag_ty);
                self.emit(
                    tag_place,
                    Kind::FieldLoad { src: Operand::Place(place), offset: 0, typ: tag_ty },
                );
                let tag_const = Const::Int(tag_val, tag_ty);
                let tag_place = Operand::Place(tag_place);

                match sub {
                    // first branch on the tag: a match continues into `sub_block`,
                    // a mismatch falls through to `fail_block`
                    Some(sub_pat) => {
                        let sub_block = self.new_block();
                        self.emit_eq_branch(tag_place, tag_const, sub_block, fail_block);
                        self.switch_to(sub_block);

                        let offset =
                            hir::enum_payload_offset(place.typ, self.types, self.adts, self.arrays);
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
                        let typ = typ.subst(self.types, self.arrays, args);
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
        let span = self.span;
        self.blocks[self.current].instructions.push(Instruction { dest, kind, span });
    }

    /// emits the pending run of compile-time text, if any, and clears it
    #[inline]
    fn flush_text(&mut self, pending: &mut String) {
        if !pending.is_empty() {
            self.emit_write_string(std::mem::take(pending));
        }
    }

    fn emit_write_value(&mut self, expr: &'hir Expression<'hir>) -> Result<(), MirError> {
        let typ = self.typeck.type_of(expr.id);
        let kind = hir::lang::print_kind(typ).expect("HIR rejects an interpolated value");
        let operand = self.lower_expr(expr)?;

        match kind {
            PrintKind::Str => self.emit_write_str(operand),
            PrintKind::Bool => self.emit_write_bool(operand),
            PrintKind::Char => self.emit_write_char(operand),
            PrintKind::Int => self.emit_write_digits(operand, true),
            PrintKind::Uint => self.emit_write_digits(operand, false),
        }

        Ok(())
    }

    /// a `str` is a pointer and a length side by side, so writing one is the syscall and nothing else
    fn emit_write_str(&mut self, operand: Operand<'hir>) {
        let uptr = self.types.common.uptr;
        let pointer = self.fresh_temporary(uptr);
        self.emit(pointer, Kind::FieldLoad { src: operand.clone(), offset: 0, typ: uptr });

        let len = self.fresh_temporary(uptr);
        self.emit(len, Kind::FieldLoad { src: operand, offset: 8, typ: uptr });

        self.emit_write(Operand::Place(pointer), Operand::Place(len));
    }

    fn emit_write_bool(&mut self, condition: Operand<'hir>) {
        let (then_id, else_id, merge_id) = (self.new_block(), self.new_block(), self.new_block());

        self.terminate(Terminator::Branch { condition, then_block: then_id, else_block: else_id });

        self.switch_to(then_id);
        self.emit_write_string("true".to_owned());
        self.terminate(Terminator::Jump(merge_id));

        self.switch_to(else_id);
        self.emit_write_string("false".to_owned());
        self.terminate(Terminator::Jump(merge_id));

        self.switch_to(merge_id);
    }

    /// encodes a `char` as UTF-8
    fn emit_write_char(&mut self, point: Operand<'hir>) {
        let (u32, buffer) = (self.types.common.u32, self.byte_buffer(4));

        let point = self.cast(point, u32);
        let one = self.compare(BinaryOperator::Lt, point, self.int(128, u32), u32);
        let two = self.compare(BinaryOperator::Lt, point, self.int(2048, u32), u32);
        let three = self.compare(BinaryOperator::Lt, point, self.int(65536, u32), u32);

        // the continuation bytes of each width, low six bits first
        let low = self.trailing_byte(point, 1, u32);
        let mid = self.trailing_byte(point, 64, u32);
        let high = self.trailing_byte(point, 4096, u32);

        let lead_two = self.lead_byte(point, 64, 192, u32);
        let lead_three = self.lead_byte(point, 4096, 224, u32);
        let lead_four = self.lead_byte(point, 262144, 240, u32);

        let byte0 = self.select3(&one, &two, &three, point, lead_two, lead_three, lead_four, u32);
        let byte1 = self.select3(&one, &two, &three, low, low, mid, high, u32);
        let byte2 = self.select3(&one, &two, &three, low, low, low, mid, u32);
        let byte3 = self.select3(&one, &two, &three, low, low, low, low, u32);

        for (index, byte) in [byte0, byte1, byte2, byte3].into_iter().enumerate() {
            self.store_byte(buffer, index as i64, byte, 4);
        }

        let len = self.select3(
            &one,
            &two,
            &three,
            self.int(1, u32),
            self.int(2, u32),
            self.int(3, u32),
            self.int(4, u32),
            u32,
        );

        let pointer = self.element_address(buffer, self.int(0, self.types.common.uptr), 4);
        self.emit_write(pointer, len);
    }

    /// writes an integer as decimal
    fn emit_write_digits(&mut self, value: Operand<'hir>, signed: bool) {
        const WIDTH: u32 = 24;
        let (uptr, typ) = (self.types.common.uptr, self.types.common.u64);
        let buffer = self.byte_buffer(WIDTH);

        let index = self.fresh_temporary(uptr);
        self.emit(index, Kind::Assign(self.int(WIDTH as i64, uptr)));

        let rest = self.fresh_temporary(typ);
        let negative = match signed {
            true => {
                let signed_typ = self.types.common.i64;
                let value = self.cast(value, signed_typ);
                let zero = self.int(0, signed_typ);
                let negative = self.compare(BinaryOperator::Lt, value, zero, signed_typ);

                let raw = self.cast(value, typ);
                let flipped = self.binary(BinaryOperator::Sub, self.int(0, typ), raw, typ);
                let magnitude = self.fresh_temporary(typ);

                self.emit(
                    magnitude,
                    Kind::Select { condition: negative, then_value: flipped, else_value: raw },
                );
                self.emit(rest, Kind::Assign(Operand::Place(magnitude)));

                Some(negative)
            },
            _ => {
                let value = self.cast(value, typ);
                self.emit(rest, Kind::Assign(value));
                None
            },
        };

        let (body_id, done_id) = (self.new_block(), self.new_block());
        self.terminate(Terminator::Jump(body_id));
        self.switch_to(body_id);

        let next = self.binary(BinaryOperator::Sub, Operand::Place(index), self.int(1, uptr), uptr);
        self.emit(index, Kind::Assign(next));

        let quotient =
            self.binary(BinaryOperator::Div, Operand::Place(rest), self.int(10, typ), typ);
        let scaled = self.binary(BinaryOperator::Mul, quotient, self.int(10, typ), typ);
        let digit = self.binary(BinaryOperator::Sub, Operand::Place(rest), scaled, typ);
        let character = self.binary(BinaryOperator::Add, digit, self.int(48, typ), typ);

        self.store_byte_at(buffer, Operand::Place(index), character, WIDTH);
        self.emit(rest, Kind::Assign(quotient));

        let finished =
            self.compare(BinaryOperator::Eq, Operand::Place(rest), self.int(0, typ), typ);
        self.terminate(Terminator::Branch {
            condition: finished,
            then_block: done_id,
            else_block: body_id,
        });

        self.switch_to(done_id);

        if let Some(negative) = negative {
            let (sign_id, write_id) = (self.new_block(), self.new_block());
            self.terminate(Terminator::Branch {
                condition: negative,
                then_block: sign_id,
                else_block: write_id,
            });

            self.switch_to(sign_id);
            let before =
                self.binary(BinaryOperator::Sub, Operand::Place(index), self.int(1, uptr), uptr);
            self.emit(index, Kind::Assign(before));
            self.store_byte_at(buffer, Operand::Place(index), self.int(45, typ), WIDTH);
            self.terminate(Terminator::Jump(write_id));

            self.switch_to(write_id);
        }

        let pointer = self.element_address(buffer, Operand::Place(index), WIDTH);
        let len = self.binary(
            BinaryOperator::Sub,
            self.int(WIDTH as i64, uptr),
            Operand::Place(index),
            uptr,
        );

        self.emit_write(pointer, len);
    }

    fn byte_buffer(&mut self, len: u32) -> Place<'hir> {
        let id = self.arrays.intern(self.types.common.u8, len);
        self.fresh_temporary(self.types.array(id))
    }

    fn emit_write(&mut self, pointer: Operand<'hir>, len: Operand<'hir>) {
        let i32 = self.types.common.i32;
        let dest = self.fresh_temporary(i32);

        self.emit(
            dest,
            Kind::Syscall {
                code: hir::Syscall::Write,
                args: vec![Operand::Const(Const::Int(1, i32)), pointer, len],
                returns: false,
            },
        );
    }

    #[inline]
    fn int(&self, value: i64, typ: Type<'hir>) -> Operand<'hir> {
        Operand::Const(Const::Int(value, typ))
    }

    fn binary(
        &mut self,
        operation: BinaryOperator,
        lhs: Operand<'hir>,
        rhs: Operand<'hir>,
        typ: Type<'hir>,
    ) -> Operand<'hir> {
        let dest = self.fresh_temporary(typ);
        self.emit(dest, Kind::Binary { operation, lhs, rhs, overflow: OverflowMode::Wrapping });

        Operand::Place(dest)
    }

    fn compare(
        &mut self,
        operation: BinaryOperator,
        lhs: Operand<'hir>,
        rhs: Operand<'hir>,
        _typ: Type<'hir>,
    ) -> Operand<'hir> {
        let dest = self.fresh_temporary(self.types.common.bool);
        self.emit(dest, Kind::Binary { operation, lhs, rhs, overflow: OverflowMode::Unchecked });

        Operand::Place(dest)
    }

    fn cast(&mut self, src: Operand<'hir>, typ: Type<'hir>) -> Operand<'hir> {
        let dest = self.fresh_temporary(typ);
        self.emit(dest, Kind::Cast { src, typ });

        Operand::Place(dest)
    }

    /// `128 + (point / shift) % 64`, one UTF-8 continuation byte
    fn trailing_byte(
        &mut self,
        point: Operand<'hir>,
        shift: i64,
        typ: Type<'hir>,
    ) -> Operand<'hir> {
        let shifted = self.binary(BinaryOperator::Div, point, self.int(shift, typ), typ);
        let folded = self.binary(BinaryOperator::Div, shifted, self.int(64, typ), typ);
        let scaled = self.binary(BinaryOperator::Mul, folded, self.int(64, typ), typ);
        let low = self.binary(BinaryOperator::Sub, shifted, scaled, typ);

        self.binary(BinaryOperator::Add, low, self.int(128, typ), typ)
    }

    /// `marker + point / shift`, the leading byte of a multi-byte encoding
    fn lead_byte(
        &mut self,
        point: Operand<'hir>,
        shift: i64,
        marker: i64,
        typ: Type<'hir>,
    ) -> Operand<'hir> {
        let shifted = self.binary(BinaryOperator::Div, point, self.int(shift, typ), typ);
        self.binary(BinaryOperator::Add, shifted, self.int(marker, typ), typ)
    }

    #[allow(clippy::too_many_arguments)]
    fn select3(
        &mut self,
        one: &Operand<'hir>,
        two: &Operand<'hir>,
        three: &Operand<'hir>,
        a: Operand<'hir>,
        b: Operand<'hir>,
        c: Operand<'hir>,
        d: Operand<'hir>,
        typ: Type<'hir>,
    ) -> Operand<'hir> {
        let inner = self.fresh_temporary(typ);
        self.emit(inner, Kind::Select { condition: *three, then_value: c, else_value: d });

        let middle = self.fresh_temporary(typ);
        let else_value = Operand::Place(inner);
        self.emit(middle, Kind::Select { condition: *two, then_value: b, else_value });

        let outer = self.fresh_temporary(typ);
        let else_value = Operand::Place(middle);
        self.emit(outer, Kind::Select { condition: *one, then_value: a, else_value });

        Operand::Place(outer)
    }

    #[inline]
    fn store_byte(&mut self, buffer: Place<'hir>, index: i64, value: Operand<'hir>, bound: u32) {
        let uptr = self.types.common.uptr;
        self.store_byte_at(buffer, self.int(index, uptr), value, bound);
    }

    fn store_byte_at(
        &mut self,
        buffer: Place<'hir>,
        index: Operand<'hir>,
        value: Operand<'hir>,
        bound: u32,
    ) {
        let uptr = self.types.common.uptr;
        let byte = self.cast(value, self.types.common.u8);
        let bound = self.int(bound as i64, uptr);

        self.emit(buffer, Kind::ElementStore { index, bound, value: byte, stride: 1 });
    }

    fn element_address(
        &mut self,
        buffer: Place<'hir>,
        index: Operand<'hir>,
        bound: u32,
    ) -> Operand<'hir> {
        let uptr = self.types.common.uptr;
        let dest = self.fresh_temporary(uptr);
        let bound = self.int(bound as i64, uptr);

        self.emit(
            dest,
            Kind::ElementAddr { base: Operand::Place(buffer), index, bound, stride: 1 },
        );

        Operand::Place(dest)
    }

    fn emit_write_string(&mut self, text: String) {
        let len = text.len();
        let id = self.strings.intern(text);
        let dest = self.fresh_temporary(TypeKind::I32.into());

        self.emit(
            dest,
            InstructionKind::Syscall {
                code: hir::Syscall::Write,
                args: vec![
                    Operand::Const(Const::Int(1, TypeKind::I32.into())),
                    Operand::Const(Const::Str(id)),
                    Operand::Const(Const::Int(len as i64, TypeKind::I32.into())),
                ],
                returns: false,
            },
        );
    }

    #[inline]
    fn constant_text(&self, expr: &Expression<'hir>) -> Option<String> {
        hir::lang::print_kind(self.typeck.type_of(expr.id))?;

        self.capture_constant_expr(expr)
    }

    #[inline]
    fn get_fn_unchecked(&self, id: &FunctionId) -> &'a hir::Function<'hir> {
        self.functions
            .get(*id)
            .unwrap_or_else(|| panic!("callee function {:?} not found", id))
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
                    L::Str(sym) => self.symbols.get(*sym).to_owned(),
                    L::Unit => String::new(),
                })
            },
            ExpressionKind::Local(id) => self.constant_locals[*id].clone(),
            _ => None,
        }
    }

    fn emit_variant(
        &mut self,
        id: hir::AdtId,
        index: usize,
        payload: Option<&'hir Expression<'hir>>,
        typ: Type<'hir>,
    ) -> Result<Operand<'hir>, MirError> {
        let dest = self.fresh_temporary(typ);

        let tag_ty = self.adts[id].enum_repr().typ();
        let tag = self.adts[id].variants()[index].value;
        self.emit(
            dest,
            InstructionKind::FieldStore {
                value: Operand::Const(Const::Int(tag, tag_ty)),
                offset: 0,
            },
        );

        if let Some(payload) = payload {
            let offset = hir::enum_payload_offset(typ, self.types, self.adts, self.arrays);
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
        let callee = &self.functions[callee_id];

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
                self.symbols.get(callee.name),
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
        return_place: Option<Place<'hir>>,
    ) -> InlineContext<'a, 'hir> {
        use std::mem::replace;

        InlineContext {
            local_map: replace(&mut self.local_map, local_map),
            constant_locals: replace(
                &mut self.constant_locals,
                IndexVec::from_elem(None, callee.locals.len()),
            ),
            runtime_local_uses: replace(
                &mut self.runtime_local_uses,
                self.runtime_uses[callee.id].clone(),
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

impl<'hir> PartialBlock<'hir> {
    fn new(id: BlockId) -> Self {
        Self { id, instructions: Vec::new(), terminator: None }
    }

    #[inline(always)]
    const fn is_terminated(&self) -> bool {
        self.terminator.is_some()
    }

    fn finalise(self) -> Block<'hir> {
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
        visit_statement_runtime_uses(statement, uses);
    }
}

fn visit_statement_runtime_uses(statement: &Statement<'_>, uses: &mut IndexVec<LocalId, bool>) {
    match statement {
        Statement::LetUninit { .. } | Statement::Return(None) => {},
        Statement::LetInit { init, .. } => visit_expr_runtime_uses(init, uses),
        Statement::Expr(expr) => visit_expr_runtime_uses(expr, uses),
        Statement::Return(Some(expr)) => visit_expr_runtime_uses(expr, uses),
        Statement::Loop { kind, body } => {
            match kind {
                hir::LoopKind::Infinite => {},
                hir::LoopKind::Range { binding, start, end, .. } => {
                    if let Some(binding) = binding {
                        uses[*binding] = true;
                    }
                    visit_expr_runtime_uses(start, uses);
                    visit_expr_runtime_uses(end, uses);
                },
                hir::LoopKind::Iterable { binding, iterable } => {
                    uses[*binding] = true;
                    visit_expr_runtime_uses(iterable, uses);
                },
            }
            visit_block_runtime_uses(body, uses);
        },
        Statement::Break | Statement::Continue => {},
    }
}

fn visit_expr_runtime_uses(expr: &hir::Expression<'_>, uses: &mut IndexVec<LocalId, bool>) {
    use ExpressionKind::*;
    match &expr.kind {
        Local(id) => uses[*id] = true,
        // a constant's tree has its own local space, nothing here can
        // reference the enclosing body's locals
        Const(_) | ParamConst { .. } | Static(_) => {},
        Unary { expr: inner, .. } => visit_expr_runtime_uses(inner, uses),
        Cast { from, .. } => visit_expr_runtime_uses(from, uses),
        Binary { left, right, .. } => {
            visit_expr_runtime_uses(left, uses);
            visit_expr_runtime_uses(right, uses);
        },
        Block { statements, tail } => {
            for statement in *statements {
                visit_statement_runtime_uses(statement, uses);
            }
            if let Some(tail) = tail {
                visit_expr_runtime_uses(tail, uses);
            }
        },
        If { condition, then_block, else_block } => {
            visit_expr_runtime_uses(condition, uses);
            visit_expr_runtime_uses(then_block, uses);
            if let Some(else_block) = else_block {
                visit_expr_runtime_uses(else_block, uses);
            }
        },
        Assign { target, value } => {
            if !matches!(&target.kind, Local(_)) {
                visit_place_runtime_uses(target, uses);
            }
            visit_expr_runtime_uses(value, uses);
        },
        // unlike a plain assignment the target is read before it is written, so even a local target counts as a use
        CompoundAssign { target, value, .. } => {
            visit_expr_runtime_uses(target, uses);
            visit_expr_runtime_uses(value, uses);
        },
        Struct { fields, .. } => {
            for &(_, value) in *fields {
                visit_expr_runtime_uses(value, uses);
            }
        },
        Call { args, .. } => {
            for arg in *args {
                visit_expr_runtime_uses(arg, uses);
            }
        },
        MethodCall { receiver, args, .. } => {
            let receiver = *receiver;
            let is_place = matches!(&receiver.kind, Local(_) | Field { .. });

            match is_place {
                true => visit_place_runtime_uses(receiver, uses),
                _ => visit_expr_runtime_uses(receiver, uses),
            }

            for arg in *args {
                visit_expr_runtime_uses(arg, uses);
            }
        },
        Field { .. } => visit_place_runtime_uses(expr, uses),
        Array { elements } => {
            for element in *elements {
                visit_expr_runtime_uses(element, uses);
            }
        },
        ArrayRepeat { value, .. } => visit_expr_runtime_uses(value, uses),
        Index { base, index } => {
            visit_place_runtime_uses(base, uses);
            visit_expr_runtime_uses(index, uses);
        },
        TypeIntrinsic { .. } | Literal(_) | Path(_) => {},
        Match { scrutinee, arms } => {
            visit_expr_runtime_uses(scrutinee, uses);
            for arm in *arms {
                if let Some(guard) = arm.guard {
                    visit_expr_runtime_uses(guard, uses);
                }
                if let Some(body) = arm.body.value() {
                    visit_expr_runtime_uses(body, uses);
                }
            }
        },
    }
}

fn visit_place_runtime_uses(expr: &hir::Expression<'_>, uses: &mut IndexVec<LocalId, bool>) {
    use ExpressionKind::*;
    match &expr.kind {
        Local(local) => uses[*local] = true,
        Field { base, .. } => visit_place_runtime_uses(base, uses),
        Index { base, index } => {
            visit_place_runtime_uses(base, uses);
            visit_expr_runtime_uses(index, uses);
        },
        Unary { operator: UnaryOperator::Deref, expr } => visit_expr_runtime_uses(expr, uses),
        _ => visit_expr_runtime_uses(expr, uses),
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
