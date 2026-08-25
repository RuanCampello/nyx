use crate::{
    hir::ids::{Idx, IndexVec},
    mir::{BlockId, Function, Terminator},
};

pub(in crate::mir) struct CfgEditor<'a, 'hir> {
    function: &'a mut Function<'hir>,
}

/// block indices control transfers to directly from [Terminator]
pub(in crate::mir) fn successors(terminator: &Terminator<'_>) -> Vec<BlockId> {
    match terminator {
        Terminator::Jump(target) => vec![*target],
        Terminator::Branch { then_block, else_block, .. } => vec![*then_block, *else_block],
        Terminator::Return(_) => Vec::new(),
    }
}

/// every block's predecessors, computed once over the whole function
pub(in crate::mir) fn predecessors(
    function: &Function<'_>,
    only: Option<&IndexVec<BlockId, bool>>,
) -> IndexVec<BlockId, Vec<BlockId>> {
    let mut predecessors = IndexVec::from_elem(Vec::new(), function.blocks.len());

    for (id, block) in function.blocks.iter_enumerated() {
        if only.is_some_and(|mask| !mask[id]) {
            continue;
        }

        for successor in successors(&block.terminator) {
            predecessors[successor].push(id);
        }
    }

    predecessors
}

/// blocks reachable from the entry block, by structural DFS over `successors`
pub(in crate::mir) fn reachable(function: &Function<'_>) -> IndexVec<BlockId, bool> {
    let mut reachable = IndexVec::from_elem(false, function.blocks.len());
    let mut stack = vec![BlockId::ENTRY];
    reachable[BlockId::ENTRY] = true;

    while let Some(block) = stack.pop() {
        for successor in successors(&function.blocks[block].terminator) {
            if !reachable[successor] {
                reachable[successor] = true;
                stack.push(successor);
            }
        }
    }

    reachable
}

impl<'a, 'hir> CfgEditor<'a, 'hir> {
    pub(in crate::mir) fn new(function: &'a mut Function<'hir>) -> Self {
        Self { function }
    }

    pub(in crate::mir) fn replace_terminator(
        &mut self,
        block: BlockId,
        terminator: Terminator<'hir>,
    ) {
        self.function.blocks[block].terminator = terminator;
    }

    pub(in crate::mir) fn remove_unreachable(&mut self) -> bool {
        let reachable = reachable(self.function);
        if reachable.iter().all(|live| *live) {
            return false;
        }

        let old = std::mem::take(&mut self.function.blocks);
        let mut remapped = IndexVec::from_elem(None, old.len());
        let mut blocks = IndexVec::with_capacity(old.len());

        for (old_id, block) in old.into_iter().enumerate() {
            let old_id = BlockId::from_usize(old_id);
            if reachable[old_id] {
                remapped[old_id] = Some(blocks.push(block));
            }
        }

        for block in &mut blocks {
            block.terminator.each_target_mut(|target| {
                *target = remapped[*target]
                    .expect("a reachable block cannot target an unreachable block");
            });
        }

        self.function.blocks = blocks;
        true
    }
}
