use super::{graph::ModuleGraph, ModuleError};
use crate::{
    diagnostic::{self, Diagnostic},
    hir::{scope::Scope, Declarations, SymbolTable},
    parser::statement::{Interface, Statement},
};
use std::collections::HashMap;

pub(super) fn build_signatures(
    graph: &mut ModuleGraph,
    order: &[usize],
    scope: &mut Scope<'static>,
    symbols: &mut SymbolTable,
) -> Result<HashMap<String, Interface<'static>>, ModuleError> {
    let interfaces = collect_interfaces(graph);

    for &idx in order {
        let node = &mut graph.nodes[idx];
        let path = node.path.clone();
        diagnostic::initialise(node.source, path.to_str().unwrap_or("<unknown>"));

        let declarations =
            Declarations::partition(&mut node.statements, |name| interfaces.get(name))
                .map_err(Diagnostic::from)?;

        scope.extend(&declarations, symbols, node.in_std).map_err(Diagnostic::from)?;
    }

    Ok(interfaces)
}

fn collect_interfaces(graph: &ModuleGraph) -> HashMap<String, Interface<'static>> {
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
