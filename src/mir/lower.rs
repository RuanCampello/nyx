//! HIR -> MIR lowering

use crate::{
    hir::{self, Expression, ExpressionKind, FunctionId, Hir, LocalId, Struct, SymbolId, Type},
    mir::{
        self, Block, BlockId, Const, Function, Instruction, InstructionKind, Mir, Operand, Place,
        Terminator, ValueId, error::MirError,
    },
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
    local_map: Vec<ValueId>,
    locals: Vec<(ValueId, Type)>,
    symbols: &'a [String],
    strings: &'a mut Vec<String>,
    structs: &'a [Struct],
    local_symbols: Vec<usize>,
    constant_locals: Vec<Option<String>>,
    runtime_local_uses: Vec<bool>,
    functions_map: &'a HashMap<FunctionId, &'a hir::Function>,
    inlined_return_target: Option<(BlockId, Option<Place>)>,
}

pub fn lower(hir: Hir) -> Result<Mir, MirError> {
    let mut functions = Vec::with_capacity(hir.functions.len());
    let mut strings = Vec::new();
    let symbols = hir.symbols;

    let functions_map = hir.functions.iter().map(|f| (f.id, f)).collect();

    for function in &hir.functions {
        functions.push(FunctionLower::run(
            function.clone(),
            &symbols,
            &hir.structs,
            &mut strings,
            &functions_map,
        )?);
    }

    Ok(Mir {
        functions,
        symbols,
        strings,
        struct_layouts: struct_layouts(&hir.structs),
    })
}

impl<'a> FunctionLower<'a> {
    fn run(
        function: hir::Function,
        symbols: &'a [String],
        structs: &'a [Struct],
        strings: &'a mut Vec<String>,
        functions_map: &'a HashMap<FunctionId, &'a hir::Function>,
    ) -> Result<mir::Function, MirError> {
        let id = function.id;
        let intrinsic = function.intrinsic;
        let name_symbol = function.name.0.into_usize();
        let return_type = function.return_type;
        let n_hir_locals = function.locals.len();

        let mut local_map = vec![ValueId(0); n_hir_locals];
        let mut local_symbols = vec![0; n_hir_locals];
        let mut locals = Vec::with_capacity(n_hir_locals);

        for local in &function.locals {
            let value_id = ValueId(locals.len() as u32);
            let idx = local.id.0 as usize;
            local_map[idx] = value_id;
            local_symbols[idx] = local.name.0.into_usize();
            locals.push((value_id, local.typ))
        }

        let params = function
            .params
            .iter()
            .map(|param| (local_map[param.id.0 as usize], param.typ))
            .collect();

        let next = locals.len() as u32;

        let mut builder = FunctionLower {
            blocks: Vec::new(),
            current: 0,
            local_map,
            locals,
            next,
            symbols,
            strings,
            structs,
            local_symbols,
            constant_locals: vec![None; n_hir_locals],
            runtime_local_uses: collect_runtime_local_uses(&function),
            functions_map,
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
            Stmt::Let { id, init } => {
                if let Some(expr) = init {
                    self.constant_locals[id.0 as usize] = self.capture_constant_expr(expr);

                    if !self.runtime_local_uses(*id)
                        && self.constant_locals[id.0 as usize].is_some()
                    {
                        return Ok(());
                    }

                    let src = self.lower_expr(expr)?;
                    let dest = self.place_for_local(*id, expr.typ);

                    if self.runtime_local_uses(*id) {
                        self.emit(dest, InstructionKind::Assign(src));
                    }
                }
            }

            Stmt::Expr(expr) => {
                self.lower_expr(expr)?;
            }

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
            }

            Stmt::If {
                condition,
                then_block,
                else_block,
            } => {
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
            }

            Stmt::While { condition, body } => {
                // cfg shape:
                //   <current> ── jump ──► header
                //   header    ── branch(cond) ──► body | exit
                //   body      ──…── jump ──► header   (back-edge)
                //   exit      ← continue emission here
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
            }

            Stmt::Block(inner) => {
                self.lower_block(inner)?;
            }
        }

