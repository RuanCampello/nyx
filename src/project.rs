//! Multi-file project compilation

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use crate::{
    NyxError, diagnostic,
    hir::{self, Hir},
    parser::{
        Parser,
        statement::{self, Statement},
    },
};

/// Resolves and loads a multi-file nyx program starting from one entry point
#[derive(Debug)]
pub(crate) struct Loader {
    name: String,
    root: PathBuf,
    sources: Vec<(PathBuf, String)>,
    visited: HashSet<PathBuf>,
    in_flight: HashSet<PathBuf>,
}

impl Loader {
    pub fn new(name: String, root: PathBuf) -> Self {
        Self {
            name,
            root,
            sources: Vec::new(),
            visited: HashSet::new(),
            in_flight: HashSet::new(),
        }
    }

    pub fn lower_all(&self) -> Result<Hir, NyxError> {
        let mut statements = Vec::new();

        for (path, source) in &self.sources {
            diagnostic::initialise(source, path.to_str().unwrap_or("<unknown>"));

            let stmts = Parser::new(source.as_str()).parse()?;
            for statement in stmts {
                match statement {
                    // 'use' statements were already being used in file discovery
                    Statement::Use(_) => {}
                    other => statements.push(other),
                }
            }
        }

        let hir = hir::lower(statements)?;
        Ok(hir)
    }

    pub fn discover(&mut self, path: &Path) -> Result<(), NyxError> {
        let canonical = path.canonicalize()?;

        if self.visited.contains(&canonical) {
            return Ok(());
        }

        if self.in_flight.contains(&canonical) {
            todo!("circular import error");
        }

        self.in_flight.insert(canonical.clone());

        let src = std::fs::read_to_string(&canonical)?;
        let paths = self.paths(&src);

        for import in paths {
            self.discover(&import)?;
        }

        self.in_flight.remove(&canonical);
        self.visited.insert(canonical.clone());
        self.sources.push((canonical, src));

        Ok(())
    }

    fn paths(&self, src: &str) -> Vec<PathBuf> {
        let Ok(statement) = Parser::new(src).parse() else {
            return Vec::new();
        };

        statement
            .iter()
            .filter_map(|statement| match statement {
                Statement::Use(declaration) => self.resolve_use(&declaration.path.segments).ok(),
                _ => None,
            })
            .collect()
    }

    fn resolve_use(&self, segments: &[&str]) -> Result<PathBuf, ()> {
        let (&first, rest) = segments.split_first().ok_or(())?;

        if first != self.name || rest.is_empty() {
            return Err(());
        }

        let (dirs, file) = rest.split_at(rest.len() - 1);
        let mut path = self.root.clone();
        path.extend(dirs);
        path.push(format!("{}.nyx", file[0]));

        Ok(path)
    }
}
