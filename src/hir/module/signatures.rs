use super::{ModuleError, graph::ModuleGraph};
use crate::{
    diagnostic::{self, Diagnostic},
    hir::{Declarations, SymbolTable, monomorph, scope::Scope},
    parser::statement::{Interface, Statement},
};
use std::collections::HashMap;

pub(super) fn build_signatures<'src>(
    graph: &mut ModuleGraph<'src>,
    order: &[usize],
    scope: &mut Scope<'static>,
    symbols: &mut SymbolTable,
    arena: &'src bumpalo::Bump,
) -> Result<HashMap<String, Interface<'src>>, ModuleError> {
    let interfaces = collect_interfaces(graph);

    // run monomorphization on each non-std module before scope-extending
    for &idx in order {
        let node = &mut graph.nodes[idx];
        if !node.in_std {
            monomorph::monomorphize(&mut node.statements, arena).map_err(Diagnostic::from)?;
        }
    }

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

fn collect_interfaces<'src>(graph: &ModuleGraph<'src>) -> HashMap<String, Interface<'src>> {
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
