//! HIR -> MIR lowering

use crate::{
    hir::{
        self, ExprId, Expression, ExpressionKind, FunctionId, Hir, LocalId, PlaceKind,
        ReceiverKind, RefTargetKind, Type, TypeKind, index_vec::IndexVec,
    },
    mir::{
        self, Block, BlockId, Const, Function, Instruction, InstructionKind, Mir, Operand, Place,
        Terminator, ValueId, error::MirError, layout::LayoutTable,
    },
    parser::expression::{BinaryOperator, TypeIntrinsicKind, UnaryOperator},
};
use lasso::Key;
use std::collections::HashMap;

struct PartialBlock {
    id: BlockId,
    instructions: Vec<Instruction>,
    terminator: Option<Terminator>,
}

struct FunctionLower<'a> {
    blocks: Vec<PartialBlock>,
    current: usize,
    next: u32,
    local_map: IndexVec<LocalId, ValueId>,
    locals: Vec<(ValueId, Type)>,
    symbols: &'a [String],
    strings: &'a mut Vec<String>,
    layouts: &'a LayoutTable,
    exprs: &'a IndexVec<ExprId, Expression>,
    typeck: &'a hir::TypeckResults,
    local_symbols: IndexVec<LocalId, usize>,
    constant_locals: IndexVec<LocalId, Option<String>>,
    runtime_local_uses: IndexVec<LocalId, bool>,
    functions_map: &'a HashMap<FunctionId, &'a hir::Function>,
    runtime_uses_map: &'a HashMap<FunctionId, IndexVec<LocalId, bool>>,
    inlined_return_target: Option<(BlockId, Option<Place>)>,
}

pub fn lower(hir: Hir) -> Result<Mir, MirError> {
    debug_assert!(
        !hir.functions.iter().any(has_open_generic),
        r#"MIR lowering received HIR containing unresolved GenericParam
        monomorphisation should have produced fully concrete signatures"#
    );

    let mut functions = Vec::with_capacity(hir.functions.len());
    let mut strings = Vec::new();
    let layouts = LayoutTable::build(&hir.structs);
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
            &mut strings,
            &functions_map,
            &runtime_uses_map,
        )?);
    }

    Ok(Mir {
        functions,
        symbols,
        strings,
        struct_layouts: layouts.summaries(),
    })
}