        Ok(())
    }

    fn lower_expr(&mut self, expr: &Expression) -> Result<Operand, MirError> {
        match &expr.kind {
            ExpressionKind::Unit => Ok(Operand::Const(Const::Unit)),
            ExpressionKind::Integer(n) => Ok(Operand::Const(Const::Int(*n, expr.typ))),
            ExpressionKind::Float(f) => Ok(Operand::Const(Const::Float(*f, expr.typ))),
            ExpressionKind::Bool(b) => Ok(Operand::Const(Const::Bool(*b))),
            ExpressionKind::String(s) => {
                let id = self.intern_string(s);
                let len = s.len();
                Ok(Operand::Const(Const::Str { id, len }))
            }

            ExpressionKind::Local(local_id) => {
                Ok(Operand::Place(self.place_for_local(*local_id, expr.typ)))
            }

            ExpressionKind::Unary {
                operator,
                expr: inner,
            } => {
                let rhs = self.lower_expr(inner)?;
                let dest = self.fresh_temporary(expr.typ.unwrap_unit());

                self.emit(
                    dest,
                    InstructionKind::Unary {
                        operation: *operator,
                        rhs,
                    },
                );

                Ok(Operand::Place(dest))
            }

            ExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let lhs = self.lower_expr(left)?;
                let rhs = self.lower_expr(right)?;
                let dest = self.fresh_temporary(expr.typ.unwrap_unit());

                self.emit(
                    dest,
                    InstructionKind::Binary {
                        operation: *operator,
                        lhs,
                        rhs,
                    },
                );

                Ok(Operand::Place(dest))
            }

            ExpressionKind::Assign { target, value } => {
                self.constant_locals[target.0 as usize] = self.capture_constant_expr(value);

                let src = self.lower_expr(value)?;
                let dest = self.place_for_local(*target, expr.typ);

                if self.runtime_local_uses(*target) {
                    self.emit(dest, InstructionKind::Assign(src));
                    return Ok(Operand::Place(dest));
                }

                Ok(src)
            }

            ExpressionKind::Call { function, args, .. } => {
                let mut lowered_args = Vec::with_capacity(args.len());
                for arg in args {
                    let operand = self.lower_expr(arg)?;
                    lowered_args.push(operand);
                }

                let callee = self.functions_map.get(function).expect("callee must exist");
                match callee.inline {
                    true => self.inline_call(*function, lowered_args),
                    _ => {
                        let dest = self.fresh_temporary(expr.typ.unwrap_unit());

                        self.emit(
                            dest,
                            InstructionKind::Call {
                                callee: *function,
                                args: lowered_args,
                            },
                        );

                        Ok(Operand::Place(dest))
                    }
                }
            }

            ExpressionKind::MethodCall {
                function,
                receiver,
                args,
            } => {
                let origin = self.place_for_local(receiver.local, self.local_type(receiver.local));
                let (offset, receiver_type) = match receiver.fields.is_empty() {
                    true => (0, origin.typ),
                    false => self.field_path_info(origin.typ, &receiver.fields),
                };
                debug_assert!(matches!(receiver_type, Type::Struct(_) | Type::Ref { .. }));

                let receiver_place = self.fresh_temporary(receiver.typ);
                self.emit(
                    receiver_place,
                    InstructionKind::AddressOf {
                        src: origin,
                        offset,
                    },
                );

                let mut lowered_args = Vec::with_capacity(args.len() + 1);
                lowered_args.push(Operand::Place(receiver_place));
                for arg in args {
                    let operand = self.lower_expr(arg)?;
                    lowered_args.push(operand);
                }

                let callee = self.functions_map.get(function).expect("callee must exist");
                match callee.inline {
                    true => self.inline_call(*function, lowered_args),
                    _ => {
                        let dest = self.fresh_temporary(expr.typ.unwrap_unit());
                        self.emit(
                            dest,
                            InstructionKind::Call {
                                callee: *function,
                                args: lowered_args,
                            },
                        );

                        Ok(Operand::Place(dest))
                    }
                }
            }

            ExpressionKind::IntrinsicCall { intrinsic, args } => {
                use crate::hir::Intrinsic;
                use crate::mir::SyscallCode;

                // the return value is ignored for those functions
                let dest = self.fresh_temporary(Type::I32);

                match intrinsic {
                    Intrinsic::PrintLn | Intrinsic::Print => {
                        let mut output = String::new();
                        for arg in args {
                            self.push_print_arg(&mut output, arg);
                        }

                        if *intrinsic == Intrinsic::PrintLn {
                            output.push('\n');
                        }

                        if !output.is_empty() {
                            self.emit_write_string(output);
                        }

                        Ok(Operand::Const(Const::Unit))
                    }

                    Intrinsic::Exit => {
                        let lowered_args = args
                            .iter()
                            .map(|a| self.lower_expr(a))
                            .collect::<Result<Vec<_>, _>>()?;

                        self.emit(
                            dest,
                            InstructionKind::Syscall {
                                code: SyscallCode::Exit,
                                args: lowered_args,
                                returns: false,
                            },
                        );

                        Ok(Operand::Const(Const::Unit))
                    }
                }
            }

            ExpressionKind::Struct { id, fields } => {
                let dest = self.fresh_temporary(Type::Struct(*id));
                let field_offsets: Vec<_> =
                    self.structs[id.0 as usize].fields.iter().map(|f| (f.name, f.offset)).collect();

                for (sym, value) in fields {
                    let offset = field_offsets
                        .iter()
                        .find(|(name, _)| name == sym)
                        .map(|&(_, offset)| offset)
                        .expect("field offset must exist after HIR validation");
                    let value = self.lower_expr(value)?;

                    self.emit(dest, InstructionKind::FieldStore { value, offset });
                }

                Ok(Operand::Place(dest))
            }

            ExpressionKind::FieldAccess { local, fields, .. } => {
                let origin = self.place_for_local(*local, self.local_type(*local));
                let (offset, typ) = self.field_path_info(origin.typ, fields);

                let dest = self.fresh_temporary(typ);
                self.emit(
                    dest,
                    InstructionKind::FieldLoad {
                        src: Operand::Place(origin),
                        offset,
                        typ,
                    },
                );

                Ok(Operand::Place(dest))
            }

            ExpressionKind::FieldAssign {
                local,
                fields,
                value,
            } => {
                let value = self.lower_expr(value)?;
                let origin = self.place_for_local(*local, self.local_type(*local));
                let (offset, typ) = self.field_path_info(origin.typ, fields);
                debug_assert_eq!(typ, value.typ());

                self.emit(origin, InstructionKind::FieldStore { value, offset });

                Ok(value)
            }
        }
    }

    #[inline(always)]
    fn local_type(&self, id: LocalId) -> Type {
        self.locals[self.local_map[id.0 as usize].0 as usize].1
    }

    fn field_path_info(&self, origin: Type, fields: &[SymbolId]) -> (u32, Type) {
        let mut current_type = origin;
        let mut total_offset = 0;

        // PERFORMANCE: field paths are small enough to a linear scan don't matter :D
        for &sym in fields {
            let (Type::Struct(sid) | Type::Ref { to: sid, .. }) = current_type else {
                unreachable!("field projection on non-struct");
            };

            let (offset, typ) = self.structs[sid.0 as usize]
                .fields
                .iter()
                .find(|field| field.name == sym)
                .map(|field| (field.offset, field.typ))
                .expect("field offset must exist after HIR validation");
            total_offset += offset;
            current_type = typ;
        }

        (total_offset, current_type)
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
        Place {
            id: self.local_map[local_id.0 as usize],
            typ,
        }
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
        let dest = self.fresh_temporary(Type::I32);

        self.emit(
            dest,
            InstructionKind::Syscall {
                code: mir::SyscallCode::Write,
                args: vec![
                    Operand::Const(Const::Int(1, Type::I32)),
                    Operand::Const(Const::Str { id, len }),
                    Operand::Const(Const::Int(len as i64, Type::I32)),
                ],
                returns: false,
            },
        );
    }

    fn push_print_arg(&self, output: &mut String, expr: &Expression) {
        match &expr.kind {
            ExpressionKind::String(text) => output.push_str(&self.expand_interpolation(text)),
            _ => {
                if let Some(text) = self.capture_constant_expr(expr) {
                    output.push_str(&text);
                }
            }
        }
    }

    fn capture_constant_expr(&self, expr: &Expression) -> Option<String> {
        match &expr.kind {
            ExpressionKind::Integer(value) => Some(value.to_string()),
            ExpressionKind::Float(value) => Some(value.to_string()),
            ExpressionKind::Bool(value) => Some(value.to_string()),
            ExpressionKind::String(value) => Some(value.clone()),
            ExpressionKind::Local(id) => self.constant_locals[id.0 as usize].clone(),
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
                    }
                }
            } else {
                output.push('{');
                output.push_str(&name);
            }
        }

        output
    }

    fn lookup_constant_local(&self, name: &str) -> Option<&str> {
        self.local_symbols
            .iter()
            .enumerate()
            .find(|(_, symbol)| self.symbols.get(**symbol).is_some_and(|local| local == name))
            .and_then(|(idx, _)| self.constant_locals[idx].as_deref())
    }

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

    fn inline_call(
        &mut self,
        callee_id: FunctionId,
        lowered_args: Vec<Operand>,
    ) -> Result<Operand, MirError> {
        use std::mem::replace;

        let callee = self.functions_map.get(&callee_id).expect("callee must exist");

        let inline_ret_place = match callee.return_type != Type::Unit {
            true => Some(self.fresh_temporary(callee.return_type)),
            _ => None,
        };
        let exit_block_id = self.new_block();

        let callee_n_locals = callee.locals.len();
        let mut callee_local_map = vec![ValueId(0); callee_n_locals];
        for idx in 0..callee_n_locals {
            let local = &callee.locals[idx];
            let place = self.fresh_temporary(local.typ);
            callee_local_map[idx] = place.id;
        }

        // emit assignment of arguments to callee parameters
        for (param, arg_operand) in callee.params.iter().zip(lowered_args) {
            let dest_val_id = callee_local_map[param.id.0 as usize];
            let dest_place = Place {
                id: dest_val_id,
                typ: param.typ,
            };
            self.emit(dest_place, InstructionKind::Assign(arg_operand));
        }

        // push the new context for lowering the callee
        let old_local_map = replace(&mut self.local_map, callee_local_map);
        let old_constant_locals = replace(&mut self.constant_locals, vec![None; callee_n_locals]);
        let old_runtime_local_uses = replace(
            &mut self.runtime_local_uses,
            collect_runtime_local_uses(callee),
        );
        let old_local_symbols = replace(
            &mut self.local_symbols,
            callee.locals.iter().map(|l| l.name.0.into_usize()).collect(),
        );
        let old_inlined_return_target = replace(
            &mut self.inlined_return_target,
            Some((exit_block_id, inline_ret_place)),
        );

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

        self.switch_to(exit_block_id);

        let result = match inline_ret_place {
            Some(place) => Operand::Place(place),
            None => Operand::Const(Const::Unit),
        };
        Ok(result)
    }

    #[inline(always)]
    fn runtime_local_uses(&self, id: LocalId) -> bool {
        self.runtime_local_uses.get(id.0 as usize).copied().unwrap_or(false)
    }
}

