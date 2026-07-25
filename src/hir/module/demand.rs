use super::{ModuleError, graph::ModuleGraph};
use crate::{
    hir::{self, Declarations, FunctionId, index_vec::IndexVec, scope::Scope},
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
    found: &'a mut Vec<FunctionId>,
}

pub(super) fn lower_reachable<'hir, 'src>(
    graph: &ModuleGraph<'src>,
    declarations: &[Declarations<'_, 'src>],
    order: &[usize],
    scope: &mut Scope<'hir>,
    arena: &'hir bumpalo::Bump,
    keep_all: bool,
) -> Result<IndexVec<FunctionId, hir::Function<'hir>>, ModuleError>
where
    'src: 'hir,
{
    let function_map = collect_functions(graph, declarations, order, scope)?;
    let demand = build_demand(&function_map, scope, keep_all);
    let mut functions = IndexVec::new();

    for &idx in order {
        scope.in_std = graph.nodes[idx].in_std;
        let lowered = scope.lower_matching_functions(
            &declarations[idx],
            |id| {
                demand.contains(id)
                    || function_map.get(&id).is_some_and(|&(_, in_project)| in_project)
            },
            arena,
        )?;

        for function in lowered {
            if demand.contains(function.id) {
                functions.push(function);
            }
        }
    }

    Ok(functions)
}

fn build_demand<'src>(
    function_map: &HashMap<FunctionId, (Function<'src>, bool)>,
    scope: &Scope<'_>,
    keep_all: bool,
) -> DemandSet {
    let main = scope.resolve_function(|m| m.item("main"));

    let mut demand = DemandSet::default();
    let mut stack = Vec::new();

    if let Some(main) = main {
        demand.insert(main);
        stack.push(main);
    }

    // an editor keeps every project function, reachable from `main` or not,
    // so features work anywhere in the project
    if keep_all {
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
        let mut visitor = ReachabilityVisitor { scope, found: &mut found };
        visitor.visit_block(&function.body);

        for callee in found {
            if demand.insert(callee) {
                stack.push(callee);
            }
        }
    }

    demand
}

/// Collect every declared function keyed by its signature id, paired with
/// whether it belongs to the project (or entry module) rather than to std
fn collect_functions<'hir, 'src>(
    graph: &ModuleGraph<'src>,
    declarations: &[Declarations<'_, 'src>],
    order: &[usize],
    scope: &Scope<'hir>,
) -> Result<HashMap<FunctionId, (Function<'src>, bool)>, ModuleError> {
    let mut functions = HashMap::new();

    for &idx in order {
        let in_project = !graph.nodes[idx].in_std || graph.entry == idx;

        for function in declarations[idx].functions() {
            if let Some(id) = lookup_declaration_id(function, scope) {
                functions.insert(id, (function.clone(), in_project));
            }
        }
    }

    Ok(functions)
}

fn lookup_declaration_id(function: &Function<'_>, scope: &Scope<'_>) -> Option<FunctionId> {
    scope
        .function_id(function, None, |name| hir::error::HirErrorKind::UnknownFunction { name })
        .ok()
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
                        if let Some(id) = self.scope.resolve_function_call(None, name) {
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
            Expression::QualifiedCall { path, name, args, .. } => {
                if let Some(id) = self.scope.resolve_qualified_function_call(path, name) {
                    self.found.push(id);
                }
                for arg in args {
                    self.visit_expression(arg);
                }
            },
            Expression::TypeIntrinsic { kind, path, .. } => {
                let name = kind.into();
                let id = match path {
                    Some(path) => self.scope.resolve_qualified_function_call(path, name),
                    None => self.scope.resolve_function_call(None, name),
                };
                if let Some(id) = id {
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
