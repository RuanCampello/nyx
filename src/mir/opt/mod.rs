//! MIR optimisation passes.
//!
//! Passes run on the CFG after lowering and before instruction selection, so every
//! backend benefits from them equally.
//!
//! The pipeline is the conventional one: conditional constant propagation (which folds
//! and evaluates `const fn` calls as it goes), then full unrolling of constant-trip-count
//! loops, then propagation again over the now straight-line code.

use crate::{
    hir::FunctionId,
    mir::{Const, Function, InstructionKind, Mir, Terminator},
    optimisation::{self, Level},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{Hash, Hasher},
};

/// The whole program, with `FunctionId` lookup resolved once.
pub(crate) struct Program<'a> {
    mir: &'a Mir,
    by_id: HashMap<FunctionId, usize>,
    cache: &'a Cache,
}

/// One memoised compile-time evaluation.
///
/// `Const` cannot key a map directly: `f64` is neither `Eq` nor `Hash`, and under IEEE
/// comparison a `NaN` argument would never match itself. Both halves therefore work on
/// the bit pattern, for the same reason [identical] does
struct Key {
    callee: FunctionId,
    args: Box<[Const]>,
}

/// A rewrite the analysis justified, applied once its borrow of the program ends
enum Edit {
    Instruction { block: usize, index: usize, kind: InstructionKind },
    Terminator { block: usize, terminator: Terminator },
}

/// passes re-enable each other, but in practice everything settles in two or three
/// rounds, a cap keeps a pathological program from pinning the compiler
const MAX_ROUNDS: u32 = 4;

/// evaluating a `const fn` is a pure function of the callee and its arguments, so rustc
/// models it as a memoised query rather than a fresh interpretation each time.
type Cache = RefCell<HashMap<Key, Option<Const>>>;

mod fold;
mod interpret;
mod panics;
mod propagate;
mod unroll;

pub(crate) use panics::known_panics;

pub fn optimise(mir: &mut Mir) {
    let level = optimisation::get();
    if level < Level::Sane {
        return;
    }

    let cache = Cache::default();

    for _ in 0..MAX_ROUNDS {
        let mut changed = false;

        for index in 0..mir.functions.len() {
            let edits = {
                let program = Program::new(mir, &cache);
                propagate::analyse(&program, index, level)
            };

            changed |= apply(&mut mir.functions[index], edits);
        }

        if level >= Level::Max {
            for function in &mut mir.functions {
                changed |= unroll::run(function);
            }
        }

        if !changed {
            break;
        }
    }
}

fn apply(function: &mut Function, edits: Vec<Edit>) -> bool {
    let changed = !edits.is_empty();

    for edit in edits {
        match edit {
            Edit::Instruction { block, index, kind } => {
                function.blocks[block].instructions[index].kind = kind;
            },
            Edit::Terminator { block, terminator } => {
                function.blocks[block].terminator = terminator;
            },
        }
    }

    changed
}

/// bit-level identity, unlike `PartialEq`, which inherits `f64`'s, so a lattice built on it would never settle and
/// a cache keyed on it would never hit
/// Both need a reflexive comparison
fn identical(a: Const, b: Const) -> bool {
    match (a, b) {
        (Const::Float(a, x), Const::Float(b, y)) => a.to_bits() == b.to_bits() && x == y,
        _ => a == b,
    }
}

impl<'a> Program<'a> {
    fn new(mir: &'a Mir, cache: &'a Cache) -> Self {
        let by_id = mir.functions.iter().enumerate().map(|(index, f)| (f.id, index)).collect();
        Self { mir, by_id, cache }
    }

    fn function(&self, id: FunctionId) -> Option<&'a Function> {
        self.by_id.get(&id).map(|&index| &self.mir.functions[index])
    }

    fn at(&self, index: usize) -> &'a Function {
        &self.mir.functions[index]
    }

    fn cached(&self, key: &Key) -> Option<Option<Const>> {
        self.cache.borrow().get(key).copied()
    }

    fn memoise(&self, key: Key, result: Option<Const>) {
        self.cache.borrow_mut().insert(key, result);
    }
}

impl Key {
    fn new(callee: FunctionId, args: &[Const]) -> Self {
        Self { callee, args: args.into() }
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.callee == other.callee
            && self.args.len() == other.args.len()
            && self.args.iter().zip(&other.args).all(|(&a, &b)| identical(a, b))
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.callee.hash(state);
        for argument in &self.args {
            match argument {
                Const::Int(value, typ) => (0, *value as u64, typ).hash(state),
                Const::Float(value, typ) => (1, value.to_bits(), typ).hash(state),
                Const::Bool(value) => (2, u64::from(*value)).hash(state),
                Const::Str { id, len } => (3, *id, *len).hash(state),
                Const::Unit => 4.hash(state),
            }
        }
    }
}
