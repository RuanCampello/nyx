use super::{FileSystem, ModuleError, resolver::ModuleResolver};
use crate::{
    diagnostic,
    hir::Declarations,
    lexer::token::Span,
    parser::{
        Parser,
        expression::Expression,
        statement::{Interface, Item, ItemKind, Statement, UseItems, inject_default_methods},
        visitor::{self, Visitor},
    },
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

/// **std** modules loaded eagerly so their inherent methods and interface are always in scope without an explicit `use`
const PRELUDE: &[&str] = &[
    "int.nyx",
    "float.nyx",
    "char.nyx",
    "clone.nyx",
    "mem.nyx",
    "default.nyx",
    "cmp.nyx",
    "slice.nyx",
];

#[derive(Debug)]
pub(super) struct ModuleGraph<'src> {
    pub(super) nodes: Vec<ModuleNode<'src>>,
    pub(super) edges: Vec<(usize, usize)>,
    pub(super) entry: usize,
}

#[derive(Debug)]
pub(super) struct ModuleNode<'src> {
    pub(super) path: PathBuf,
    pub(super) statements: Vec<Statement<'src>>,
    pub(super) exports: HashSet<String>,
    pub(super) in_std: bool,
}

struct GraphBuilder<'a, 'src, F> {
    resolver: &'a ModuleResolver,
    fs: &'a F,
    arena: &'src bumpalo::Bump,
    nodes: Vec<ModuleNode<'src>>,
    by_path: HashMap<PathBuf, usize>,
    edges: Vec<(usize, usize)>,
    in_flight: HashSet<PathBuf>,
}

struct QualifiedCallCollector<'src> {
    calls: Vec<(Vec<&'src str>, &'src str, Span)>,
}

pub(super) fn build_graph<'src, F: FileSystem>(
    entry: &Path,
    resolver: &ModuleResolver,
    fs: &F,
    arena: &'src bumpalo::Bump,
) -> Result<ModuleGraph<'src>, ModuleError> {
    let canonical = fs
        .canonicalise(entry)
        .map_err(|_| ModuleError::FileNotFound { path: entry.into(), span: None })?;

    let mut builder = GraphBuilder {
        resolver,
        fs,
        arena,
        nodes: Vec::new(),
        by_path: HashMap::new(),
        edges: Vec::new(),
        in_flight: HashSet::new(),
    };

    let entry = builder.discover(canonical, None)?;
    builder.discover_prelude()?;

    // interface default methods are injected once here, up front, so every
    // later pass reads the same already-completed AST without re-injecting
    let interfaces = collect_interfaces(&builder.nodes);
    for node in &mut builder.nodes {
        inject_default_methods(&mut node.statements, |name| interfaces.get(name));
    }

    Ok(ModuleGraph { nodes: builder.nodes, edges: builder.edges, entry })
}

impl<'src> ModuleGraph<'src> {
    pub(super) fn all_nodes_order(&self) -> Vec<usize> {
        let mut adjacency = vec![Vec::new(); self.nodes.len()];
        for &(from, to) in &self.edges {
            adjacency[from].push(to);
        }
        for deps in &mut adjacency {
            deps.sort_by(|a, b| self.nodes[*a].path.cmp(&self.nodes[*b].path));
        }

        let mut visited = HashSet::new();
        let mut order = Vec::new();
        self.visit(self.entry, &adjacency, &mut visited, &mut order);

        let mut detached: Vec<_> =
            (0..self.nodes.len()).filter(|idx| !visited.contains(idx)).collect();
        detached.sort_by(|a, b| self.nodes[*a].path.cmp(&self.nodes[*b].path));
        for idx in detached {
            self.visit(idx, &adjacency, &mut visited, &mut order);
        }

        order
    }

    /// partition every node's items once, indexed by node id
    ///
    /// the returned declarations borrow the graph, so all later passes
    /// share a single categorisation instead of re-scanning the ast per pass
    pub(super) fn collect_declarations(&self) -> Result<Vec<Declarations<'_, 'src>>, ModuleError> {
        self.nodes
            .iter()
            .map(|node| Declarations::collect(&node.statements).map_err(Into::into))
            .collect()
    }

    fn visit(
        &self,
        idx: usize,
        adjacency: &[Vec<usize>],
        visited: &mut HashSet<usize>,
        order: &mut Vec<usize>,
    ) {
        if !visited.insert(idx) {
            return;
        }

        for &dep in &adjacency[idx] {
            self.visit(dep, adjacency, visited, order);
        }

        order.push(idx);
    }
}

