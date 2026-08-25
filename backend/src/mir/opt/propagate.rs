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
    hir::{FunctionId, ids::IndexVec},
    mir::{
        Block, BlockId, Const, Function, Instruction, InstructionKind, Operand, Terminator, cfg,
        opt::{Edit, Program, fold, identical, interpret},
    },
    optimisation::Level,
};

struct Solver<'a, 'hir> {
    program: &'a Program<'a, 'hir>,
    function: &'a Function<'hir>,
    level: Level,
    entry: IndexVec<BlockId, Vec<Lattice<'hir>>>,
    reachable: IndexVec<BlockId, bool>,
    escaped: Vec<bool>,
    worklist: Vec<BlockId>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Lattice<'hir> {
    Top,
    Const(Const<'hir>),
    Bottom,
}

/// ceiling on the values tracked in one function
///
/// without SSA the environment is dense, so the analysis costs
/// `blocks * values` per round, rustc's equivalent pass caps the same product
const VALUE_LIMIT: usize = 4096;

pub(super) fn analyse<'hir>(
    program: &Program<'_, 'hir>,
    index: usize,
    level: Level,
) -> Vec<Edit<'hir>> {
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

pub(super) fn walk<'hir>(
    program: &Program<'_, 'hir>,
    index: usize,
    level: Level,
    mut visit: impl FnMut(&Instruction<'hir>, &[Lattice<'hir>]),
) {
    let function = program.at(index);
    if function.blocks.is_empty() || function.locals.len() > VALUE_LIMIT {
        return;
    }

    let mut solver = Solver::new(program, function, level);
    solver.solve();

    for (id, block) in function.blocks.iter_enumerated() {
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

/// the environment each block leaves behind, `None` where the block is unreachable
pub(super) fn exit_states<'hir>(
    program: &Program<'_, 'hir>,
    index: usize,
    level: Level,
) -> IndexVec<BlockId, Option<Vec<Lattice<'hir>>>> {
    let function = program.at(index);
    if function.blocks.is_empty() || function.locals.len() > VALUE_LIMIT {
        return IndexVec::new();
    }

    let mut solver = Solver::new(program, function, level);
    solver.solve();

    function
        .blocks
        .iter_enumerated()
        .map(|(id, block)| {
            solver.reachable[id].then(|| {
                let mut state = solver.entry[id].clone();
                for instruction in &block.instructions {
                    solver.step(instruction, &mut state);
                }
                state
            })
        })
        .collect()
}

impl<'a, 'hir> Solver<'a, 'hir> {
    fn new(program: &'a Program<'a, 'hir>, function: &'a Function<'hir>, level: Level) -> Self {
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

        let mut entry = IndexVec::from_elem(vec![Lattice::Top; values], blocks);
        entry[BlockId::ENTRY] = initial;

        let mut reachable = IndexVec::from_elem(false, blocks);
        reachable[BlockId::ENTRY] = true;

        Self {
            program,
            function,
            level,
            entry,
            reachable,
            escaped,
            worklist: vec![BlockId::ENTRY],
        }
    }

    fn solve(&mut self) {
        while let Some(id) = self.worklist.pop() {
            let block = &self.function.blocks[id];
            let mut state = self.entry[id].clone();

            for instruction in &block.instructions {
                self.step(instruction, &mut state);
            }

            for successor in successors(&block.terminator, &state) {
                self.merge_into(successor, &state);
            }
        }
    }

