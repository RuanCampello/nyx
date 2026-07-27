//! Sparse conditional constant propagation over the MIR control flow graph.
//!
//! This is Wegman & Zadeck's algorithm adapted to a non-SSA CFG: because a `ValueId`
//! may be assigned more than once, the lattice is *dense*: a full `ValueId -> Lattice`
//! environment is carried per block edge and joined at merge points (Kildall's
//! formulation) rather than propagated along SSA def-use edges
//!
//! The lattice per value is the usual three-point one, ordered `Top > Const > Bottom`:
//!
//! - `Top`    — no reaching definition yet, optimistically assumed constant
//! - `Const`  — one reaching definition, with this value
//! - `Bottom` — provably not a compile-time constant
//!
//! Iteration is monotonically descending and the lattice has height two, so the
//! worklist always terminates

use crate::{
    hir::FunctionId,
    mir::{
        Block, BlockId, Const, Function, Instruction, InstructionKind, Operand, Terminator,
        opt::{Edit, Program, fold, identical, interpret},
    },
    optimisation::Level,
};

struct Solver<'a> {
    program: &'a Program<'a>,
    function: &'a Function,
    level: Level,
    entry: Vec<Vec<Lattice>>,
    reachable: Vec<bool>,
    escaped: Vec<bool>,
    worklist: Vec<BlockId>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Lattice {
    Top,
    Const(Const),
    Bottom,
}

/// ceiling on the values tracked in one function
///
/// without SSA the environment is dense, so the analysis costs
/// `blocks * values` per round, rustc's equivalent pass caps the same product.
const VALUE_LIMIT: usize = 4096;

pub(super) fn analyse(program: &Program<'_>, index: usize, level: Level) -> Vec<Edit> {
    let function = program.at(index);
    match function.blocks.is_empty() || function.locals.len() > VALUE_LIMIT {
        true => Vec::new(),
        false => {
            let mut solver = Solver::new(program, function, level);
            solver.solve();
            solver.rewrite()
        },
    }
}

pub(super) fn walk(
    program: &Program<'_>,
    index: usize,
    level: Level,
    mut visit: impl FnMut(&Instruction, &[Lattice]),
) {
    let function = program.at(index);
    if function.blocks.is_empty() || function.locals.len() > VALUE_LIMIT {
        return;
    }

    let mut solver = Solver::new(program, function, level);
    solver.solve();

    for (id, block) in function.blocks.iter().enumerate() {
        if !solver.reachable[id] {
            continue;
        }

        let mut state = solver.entry[id].clone();
        for instruction in &block.instructions {
            visit(instruction, &state);
            solver.step(instruction, &mut state);
        }
    }
}

impl<'a> Solver<'a> {
    fn new(program: &'a Program<'a>, function: &'a Function, level: Level) -> Self {
        let values = function.locals.len();
        let blocks = function.blocks.len();

        let mut initial = vec![Lattice::Top; values];
        for (id, _) in &function.params {
            initial[id.0 as usize] = Lattice::Bottom;
        }

        let mut escaped = vec![false; values];
        for block in &function.blocks {
            for instruction in &block.instructions {
                match &instruction.kind {
                    InstructionKind::AddressOf { src, .. } => escaped[src.id.0 as usize] = true,
                    InstructionKind::ElementAddr { base: Operand::Place(place), .. } => {
                        escaped[place.id.0 as usize] = true;
                    },
                    _ => {},
                }
            }
        }

        let mut entry = vec![vec![Lattice::Top; values]; blocks];
        entry[0] = initial;

        let mut reachable = vec![false; blocks];
        reachable[0] = true;

        Self {
            program,
            function,
            level,
            entry,
            reachable,
            escaped,
            worklist: vec![BlockId(0)],
        }
    }

    fn solve(&mut self) {
        while let Some(BlockId(id)) = self.worklist.pop() {
            let block = &self.function.blocks[id as usize];
            let mut state = self.entry[id as usize].clone();

            for instruction in &block.instructions {
                self.step(instruction, &mut state);
            }

            for successor in successors(&block.terminator, &state) {
                self.merge_into(successor, &state);
            }
        }
    }

    fn step(&self, instruction: &Instruction, state: &mut [Lattice]) {
        let dest = instruction.dest.id.0 as usize;
        state[dest] = match self.escaped[dest] {
            true => Lattice::Bottom,
            false => self.evaluate(&instruction.kind, state),
        };
    }

