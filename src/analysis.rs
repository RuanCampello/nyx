use crate::hir::{
    Block, ExpressionKind, Hir, Local, LocalId, Res, Statement, SymbolId, SymbolTable, Type,
    TypeKind, TypeckResults, index_vec::IndexVec,
};
use crate::{
    diagnostic::AsDiagnostic,
    lexer::{HasSpan, token::Span},
    source_map::SourceMap,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Analysis {
    entry: PathBuf,
    overlays: HashMap<PathBuf, String>,
}

/// Hover and go-to-definition data extracted from a HIR pass
#[derive(Debug, Default)]
pub struct SemanticAnalysis {
    pub diagnostics: Vec<CheckError>,
    /// `(span, type string)` sorted by `span.start.offset()` for binary search from a cursor position
    pub hover_types: Vec<(Span, String)>,
    /// identifier-use span to definition-site span
    pub goto_definitions: HashMap<Span, Span>,
    /// `(name_span, type string)` for `let` bindings, hint appears immediately after the binding name
    pub inlay_hints: Vec<(Span, String)>,
    pub document_symbols: Vec<DocumentSymbol>,
    /// Resolves the global spans above to concrete files and line/column.
    pub source_map: SourceMap,
    /// whether the project analysed into hir. when false the feature data above
    /// is empty and an editor should keep its previous results rather than blank them on a transient parse error
    pub ok: bool,
}

/// A top-level declared symbol for the document outline
#[derive(Debug)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
}

struct Walker<'a, 'h> {
    typeck: &'a TypeckResults,
    locals: &'a IndexVec<LocalId, Local>,
    hir: &'a Hir<'h>,
    hover: &'a mut Vec<(Span, String)>,
    defs: &'a mut HashMap<Span, Span>,
    hints: &'a mut Vec<(Span, String)>,
}

#[derive(Debug, Clone, Copy)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
}

/// A single compile-time error in structured form so consumers can render it as richly as the CLI
pub type CheckError = crate::diagnostic::RichDiagnostic;

impl Analysis {
    /// Create a new analysis builder starting at the given entry path.
    pub fn new(entry: impl Into<PathBuf>) -> Self {
        Self { entry: entry.into(), overlays: HashMap::new() }
    }

    /// Add a single in-memory file overlay (e.g. unsaved editor buffer).
    pub fn with_overlay(mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        self.overlays.insert(path.into(), content.into());
        self
    }

    /// Set multiple in-memory file overlays at once.
    pub fn with_overlays(mut self, overlays: HashMap<PathBuf, String>) -> Self {
        self.overlays.extend(overlays);
        self
    }

    /// Execute the semantic analysis and return the results.
    pub fn run(self) -> SemanticAnalysis {
        let root = match self.entry.parent().unwrap_or(Path::new(".")).canonicalize() {
            Ok(r) => r,
            Err(e) => {
                return SemanticAnalysis {
                    diagnostics: vec![CheckError::bare(e.to_string())],
                    ..Default::default()
                };
            },
        };

        let name = root.file_name().and_then(|n| n.to_str()).unwrap_or("project").to_string();

        let arena = bumpalo::Bump::new();
        let mut loader = crate::hir::module::ModuleLoader::with_file_system(
            name,
            root,
            crate::hir::module::resolve_std_root(),
            crate::hir::module::OverlayFS { overlay: self.overlays },
            &arena,
        );

        let mut analysis = match loader.load(&self.entry) {
            Ok(hir) => walk_hir(&hir),
            Err(e) => {
                let span = e.span().unwrap_or_default();
                SemanticAnalysis { diagnostics: vec![e.rich(span)], ..Default::default() }
            },
        };
        analysis.source_map = crate::diagnostic::take_source_map();
        analysis
    }
}

impl<'a, 'h> Walker<'a, 'h> {
    fn block(&mut self, block: &Block<'h>) {
        for stmt in block.statements {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Statement<'h>) {
        match stmt {
            Statement::LetInit { id, init } => {
                let local = &self.locals[*id];
                self.hints.push((local.decl_span, format_type(local.typ, self.hir)));
                self.expr(init);
            },
            Statement::Expr(e) | Statement::Return(Some(e)) => self.expr(e),
            Statement::If { condition, then_block, else_block } => {
                self.expr(condition);
                self.block(then_block);
                if let Some(eb) = else_block {
                    self.block(eb);
                }
            },
            Statement::While { condition, body } => {
                self.expr(condition);
                self.block(body);
            },
            Statement::Block(b) => self.block(b),
            Statement::Return(None) | Statement::LetUninit { .. } => {},
        }
    }

    fn expr(&mut self, expr: &crate::hir::Expression<'h>) {
        self.hover
            .push((expr.span, format_type(self.typeck.type_of(expr.id), self.hir)));

        match &expr.kind {
            ExpressionKind::Local(id) => {
                self.defs.insert(expr.span, self.locals[*id].decl_span);
            },
            ExpressionKind::Call { callee, args } => {
                if let Some(Res::Function(fn_id)) = self.typeck.type_dependent_def(expr.id) {
                    if let Some(target) = self.hir.functions.get(fn_id) {
                        self.defs.insert(callee.span, target.decl_span);
                    }
                }
                self.expr(callee);
                for arg in *args {
                    self.expr(arg);
                }
            },
            ExpressionKind::MethodCall { receiver, args, .. } => {
                if let Some(Res::Function(fn_id)) = self.typeck.type_dependent_def(expr.id) {
                    if let Some(target) = self.hir.functions.get(fn_id) {
                        self.defs.insert(expr.span, target.decl_span);
                    }
                }
                self.expr(receiver);
                for arg in *args {
                    self.expr(arg);
                }
            },
            ExpressionKind::Unary { expr: sub, .. } => self.expr(sub),
            ExpressionKind::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            },
            ExpressionKind::Field { base, .. } => self.expr(base),
            ExpressionKind::Assign { target, value } => {
                self.expr(target);
                self.expr(value);
            },
            ExpressionKind::Struct { fields, .. } => {
                for (_, fexpr) in *fields {
                    self.expr(fexpr);
                }
            },
            ExpressionKind::Syscall { args, .. } | ExpressionKind::IntrinsicCall { args, .. } => {
                for arg in *args {
                    self.expr(arg);
                }
            },
            ExpressionKind::Cast { from, .. } => self.expr(from),
            ExpressionKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in *arms {
                    self.expr(arm.body);
                    if let Some(guard) = arm.guard {
                        self.expr(guard);
                    }
                }
            },
            ExpressionKind::Literal(_)
            | ExpressionKind::Path(_)
            | ExpressionKind::TypeIntrinsic { .. } => {},
        }
    }
}