    fn step(&self, instruction: &Instruction<'hir>, state: &mut [Lattice<'hir>]) {
        let dest = instruction.dest.id.0 as usize;
        state[dest] = match self.escaped[dest] {
            true => Lattice::Bottom,
            false => self.evaluate(&instruction.kind, state),
        };
    }

    fn evaluate(&self, kind: &InstructionKind<'hir>, state: &[Lattice<'hir>]) -> Lattice<'hir> {
        use InstructionKind::*;
        match kind {
            Assign(operand) => self.resolve(*operand, state),
            Unary { operation, rhs } => {
                lift1(self.resolve(*rhs, state), |value| fold::unary(*operation, value))
            },
            Binary { operation, lhs, rhs, .. } => {
                lift2(self.resolve(*lhs, state), self.resolve(*rhs, state), |lhs, rhs| {
                    fold::binary(*operation, lhs, rhs)
                })
            },
            Cast { src, typ } => lift1(self.resolve(*src, state), |value| fold::cast(value, *typ)),
            Call { callee, args } => self.call(*callee, args, state),
            _ => Lattice::Bottom,
        }
    }

    fn call(
        &self,
        callee: FunctionId,
        args: &[Operand<'hir>],
        state: &[Lattice<'hir>],
    ) -> Lattice<'hir> {
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

    fn resolve(&self, operand: Operand<'hir>, state: &[Lattice<'hir>]) -> Lattice<'hir> {
        match operand {
            Operand::Const(value) => Lattice::Const(value),
            Operand::Place(place) => state[place.id.0 as usize],
        }
    }

    fn merge_into(&mut self, id: BlockId, incoming: &[Lattice<'hir>]) {
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

        if changed && !self.worklist.contains(&id) {
            self.worklist.push(id);
        }
    }

    fn rewrite(&self) -> Vec<Edit<'hir>> {
        let mut edits = Vec::new();

        for (id, block) in self.function.blocks.iter_enumerated() {
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

    fn rewritten(
        &self,
        instruction: &Instruction<'hir>,
        state: &[Lattice<'hir>],
    ) -> Option<InstructionKind<'hir>> {
        let foldable = !matches!(instruction.kind, InstructionKind::Assign(Operand::Const(_)));
        if foldable
            && !self.escaped[instruction.dest.id.0 as usize]
            && let Lattice::Const(value) = self.evaluate(&instruction.kind, state)
        {
            return Some(InstructionKind::Assign(Operand::Const(value)));
        }

        self.substituted(&instruction.kind, state)
    }

    fn substituted(
        &self,
        kind: &InstructionKind<'hir>,
        state: &[Lattice<'hir>],
    ) -> Option<InstructionKind<'hir>> {
        use InstructionKind::*;

        let mut changed = false;
        let mut operand = |operand: Operand<'hir>| match self.resolve(operand, state) {
            Lattice::Const(value) if matches!(operand, Operand::Place(_)) => {
                changed = true;
                Operand::Const(value)
            },
            _ => operand,
        };

        let rewritten = match kind {
            Assign(source) => Assign(operand(*source)),
            Unary { operation, rhs } => Unary { operation: *operation, rhs: operand(*rhs) },
            Binary { operation, lhs, rhs, overflow } => Binary {
                operation: *operation,
                lhs: operand(*lhs),
                rhs: operand(*rhs),
                overflow: *overflow,
            },
            Cast { src, typ } => Cast { src: operand(*src), typ: *typ },
            Call { callee, args } => Call {
                callee: *callee,
                args: args.iter().map(|&arg| operand(arg)).collect(),
            },
            StaticAddr { id } => StaticAddr { id: *id },
            Syscall { code, args, returns } => Syscall {
                code: *code,
                args: args.iter().map(|&arg| operand(arg)).collect(),
                returns: *returns,
            },
            FieldStore { value, offset } => FieldStore { value: operand(*value), offset: *offset },
            FieldLoad { src, offset, typ } => FieldLoad { src: *src, offset: *offset, typ: *typ },
            ElementLoad { base, index, bound, stride, typ } => ElementLoad {
                base: *base,
                index: operand(*index),
                bound: *bound,
                stride: *stride,
                typ: *typ,
            },
            ElementStore { index, bound, value, stride } => ElementStore {
                index: operand(*index),
                bound: *bound,
                value: operand(*value),
                stride: *stride,
            },
            Select { condition, then_value, else_value } => Select {
                condition: operand(*condition),
                then_value: operand(*then_value),
                else_value: operand(*else_value),
            },
            ElementAddr { .. } | AddressOf { .. } => return None,
        };

        changed.then_some(rewritten)
    }
}

impl<'hir> Lattice<'hir> {
    pub(super) const fn constant(self) -> Option<Const<'hir>> {
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

fn lift1<'hir>(
    value: Lattice<'hir>,
    fold: impl Fn(Const<'hir>) -> Option<Const<'hir>>,
) -> Lattice<'hir> {
    match value {
        Lattice::Top => Lattice::Top,
        Lattice::Bottom => Lattice::Bottom,
        Lattice::Const(value) => match fold(value) {
            Some(folded) => Lattice::Const(folded),
            None => Lattice::Bottom,
        },
    }
}

fn lift2<'hir>(
    lhs: Lattice<'hir>,
    rhs: Lattice<'hir>,
    fold: impl Fn(Const<'hir>, Const<'hir>) -> Option<Const<'hir>>,
) -> Lattice<'hir> {
    match (lhs, rhs) {
        (Lattice::Bottom, _) | (_, Lattice::Bottom) => Lattice::Bottom,
        (Lattice::Top, _) | (_, Lattice::Top) => Lattice::Top,
        (Lattice::Const(lhs), Lattice::Const(rhs)) => match fold(lhs, rhs) {
            Some(folded) => Lattice::Const(folded),
            None => Lattice::Bottom,
        },
    }
}

fn successors(terminator: &Terminator<'_>, state: &[Lattice<'_>]) -> Vec<BlockId> {
    let Terminator::Branch { condition, then_block, else_block } = terminator else {
        return cfg::successors(terminator);
    };

    match condition {
        Operand::Const(Const::Bool(true)) => vec![*then_block],
        Operand::Const(Const::Bool(false)) => vec![*else_block],
        Operand::Place(place) => match state[place.id.0 as usize] {
            Lattice::Const(Const::Bool(true)) => vec![*then_block],
            Lattice::Const(Const::Bool(false)) => vec![*else_block],
            Lattice::Top => Vec::new(),
            _ => vec![*then_block, *else_block],
        },
        _ => vec![*then_block, *else_block],
    }
}

fn rewritten_terminator<'hir>(
    block: &Block<'hir>,
    state: &[Lattice<'hir>],
) -> Option<Terminator<'hir>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TargetArch, hir, mir, optimisation, parser::Parser};

    #[test]
    fn constant_propagation_preserves_enum_field_load_origins() {
        let source = r#"
            enum Choice { First, Second }

            inline const fn number(choice: Choice): u8 {
                match choice {
                    Choice::First -> 1,
                    Choice::Second -> 2,
                }
            }

            fn main(): u8 { number(Choice::First) }
        "#;
        let arena = bumpalo::Bump::new();
        let statements = Parser::new(source).parse().unwrap();
        let hir = hir::lower(statements, &arena).unwrap();
        let mut mir = mir::lower(hir).unwrap();

        optimisation::set(Level::Sane);
        mir::optimise(&mut mir, TargetArch::X86_64);
        optimisation::set(Level::Debug);

        let field_loads = mir
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction.kind {
                InstructionKind::FieldLoad { src, .. } => Some(src),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(!field_loads.is_empty(), "expected enum tag field loads");
        assert!(field_loads.iter().all(|src| matches!(src, Operand::Place(_))));
    }
}
