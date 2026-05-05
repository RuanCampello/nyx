//! HIR -> MIR lowering

use crate::{
    hir::{self, Expression, ExpressionKind, Hir, LocalId},
    mir::{
        self, Block, BlockId, Const, Function, Instruction, InstructionKind, Mir, Operand, Place, Terminator, ValueId,
        error::MirError,
    },
    parser::statement::Type,
};
use lasso::Key;

struct PartialBlock {
    id: BlockId,
    instructions: Vec<Instruction>,
    terminator: Option<Terminator>,
}

#[allow(unused)]
struct FunctionLower<'a> {
    blocks: Vec<PartialBlock>,
    current: usize,
    next: u32,
    local_map: Vec<ValueId>,
    locals: Vec<(ValueId, Type)>,
    return_type: Type,
    symbols: &'a mut Vec<String>,
}

pub fn lower(hir: Hir) -> Result<Mir, MirError> {
    let mut functions = Vec::with_capacity(hir.functions.len());
    let mut symbols = hir.symbols;

    for function in hir.functions {
        functions.push(FunctionLower::run(function, &mut symbols)?);
    }

    Ok(Mir { functions, symbols })
}

impl<'a> FunctionLower<'a> {
    fn run(function: hir::Function, symbols: &'a mut Vec<String>) -> Result<mir::Function, MirError> {
        let id = function.id;
        let name_symbol = function.name.0.into_usize();
        let return_type = function.return_type;
        let n_hir_locals = function.locals.len();

        let mut local_map = vec![ValueId(0); n_hir_locals];
        let mut locals = Vec::with_capacity(n_hir_locals);

        for local in &function.locals {
            let value_id = ValueId(locals.len() as u32);
            local_map[local.id.0 as usize] = value_id;
            locals.push((value_id, local.typ));
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
            return_type,
            next,
            symbols,
        };

        builder.new_block();
        builder.lower_block(&function.body)?;

        if !builder.blocks[builder.current].is_terminated() {
            builder.terminate(Terminator::Return(None));
        }

        let blocks = builder.blocks.into_iter().map(PartialBlock::finalise).collect();

        Ok(Function {
            id,
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
                    let src = self.lower_expr(expr)?;
                    let dest = self.place_for_local(*id, expr.typ);

                    self.emit(dest, InstructionKind::Assign(src));
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
                let id = self.symbols.len();
                let len = s.len();
                self.symbols.push(s.clone());
                Ok(Operand::Const(Const::Str { id, len }))
            }

            ExpressionKind::Local(local_id) => Ok(Operand::Place(self.place_for_local(*local_id, expr.typ))),

            ExpressionKind::Unary { operator, expr: inner } => {
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

            ExpressionKind::Binary { operator, left, right } => {
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

            ExpressionKind::Call { function, args, .. } => {
                let lowered_args = args.iter().map(|a| self.lower_expr(a)).collect::<Result<Vec<_>, _>>()?;

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

                let lowered_args = args.iter().map(|a| self.lower_expr(a)).collect::<Result<Vec<_>, _>>()?;
                // the return value is ignored for those functions
                let dest = self.fresh_temporary(Type::I32);

                match intrinsic {
                    Intrinsic::PrintLn | Intrinsic::Print => {
                        let fd = Operand::Const(Const::Int(1, Type::I32));

                        for arg in lowered_args {
                            let len_op = match arg {
                                Operand::Const(Const::Str { len, .. }) => {
                                    Operand::Const(Const::Int(len as i64, Type::I32))
                                }
                                _ => continue, // ignore non-strings since we formatted at compile time
                            };

                            let dest = self.fresh_temporary(Type::I32);
                            self.emit(
                                dest,
                                InstructionKind::Syscall {
                                    code: SyscallCode::Write,
                                    args: vec![fd, arg, len_op],
                                },
                            );
                        }

                        // add the newline to println :X
                        if *intrinsic == Intrinsic::PrintLn {
                            let id = self.symbols.len();
                            self.symbols.push("\n".to_string());
                            let newline = Operand::Const(Const::Str { id, len: 1 });

                            let dest = self.fresh_temporary(Type::I32);
                            self.emit(
                                dest,
                                InstructionKind::Syscall {
                                    code: SyscallCode::Write,
                                    args: vec![fd, newline, Operand::Const(Const::Int(1, Type::I32))],
                                },
                            );
                        }

                        Ok(Operand::Const(Const::Unit))
                    }

                    Intrinsic::Exit => {
                        self.emit(
                            dest,
                            InstructionKind::Syscall {
                                code: SyscallCode::Exit,
                                args: lowered_args,
                            },
                        );

                        Ok(Operand::Const(Const::Unit))
                    }
                }
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
