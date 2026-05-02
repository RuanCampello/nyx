//! Multi-file module system with path resolution, cycle detection, and symbol merging.

use crate::{
    NyxError, diagnostic,
    hir::{
        Function, FunctionBuilder, FunctionId, Hir, SymbolTable,
        functions::{collect_function_signatures, signatures_from_hir},
    },
    parser::{Parser, statement::Statement},
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Orchestrates module loading, path resolution, and HIR construction.
///
/// Maintains a cache of loaded modules and the shared symbol table
/// to ensure symbol IDs remain unique across the entire compilation.
pub(crate) struct ModuleLoader {
    name: String,
    root: PathBuf,
    cache: HashMap<PathBuf, Arc<Module>>,
    /// modules currently being loaded
    /// used for cycle detection
    in_flight: HashSet<PathBuf>,
    /// shared symbols interner for all modules
    symbols: SymbolTable,
}

/// A fully-parsed and validated module
#[derive(Debug, Clone)]
struct Module {
    exports: HashMap<String, usize>,
    /// all functions in declaration order
    functions: Vec<Function>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("module file not found: {}", path.display())]
    FileNotFound { path: PathBuf },
    #[error("circular import detected: {}", path.display())]
    CircularImport { path: PathBuf },
    #[error("empty import path")]
    EmptyPath,
    #[error("unknown module root `{name}` (expected project name or `std`)")]
    UnknownRoot { name: String },
    #[error("module `{}` exports no symbol `{name}`", path.display())]
    UnknownExport { path: PathBuf, name: String },
    #[error("only function declarations allowed at top level in {}", path.display())]
    TopLevelNonFunction { path: PathBuf },
    #[error("parse error in {}: {error}", path.display())]
    Parse { path: PathBuf, error: String },
    #[error("semantic error in {}: {error}", path.display())]
    Semantic { path: PathBuf, error: String },
}

impl ModuleLoader {
    pub fn new(name: String, root: PathBuf) -> Self {
        Self {
            name,
            root,
            cache: HashMap::new(),
            in_flight: HashSet::new(),
            symbols: SymbolTable::new(),
        }
    }

    /// Load all modules reacheable from the `entry` point and produce a merged `HIR`
    ///
    /// Modules are merged in dependency-first order. The entry module is always last,
    /// ensuring that `main` gets an id that the `_start` can call
    pub fn load(&mut self, entry: &Path) -> Result<Hir, ModuleError> {
        let canonical = entry
            .canonicalize()
            .map_err(|_| ModuleError::FileNotFound { path: entry.into() })?;

        self.discover(&canonical)?;

        let mut dependencies: Vec<_> = self.cache.keys().cloned().collect();
        dependencies.sort_unstable();

        if let Some(position) = dependencies.iter().position(|pos| pos == &canonical) {
            let entry = dependencies.remove(position);

            dependencies.push(entry);
        }

        let mut functions = Vec::with_capacity(1 << 8);
        for path in &dependencies {
            let module = Arc::clone(&self.cache[path]);
            let offset = functions.len() as u32;

            functions.extend(module.functions.iter().map(|func| func.clone().with_id_offset(offset)));
        }

        Ok(Hir {
            functions,
            symbols: self.symbols.clone().into_symbols(),
        })
    }

    /// Recursevly load a module and all its dependencies
    fn discover(&mut self, canonical: &Path) -> Result<(), ModuleError> {
        if self.cache.contains_key(canonical) {
            return Ok(());
        }

        if self.in_flight.contains(canonical) {
            return Err(ModuleError::CircularImport { path: canonical.into() });
        }

        self.in_flight.insert(canonical.to_path_buf());
        let source =
            std::fs::read_to_string(canonical).map_err(|_| ModuleError::FileNotFound { path: canonical.into() })?;

        diagnostic::initialise(&source, canonical.to_str().unwrap_or("<unknown>"));

        let statements = Parser::new(&source).parse().map_err(|e| ModuleError::Parse {
            path: canonical.into(),
            error: e.to_string(),
        })?;

        for statement in &statements {
            if let Statement::Use(declaration) = statement {
                let import = self.resolve_path(&declaration.path.segments)?;
                self.discover(&import)?;
            }
        }

        self.in_flight.remove(canonical);

        let module = self.analyse(canonical, statements)?;
        self.cache.insert(canonical.to_path_buf(), Arc::new(module));

        Ok(())
    }

