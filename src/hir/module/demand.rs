use super::{ModuleError, graph::ModuleGraph};
use crate::{
    diagnostic::{self, Diagnostic},
    hir::{
        self, Declarations, FunctionId, SymbolTable,
        scope::{self, Scope},
    },
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

impl DemandSet {
    pub(super) fn contains(&self, id: FunctionId) -> bool {
        self.needed.contains(&id)
    }

    fn insert(&mut self, id: FunctionId) -> bool {
        self.needed.insert(id)
    }
}

pub(super) fn lower_reachable<'src>(
    graph: &mut ModuleGraph<'src>,
    order: &[usize],
    interfaces: &HashMap<String, Interface<'src>>,
    scope: &Scope<'static>,
    symbols: &mut SymbolTable,
) -> Result<Vec<crate::hir::Function>, ModuleError> {
    let demand = build_demand(graph, order, interfaces, scope, symbols)?;
    let mut functions = Vec::new();

    for &idx in order {
        let node = &mut graph.nodes[idx];
        diagnostic::initialise(node.source, node.path.to_str().unwrap_or("<unknown>"));

        let declarations =
            Declarations::partition(&mut node.statements, |name| interfaces.get(name))
                .map_err(Diagnostic::from)?;

        let mut lowered = scope
            .lower_matching_functions(&declarations, symbols, node.in_std, |id| demand.contains(id))
            .map_err(Diagnostic::from)?;

        functions.append(&mut lowered);
    }

    Ok(functions)
}

fn build_demand<'src>(
    graph: &mut ModuleGraph<'src>,
    order: &[usize],
    interfaces: &HashMap<String, Interface<'src>>,
    scope: &Scope<'static>,
    symbols: &mut SymbolTable,
) -> Result<DemandSet, ModuleError> {
    let declarations = collect_functions(graph, order, interfaces, scope, symbols)?;
    let main = symbols
        .get_id(&scope.mangler.item("main"))
        .and_then(|symbol| scope.functions.get(&symbol).copied());

    let mut demand = DemandSet::default();
    let mut stack = Vec::new();

    if let Some(main) = main {
        demand.insert(main);
        stack.push(main);
    }

    while let Some(id) = stack.pop() {
        let Some(function) = declarations.get(&id) else {
            continue;
        };

        let mut found = Vec::new();
        let mut visitor = ReachabilityVisitor { scope, symbols, found: &mut found };
        use crate::parser::visitor::Visitor;
        visitor.visit_block(&function.body);

        for callee in found {
            if demand.insert(callee) {
                stack.push(callee);
            }
        }
    }

    Ok(demand)
}

fn collect_functions<'a, 'src>(
    graph: &'a mut ModuleGraph<'src>,
    order: &[usize],
    interfaces: &HashMap<String, Interface<'src>>,
    scope: &Scope<'static>,
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
        Some(impl_type) => {
            if let Some(interface) = find_interface_for_method(function, scope, symbols) {
                let name = scope.mangler.interface_item(impl_type, &interface, function.name);
                symbols.get_id(&name).and_then(|sym| scope.functions.get(&sym).copied())
            } else {
                let name = scope.mangler.scoped_item(impl_type, function.name);
                symbols.get_id(&name).and_then(|sym| scope.functions.get(&sym).copied())
            }
        }
        None => {
            let name = scope.mangler.item(function.name);
            symbols.get_id(&name).and_then(|sym| scope.functions.get(&sym).copied())
        }
    }
}

fn find_interface_for_method(
    function: &Function<'_>,
    scope: &Scope<'_>,
    symbols: &SymbolTable,
) -> Option<String> {
    let impl_type = function.impl_type?;
    let receiver_type = match scope::resolve_primitive_type(impl_type) {
        Some(primitive) => primitive,
        _ => {
            let struct_symbol = symbols.get_id(impl_type)?;
            let struct_id = scope.struct_map.get(&struct_symbol)?;
            hir::Type::Struct(*struct_id)
        }
    };
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

struct ReachabilityVisitor<'a> {
    scope: &'a Scope<'static>,
    symbols: &'a SymbolTable,
    found: &'a mut Vec<FunctionId>,
}

impl<'a, 'i> crate::parser::visitor::Visitor<'i> for ReachabilityVisitor<'a> {
    fn visit_expression(&mut self, expr: &Expression<'i>) {
        match expr {
            Expression::Call { callee, args, .. } => {
                match callee.as_ref() {
                    Expression::Identifier(name, _) => {
                        if let Some(id) = resolve_top_level(name, self.scope, self.symbols) {
                            self.found.push(id);
                        }
                    }
                    Expression::Field { .. } => {
                        self.found.extend(self.scope.methods.values().copied())
                    }
                    _ => self.visit_expression(callee),
                }
                for arg in args {
                    self.visit_expression(arg);
                }
            }
            Expression::QualifiedCall { qualifier, name, args, .. } => {
                if let Some(id) = resolve_qualified(qualifier, name, self.scope, self.symbols) {
                    self.found.push(id);
                }
                for arg in args {
                    self.visit_expression(arg);
                }
            }
            Expression::TypeIntrinsic { kind, qualifier, .. } => {
                let name: &str = kind.into();
                let id = qualifier
                    .and_then(|q| resolve_qualified(q, name, self.scope, self.symbols))
                    .or_else(|| resolve_top_level(name, self.scope, self.symbols));
                if let Some(id) = id {
                    self.found.push(id);
                }
            }
            _ => visitor::walk_expression(self, expr),
        }
    }
}

#[inline]
fn resolve_top_level(name: &str, scope: &Scope<'_>, symbols: &SymbolTable) -> Option<FunctionId> {
    let symbol = symbols.get_id(&scope.mangler.item(name))?;
    scope.functions.get(&symbol).copied()
}

#[inline]
fn resolve_qualified(
    qualifier: &str,
    name: &str,
    scope: &Scope<'_>,
    symbols: &SymbolTable,
) -> Option<FunctionId> {
    let scoped = symbols.get_id(&scope.mangler.scoped_item(qualifier, name));
    scoped
        .and_then(|symbol| scope.functions.get(&symbol).copied())
        .or_else(|| {
            let receiver_type = match scope::resolve_primitive_type(qualifier) {
                Some(primitive) => primitive,
                _ => {
                    let struct_symbol = symbols.get_id(qualifier)?;
                    let struct_id = scope.struct_map.get(&struct_symbol)?;
                    hir::Type::Struct(*struct_id)
                }
            };

            scope.interface_impls.iter().filter(|&&(t, _)| t == receiver_type).find_map(
                |&(_, interface_sym)| {
                    let interface_name = symbols.get(interface_sym);
                    let mangled = scope.mangler.interface_item(qualifier, interface_name, name);
                    symbols
                        .get_id(&mangled)
                        .and_then(|symbol| scope.functions.get(&symbol).copied())
                },
            )
        })
        .or_else(|| resolve_top_level(name, scope, symbols))
}