impl<'src, F: FileSystem> GraphBuilder<'_, 'src, F> {
    fn discover_prelude(&mut self) -> Result<(), ModuleError> {
        for name in PRELUDE {
            let path = self.resolver.std_root().join(name);
            let Ok(canonical) = self.fs.canonicalise(&path) else {
                continue;
            };
            if self.fs.read(&canonical).is_ok() {
                self.discover(canonical, None)?;
            }
        }

        Ok(())
    }

    fn discover(
        &mut self,
        canonical: PathBuf,
        triggered_by: Option<Span>,
    ) -> Result<usize, ModuleError> {
        if self.in_flight.contains(&canonical) {
            return Err(ModuleError::CircularImport {
                path: canonical,
                span: triggered_by.unwrap_or_default(),
            });
        }

        if let Some(&idx) = self.by_path.get(&canonical) {
            return Ok(idx);
        }

        self.in_flight.insert(canonical.clone());
        let source = self.fs.read(&canonical).map_err(|_| ModuleError::FileNotFound {
            path: canonical.clone(),
            span: triggered_by,
        })?;

        let source = self.arena.alloc_str(&source);

        let (_, base) = diagnostic::add_file(canonical.clone(), source as &str);
        let statements = Parser::with_base(source, base).parse()?;

        let idx = self.nodes.len();
        let in_std = canonical.starts_with(self.resolver.std_root());
        let exports = exports(&statements);

        self.by_path.insert(canonical.clone(), idx);
        self.nodes
            .push(ModuleNode { path: canonical.clone(), statements, exports, in_std });

        let uses: Vec<_> = self.nodes[idx]
            .statements
            .iter()
            .filter_map(|stmt| match stmt {
                Statement::Item(Item { kind: ItemKind::Use(declaration), .. }) => {
                    Some(declaration.clone())
                },
                _ => None,
            })
            .collect();

        for declaration in uses {
            let resolved =
                self.resolver.resolve_path(&declaration.path.segments, declaration.span)?;
            if self.fs.read(&resolved).is_err() {
                continue;
            }
            let import = self.fs.canonicalise(&resolved).unwrap_or(resolved);
            let import_idx = self.discover(import.clone(), Some(declaration.span))?;
            self.edges.push((idx, import_idx));

            if let UseItems::Named(items) = declaration.items {
                let module = &self.nodes[import_idx];

                for item in items {
                    if !module.exports.contains(item.name) {
                        return Err(ModuleError::UnknownExport {
                            path: import.clone(),
                            name: item.name.into(),
                            span: item.span,
                        });
                    }
                }
            }
        }

        for (path, name, span) in qualified_calls(&self.nodes[idx].statements) {
            if !self.resolver.is_known_root(path[0]) {
                continue;
            }

            let resolved = self.resolver.resolve_path(&path, span)?;
            let import = self.fs.canonicalise(&resolved).unwrap_or(resolved);
            let import_idx = self.discover(import.clone(), Some(span))?;
            self.edges.push((idx, import_idx));

            if !self.nodes[import_idx].exports.contains(name) {
                return Err(ModuleError::UnknownExport { path: import, name: name.into(), span });
            }
        }

        self.in_flight.remove(&canonical);
        Ok(idx)
    }
}

impl<'src> Visitor<'src> for QualifiedCallCollector<'src> {
    fn visit_expression(&mut self, expr: &Expression<'src>) {
        match expr {
            Expression::QualifiedCall { path, name, .. } => {
                self.calls.push((path.clone(), name, expr.span()));
            },
            _ => visitor::walk_expression(self, expr),
        }
    }
}

fn qualified_calls<'src>(statements: &[Statement<'src>]) -> Vec<(Vec<&'src str>, &'src str, Span)> {
    let mut collector = QualifiedCallCollector { calls: Vec::new() };
    for statement in statements {
        collector.visit_statement(statement);
    }
    collector.calls
}

fn collect_interfaces<'src>(nodes: &[ModuleNode<'src>]) -> HashMap<String, Interface<'src>> {
    let mut interfaces = HashMap::new();

    for node in nodes {
        for statement in &node.statements {
            if let Statement::Item(Item { kind: ItemKind::Interface(interface), .. }) = statement {
                interfaces.insert(interface.name.to_string(), interface.clone());
            }
        }
    }

    interfaces
}

fn exports(statements: &[Statement<'_>]) -> HashSet<String> {
    let mut exports = HashSet::new();

    for statement in statements {
        let Statement::Item(item) = statement else {
            continue;
        };
        match &item.kind {
            ItemKind::Struct(s) if s.is_pub => exports.insert(s.name.to_string()),
            ItemKind::Enum(e) if e.is_pub => exports.insert(e.name.to_string()),
            ItemKind::Interface(i) if i.is_pub => exports.insert(i.name.to_string()),
            ItemKind::Fn(f) if f.is_pub => exports.insert(f.name.to_string()),
            ItemKind::Const(c) if c.is_pub => exports.insert(c.name.to_string()),
            _ => false,
        };
    }

    exports
}
