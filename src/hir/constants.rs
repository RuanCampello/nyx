//! Constant-declaration analysis
//!
//! Constants form their own mini-pass because their initialiser may reference
//! other constants, so we must walk a dependency graph and lower in
//! topological order.

use crate::{
    hir::{
        Constant, SymbolId, SymbolTable,
        declarations::Declarations,
        error::{HirError, hir_error},
        lower,
        scope::Scope,
        symbols::{Mangler, qualified},
        type_resolver,
    },
    parser::{expression, statement, visitor},
};
use std::collections::{HashMap, HashSet};

struct ConstDecl<'d, 's> {
    typ: Option<&'d str>,
    ast: &'d statement::Const<'s>,
}

/// Walks a constant initialiser and records references to other constants in
/// the same compilation unit.
/// Both the impl-scoped (`Type::CONST`) and the top-level (`CONST`) name are checked
struct DepVisitor<'a, 'd, 'i, 'sc> {
    current_impl: Option<&'a str>,
    mangler: &'a Mangler<'sc>,
    symbols: &'a SymbolTable,
    decls: &'a HashMap<SymbolId, ConstDecl<'d, 'i>>,
    deps: &'a mut Vec<SymbolId>,
}

struct Dfs<'a, 'hir, 'd, 's> {
    mangler: &'a Mangler<'a>,
    symbols: &'a SymbolTable,
    decls: &'a HashMap<SymbolId, ConstDecl<'d, 's>>,
    arena: &'hir bumpalo::Bump,
    visiting: HashSet<SymbolId>,
    visited: HashSet<SymbolId>,
    sorted: Vec<SymbolId>,
}

/// Collect every top-level and impl-scoped constant, topologically sort by
/// dependency, then lower each initialiser and insert it into `scope`
pub(in crate::hir) fn extend<'hir, 'd, 's>(
    scope: &mut Scope<'hir>,
    declarations: &Declarations<'d, 's>,
    symbols: &mut SymbolTable,
    in_std: bool,
    arena: &'hir bumpalo::Bump,
) -> Result<(), HirError<'hir>>
where
    's: 'hir,
{
    let decls = collect(scope, declarations, symbols)?;
    let sorted = match topo_sort(&decls, &scope.mangler, symbols, arena) {
        Ok(sorted) => sorted,
        // a dependency cycle leaves no usable order, skip the whole batch
        Err(error) => return scope.soft(error),
    };

    for symbol_id in sorted {
        let decl = &decls[&symbol_id];
        let ctx = type_resolver::ResolveCtx::root(symbols, &scope.struct_map, &scope.enum_map);
        let expected_type =
            type_resolver::resolve_annotation(&ctx, &decl.ast.typ.value(), decl.ast.typ.span())
                .or_else(|error| scope.poison(error))?;

        let (value, typeck) =
            match lower::lower_const(scope, symbols, &decl.ast.value, expected_type, in_std, arena)
            {
                Ok(lowered) => lowered,
                Err(error) => {
                    scope.soft(error)?;
                    continue;
                },
            };

        scope.constants.insert(
            symbol_id,
            Constant {
                name: symbol_id,
                typ: expected_type,
                typeck,
                value,
                is_pub: decl.ast.is_pub,
                decl_span: decl.ast.span,
            },
        );
    }

    Ok(())
}

