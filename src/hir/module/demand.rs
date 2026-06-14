use super::{ModuleError, graph::ModuleGraph};
use crate::{
    hir::{self, Declarations, FunctionId, SymbolTable, index_vec::IndexVec, scope::Scope},
    parser::{
        expression::{BinaryOperator, Expression},
        statement::Function,
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

pub(super) fn lower_reachable<'hir, 'src>(
    graph: &ModuleGraph<'src>,
    declarations: &[Declarations<'_, 'src>],
    order: &[usize],
    scope: &mut Scope<'hir>,
    symbols: &mut SymbolTable,
    arena: &'hir bumpalo::Bump,
) -> Result<IndexVec<FunctionId, hir::Function<'hir>>, ModuleError>
where
    'src: 'hir,
{
    let demand = build_demand(graph, declarations, order, scope, symbols)?;
    let mut functions = IndexVec::new();

    for &idx in order {
        let mut lowered = scope.lower_matching_functions(
            &declarations[idx],
            symbols,
            graph.nodes[idx].in_std,
            |id| demand.contains(id),
            arena,
        )?;

        functions.append(&mut lowered);
    }

    Ok(functions)
}

fn build_demand<'hir, 'src>(
    graph: &ModuleGraph<'src>,
    declarations: &[Declarations<'_, 'src>],
    order: &[usize],
    scope: &Scope<'hir>,
    symbols: &mut SymbolTable,
) -> Result<DemandSet, ModuleError> {
    let function_map = collect_functions(graph, declarations, order, scope, symbols)?;
    let main = scope.resolve_function(symbols, |m| m.item("main"));

    let mut demand = DemandSet::default();
    let mut stack = Vec::new();

    if let Some(main) = main {
        demand.insert(main);
        stack.push(main);
    }

    // an editor analyses every project function, reachable from `main` or not
    // std stays demand-driven so unused library code is never lowered
    if scope.recover {
        for (&id, &(_, seed)) in function_map.iter() {
            if seed && demand.insert(id) {
                stack.push(id);
            }
        }
    }

    while let Some(id) = stack.pop() {
        use visitor::Visitor;

        let Some((function, _)) = function_map.get(&id) else {
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

/// Collect every declared function keyed by its signature id, paired with
/// whether it belongs to the project (or entry module) rather than to std
fn collect_functions<'hir, 'src>(
    graph: &ModuleGraph<'src>,
    declarations: &[Declarations<'_, 'src>],
    order: &[usize],
    scope: &Scope<'hir>,
    symbols: &SymbolTable,
) -> Result<HashMap<FunctionId, (Function<'src>, bool)>, ModuleError> {
    let mut functions = HashMap::new();
    let entry = order.last().copied();

    for &idx in order {
        let in_project = !graph.nodes[idx].in_std || entry == Some(idx);

        for function in declarations[idx].functions() {
            if let Some(id) = lookup_declaration_id(function, scope, symbols) {
                functions.insert(id, (function.clone(), in_project));
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

impl DemandSet {
    pub(super) fn contains(&self, id: FunctionId) -> bool {
        self.needed.contains(&id)
    }

    fn insert(&mut self, id: FunctionId) -> bool {
        self.needed.insert(id)
    }
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
                let name = kind.into();
                if let Some(id) = self.scope.resolve_function_call(*qualifier, name, self.symbols) {
                    self.found.push(id);
                }
            },
            Expression::Binary { operator, left, right, .. } => {
                match operator {
                    BinaryOperator::Eq
                    | BinaryOperator::Ne
                    | BinaryOperator::Lt
                    | BinaryOperator::LtEq
                    | BinaryOperator::Gt
                    | BinaryOperator::GtEq => {
                        self.found.extend(self.scope.methods.values().copied());
                    },
                    _ => {},
                }
                self.visit_expression(left);
                self.visit_expression(right);
            },
            _ => visitor::walk_expression(self, expr),
        }
    }
}
