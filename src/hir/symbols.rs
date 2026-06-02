use crate::hir::SymbolId;
use lasso::Rodeo;

/// Interns all identifiers strings encountered during compilation and maps them
/// to stable numeric [`SymbolId`]s
#[derive(Clone)]
pub(in crate::hir) struct SymbolTable {
    interner: Rodeo,
}

/// Constructs the canonical fully-qualified names used throughout the [HIR](crate::hir)
///
/// Every function, method and constant gets a mangled name of the form:
/// - `module::name`
/// - `module::Type::name`
/// - `module::Type::Interface::name`
pub(in crate::hir) struct Mangler<'m> {
    module: &'m str,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self { interner: Rodeo::new() }
    }

    #[inline(always)]
    pub fn insert(&mut self, name: &str) -> SymbolId {
        SymbolId(self.interner.get_or_intern(name))
    }

    #[inline(always)]
    pub fn get_id(&self, name: &str) -> Option<SymbolId> {
        self.interner.get(name).map(SymbolId)
    }

    #[inline(always)]
    pub fn get(&self, id: SymbolId) -> &str {
        self.interner.resolve(&id.0)
    }

    #[inline(always)]
    pub fn into_symbols(self) -> Vec<String> {
        self.interner.into_iter().map(|(_, s)| s.to_string()).collect()
    }
}

impl<'m> Mangler<'m> {
    pub const DEFAULT_MODULE: &'m str = "nyx";

    pub fn new(module: &'m str) -> Self {
        Self { module }
    }

    pub fn default() -> Self {
        Self::new(Self::DEFAULT_MODULE)
    }

    pub fn item(&self, name: &str) -> String {
        format!("{}::{name}", self.module)
    }

    pub fn scoped_item(&self, scope: &str, name: &str) -> String {
        format!("{}::{scope}::{name}", self.module)
    }

    pub fn interface_item(&self, scope: &str, interface: &str, name: &str) -> String {
        format!("{}::{scope}::{interface}::{name}", self.module)
    }
}

#[inline]
pub(in crate::hir) fn qualified<'a>(arena: &'a bumpalo::Bump, scope: &str, name: &str) -> &'a str {
    arena.alloc_str(&format!("{scope}::{name}"))
}