fn collect<'hir, 'd, 's>(
    scope: &mut Scope<'hir>,
    declarations: &Declarations<'d, 's>,
    symbols: &mut SymbolTable,
) -> Result<HashMap<SymbolId, ConstDecl<'d, 's>>, HirError<'hir>>
where
    's: 'hir,
{
    let mut decls: HashMap<SymbolId, ConstDecl<'d, 's>> = HashMap::new();

    for c in &declarations.constants {
        let symbol_id = symbols.insert(&scope.mangler.item(c.name));
        if decls.contains_key(&symbol_id) {
            scope.soft(hir_error!(c.span, DuplicateConstant { name: c.name }))?;
            continue;
        }
        decls.insert(symbol_id, ConstDecl { typ: None, ast: c });
    }

    for imp in &declarations.impls {
        for c in &imp.constants {
            let symbol_id = symbols.insert(&scope.mangler.scoped_item(imp.name, c.name));
            if decls.contains_key(&symbol_id) {
                let name = qualified(scope.arena, imp.name, c.name);
                scope.soft(hir_error!(c.span, DuplicateConstant { name }))?;
                continue;
            }

            decls.insert(symbol_id, ConstDecl { typ: Some(imp.name), ast: c });
        }
    }

    Ok(decls)
}

fn topo_sort<'hir, 'd, 's>(
    decls: &HashMap<SymbolId, ConstDecl<'d, 's>>,
    mangler: &Mangler<'_>,
    symbols: &SymbolTable,
    arena: &'hir bumpalo::Bump,
) -> Result<Vec<SymbolId>, HirError<'hir>>
where
    's: 'hir,
{
    let mut dfs = Dfs {
        mangler,
        symbols,
        decls,
        arena,
        visiting: HashSet::new(),
        visited: HashSet::new(),
        sorted: Vec::new(),
    };

    for &symbol_id in decls.keys() {
        if !dfs.visited.contains(&symbol_id) {
            dfs.visit(symbol_id)?;
        }
    }

    Ok(dfs.sorted)
}

impl<'a, 'hir, 'd, 's> Dfs<'a, 'hir, 'd, 's>
where
    's: 'hir,
{
    fn visit(&mut self, symbol_id: SymbolId) -> Result<(), HirError<'hir>> {
        use visitor::Visitor;

        if self.visiting.contains(&symbol_id) {
            let decl = &self.decls[&symbol_id];
            let name = decl
                .typ
                .map_or(decl.ast.name, |impl_type| qualified(self.arena, impl_type, decl.ast.name));
            return Err(hir_error!(decl.ast.span, CircularConstant { name }));
        }

        if self.visited.contains(&symbol_id) {
            return Ok(());
        }

        self.visiting.insert(symbol_id);
        if let Some(decl) = self.decls.get(&symbol_id) {
            let mut deps = Vec::new();
            let mut walker = DepVisitor {
                current_impl: decl.typ,
                mangler: self.mangler,
                symbols: self.symbols,
                decls: self.decls,
                deps: &mut deps,
            };
            walker.visit_expression(&decl.ast.value);
            for dep in deps {
                if self.decls.contains_key(&dep) {
                    self.visit(dep)?;
                }
            }
        }

        self.visiting.remove(&symbol_id);
        self.visited.insert(symbol_id);
        self.sorted.push(symbol_id);

        Ok(())
    }
}

impl<'i> visitor::Visitor<'i> for DepVisitor<'_, '_, 'i, '_> {
    fn visit_expression(&mut self, expr: &expression::Expression<'i>) {
        use expression::Expression as Expr;

        match expr {
            Expr::Identifier(name, _) => {
                if let Some(impl_type) = self.current_impl {
                    let scoped = self.mangler.scoped_item(impl_type, name);
                    if let Some(sym) = self.symbols.get_id(&scoped)
                        && self.decls.contains_key(&sym)
                    {
                        self.deps.push(sym);
                        return;
                    }
                }
                if let Some(sym) = self.symbols.get_id(&self.mangler.item(name))
                    && self.decls.contains_key(&sym)
                {
                    self.deps.push(sym);
                }
            },
            Expr::QualifiedName { qualifier, name, .. } => {
                let mangled = self.mangler.scoped_item(qualifier, name);
                if let Some(sym) = self.symbols.get_id(&mangled)
                    && self.decls.contains_key(&sym)
                {
                    self.deps.push(sym);
                }
            },
            _ => visitor::walk_expression(self, expr),
        }
    }
}
