//! HIR -> MIR lowering

use crate::{
    hir::{self, Expression, ExpressionKind, Hir, LocalId, Struct, StructId, SymbolId, Type},
    mir::{
        self, Block, BlockId, Const, Function, Instruction, InstructionKind, Mir, Operand, Place,
        Terminator, ValueId, error::MirError,
    },
};
use lasso::Key;

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
    struct_fields: Vec<Option<Vec<ValueId>>>,
    local_struct_id: Vec<Option<StructId>>,
    local_symbols: Vec<usize>,
    constant_locals: Vec<Option<String>>,
    runtime_local_uses: Vec<bool>,
}

pub fn lower(hir: Hir) -> Result<Mir, MirError> {
    let mut functions = Vec::with_capacity(hir.functions.len());
    let mut strings = Vec::new();
    let symbols = hir.symbols;
    let structs = hir.structs;

    for function in hir.functions {
        functions.push(FunctionLower::run(
            function,
            &symbols,
            &structs,
            &mut strings,
        )?);
    }

    Ok(Mir {
        functions,
        symbols,
        strings,
    })
}

impl<'a> FunctionLower<'a> {
    fn run(
        function: hir::Function,
        symbols: &'a [String],
        structs: &'a [Struct],
        strings: &'a mut Vec<String>,
    ) -> Result<mir::Function, MirError> {
        let id = function.id;
        let intrinsic = function.intrinsic;
        let name_symbol = function.name.0.into_usize();
        let return_type = function.return_type;
        let n_hir_locals = function.locals.len();

        let mut local_map = vec![ValueId(0); n_hir_locals];
        let mut local_symbols = vec![0; n_hir_locals];
        let mut locals = Vec::with_capacity(n_hir_locals);
        let mut struct_fields = vec![None; n_hir_locals];
        let mut local_struct_id = vec![None; n_hir_locals];

        for local in &function.locals {
            match local.typ {
                Type::Struct(id) => {
                    let definition = &structs[id.0 as usize];
                    let ids: Vec<_> = definition
                        .fields
                        .iter()
                        .map(|f| {
                            let id = ValueId(locals.len() as u32);
                            locals.push((id, f.typ));
                            id
                        })
                        .collect();

                    local_map[local.id.0 as usize] = ids[0];
                    local_symbols[local.id.0 as usize] = local.name.0.into_usize();
                    local_struct_id[local.id.0 as usize] = Some(id);
                    struct_fields[local.id.0 as usize] = Some(ids);
                }

                _ => {
                    let value_id = ValueId(locals.len() as u32);
                    local_map[local.id.0 as usize] = value_id;
                    local_symbols[local.id.0 as usize] = local.name.0.into_usize();
                    locals.push((value_id, local.typ));
                }
            }
        }

        let params = function
            .params
            .iter()
            .map(|param| {
                let id = local_map[param.id.0 as usize];
                (id, param.typ)
            })
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
            struct_fields,
            local_struct_id,
            local_symbols,
            constant_locals: vec![None; n_hir_locals],
            runtime_local_uses: collect_runtime_local_uses(&function),
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
                self.terminate(Terminator::Return(operand));
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
                let dest = self.fresh_temporary(expr.typ);

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
                let dest = self.fresh_temporary(expr.typ);

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
                let lowered_args =
                    args.iter().map(|a| self.lower_expr(a)).collect::<Result<Vec<_>, _>>()?;

                let dest = self.fresh_temporary(expr.typ);

                self.emit(
                    dest,
                    InstructionKind::Call {
                        callee: *function,
                        args: lowered_args,
                    },
                );

                Ok(Operand::Place(dest))
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
                let definition = &self.structs[id.0 as usize];

                for (field, value) in fields {
                    let src = self.lower_expr(value)?;
                    let layout_idx = definition
                        .fields
                        .iter()
                        .position(|f| &f.name == field)
                        .expect("field must exist");

                    let typ = definition.fields[layout_idx].typ;
                    let place = self.fresh_temporary(typ);
                    self.emit(place, InstructionKind::Assign(src));
                }

                Ok(Operand::Const(Const::Unit))
            }

            ExpressionKind::FieldAccess { local, field, typ } => {
                let id = self.field_value_id(local, *field);
                Ok(Operand::Place(Place { id, typ: *typ }))
            }

            ExpressionKind::FieldAssign {
                local,
                field,
                value,
            } => {
                let src = self.lower_expr(value)?;
                let id = self.field_value_id(local, *field);
                let dest = Place { typ: value.typ, id };

                self.emit(dest, InstructionKind::Assign(src));

                Ok(Operand::Place(dest))
            }
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

    #[inline(always)]
    fn runtime_local_uses(&self, id: LocalId) -> bool {
        self.runtime_local_uses.get(id.0 as usize).copied().unwrap_or(false)
    }

    fn field_value_id(&self, local_id: &LocalId, field: SymbolId) -> ValueId {
        let sid =
            self.local_struct_id[local_id.0 as usize].expect("field access on non-struct local");

        let field_vids = self.struct_fields[local_id.0 as usize]
            .as_ref()
            .expect("struct local must have field slots");

        let layout_idx = self.structs[sid.0 as usize]
            .fields
            .iter()
            .position(|f| f.name == field)
            .expect("field not found in struct definition");

        field_vids[layout_idx]
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
        ExpressionKind::Local(id) => {
            uses[id.0 as usize] = true;
        }
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
