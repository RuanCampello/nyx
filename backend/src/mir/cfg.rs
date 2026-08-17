use crate::mir::{Function, Terminator};

/// block indices control transfers to directly from [Terminator]
pub(in crate::mir) fn successors(terminator: &Terminator<'_>) -> Vec<usize> {
    match terminator {
        Terminator::Jump(target) => vec![target.0 as usize],
        Terminator::Branch { then_block, else_block, .. } => {
            vec![then_block.0 as usize, else_block.0 as usize]
        },
        Terminator::Return(_) => Vec::new(),
    }
}

/// every block's predecessors, computed once over the whole function
pub(in crate::mir) fn predecessors(
    function: &Function<'_>,
    only: Option<&[bool]>,
) -> Vec<Vec<usize>> {
    let mut predecessors = vec![Vec::new(); function.blocks.len()];

    for (id, block) in function.blocks.iter().enumerate() {
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
pub(in crate::mir) fn reachable(function: &Function<'_>) -> Vec<bool> {
    let mut reachable = vec![false; function.blocks.len()];
    let mut stack = vec![0];
    reachable[0] = true;

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
