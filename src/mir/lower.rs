//! HIR -> MIR lowering

use crate::{
    hir::{self, Expression, ExpressionKind, Hir, LocalId, Type},
    mir::{
        self, Block, BlockId, Const, Function, Instruction, InstructionKind, Mir, Operand, Place,
        Terminator, ValueId, error::MirError,
    },
};

struct PartialBlock {
    id: BlockId,
    instructions: Vec<Instruction>,
    terminator: Option<Terminator>,
}

struct FunctionLower {
    blocks: Vec<PartialBlock>,
    current: usize,
    next: u32,
    local_map: Vec<ValueId>,
    locals: Vec<(ValueId, Type)>,
    return_type: Type,
}

pub fn lower(hir: Hir) -> Result<Mir, MirError> {
    let functions = hir
        .functions
        .into_iter()
        .map(FunctionLower::run)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Mir {
        functions,
        symbols: hir.symbols,
    })
}

impl FunctionLower {
    fn run(function: hir::Function) -> Result<mir::Function, MirError> {
        let id = function.id;
        let return_type = function.return_type;
        let n_hir_locals = function.locals.len();

        let mut local_map = vec![ValueId(0); n_hir_locals];
        let mut locals = Vec::with_capacity(n_hir_locals);

        for local in &function.locals {
            let value_id = ValueId(locals.len() as u32);
            local_map[local.id.0 as usize] = value_id;
            locals.push((value_id, local.typ));
        }

        let next = locals.len() as u32;

        let mut builder = FunctionLower {
            blocks: Vec::new(),
            current: 0,
            local_map,
            locals,
            return_type,
            next,
        };

        builder.new_block();
        builder.lower_block(&function.body)?;

        if !builder.blocks[builder.current].is_terminated() {
            builder.terminate(Terminator::Return(None));
        }

        let blocks = builder
            .blocks
            .into_iter()
            .map(PartialBlock::finalise)
            .collect();

        Ok(Function {
            id,
            blocks,
            return_type,
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
            Stmt::Let { .. } => {
                // the locals ValueId was pre-allocated in `run` no instruction needed
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
            ExpressionKind::String(_) => Ok(Operand::Const(Const::Unit)),

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
                let src = self.lower_expr(value)?;
                let dest = self.place_for_local(*target, expr.typ);

                self.emit(dest, InstructionKind::Assign(src));

                Ok(Operand::Place(dest))
            }

            ExpressionKind::Call { function, args } => {
                let lowered_args = args
                    .iter()
                    .map(|a| self.lower_expr(a))
                    .collect::<Result<Vec<_>, _>>()?;

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
        self.blocks[self.current]
            .instructions
            .push(Instruction { dest, kind });
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
