//! Module path resolution, cross-file import management and per-compilation cache
//!
//! ### Semantic Model
//!
//! Every `.nyx` file is a module. Module are identifiers by their canonical filesystem path.
//! The `use` keyword binds symbols from other modules into the local scope without merging
//! namespaces.
//!
//! ```rust,ignore
//! use std::io::{println};     // item import: println enters the scope direcly
//! use std::io;                // namespace import: io::println() syntax
//! use my_app::math::{add};    // project-level item import
//! ```
//! ### Root disambiguation
//!
//! The first path segment identifies the root:
//! - [project_name] -> project root
//! - anything else  -> error (external packages, std)

#![allow(unused)]

use crate::{
    hir::{self, Function, FunctionId, SymbolTable, error::ResolverError},
    parser::{
        self, Parser,
        statement::{Statement, UseDecl, UseItems},
    },
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Drives path resolution and module loading for a single compilation.
///
/// Owns the shared `SymbolTable` so that `SymbolId`s from different modules remain
/// comparable after merging
pub(in crate::hir) struct Resolver {
    name: String,
    root: PathBuf,
    /// canonical path -> cached module populated lazily on access populated lazily on access
    cache: HashMap<PathBuf, Arc<CacheModule>>,
    /// paths currently being loaded
    /// this is used to detect cycles
    in_flight: HashSet<PathBuf>,

    symbols: SymbolTable,
}

pub(in crate::hir) struct ResolvedImport {
    module: Arc<CacheModule>,
    bindings: Import,
}

/// A fully-analysed module, cached for the lifetime of the compilation.
#[derive(Debug)]
pub(in crate::hir) struct CacheModule {
    path: PathBuf,
    /// `name` -> `ìndex` in `self.functions` for the **pub** items
    exports: HashMap<String, usize>,
    /// All functions in declaration order with the module-local ids
    functions: Vec<hir::Function>,
}

pub(in crate::hir) enum Import {
    /// `use foo::bar;`: the last segment becomes the local namespaces name
    Namespace { bound: String },
    /// `use foo::bar::{a, b};`: specific names brought directly into the scope
    Named(Vec<String>),
}

pub(in crate::hir) enum ImportBinding {
    Item {
        name: String,
        function_if: FunctionId,
    },
    Namespace {
        bound: String,
        function_range: std::ops::Range<usize>,
    },
}

impl Resolver {
    pub fn new(name: String, root: PathBuf) -> Self {
        Self {
            name,
            root,
            cache: HashMap::new(),
            in_flight: HashSet::new(),
            symbols: SymbolTable::new(),
        }
    }

    pub fn resolve<'d>(&mut self, declaration: &UseDecl<'d>, _from: &Path) -> Result<ResolvedImport, ResolverError> {
        let path = self.resolve_path(&declaration.path.segments)?;
        let module = self.load_module(path)?;

        let bindings = match &declaration.items {
            UseItems::Namespace => Import::Namespace {
                bound: declaration.path.segments.last().copied().unwrap_or_default().to_string(),
            },
            UseItems::Named(items) => {
                for item in items {
                    if !module.exports.contains_key(item.name) {
                        return Err(ResolverError::UnknownExport {
                            module_path: module.path.to_string_lossy().into(),
                            name: item.name.to_string(),
                        });
                    }
                }

                Import::Named(items.iter().map(|item| item.name.to_string()).collect())
            }
        };

        Ok(ResolvedImport { module, bindings })
    }

    fn resolve_path<'s>(&self, segments: &[&'s str]) -> Result<PathBuf, ResolverError> {
        let (&root, rest) = segments.split_first().ok_or(ResolverError::EmptyPath)?;

        if rest.is_empty() {
            return Err(ResolverError::EmptyPath);
        }

        let base = match root {
            root if root == self.name => self.root.clone(),
            other => {
                return Err(ResolverError::UnknownRoot {
                    name: other.to_string(),
                });
            }
        };

        let (dirs, file_segment) = rest.split_at(rest.len() - 1);

        let mut path = base;
        for segment in dirs {
            path.push(segment);
        }
        path.push(format!("{}.nyx", file_segment[0]));

        Ok(path)
    }

    fn load_module(&mut self, path: PathBuf) -> Result<Arc<CacheModule>, ResolverError> {
        let canonical = path
            .canonicalize()
            .map_err(|_| ResolverError::FileNotFound { path: path.clone() })?;

        if let Some(cached) = self.cache.get(&canonical) {
            return Ok(Arc::clone(cached));
        }

        if self.in_flight.contains(&canonical) {
            return Err(ResolverError::CircularImport { path: canonical });
        }

        self.in_flight.insert(canonical.clone());
        let result = self.load_module_io(canonical.clone());
        self.in_flight.remove(&canonical);

        result
    }

    fn load_module_io(&mut self, canonical: PathBuf) -> Result<Arc<CacheModule>, ResolverError> {
        let src = std::fs::read_to_string(&canonical).unwrap();

        let statements = Parser::new(&src).parse().unwrap();
        // load all transitive deps first so their signatures are in the shared symbol table
        // before we lower this file's HIR
        let from = canonical.parent().unwrap_or(Path::new(""));
        let mut transitives = Vec::new();

        for statement in statements {
            if let Statement::Use(ref declaration) = statement {
                let import = self.resolve(declaration, from)?;
                transitives.push(import);
            }
        }

        let mut all_functions = Vec::new();
        let transitive_bindings = Self::merge(&mut all_functions, transitives);

        let module_functions = todo!();
        let exports = HashMap::new();

        let module = Arc::new(CacheModule {
            path: canonical.clone(),
            exports,
            functions: all_functions,
        });
        self.cache.insert(canonical, Arc::clone(&module));

        Ok(module)
    }

    fn merge(base: &mut Vec<Function>, imports: Vec<ResolvedImport>) -> Vec<ImportBinding> {
        let mut bindings = Vec::new();

        for import in imports {
            let offset = base.len() as u32;
            let start = base.len();

            base.extend(import.module.functions.iter().map(|func| func.clone().with_id_offset(offset)));

            let binding = match import.bindings {
                Import::Namespace { bound } => ImportBinding::Namespace {
                    bound,
                    function_range: start..base.len(),
                },
                Import::Named(names) => {
                    for name in names {
                        if let Some(&local) = import.module.exports.get(&name) {
                            bindings.push(ImportBinding::Item {
                                name,
                                function_if: FunctionId((start + local) as u32),
                            })
                        }
                    }

                    continue;
                }
            };

            bindings.push(binding);
        }

        bindings
    }
}