    fn evaluate(&self, kind: &InstructionKind, state: &[Lattice]) -> Lattice {
        match kind {
            InstructionKind::Assign(operand) => self.resolve(*operand, state),
            InstructionKind::Unary { operation, rhs } => {
                lift1(self.resolve(*rhs, state), |value| fold::unary(*operation, value))
            },
            InstructionKind::Binary { operation, lhs, rhs, .. } => {
                lift2(self.resolve(*lhs, state), self.resolve(*rhs, state), |lhs, rhs| {
                    fold::binary(*operation, lhs, rhs)
                })
            },
            InstructionKind::Cast { src, typ } => {
                lift1(self.resolve(*src, state), |value| fold::cast(value, *typ))
            },
            InstructionKind::Call { callee, args } => self.call(*callee, args, state),
            _ => Lattice::Bottom,
        }
    }

    fn call(&self, callee: FunctionId, args: &[Operand], state: &[Lattice]) -> Lattice {
        if self.level < Level::Sane {
            return Lattice::Bottom;
        }

        let mut values = Vec::with_capacity(args.len());
        for &arg in args {
            match self.resolve(arg, state) {
                Lattice::Const(value) => values.push(value),
                Lattice::Top => return Lattice::Top,
                Lattice::Bottom => return Lattice::Bottom,
            }
        }

        match interpret::call(self.program, callee, &values, self.level) {
            Some(value) => Lattice::Const(value),
            None => Lattice::Bottom,
        }
    }

    fn resolve(&self, operand: Operand, state: &[Lattice]) -> Lattice {
        match operand {
            Operand::Const(value) => Lattice::Const(value),
            Operand::Place(place) => state[place.id.0 as usize],
        }
    }

    fn merge_into(&mut self, BlockId(id): BlockId, incoming: &[Lattice]) {
        let id = id as usize;
        let first = !self.reachable[id];
        self.reachable[id] = true;

        let mut changed = first;
        for (slot, &value) in self.entry[id].iter_mut().zip(incoming) {
            let merged = match first {
                true => value,
                false => slot.meet(value),
            };
            if !slot.same(merged) {
                *slot = merged;
                changed = true;
            }
        }

        if changed && !self.worklist.contains(&BlockId(id as u32)) {
            self.worklist.push(BlockId(id as u32));
        }
    }

    fn rewrite(&self) -> Vec<Edit> {
        let mut edits = Vec::new();

        for (id, block) in self.function.blocks.iter().enumerate() {
            if !self.reachable[id] {
                continue;
            }

            let mut state = self.entry[id].clone();
            for (index, instruction) in block.instructions.iter().enumerate() {
                if let Some(kind) = self.rewritten(instruction, &state) {
                    edits.push(Edit::Instruction { block: id, index, kind });
                }
                self.step(instruction, &mut state);
            }

            if let Some(terminator) = rewritten_terminator(block, &state) {
                edits.push(Edit::Terminator { block: id, terminator });
            }
        }

        edits
    }

    fn rewritten(&self, instruction: &Instruction, state: &[Lattice]) -> Option<InstructionKind> {
        let foldable = !matches!(instruction.kind, InstructionKind::Assign(Operand::Const(_)));
        if foldable
            && !self.escaped[instruction.dest.id.0 as usize]
            && let Lattice::Const(value) = self.evaluate(&instruction.kind, state)
        {
            return Some(InstructionKind::Assign(Operand::Const(value)));
        }

        self.substituted(&instruction.kind, state)
    }

