use frontend::hir::{self, Expression, ExpressionKind, IndexVec, LocalId, SymbolTable};

pub(super) struct ConstantValues<'a> {
    symbols: &'a SymbolTable,
    locals: IndexVec<LocalId, Option<String>>,
}

impl<'a> ConstantValues<'a> {
    pub(super) fn new(symbols: &'a SymbolTable, local_count: usize) -> Self {
        Self { symbols, locals: IndexVec::from_elem(None, local_count) }
    }

    pub(super) fn capture(&self, expr: &Expression<'_>) -> Option<String> {
        match &expr.kind {
            ExpressionKind::Literal(literal) => {
                use hir::Literal::*;
                Some(match literal {
                    Int(value) => value.to_string(),
                    Float(value) => value.to_string(),
                    Bool(value) => value.to_string(),
                    Char(value) => value.to_string(),
                    Str(symbol) => self.symbols.get(*symbol).to_owned(),
                    Unit => String::new(),
                })
            },
            ExpressionKind::Local(id) => self.locals[*id].clone(),
            _ => None,
        }
    }

    pub(super) fn record(&mut self, id: LocalId, value: Option<String>) {
        self.locals[id] = value;
    }

    pub(super) fn get(&self, id: LocalId) -> Option<&str> {
        self.locals[id].as_deref()
    }

    pub(super) fn clear(&mut self, id: LocalId) {
        self.locals[id] = None;
    }
}