impl PartialBlock {
    fn new(id: BlockId) -> Self {
        Self {
            id,
            instructions: Vec::new(),
            terminator: None,
        }
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

fn collect_runtime_local_uses(function: &hir::Function) -> Vec<bool> {
    let mut uses = vec![false; function.locals.len()];
    visit_block_runtime_uses(&function.body, &mut uses);

    uses
}

fn visit_block_runtime_uses(block: &hir::Block, uses: &mut [bool]) {
    for statement in &block.statements {
        match statement {
            hir::Statement::Let { init, .. } => {
                if let Some(init) = init {
                    visit_expr_runtime_uses(init, uses);
                }
            }
            hir::Statement::Expr(expr) => visit_expr_runtime_uses(expr, uses),
            hir::Statement::Return(Some(expr)) => visit_expr_runtime_uses(expr, uses),
            hir::Statement::Return(None) => {}
            hir::Statement::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_expr_runtime_uses(condition, uses);
                visit_block_runtime_uses(then_block, uses);
                if let Some(else_block) = else_block {
                    visit_block_runtime_uses(else_block, uses);
                }
            }
            hir::Statement::While { condition, body } => {
                visit_expr_runtime_uses(condition, uses);
                visit_block_runtime_uses(body, uses);
            }
            hir::Statement::Block(block) => visit_block_runtime_uses(block, uses),
        }
    }
}

fn visit_expr_runtime_uses(expr: &Expression, uses: &mut [bool]) {
    match &expr.kind {
        ExpressionKind::Local(id) => uses[id.0 as usize] = true,
        ExpressionKind::Unary { expr, .. } => visit_expr_runtime_uses(expr, uses),
        ExpressionKind::Binary { left, right, .. } => {
            visit_expr_runtime_uses(left, uses);
            visit_expr_runtime_uses(right, uses);
        }
        ExpressionKind::Assign { value, .. } => visit_expr_runtime_uses(value, uses),
        ExpressionKind::Struct { fields, .. } => {
            for (_, value) in fields {
                visit_expr_runtime_uses(value, uses);
            }
        }
        ExpressionKind::Call { args, .. } | ExpressionKind::IntrinsicCall { args, .. } => {
            for arg in args {
                visit_expr_runtime_uses(arg, uses);
            }
        }
        ExpressionKind::MethodCall { receiver, args, .. } => {
            uses[receiver.local.0 as usize] = true;
            for arg in args {
                visit_expr_runtime_uses(arg, uses);
            }
        }
        ExpressionKind::FieldAccess { local, .. } => uses[local.0 as usize] = true,
        ExpressionKind::FieldAssign { local, value, .. } => {
            uses[local.0 as usize] = true;
            visit_expr_runtime_uses(value, uses);
        }
        ExpressionKind::Unit
        | ExpressionKind::Integer(_)
        | ExpressionKind::Float(_)
        | ExpressionKind::String(_)
        | ExpressionKind::Bool(_) => {}
    }
}

fn struct_layouts(structs: &[Struct]) -> Vec<mir::Layout> {
    structs
        .iter()
        .map(|definition| {
            mir::Layout::new(
                definition.size,
                definition.align,
                definition.fields.iter().any(|field| type_contains_float(field.typ, structs)),
            )
        })
        .collect()
}

#[inline]
fn type_contains_float(typ: Type, structs: &[Struct]) -> bool {
    match typ {
        Type::F32 | Type::F64 => true,
        Type::Struct(id) => structs[id.0 as usize]
            .fields
            .iter()
            .any(|field| type_contains_float(field.typ, structs)),
        _ => false,
    }
}
