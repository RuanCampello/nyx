use crate::hir::SymbolId;
use lasso::Rodeo;

pub(in crate::hir) struct SymbolTable {
    interner: Rodeo,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            interner: Rodeo::new(),
        }
    }

    #[inline(always)]
    pub fn insert(&mut self, name: &str) -> SymbolId {
        SymbolId(self.interner.get_or_intern(name))
    }

    #[inline(always)]
    pub fn get(&self, id: SymbolId) -> &str {
        self.interner.resolve(&id.0)
    }

    #[inline(always)]
    pub fn into_symbols(self) -> Vec<String> {
        self.interner
            .into_iter()
            .map(|(_, s)| s.to_string())
            .collect()
    }
}
