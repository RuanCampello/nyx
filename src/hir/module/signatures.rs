use super::{ModuleError, graph::ModuleGraph};
use crate::{
    diagnostic::{self, Diagnostic},
    hir::{Declarations, SymbolTable, monomorph, scope::Scope},
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

    // FIXME: we can do this better in the future but be it for now :/

    let mut templates = monomorph::Templates::default();
    for &idx in order {
        let node = &mut graph.nodes[idx];
        monomorph::extract_templates(&mut node.statements, &mut templates);
    }

    // run monomorphization on each non-std module before scope-extending
    for &idx in order {
        let node = &mut graph.nodes[idx];
        if !node.in_std {
            monomorph::monomorphise_with_templates(&mut node.statements, &templates, arena)
                .map_err(Diagnostic::from)?;
        }
    }

    for &idx in order {
        let node = &mut graph.nodes[idx];
        let path = node.path.clone();
        diagnostic::initialise(node.source, path.to_str().unwrap_or("<unknown>"));

        let declarations =
            Declarations::partition(&mut node.statements, |name| interfaces.get(name))
                .map_err(Diagnostic::from)?;

        scope
            .extend(&declarations, symbols, node.in_std, arena)
            .map_err(Diagnostic::from)?;
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
