use super::{ModuleError, graph::ModuleGraph};
use crate::hir::{Declarations, SymbolTable, scope::Scope};

pub(super) fn build_signatures<'hir>(
    graph: &ModuleGraph<'hir>,
    declarations: &[Declarations<'_, 'hir>],
    order: &[usize],
    scope: &mut Scope<'hir>,
    symbols: &mut SymbolTable,
    arena: &'hir bumpalo::Bump,
) -> Result<(), ModuleError> {
    for &idx in order {
        scope.extend(&declarations[idx], symbols, graph.nodes[idx].in_std, arena)?;
    }

    Ok(())
}
