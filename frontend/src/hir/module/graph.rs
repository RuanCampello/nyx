use crate::{
    diagnostic::{self, AsDiagnostic, RichDiagnostic},
    hir::{
        Declarations,
        module::{FileSystem, ModuleError, resolver::ModuleResolver},
    },
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
///
/// An editor session loads every `std` module instead, so features answer for the
/// whole library and not only the part this program happens to reach
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
    pub(super) roots: Vec<usize>,
    /// syntax and import failures recovery swallowed while building the graph
    pub(super) diagnostics: Vec<RichDiagnostic>,
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
    editor: bool,
    diagnostics: Vec<RichDiagnostic>,
}

struct QualifiedCallCollector<'src> {
    calls: Vec<(Vec<&'src str>, &'src str, Span)>,
}

pub(in crate::hir::module) fn build_graph<'src, F: FileSystem>(
    entries: &[PathBuf],
    resolver: &ModuleResolver,
    fs: &F,
    arena: &'src bumpalo::Bump,
    editor: bool,
) -> Result<ModuleGraph<'src>, ModuleError> {
    let mut builder = GraphBuilder {
        resolver,
        fs,
        arena,
        nodes: Vec::new(),
        by_path: HashMap::new(),
        edges: Vec::new(),
        in_flight: HashSet::new(),
        editor,
        diagnostics: Vec::new(),
    };

    let mut roots = Vec::with_capacity(entries.len());
    for entry in entries {
        let canonical = fs
            .canonicalise(entry)
            .map_err(|_| ModuleError::FileNotFound { path: entry.into(), span: None })?;
        roots.push(builder.discover(canonical, None)?);
    }
    builder.discover_std()?;

    // interface default methods are injected once here, up front, so every
    // later pass reads the same already-completed AST without re-injecting
    let interfaces = collect_interfaces(&builder.nodes);
    for node in &mut builder.nodes {
        inject_default_methods(&mut node.statements, |name| interfaces.get(name));
    }

    Ok(ModuleGraph {
        nodes: builder.nodes,
        edges: builder.edges,
        roots,
        diagnostics: builder.diagnostics,
    })
}

impl<'src> ModuleGraph<'src> {
    pub(in crate::hir::module) fn all_nodes_order(&self) -> Vec<usize> {
        let mut adjacency = vec![Vec::new(); self.nodes.len()];
        for &(from, to) in &self.edges {
            adjacency[from].push(to);
        }
        for deps in &mut adjacency {
            deps.sort_by(|a, b| self.nodes[*a].path.cmp(&self.nodes[*b].path));
        }

        let mut visited = HashSet::new();
        let mut order = Vec::new();
        for &root in &self.roots {
            self.visit(root, &adjacency, &mut visited, &mut order);
        }

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
    pub(in crate::hir::module) fn collect_declarations(
        &self,
    ) -> (Vec<Declarations<'_, 'src>>, Vec<RichDiagnostic>) {
        let mut all = Vec::with_capacity(self.nodes.len());
        let mut diagnostics = Vec::new();

        for node in &self.nodes {
            let (declarations, errors) = Declarations::collect_recovering(&node.statements);
            diagnostics.extend(errors.into_iter().map(|error| error.kind.rich(error.span)));
            all.push(declarations);
        }

        (all, diagnostics)
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
    fn discover_std(&mut self) -> Result<(), ModuleError> {
        let root = self.resolver.std_root();
        let modules = match self.editor {
            true => self.fs.modules_in(root),
            false => PRELUDE.iter().map(|name| root.join(name)).collect(),
        };

        for path in modules {
            let Ok(canonical) = self.fs.canonicalise(&path) else {
                continue;
            };
            if self.fs.read(&canonical).is_ok()
                && let Err(error) = self.discover(canonical, None)
            {
                self.soft(error);
            }
        }

        Ok(())
    }

    fn soft(&mut self, error: ModuleError) {
        let span = error.span().unwrap_or_default();
        self.diagnostics.push(error.rich(span));
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
        let discovered = self.discover_module(&canonical, triggered_by);
        self.in_flight.remove(&canonical);

        discovered
    }

    fn discover_module(
        &mut self,
        canonical: &Path,
        triggered_by: Option<Span>,
    ) -> Result<usize, ModuleError> {
        let source = self.fs.read(canonical).map_err(|_| ModuleError::FileNotFound {
            path: canonical.into(),
            span: triggered_by,
        })?;

        let source = self.arena.alloc_str(&source);

        let (_, base) = diagnostic::add_file(canonical.to_path_buf(), source as &str);
        let parsed = Parser::with_base(source, base).parse();
        for error in parsed.diagnostics {
            self.diagnostics.push(error.kind.clone().rich(error.span));
        }
        let statements = parsed.statements;

        let idx = self.nodes.len();
        let in_std = canonical.starts_with(self.resolver.std_root());
        let exports = exports(&statements);

        self.by_path.insert(canonical.to_path_buf(), idx);
        self.nodes
            .push(ModuleNode { path: canonical.to_path_buf(), statements, exports, in_std });

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
            let Some((import, import_idx)) =
                self.import(idx, &declaration.path.segments, declaration.span)?
            else {
                continue;
            };

            let items = match declaration.items {
                UseItems::Named(items) => items,
                _ => Vec::new(),
            };

            for item in items {
                if !self.nodes[import_idx].exports.contains(item.name) {
                    let kind = declared_kind(&self.nodes[import_idx].statements, item.name);
                    self.soft(ModuleError::UnknownExport {
                        path: import.clone(),
                        name: item.name.into(),
                        kind,
                        span: item.span,
                    });
                }
            }
        }

        for (path, name, span) in qualified_calls(&self.nodes[idx].statements) {
            if !self.resolver.is_known_root(path[0]) {
                continue;
            }

            let Some((import, import_idx)) = self.import(idx, &path, span)? else {
                continue;
            };

            if !self.nodes[import_idx].exports.contains(name) {
                let kind = declared_kind(&self.nodes[import_idx].statements, name);
                self.soft(ModuleError::UnknownExport {
                    path: import,
                    name: name.into(),
                    kind,
                    span,
                });
            }
        }

        Ok(idx)
    }

    fn import(
        &mut self,
        from: usize,
        path: &[&str],
        span: Span,
    ) -> Result<Option<(PathBuf, usize)>, ModuleError> {
        let resolved = match self.resolver.resolve_path(path, span) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.soft(error);
                return Ok(None);
            },
        };

        if self.fs.read(&resolved).is_err() {
            let error = ModuleError::FileNotFound { path: resolved, span: Some(span) };
            self.soft(error);
            return Ok(None);
        }

        let import = self.fs.canonicalise(&resolved).unwrap_or(resolved);
        match self.discover(import.clone(), Some(span)) {
            Ok(idx) => {
                self.edges.push((from, idx));
                Ok(Some((import, idx)))
            },
            Err(error) => {
                self.soft(error);
                Ok(None)
            },
        }
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

fn declared_kind<'k>(statements: &[Statement<'_>], name: &str) -> &'k str {
    statements
        .iter()
        .find_map(|statement| {
            let Statement::Item(item) = statement else {
                return None;
            };

            match &item.kind {
                ItemKind::Fn(f) if f.name == name => Some("fn"),
                ItemKind::Struct(s) if s.name == name => Some("struct"),
                ItemKind::Enum(e) if e.name == name => Some("enum"),
                ItemKind::Interface(i) if i.name == name => Some("interface"),
                ItemKind::Const(c) if c.name == name => Some("const"),
                _ => None,
            }
        })
        .unwrap_or("fn")
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
