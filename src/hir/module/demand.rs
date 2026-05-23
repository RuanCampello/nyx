use super::{ModuleError, graph::ModuleGraph};
use crate::{
    diagnostic::{self, Diagnostic},
    hir::{Declarations, FunctionId, SymbolTable, scope::Scope},
    parser::{
        expression::Expression,
        statement::{Block, Else, Function, Interface, Statement},
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

pub(super) fn lower_reachable(
    graph: &mut ModuleGraph,
    order: &[usize],
    interfaces: &HashMap<String, Interface<'static>>,
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
            .lower_matching_functions(&declarations, symbols, node.in_std, |id| {
                demand.contains(id)
            })
            .map_err(Diagnostic::from)?;

        functions.append(&mut lowered);
    }

    Ok(functions)
}

fn build_demand(
    graph: &mut ModuleGraph,
    order: &[usize],
    interfaces: &HashMap<String, Interface<'static>>,
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
        walk_block(&function.body, scope, symbols, &mut found);

        for callee in found {
            if demand.insert(callee) {
                stack.push(callee);
            }
        }
    }

    Ok(demand)
}

fn collect_functions<'a>(
    graph: &'a mut ModuleGraph,
    order: &[usize],
    interfaces: &HashMap<String, Interface<'static>>,
    scope: &Scope<'static>,
    symbols: &SymbolTable,
) -> Result<HashMap<FunctionId, Function<'static>>, ModuleError> {
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
    let struct_symbol = symbols.get_id(impl_type)?;
    let struct_id = scope.struct_map.get(&struct_symbol)?;
    let method_name = symbols.get_id(function.name)?;

    scope.interface_impls.iter().filter(|&&(sid, _)| sid == *struct_id).find_map(
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

fn walk_block(
    block: &Block<'_>,
    scope: &Scope<'_>,
    symbols: &SymbolTable,
    found: &mut Vec<FunctionId>,
) {
    for statement in &block.statements {
        walk_statement(statement, scope, symbols, found);
    }
}

fn walk_statement(
    statement: &Statement<'_>,
    scope: &Scope<'_>,
    symbols: &SymbolTable,
    found: &mut Vec<FunctionId>,
) {
    match statement {
        Statement::Let(stmt) => {
            if let Some(value) = &stmt.value {
                walk_expr(value, scope, symbols, found);
            }
        }
        Statement::Const(stmt) => walk_expr(&stmt.value, scope, symbols, found),
        Statement::Return(stmt) => {
            if let Some(value) = &stmt.value {
                walk_expr(value, scope, symbols, found);
            }
        }
        Statement::If(stmt) => {
            walk_expr(&stmt.condition, scope, symbols, found);
            walk_block(&stmt.then_branch, scope, symbols, found);
            if let Some(else_branch) = &stmt.else_branch {
                match else_branch.as_ref() {
                    Else::If(stmt) => {
                        walk_statement(&Statement::If(stmt.clone()), scope, symbols, found)
                    }
                    Else::Block(block) => walk_block(block, scope, symbols, found),
                    Else::Expr(expr) => walk_expr(expr, scope, symbols, found),
                }
            }
        }
        Statement::While(stmt) => {
            walk_expr(&stmt.condition, scope, symbols, found);
            walk_block(&stmt.body, scope, symbols, found);
        }
        Statement::Expr(expr, _) => walk_expr(expr, scope, symbols, found),
        Statement::Block(block) => walk_block(block, scope, symbols, found),
        Statement::Fn(_)
        | Statement::Struct(_)
        | Statement::Impl(_)
        | Statement::Interface(_)
        | Statement::Use(_) => {}
    }
}

fn walk_expr(
    expr: &Expression<'_>,
    scope: &Scope<'_>,
    symbols: &SymbolTable,
    found: &mut Vec<FunctionId>,
) {
    match expr {
        Expression::Call { callee, args, .. } => {
            match callee.as_ref() {
                Expression::Identifier(name, _) => {
                    if let Some(id) = resolve_top_level(name, scope, symbols) {
                        found.push(id);
                    }
                }
                Expression::Field { .. } => found.extend(scope.methods.values().copied()),
                _ => walk_expr(callee, scope, symbols, found),
            }
            for arg in args {
                walk_expr(arg, scope, symbols, found);
            }
        }
        Expression::QualifiedCall {
            qualifier,
            name,
            args,
            ..
        } => {
            if let Some(id) = resolve_qualified(qualifier, name, scope, symbols) {
                found.push(id);
            }
            for arg in args {
                walk_expr(arg, scope, symbols, found);
            }
        }
        Expression::Unary { expr, .. } | Expression::Cast { expr, .. } => {
            walk_expr(expr, scope, symbols, found);
        }
        Expression::Binary { left, right, .. } => {
            walk_expr(left, scope, symbols, found);
            walk_expr(right, scope, symbols, found);
        }
        Expression::Assignment { target, value, .. } => {
            walk_expr(target, scope, symbols, found);
            walk_expr(value, scope, symbols, found);
        }
        Expression::Field { expr, .. } => walk_expr(expr, scope, symbols, found),
        Expression::Struct { fields, .. } => {
            for field in fields {
                walk_expr(&field.value, scope, symbols, found);
            }
        }
        Expression::TypeIntrinsic {
            kind, qualifier, ..
        } => {
            let name: &str = kind.into();
            let id = qualifier
                .and_then(|q| resolve_qualified(q, name, scope, symbols))
                .or_else(|| resolve_top_level(name, scope, symbols));
            if let Some(id) = id {
                found.push(id);
            }
        }
        Expression::Integer(_, _)
        | Expression::Float(_, _)
        | Expression::String(_, _)
        | Expression::Char(_, _)
        | Expression::Bool(_, _)
        | Expression::Identifier(_, _)
        | Expression::QualifiedName { .. } => {}
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
            let struct_symbol = symbols.get_id(qualifier)?;
            let struct_id = *scope.struct_map.get(&struct_symbol)?;

            scope.interface_impls.iter().filter(|&&(sid, _)| sid == struct_id).find_map(
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
