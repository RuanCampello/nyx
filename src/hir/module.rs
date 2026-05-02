//! Multi-file module system with path resolution, cycle detection, and symbol merging.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    NyxError, diagnostic,
    hir::{self, Function, FunctionId, Hir, SymbolTable},
    parser::{
        Parser,
        statement::{self, Statement},
    },
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
    path: PathBuf,
    /// public function names -> index in `functions`
    exports: HashMap<String, usize>,
    /// all functions in declaration order
    functions: Vec<Function>,
    statements: Vec<hir::Statement>,
}

// Result of a single `use` declaration
struct ResolvedImport {
    module: Arc<Module>,
    import: ImportBinding,
}

/// Specifies which symbol from a module enter the importing scope
enum ImportBinding {
    /// `use foo::bar;`
    /// all symbols are available via `bar::name`
    Namespace {
        name: String,
        /// range of functions in merged list
        function: std::ops::Range<usize>,
    },
    /// `use foo::bar::{a, b};`
    /// symbols bound directly in scope
    Named(Vec<NamedImport>),
}

/// A single named import
///
/// `name` -> function at `function_id`
struct NamedImport {
    name: String,
    id: FunctionId,
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

    /// Load all modules reacheable from the entry point and produce a merged `HIR`
    ///
    /// # Process
    /// - discover all imports
    ///  parse and validate each module
    ///  merge function lists with ID rebasing
    ///  return unified `HIR`
    pub fn load(&mut self, entry: &Path) -> Result<Hir, ModuleError> {
        let canonical = entry
            .canonicalize()
            .map_err(|_| ModuleError::FileNotFound { path: entry.into() })?;

        self.discover(&canonical)?;

        let mut functions = Vec::new();
        let sources: Vec<_> = self.cache.values().cloned().collect();

        for module in sources {
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

        let module = self.analyse(canonical, &statements)?;
        self.cache.insert(canonical.to_path_buf(), Arc::new(module));

        Ok(())
    }

    fn analyse(&mut self, path: &Path, statements: &[Statement]) -> Result<Module, ModuleError> {
        let mut functions = Vec::new();
        let mut exports = HashMap::new();

        for statement in statements {
            match statement {
                Statement::Fn(function) => {
                    let idx = functions.len();

                    let hir = self.lower(function)?;
                    functions.push(hir);

                    if function.is_pub {
                        exports.insert(function.name.to_string(), idx);
                    }
                }

                Statement::Use(_) => {}

                _ => return Err(ModuleError::TopLevelNonFunction { path: path.into() }),
            }
        }

        Ok(Module {
            path: path.into(),
            functions,
            exports,
            statements: Vec::new(),
        })
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

    /// Lower a parsed function to HIR.
    ///
    /// full implementation would use the complete HIR lowering pipeline with type checking
    fn lower(&mut self, function: &statement::Function) -> Result<Function, ModuleError> {
        todo!()
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
