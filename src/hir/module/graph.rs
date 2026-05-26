use super::{FileSystem, ModuleError, resolver::ModuleResolver};
use crate::{
    diagnostic::{self, Diagnostic},
    lexer::token::Span,
    parser::{
        Parser,
        statement::{Statement, UseItems},
    },
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

const PRELUDE: &[&str] = &["int.nyx", "float.nyx", "char.nyx", "default.nyx"];

#[derive(Debug)]
pub(super) struct ModuleGraph<'src> {
    pub(super) nodes: Vec<ModuleNode<'src>>,
    pub(super) edges: Vec<(usize, usize)>,
    pub(super) entry: usize,
}

#[derive(Debug)]
pub(super) struct ModuleNode<'src> {
    pub(super) path: PathBuf,
    pub(super) source: &'src str,
    pub(super) statements: Vec<Statement<'src>>,
    pub(super) exports: HashSet<String>,
    pub(super) in_std: bool,
}

struct GraphBuilder<'a, 'src, F> {
    resolver: &'a ModuleResolver,
    fs: &'a F,
    arena: &'src super::arena::SourceArena,
    nodes: Vec<ModuleNode<'src>>,
    by_path: HashMap<PathBuf, usize>,
    edges: Vec<(usize, usize)>,
    in_flight: HashSet<PathBuf>,
}

pub(super) fn build_graph<'src, F: FileSystem>(
    entry: &Path,
    resolver: &ModuleResolver,
    fs: &F,
    arena: &'src super::arena::SourceArena,
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

    Ok(ModuleGraph { nodes: builder.nodes, edges: builder.edges, entry })
}

impl<'src> ModuleGraph<'src> {
    pub(super) fn all_nodes_order(&self) -> Vec<usize> {
        let mut visited = HashSet::new();
        let mut order = Vec::new();
        self.visit(self.entry, &mut visited, &mut order);

        let mut detached: Vec<_> =
            (0..self.nodes.len()).filter(|idx| !visited.contains(idx)).collect();
        detached.sort_by(|a, b| self.nodes[*a].path.cmp(&self.nodes[*b].path));
        for idx in detached {
            self.visit(idx, &mut visited, &mut order);
        }

        order
    }

    fn visit(&self, idx: usize, visited: &mut HashSet<usize>, order: &mut Vec<usize>) {
        if !visited.insert(idx) {
            return;
        }

        let mut deps: Vec<_> = self
            .edges
            .iter()
            .filter_map(|&(from, to)| (from == idx).then_some(to))
            .collect();
        deps.sort_by(|a, b| self.nodes[*a].path.cmp(&self.nodes[*b].path));

        for dep in deps {
            self.visit(dep, visited, order);
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

        let source = self.arena.alloc(source);

        diagnostic::initialise(source, canonical.to_str().unwrap_or("<unknown>"));
        let statements = Parser::new(source).parse().map_err(Diagnostic::from)?;

        let idx = self.nodes.len();
        let in_std = canonical.starts_with(self.resolver.std_root());
        let exports = exports(&statements);

        self.by_path.insert(canonical.clone(), idx);
        self.nodes.push(ModuleNode {
            path: canonical.clone(),
            source,
            statements,
            exports,
            in_std,
        });

        let uses: Vec<_> = self.nodes[idx]
            .statements
            .iter()
            .filter_map(|stmt| match stmt {
                Statement::Use(declaration) => Some(declaration.clone()),
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

        self.in_flight.remove(&canonical);
        Ok(idx)
    }
}

fn exports(statements: &[Statement<'_>]) -> HashSet<String> {
    let mut exports = HashSet::new();

    for statement in statements {
        match statement {
            Statement::Struct(s) if s.is_pub => exports.insert(s.name.to_string()),
            Statement::Enum(e) if e.is_pub => exports.insert(e.name.to_string()),
            Statement::Interface(i) if i.is_pub => exports.insert(i.name.to_string()),
            Statement::Fn(f) if f.is_pub => exports.insert(f.name.to_string()),
            Statement::Const(c) if c.is_pub => exports.insert(c.name.to_string()),
            _ => false,
        };
    }

    exports
}
