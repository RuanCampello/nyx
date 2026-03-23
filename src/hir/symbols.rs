use crate::hir::SymbolId;
use lasso::Rodeo;
use std::collections::HashMap;

pub struct SymbolTable {
    interner: Rodeo,
}

impl SymbolTable {
    fn new() -> Self {
        Self {
            interner: Rodeo::new(),
        }
    }

    #[inline(always)]
    fn insert(&mut self, name: &str) -> SymbolId {
        SymbolId(self.interner.get_or_intern(name))
    }

    #[inline(always)]
    fn get(&self, id: SymbolId) -> &str {
        self.interner.resolve(&id.0)
    }

    fn into_symbols(self) -> Vec<String> {
        self.interner
            .into_iter()
            .map(|(_, s)| s.to_string())
            .collect()
    }
}