fn walk_hir(hir: &Hir<'_>) -> SemanticAnalysis {
    let mut hover_types = Vec::new();
    let mut goto_definitions = HashMap::new();
    let mut inlay_hints = Vec::new();

    for func in &hir.functions {
        Walker {
            typeck: &func.typeck,
            locals: &func.locals,
            hir,
            hover: &mut hover_types,
            defs: &mut goto_definitions,
            hints: &mut inlay_hints,
        }
        .block(&func.body);
    }

    hover_types.sort_unstable_by_key(|(span, _)| span.start.offset());

    let mut symbols = Vec::new();
    let sym = &hir.symbols;
    let (fns, structs, enums) = (&hir.functions, &hir.structs, &hir.enums);
    collect_symbols(fns, SymbolKind::Function, sym, &mut symbols, |f| f.name, |f| f.decl_span);
    collect_symbols(structs, SymbolKind::Struct, sym, &mut symbols, |s| s.name, |s| s.decl_span);
    collect_symbols(enums, SymbolKind::Enum, sym, &mut symbols, |e| e.name, |e| e.decl_span);
    symbols.sort_unstable_by_key(|s| s.span.start.offset());

    SemanticAnalysis {
        diagnostics: vec![],
        hover_types,
        goto_definitions,
        inlay_hints,
        document_symbols: symbols,
        source_map: SourceMap::default(),
        ok: true,
    }
}

fn collect_symbols<T, N, S>(
    iter: &[T],
    kind: SymbolKind,
    symbols: &SymbolTable,
    docs: &mut Vec<DocumentSymbol>,
    name: N,
    span: S,
) where
    N: Fn(&T) -> SymbolId,
    S: Fn(&T) -> Span,
{
    docs.extend(iter.iter().filter(|item| span(item) != Span::default()).map(|item| {
        DocumentSymbol {
            name: short_name(symbols.get(name(item))),
            kind,
            span: span(item),
        }
    }))
}

#[inline]
fn short_name(qualified: &str) -> String {
    qualified.rsplit("::").next().unwrap_or(qualified).to_owned()
}

fn format_type(typ: Type, hir: &Hir<'_>) -> String {
    match typ.kind() {
        TypeKind::Unit => "()".to_owned(),
        TypeKind::Str => "str".to_owned(),
        TypeKind::Struct(id) => short_name(hir.symbols.get(hir.structs[id].name)),
        TypeKind::Enum(id) => short_name(hir.symbols.get(hir.enums[id].name)),
        TypeKind::Ref { mutable, to } => {
            let typ = format_type(Type::new(to.kind()), hir);
            match mutable {
                true => format!("&mut {typ}"),
                _ => format!("&{typ}"),
            }
        },
        kind => kind.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyse(tag: &str, content: &str) -> SemanticAnalysis {
        let entry = std::env::temp_dir().join(format!("nyx_analysis_{tag}.nyx"));
        std::fs::write(&entry, "").unwrap();
        let entry = std::fs::canonicalize(&entry).unwrap();
        let analysis = Analysis::new(entry.clone()).with_overlay(entry.clone(), content).run();
        std::fs::remove_file(&entry).ok();

        analysis
    }

    #[test]
    fn valid_buffer_analyses_with_hints() {
        let a = analyse("valid", "fn main() { let x = 232; }");
        assert!(a.ok, "valid source must analyse into HIR");
        assert!(a.diagnostics.is_empty());
        assert!(a.inlay_hints.iter().any(|(_, ty)| ty == "i32"), "expected the `: i32` hint");
    }

    #[test]
    fn broken_buffer_is_not_ok_and_yields_no_features() {
        let a = analyse("broken", "fn main() { let x = ");
        assert!(!a.ok, "a parse error must leave the analysis incomplete");
        assert!(a.inlay_hints.is_empty(), "no feature data when analysis fails");
        assert!(!a.diagnostics.is_empty(), "the error should still be reported");
    }
}
