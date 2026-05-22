use std::collections::HashMap;
use std::hash::Hash;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hir) enum SortingVisit {
    Unvisited,
    Visiting,
    Visited,
}

pub(in crate::hir) trait SortingStates<NodeId> {
    fn get(&self, node: NodeId) -> SortingVisit;
    fn set(&mut self, node: NodeId, state: SortingVisit);
}

impl SortingStates<usize> for [SortingVisit] {
    #[inline(always)]
    fn get(&self, node: usize) -> SortingVisit {
        self[node]
    }

    #[inline(always)]
    fn set(&mut self, node: usize, state: SortingVisit) {
        self[node] = state;
    }
}

impl<K> SortingStates<K> for HashMap<K, SortingVisit>
where
    K: Eq + Hash,
{
    #[inline(always)]
    fn get(&self, node: K) -> SortingVisit {
        self.get(&node).copied().unwrap_or(SortingVisit::Unvisited)
    }

    #[inline(always)]
    fn set(&mut self, node: K, state: SortingVisit) {
        self.insert(node, state);
    }
}

pub(in crate::hir) fn topological_sort<NodeId, Error, States, GetDeps, OnCycle, OnSorted, DepsIter>(
    nodes: &[NodeId],
    states: &mut States,
    mut get_deps: GetDeps,
    mut on_cycle: OnCycle,
    mut on_sorted: OnSorted,
) -> Result<(), Error>
where
    NodeId: Copy,
    States: SortingStates<NodeId> + ?Sized,
    GetDeps: FnMut(NodeId) -> Result<DepsIter, Error>,
    OnCycle: FnMut(NodeId) -> Error,
    OnSorted: FnMut(NodeId) -> Result<(), Error>,
    DepsIter: IntoIterator<Item = NodeId>,
{
    fn dfs<NodeId, Error, States, GetDeps, OnCycle, OnSorted, DepsIter>(
        node: NodeId,
        states: &mut States,
        get_deps: &mut GetDeps,
        on_cycle: &mut OnCycle,
        on_sorted: &mut OnSorted,
    ) -> Result<(), Error>
    where
        NodeId: Copy,
        States: SortingStates<NodeId> + ?Sized,
        GetDeps: FnMut(NodeId) -> Result<DepsIter, Error>,
        OnCycle: FnMut(NodeId) -> Error,
        OnSorted: FnMut(NodeId) -> Result<(), Error>,
        DepsIter: IntoIterator<Item = NodeId>,
    {
        match states.get(node) {
            SortingVisit::Visited => return Ok(()),
            SortingVisit::Visiting => return Err(on_cycle(node)),
            SortingVisit::Unvisited => {}
        }

        states.set(node, SortingVisit::Visiting);

        let deps = get_deps(node)?;
        for dep in deps {
            dfs(dep, states, get_deps, on_cycle, on_sorted)?;
        }

        states.set(node, SortingVisit::Visited);
        on_sorted(node)?;
        Ok(())
    }

    for &node in nodes {
        dfs(node, states, &mut get_deps, &mut on_cycle, &mut on_sorted)?;
    }

    Ok(())
}
