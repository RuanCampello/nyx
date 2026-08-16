//! Constant-declaration analysis
//!
//! Constants form their own mini-pass because their initialiser may reference
//! other constants, so we must walk a dependency graph and lower in
//! topological order.

use crate::{
    hir::{
        Constant, Owner, SymbolId, SymbolTable, collect,
        collect::ItemTable,
        declarations::Declarations,
        error::{HirError, hir_error},
        lower,
        symbols::{Mangler, qualified},
        type_resolver,
    },
    parser::{expression, statement, visitor},
};
use std::collections::{HashMap, HashSet};

struct ConstDecl<'d, 's, 'hir> {
    typ: Option<&'d str>,
    owner: Owner<'hir>,
    ast: &'d statement::Const<'s>,
}

/// Walks a constant initialiser and records references to other constants in
/// the same compilation unit.
/// Both the impl-scoped (`Type::CONST`) and the top-level (`CONST`) name are checked
struct DepVisitor<'a, 'd, 'i, 'sc> {
    current_impl: Option<&'a str>,
    mangler: &'a Mangler<'sc>,
    symbols: &'a SymbolTable,
    decls: &'a HashMap<SymbolId, ConstDecl<'d, 'i, 'sc>>,
    deps: &'a mut Vec<SymbolId>,
}

struct Dfs<'a, 'hir, 'd, 's> {
    mangler: &'a Mangler<'a>,
    symbols: &'a SymbolTable,
    decls: &'a HashMap<SymbolId, ConstDecl<'d, 's, 'hir>>,
    arena: &'hir bumpalo::Bump,
    visiting: HashSet<SymbolId>,
    visited: HashSet<SymbolId>,
    sorted: Vec<SymbolId>,
}

/// Collect every top-level and impl-scoped constant, topologically sort by
/// dependency, then lower each initialiser and insert it into `scope`
pub(in crate::hir) fn extend<'hir, 'd, 's>(
    scope: &mut ItemTable<'hir>,
    declarations: &Declarations<'d, 's>,
    arena: &'hir bumpalo::Bump,
) -> Result<(), HirError<'hir>>
where
    's: 'hir,
{
    let decls = collect(scope, declarations)?;
    let sorted = match topo_sort(&decls, &scope.mangler, &scope.symbols, arena) {
        Ok(sorted) => sorted,
        Err(error) => {
            scope.soft(error);
            return Ok(());
        },
    };

    for symbol_id in sorted {
        let decl = &decls[&symbol_id];
        let resolved = {
            let (structs, enums, arrays) =
                (&scope.adts.struct_map, &scope.adts.enum_map, &scope.arrays);
            let ctx = type_resolver::ResolveCtx::root(
                &scope.symbols,
                structs,
                enums,
                &scope.adts.defs,
                arrays,
                &scope.types,
            );
            type_resolver::resolve_annotation(&ctx, &decl.ast.typ.value(), decl.ast.typ.span())
        };
        let expected_type = resolved.unwrap_or_else(|error| scope.poison(error));

        let (value, typeck) = match lower::lower_const(scope, &decl.ast.value, expected_type, arena)
        {
            Ok(lowered) => lowered,
            Err(error) => {
                scope.soft(error);
                continue;
            },
        };

        let constant = arena.alloc(Constant {
            name: symbol_id,
            typ: expected_type,
            owner: decl.owner,
            typeck,
            value,
            is_pub: decl.ast.is_pub,
            decl_span: decl.ast.span,
            name_span: decl.ast.name_span,
        });
        scope.values.constants.insert(symbol_id, constant);
    }

    Ok(())
}

fn collect<'hir, 'd, 's>(
    scope: &mut ItemTable<'hir>,
    declarations: &Declarations<'d, 's>,
) -> Result<HashMap<SymbolId, ConstDecl<'d, 's, 'hir>>, HirError<'hir>>
where
    's: 'hir,
{
    let mut decls: HashMap<SymbolId, ConstDecl<'d, 's, 'hir>> = HashMap::new();

    for c in &declarations.constants {
        let symbol_id = scope.symbols.insert(&scope.mangler.item(c.name));
        if let Some(existing) = decls.get(&symbol_id) {
            let previous = collect::source_span(existing.ast.span);
            scope.soft(hir_error!(c.span, DuplicateConstant { name: c.name, previous }));
            continue;
        }
        decls.insert(symbol_id, ConstDecl { typ: None, owner: Owner::Free, ast: c });
    }

    for imp in &declarations.impls {
        for c in &imp.constants {
            let symbol_id = scope.symbols.insert(&scope.mangler.scoped_item(imp.name, c.name));
            if let Some(existing) = decls.get(&symbol_id) {
                let name = qualified(scope.arena, imp.name, c.name);
                let previous = collect::source_span(existing.ast.span);
                scope.soft(hir_error!(c.span, DuplicateConstant { name, previous }));
                continue;
            }

            let owner = match (scope.lookup_named_type(imp.name), imp.interface) {
                (Some(on), Some(interface)) => {
                    Owner::Interface { on, interface: scope.symbols.insert(interface) }
                },
                (Some(on), None) => Owner::Inherent(on),
                (None, _) => Owner::Free,
            };

            decls.insert(symbol_id, ConstDecl { typ: Some(imp.name), owner, ast: c });
        }
    }

    Ok(decls)
}

fn topo_sort<'hir, 'd, 's>(
    decls: &HashMap<SymbolId, ConstDecl<'d, 's, 'hir>>,
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
            Expr::QualifiedName { path, name, .. } => {
                let qualifier = path.join("::");
                let mangled = self.mangler.scoped_item(&qualifier, name);
                if let Some(sym) = self.symbols.get_id(&mangled)
                    && self.decls.contains_key(&sym)
                {
                    self.deps.push(sym);
                } else if let Some(sym) = self.symbols.get_id(&self.mangler.item(name))
                    && self.decls.contains_key(&sym)
                {
                    self.deps.push(sym);
                }
            },
            _ => visitor::walk_expression(self, expr),
        }
    }
}