impl<'a> FunctionLower<'a> {
    fn run(
        function: &hir::Function,
        symbols: &'a [String],
        layouts: &'a LayoutTable,
        strings: &'a mut Vec<String>,
        functions_map: &'a HashMap<FunctionId, &'a hir::Function>,
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
            exprs: &function.exprs,
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

    fn lower_block(&mut self, block: &hir::Block) -> Result<(), MirError> {
        for statement in &block.statements {
            if self.is_terminated() {
                break;
            }

            self.lower_statement(&statement)?;
        }

        Ok(())
    }

    fn lower_statement(&mut self, statement: &hir::Statement) -> Result<(), MirError> {
        use hir::Statement as Stmt;

        match statement {
            Stmt::LetInit { id, init } => {
                let init = *init;
                self.constant_locals[*id] = self.capture_constant_expr(init);

                if !self.runtime_local_uses(*id) && self.constant_locals[*id].is_some() {
                    return Ok(());
                }

                let typ = self.typeck.type_of(init);
                let src = self.lower_expr(init)?;
                let dest = self.place_for_local(*id, typ);

                if self.runtime_local_uses(*id) {
                    self.emit(dest, InstructionKind::Assign(src));
                }
            },
            Stmt::LetUninit { .. } => {},

            Stmt::Expr(expr) => {
                self.lower_expr(*expr)?;
            },

            Stmt::Return(value) => {
                let operand = value.as_ref().map(|e| self.lower_expr(*e)).transpose()?;
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
                let condition = self.lower_expr(*condition)?;

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
                let condition = self.lower_expr(*condition)?;

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

    fn lower_expr(&mut self, id: ExprId) -> Result<Operand, MirError> {
        use InstructionKind as Kind;

        let arena = self.exprs;
        let expr = &arena[id];
        let typ = self.typeck.type_of(id);

        match &expr.kind {
            ExpressionKind::Unit => Ok(Operand::Const(Const::Unit)),
            ExpressionKind::Integer(n) => Ok(Operand::Const(Const::Int(*n, typ))),
            ExpressionKind::Float(f) => Ok(Operand::Const(Const::Float(*f, typ))),
            ExpressionKind::Bool(b) => Ok(Operand::Const(Const::Bool(*b))),
            ExpressionKind::Char(c) => Ok(Operand::Const(Const::Int(*c as i64, typ))),
            ExpressionKind::String(sym) => {
                let s = &self.symbols[sym.0.into_usize()];
                let id = self.intern_string(&s.clone());
                let len = s.len();
                Ok(Operand::Const(Const::Str { id, len }))
            },

            ExpressionKind::Local(local_id) => {
                Ok(Operand::Place(self.place_for_local(*local_id, typ)))
            },

            ExpressionKind::Cast { from, to } => {
                let (from, to) = (*from, *to);
                let src = self.lower_expr(from)?;
                let dest = self.fresh_temporary(to);

                self.emit(dest, Kind::Cast { src, typ: to });

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Unary { operator, expr: inner } => {
                let (operator, inner) = (*operator, *inner);
                let rhs = self.lower_expr(inner)?;
                let dest = self.fresh_temporary(typ.unwrap_unit());

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
                use crate::optimisation;

                let (operator, left, right) = (*operator, *left, *right);
                if matches!(operator, BinaryOperator::And | BinaryOperator::Or) {
                    return self.lower_short_circuit(operator, left, right, typ);
                }

                let lhs = self.lower_expr(left)?;
                let rhs = self.lower_expr(right)?;
                let dest = self.fresh_temporary(typ.unwrap_unit());

                let is_integer = typ.is_integer();
                let is_arithmetic = matches!(
                    operator,
                    BinaryOperator::Add | BinaryOperator::Sub | BinaryOperator::Mul
                );
                let is_on_debug = optimisation::Level::Debug == optimisation::get();
                let checked = is_integer && is_arithmetic && is_on_debug;

                self.emit(dest, Kind::Binary { operation: operator, lhs, rhs, checked });

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Assign { target, value } => {
                let value_id = *value;
                if let PlaceKind::Local(local) = &target.kind {
                    self.constant_locals[*local] = self.capture_constant_expr(value_id);
                }

                let src = self.lower_expr(value_id)?;
                let (dest, offset, _) = self.place_info(target);

                match &target.kind {
                    PlaceKind::Local(local) if self.runtime_local_uses(*local) => {
                        self.emit(dest, Kind::Assign(src));
                        Ok(Operand::Place(dest))
                    },
                    PlaceKind::Local(_) => Ok(src),
                    PlaceKind::Field { .. } => {
                        self.emit(dest, Kind::FieldStore { value: src, offset });
                        Ok(src)
                    },
                }
            },

            ExpressionKind::Call { function, args, .. } => {
                let function = *function;
                let mut lowered_args = Vec::with_capacity(args.len());
                for arg in args {
                    let operand = self.lower_expr(*arg)?;
                    lowered_args.push(operand);
                }

                self.emit_call(function, lowered_args, typ)
            },

            ExpressionKind::MethodCall { function, receiver, args } => {
                let function = *function;
                let place = self.fresh_temporary(receiver.typ);

                match &receiver.kind {
                    ReceiverKind::Place(receiver_place) => {
                        let (origin, offset, receiver_type) = self.place_info(receiver_place);
                        debug_assert!(receiver_type.kind() != TypeKind::Unit);

                        match (offset, matches!(receiver_type.kind(), TypeKind::Ref { .. })) {
                            (0, true) => self.emit(place, Kind::Assign(Operand::Place(origin))),
                            (_, true) => {
                                self.emit(
                                    place,
                                    Kind::FieldLoad {
                                        src: Operand::Place(origin),
                                        offset,
                                        typ: receiver_type,
                                    },
                                );
                            },
                            (_, false) => self.emit(place, Kind::AddressOf { src: origin, offset }),
                        }
                    },
                    ReceiverKind::Computed(value) => {
                        let value = *value;
                        let val_type = self.typeck.type_of(value);
                        let lowered_receiver = self.lower_expr(value)?;

                        match matches!(val_type.kind(), TypeKind::Ref { .. }) {
                            true => self.emit(place, Kind::Assign(lowered_receiver)),
                            _ => {
                                let value_place = self.fresh_temporary(val_type);
                                self.emit(value_place, Kind::Assign(lowered_receiver));
                                self.emit(place, Kind::AddressOf { src: value_place, offset: 0 });
                            },
                        }
                    },
                }

                let mut lowered_args = Vec::with_capacity(args.len() + 1);
                lowered_args.push(Operand::Place(place));
                for arg in args {
                    let operand = self.lower_expr(*arg)?;
                    lowered_args.push(operand);
                }

                self.emit_call(function, lowered_args, typ)
            },

            ExpressionKind::IntrinsicCall { intrinsic, args } => {
                use crate::hir::Intrinsic;

                let intrinsic = *intrinsic;
                // the return value is ignored for those functions
                match intrinsic {
                    Intrinsic::PrintLn | Intrinsic::Print => {
                        let mut output = String::new();
                        for arg in args {
                            self.push_print_arg(&mut output, *arg);
                        }

                        if intrinsic == Intrinsic::PrintLn {
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

                    Intrinsic::Len => unreachable!(),
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
                    args.iter().map(|a| self.lower_expr(*a)).collect::<Result<Vec<_>, _>>()?;
                let dest = self.fresh_temporary(typ);

                self.emit(dest, Kind::Syscall { code, args: lowered_args, returns: true });

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Struct { id, fields } => {
                let id = *id;
                let dest = self.fresh_temporary(Type::new(TypeKind::Struct(id)));

                for (sym, value) in fields {
                    let layout = self.layouts.field(Type::new(TypeKind::Struct(id)), *sym);
                    let value = self.lower_expr(*value)?;

                    self.emit(dest, Kind::FieldStore { value, offset: layout.offset });
                }

                Ok(Operand::Place(dest))
            },

            ExpressionKind::Place(place) => {
                let (origin, offset, typ) = self.place_info(place);

                if matches!(&place.kind, PlaceKind::Local(_)) {
                    return Ok(Operand::Place(origin));
                }

                let dest = self.fresh_temporary(typ);
                self.emit(dest, Kind::FieldLoad { src: Operand::Place(origin), offset, typ });

                Ok(Operand::Place(dest))
            },

            other => unimplemented!("{other:#?}"),
        }
    }

    fn lower_short_circuit(
        &mut self,
        operator: BinaryOperator,
        left: ExprId,
        right: ExprId,
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

    #[inline(always)]
    fn local_type(&self, id: LocalId) -> Type {
        self.locals[self.local_map[id].0 as usize].1
    }

    fn place_info(&self, place: &hir::Place) -> (Place, u32, Type) {
        match &place.kind {
            PlaceKind::Local(id) => {
                let origin = self.place_for_local(*id, self.local_type(*id));
                (origin, 0, origin.typ)
            },
            PlaceKind::Field { base, field } => {
                let (origin, base_offset, base_type) = self.place_info(base);
                let layout = self.layouts.field(base_type, *field);
                (origin, base_offset + layout.offset, layout.typ)
            },
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
    fn push_print_arg(&self, output: &mut String, id: ExprId) {
        match &self.exprs[id].kind {
            ExpressionKind::String(sym) => {
                let text = &self.symbols[sym.0.into_usize()];
                output.push_str(&self.expand_interpolation(text))
            },
            _ if let Some(text) = self.capture_constant_expr(id) => {
                output.push_str(&text);
            },
            _ => {},
        }
    }

    #[inline]
    fn capture_constant_expr(&self, id: ExprId) -> Option<String> {
        match &self.exprs[id].kind {
            ExpressionKind::Integer(value) => Some(value.to_string()),
            ExpressionKind::Float(value) => Some(value.to_string()),
            ExpressionKind::Bool(value) => Some(value.to_string()),
            ExpressionKind::Char(value) => Some(value.to_string()),
            ExpressionKind::String(sym) => Some(self.symbols[sym.0.into_usize()].clone()),
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
    fn lookup_constant_local(&self, name: &str) -> Option<&str> {
        self.local_symbols
            .iter()
            .enumerate()
            .find(|(_, symbol)| self.symbols.get(**symbol).is_some_and(|local| local == name))
            .and_then(|(idx, _)| self.constant_locals[LocalId(idx as u32)].as_deref())
    }

    #[inline]
    fn intern_string(&mut self, value: &str) -> usize {
        if let Some(id) = self.strings.iter().position(|existing| existing == &value) {
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

    fn emit_call(
        &mut self,
        callee_id: FunctionId,
        lowered_args: Vec<Operand>,
        return_type: Type,
    ) -> Result<Operand, MirError> {
        let callee = self.functions_map.get(&callee_id).expect("callee must exist");
        match callee.inline {
            true => self.inline_call(callee_id, lowered_args),
            _ => {
                let dest = self.fresh_temporary(return_type.unwrap_unit());

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
        use std::mem::replace;

        let callee = self.functions_map.get(&callee_id).expect("callee must exist");

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

        // push the new context for lowering the callee
        let old_local_map = replace(&mut self.local_map, callee_local_map);
        let old_constant_locals =
            replace(&mut self.constant_locals, IndexVec::from_elem(None, callee_n_locals));
        let old_runtime_local_uses = replace(
            &mut self.runtime_local_uses,
            self.runtime_uses_map.get(&callee_id).cloned().unwrap(),
        );
        let old_local_symbols = replace(
            &mut self.local_symbols,
            callee.locals.iter().map(|l| l.name.0.into_usize()).collect(),
        );
        let old_inlined_return_target =
            replace(&mut self.inlined_return_target, Some((exit_block_id, inline_ret_place)));
        let old_exprs = replace(&mut self.exprs, &callee.exprs);
        let old_typeck = replace(&mut self.typeck, &callee.typeck);

        self.lower_block(&callee.body)?;

        if !self.is_terminated() {
            self.terminate(Terminator::Jump(exit_block_id));
        }

        // restore the old context
        self.local_map = old_local_map;
        self.constant_locals = old_constant_locals;
        self.runtime_local_uses = old_runtime_local_uses;
        self.local_symbols = old_local_symbols;
        self.inlined_return_target = old_inlined_return_target;
        self.exprs = old_exprs;
        self.typeck = old_typeck;

        self.switch_to(exit_block_id);

        let result = match inline_ret_place {
            Some(place) => Operand::Place(place),
            None => Operand::Const(Const::Unit),
        };
        Ok(result)
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

fn collect_runtime_local_uses(function: &hir::Function) -> IndexVec<LocalId, bool> {
    let mut uses = IndexVec::from_elem(false, function.locals.len());
    visit_block_runtime_uses(&function.exprs, &function.body, &mut uses);

    uses
}

fn visit_block_runtime_uses(
    arena: &IndexVec<ExprId, Expression>,
    block: &hir::Block,
    uses: &mut IndexVec<LocalId, bool>,
) {
    for statement in &block.statements {
        match statement {
            hir::Statement::LetInit { init, .. } => visit_expr_runtime_uses(arena, *init, uses),
            hir::Statement::LetUninit { .. } => {},
            hir::Statement::Expr(expr) => visit_expr_runtime_uses(arena, *expr, uses),
            hir::Statement::Return(Some(expr)) => visit_expr_runtime_uses(arena, *expr, uses),
            hir::Statement::Return(None) => {},
            hir::Statement::If { condition, then_block, else_block } => {
                visit_expr_runtime_uses(arena, *condition, uses);
                visit_block_runtime_uses(arena, then_block, uses);
                if let Some(else_block) = else_block {
                    visit_block_runtime_uses(arena, else_block, uses);
                }
            },
            hir::Statement::While { condition, body } => {
                visit_expr_runtime_uses(arena, *condition, uses);
                visit_block_runtime_uses(arena, body, uses);
            },
            hir::Statement::Block(block) => visit_block_runtime_uses(arena, block, uses),
        }
    }
}

fn visit_expr_runtime_uses(
    arena: &IndexVec<ExprId, Expression>,
    id: ExprId,
    uses: &mut IndexVec<LocalId, bool>,
) {
    match &arena[id].kind {
        ExpressionKind::Local(id) => uses[*id] = true,
        ExpressionKind::Unary { expr, .. } => visit_expr_runtime_uses(arena, *expr, uses),
        ExpressionKind::Cast { from, .. } => visit_expr_runtime_uses(arena, *from, uses),
        ExpressionKind::Binary { left, right, .. } => {
            visit_expr_runtime_uses(arena, *left, uses);
            visit_expr_runtime_uses(arena, *right, uses);
        },
        ExpressionKind::Assign { target, value } => {
            if matches!(&target.kind, PlaceKind::Field { .. }) {
                visit_place_runtime_uses(target, uses);
            }
            visit_expr_runtime_uses(arena, *value, uses);
        },
        ExpressionKind::Struct { fields, .. } => {
            for (_, value) in fields {
                visit_expr_runtime_uses(arena, *value, uses);
            }
        },
        ExpressionKind::Call { args, .. }
        | ExpressionKind::IntrinsicCall { args, .. }
        | ExpressionKind::Syscall { args, .. } => {
            for arg in args {
                visit_expr_runtime_uses(arena, *arg, uses);
            }
        },
        ExpressionKind::MethodCall { receiver, args, .. } => {
            match &receiver.kind {
                ReceiverKind::Place(place) => visit_place_runtime_uses(place, uses),
                ReceiverKind::Computed(val) => visit_expr_runtime_uses(arena, *val, uses),
            }
            for arg in args {
                visit_expr_runtime_uses(arena, *arg, uses);
            }
        },
        ExpressionKind::Place(place) => visit_place_runtime_uses(place, uses),
        ExpressionKind::TypeIntrinsic { .. } => {},
        ExpressionKind::Unit
        | ExpressionKind::Integer(_)
        | ExpressionKind::Float(_)
        | ExpressionKind::String(_)
        | ExpressionKind::Char(_)
        | ExpressionKind::Bool(_) => {},
        other => unimplemented!("{other:#?}"),
    }
}

fn visit_place_runtime_uses(place: &hir::Place, uses: &mut IndexVec<LocalId, bool>) {
    if let Some(id) = place.base_local() {
        uses[id] = true;
    }
}

fn has_open_generic(func: &hir::Function) -> bool {
    fn is_open(t: Type) -> bool {
        match t.kind() {
            TypeKind::GenericParam(_) => true,
            TypeKind::Ref { to, .. } => matches!(to.kind(), RefTargetKind::GenericParam(_)),
            _ => false,
        }
    }

    is_open(func.return_type) || func.params.iter().any(|p| is_open(p.typ))
}
