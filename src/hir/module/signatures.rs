use super::{ModuleError, graph::ModuleGraph};
use crate::{
    hir::{Declarations, SymbolTable, scope::Scope},
    parser::statement::{Interface, Statement},
};
use std::collections::HashMap;

pub(super) fn build_signatures<'hir>(
    graph: &mut ModuleGraph<'hir>,
    order: &[usize],
    scope: &mut Scope<'hir>,
    symbols: &mut SymbolTable,
    arena: &'hir bumpalo::Bump,
) -> Result<HashMap<String, Interface<'hir>>, ModuleError> {
    let interfaces = collect_interfaces(graph);

    for &idx in order {
        let node = &mut graph.nodes[idx];

        let declarations =
            Declarations::partition(&mut node.statements, |name| interfaces.get(name))?;

        scope.extend(&declarations, symbols, node.in_std, arena)?;
    }

    Ok(interfaces)
}

fn collect_interfaces<'hir>(graph: &ModuleGraph<'hir>) -> HashMap<String, Interface<'hir>> {
    let mut interfaces = HashMap::new();

    for node in &graph.nodes {
        for statement in &node.statements {
            if let Statement::Interface(interface) = statement {
                interfaces.insert(interface.name.to_string(), interface.clone());
            }
        }
    }

    interfaces
}
