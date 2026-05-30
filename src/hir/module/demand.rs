use super::{ModuleError, graph::ModuleGraph};
use crate::{
    diagnostic::{self, Diagnostic},
    hir::{self, Declarations, FunctionId, SymbolTable, index_vec::IndexVec, scope::Scope},
    parser::{
        expression::Expression,
        statement::{Function, Interface},
        visitor,
    },
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub(super) struct DemandSet {
    needed: HashSet<FunctionId>,
}

struct ReachabilityVisitor<'a, 'hir> {
    scope: &'a Scope<'hir>,
    symbols: &'a SymbolTable,
    found: &'a mut Vec<FunctionId>,
}

impl DemandSet {
    pub(super) fn contains(&self, id: FunctionId) -> bool {
        self.needed.contains(&id)
    }

    fn insert(&mut self, id: FunctionId) -> bool {
        self.needed.insert(id)
    }
}

pub(super) fn lower_reachable<'hir, 'src>(
    graph: &mut ModuleGraph<'src>,
    order: &[usize],
    interfaces: &HashMap<String, Interface<'src>>,
    scope: &Scope<'hir>,
    symbols: &mut SymbolTable,
    arena: &'hir bumpalo::Bump,
) -> Result<IndexVec<FunctionId, hir::Function<'hir>>, ModuleError> {
    let demand = build_demand(graph, order, interfaces, scope, symbols)?;
    let mut functions = IndexVec::new();

    for &idx in order {
        let node = &mut graph.nodes[idx];
        diagnostic::initialise(node.source, node.path.to_str().unwrap_or("<unknown>"));

        let declarations =
            Declarations::partition(&mut node.statements, |name| interfaces.get(name))
                .map_err(Diagnostic::from)?;

        let mut lowered = scope
            .lower_matching_functions(
                &declarations,
                symbols,
                node.in_std,
                |id| demand.contains(id),
                arena,
            )
            .map_err(Diagnostic::from)?;

        functions.append(&mut lowered);
    }

    Ok(functions)
}

fn build_demand<'hir, 'src>(
    graph: &mut ModuleGraph<'src>,
    order: &[usize],
    interfaces: &HashMap<String, Interface<'src>>,
    scope: &Scope<'hir>,
    symbols: &mut SymbolTable,
) -> Result<DemandSet, ModuleError> {
    let declarations = collect_functions(graph, order, interfaces, scope, symbols)?;
    let main = scope.resolve_function(symbols, |m| m.item("main"));

    let mut demand = DemandSet::default();
    let mut stack = Vec::new();

    if let Some(main) = main {
        demand.insert(main);
        stack.push(main);
    }

    while let Some(id) = stack.pop() {
        use visitor::Visitor;

        let Some(function) = declarations.get(&id) else {
            continue;
        };

        let mut found = Vec::new();
        let mut visitor = ReachabilityVisitor { scope, symbols, found: &mut found };
        visitor.visit_block(&function.body);

        for callee in found {
            if demand.insert(callee) {
                stack.push(callee);
            }
        }
    }

    Ok(demand)
}

fn collect_functions<'a, 'hir, 'src>(
    graph: &'a mut ModuleGraph<'src>,
    order: &[usize],
    interfaces: &HashMap<String, Interface<'src>>,
    scope: &Scope<'hir>,
    symbols: &SymbolTable,
) -> Result<HashMap<FunctionId, Function<'src>>, ModuleError> {
    let mut functions = HashMap::new();

    for &idx in order {
        let node = &mut graph.nodes[idx];
        let declarations =
            Declarations::partition(&mut node.statements, |name| interfaces.get(name))
                .map_err(Diagnostic::from)?;

        for function in declarations.functions() {
            if let Some(id) = lookup_declaration_id(function, scope, symbols) {
                functions.insert(id, function.clone());
            }
        }
    }

    Ok(functions)
}

fn lookup_declaration_id(
    function: &Function<'_>,
    scope: &Scope<'_>,
    symbols: &SymbolTable,
) -> Option<FunctionId> {
    match function.impl_type {
        Some(impl_type) => match find_interface_for_method(function, scope, symbols) {
            Some(interface) => scope.resolve_function(symbols, |m| {
                m.interface_item(impl_type, &interface, function.name)
            }),
            None => scope.resolve_function(symbols, |m| m.scoped_item(impl_type, function.name)),
        },
        None => scope.resolve_function(symbols, |m| m.item(function.name)),
    }
}

fn find_interface_for_method(
    function: &Function<'_>,
    scope: &Scope<'_>,
    symbols: &SymbolTable,
) -> Option<String> {
    let impl_type = function.impl_type?;
    let receiver_type = scope.lookup_named_type(impl_type, symbols)?;
    let method_name = symbols.get_id(function.name)?;

    scope.interface_impls.iter().filter(|&&(t, _)| t == receiver_type).find_map(
        |&(_, interface_sym)| {
            let interface = scope.interfaces.get(&interface_sym)?;
            interface
                .methods
                .iter()
                .any(|method| method.name == method_name)
                .then(|| symbols.get(interface_sym).to_string())
        },
    )
}

impl<'a, 'i, 'hir> visitor::Visitor<'i> for ReachabilityVisitor<'a, 'hir> {
    fn visit_expression(&mut self, expr: &Expression<'i>) {
        match expr {
            Expression::Call { callee, args, .. } => {
                match callee.as_ref() {
                    Expression::Identifier(name, _) => {
                        if let Some(id) = self.scope.resolve_function_call(None, name, self.symbols)
                        {
                            self.found.push(id);
                        }
                    },
                    Expression::Field { .. } => {
                        self.found.extend(self.scope.methods.values().copied())
                    },
                    _ => self.visit_expression(callee),
                }
                for arg in args {
                    self.visit_expression(arg);
                }
            },
            Expression::QualifiedCall { qualifier, name, args, .. } => {
                if let Some(id) =
                    self.scope.resolve_function_call(Some(qualifier), name, self.symbols)
                {
                    self.found.push(id);
                }
                for arg in args {
                    self.visit_expression(arg);
                }
            },
            Expression::TypeIntrinsic { kind, qualifier, .. } => {
                let name: &str = kind.into();
                if let Some(id) = self.scope.resolve_function_call(*qualifier, name, self.symbols) {
                    self.found.push(id);
                }
            },
            _ => visitor::walk_expression(self, expr),
        }
    }
}