    fn analyse(&mut self, path: &Path, statements: Vec<Statement>) -> Result<Module, ModuleError> {
        for statement in &statements {
            match statement {
                Statement::Fn(_) | Statement::Use(_) => {}
                _ => return Err(ModuleError::TopLevelNonFunction { path: path.into() }),
            }
        }

        // build a combined signature + id table that includes all already-lowered dependencies
        let mut dependencies: Vec<_> = self.cache.keys().collect();
        dependencies.sort_unstable();

        let mut functions = Vec::new();
        for dependency in dependencies {
            let module = &self.cache[dependency];
            let offset = functions.len() as u32;

            functions.extend(module.functions.iter().map(|func| func.clone().with_id_offset(offset)));
        }

        let (mut signatures, mut map) = signatures_from_hir(&functions);

        let local_offset = signatures.len() as u32;
        let (local_signatures, local_map) =
            collect_function_signatures(&statements, &mut self.symbols).map_err(|e| ModuleError::Semantic {
                path: path.into(),
                error: e.to_string(),
            })?;

        // merge local signatures into combined table
        for (&symbol, &local_id) in &local_map {
            map.insert(symbol, FunctionId(local_id.0 + local_offset));
        }
        signatures.extend(local_signatures);

        let mut functions = Vec::new();
        let mut exports = HashMap::new();

        for statement in statements {
            let function = match statement {
                Statement::Fn(f) => f,
                _ => continue,
            };

            let builder = FunctionBuilder::new(&signatures, &map, &mut self.symbols, function);
            let mut hir = builder.lower().map_err(|e| ModuleError::Semantic {
                path: path.into(),
                error: e.to_string(),
            })?;

            // rebase the function's own id to be relative to its position in this module's list
            hir.id = FunctionId(functions.len() as u32);

            if hir.is_pub {
                exports.insert(self.symbols.get(hir.name).to_string(), functions.len());
            }

            functions.push(hir);
        }

        Ok(Module { functions, exports })
    }

    fn resolve_path(&self, segments: &[&str]) -> Result<PathBuf, ModuleError> {
        let (&root, rest) = segments.split_first().ok_or(ModuleError::EmptyPath)?;

        if rest.is_empty() {
            return Err(ModuleError::EmptyPath);
        }

        let base = match root {
            root if root == self.name => &self.root,
            other => {
                return Err(ModuleError::UnknownRoot {
                    name: other.to_string(),
                });
            }
        };

        let (dirs, file_segment) = rest.split_at(rest.len() - 1);
        let mut path = base.to_path_buf();

        path.extend(dirs);
        path.push(format!("{}.nyx", file_segment[0]));

        Ok(path)
    }
}

impl From<ModuleError> for NyxError {
    fn from(e: ModuleError) -> Self {
        NyxError::Compile(crate::diagnostic::Diagnostic {
            message: e.to_string(),
            rendered: format!("error: {e}\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "my_app";
    const PROJECT: &str = "/project";

    #[test]
    fn resolve_simple_path() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let path = loader.resolve_path(&[APP, "math"]).unwrap();

        assert_eq!(path, PathBuf::from("/project/math.nyx"));
    }

    #[test]
    fn resolve_nested_path() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let path = loader.resolve_path(&[APP, "utils", "io", "file"]).unwrap();

        assert_eq!(path, PathBuf::from("/project/utils/io/file.nyx"));
    }

    #[test]
    fn reject_unknown_root() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let err = loader.resolve_path(&["other", "foo"]).unwrap_err();

        match err {
            ModuleError::UnknownRoot { name } => assert_eq!(name, "other"),
            _ => panic!("expected unknownroot error"),
        }
    }

    #[test]
    fn reject_empty_path() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let err = loader.resolve_path(&[]).unwrap_err();

        assert!(matches!(err, ModuleError::EmptyPath));
    }

    #[test]
    fn reject_root_only() {
        let loader = ModuleLoader::new(APP.into(), PathBuf::from(PROJECT));
        let err = loader.resolve_path(&[APP]).unwrap_err();

        assert!(matches!(err, ModuleError::EmptyPath));
    }
}