    fn substituted(&self, kind: &InstructionKind, state: &[Lattice]) -> Option<InstructionKind> {
        let mut changed = false;
        let mut operand = |operand: Operand| match self.resolve(operand, state) {
            Lattice::Const(value) if matches!(operand, Operand::Place(_)) => {
                changed = true;
                Operand::Const(value)
            },
            _ => operand,
        };

        let rewritten = match kind {
            InstructionKind::Assign(source) => InstructionKind::Assign(operand(*source)),
            InstructionKind::Unary { operation, rhs } => {
                InstructionKind::Unary { operation: *operation, rhs: operand(*rhs) }
            },
            InstructionKind::Binary { operation, lhs, rhs, checked, wrapping } => {
                InstructionKind::Binary {
                    operation: *operation,
                    lhs: operand(*lhs),
                    rhs: operand(*rhs),
                    checked: *checked,
                    wrapping: *wrapping,
                }
            },
            InstructionKind::Cast { src, typ } => {
                InstructionKind::Cast { src: operand(*src), typ: *typ }
            },
            InstructionKind::Call { callee, args } => InstructionKind::Call {
                callee: *callee,
                args: args.iter().map(|&arg| operand(arg)).collect(),
            },
            InstructionKind::Syscall { code, args, returns } => InstructionKind::Syscall {
                code: *code,
                args: args.iter().map(|&arg| operand(arg)).collect(),
                returns: *returns,
            },
            InstructionKind::FieldStore { value, offset } => {
                InstructionKind::FieldStore { value: operand(*value), offset: *offset }
            },
            InstructionKind::FieldLoad { src, offset, typ } => {
                InstructionKind::FieldLoad { src: operand(*src), offset: *offset, typ: *typ }
            },
            InstructionKind::ElementLoad { base, index, bound, stride, typ } => {
                InstructionKind::ElementLoad {
                    base: operand(*base),
                    index: operand(*index),
                    bound: *bound,
                    stride: *stride,
                    typ: *typ,
                }
            },
            InstructionKind::ElementStore { index, bound, value, stride } => {
                InstructionKind::ElementStore {
                    index: operand(*index),
                    bound: *bound,
                    value: operand(*value),
                    stride: *stride,
                }
            },
            InstructionKind::ElementAddr { .. } | InstructionKind::AddressOf { .. } => return None,
        };

        changed.then_some(rewritten)
    }
}

impl Lattice {
    pub(super) const fn constant(self) -> Option<Const> {
        match self {
            Self::Const(value) => Some(value),
            _ => None,
        }
    }

    fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Self::Top, value) | (value, Self::Top) => value,
            (Self::Bottom, _) | (_, Self::Bottom) => Self::Bottom,
            (Self::Const(a), Self::Const(b)) => match identical(a, b) {
                true => Self::Const(a),
                false => Self::Bottom,
            },
        }
    }

    fn same(self, other: Self) -> bool {
        match (self, other) {
            (Self::Top, Self::Top) | (Self::Bottom, Self::Bottom) => true,
            (Self::Const(a), Self::Const(b)) => identical(a, b),
            _ => false,
        }
    }
}

fn lift1(value: Lattice, fold: impl Fn(Const) -> Option<Const>) -> Lattice {
    match value {
        Lattice::Top => Lattice::Top,
        Lattice::Bottom => Lattice::Bottom,
        Lattice::Const(value) => match fold(value) {
            Some(folded) => Lattice::Const(folded),
            None => Lattice::Bottom,
        },
    }
}

fn lift2(lhs: Lattice, rhs: Lattice, fold: impl Fn(Const, Const) -> Option<Const>) -> Lattice {
    match (lhs, rhs) {
        (Lattice::Bottom, _) | (_, Lattice::Bottom) => Lattice::Bottom,
        (Lattice::Top, _) | (_, Lattice::Top) => Lattice::Top,
        (Lattice::Const(lhs), Lattice::Const(rhs)) => match fold(lhs, rhs) {
            Some(folded) => Lattice::Const(folded),
            None => Lattice::Bottom,
        },
    }
}

fn successors(terminator: &Terminator, state: &[Lattice]) -> Vec<BlockId> {
    match terminator {
        Terminator::Jump(target) => vec![*target],
        Terminator::Return(_) => Vec::new(),
        Terminator::Branch { condition, then_block, else_block } => match condition {
            Operand::Const(Const::Bool(true)) => vec![*then_block],
            Operand::Const(Const::Bool(false)) => vec![*else_block],
            Operand::Place(place) => match state[place.id.0 as usize] {
                Lattice::Const(Const::Bool(true)) => vec![*then_block],
                Lattice::Const(Const::Bool(false)) => vec![*else_block],
                Lattice::Top => Vec::new(),
                _ => vec![*then_block, *else_block],
            },
            _ => vec![*then_block, *else_block],
        },
    }
}

fn rewritten_terminator(block: &Block, state: &[Lattice]) -> Option<Terminator> {
    match &block.terminator {
        Terminator::Branch { condition: Operand::Place(place), then_block, else_block } => {
            match state[place.id.0 as usize] {
                Lattice::Const(Const::Bool(true)) => Some(Terminator::Jump(*then_block)),
                Lattice::Const(Const::Bool(false)) => Some(Terminator::Jump(*else_block)),
                _ => None,
            }
        },

        Terminator::Return(Some(Operand::Place(place))) => match state[place.id.0 as usize] {
            Lattice::Const(value) => Some(Terminator::Return(Some(Operand::Const(value)))),
            _ => None,
        },

        _ => None,
    }
}
