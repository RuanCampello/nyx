use crate::hir::module;
use crate::hir::{
    self, Block, Constant, Enum, ExpressionKind, Function, FunctionId, FunctionKind, Hir, Literal,
    Local, LocalId, Res, Statement, Struct, StructId, SymbolId, SymbolTable, Type, TypeKind,
    TypeckResults, index_vec::IndexVec,
};
use crate::mir::layout::LayoutTable;
use crate::{
    diagnostic::AsDiagnostic,
    lexer::{HasSpan, token::Span},
    source_map::{FileId, SourceMap},
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
    /// `(span, hover)` sorted by `span.start.offset()` for binary search from a cursor position
    pub hover_types: Vec<(Span, HoverInfo)>,
    /// identifier-use span to definition-site span
    pub goto_definitions: HashMap<Span, Span>,
    /// `(name_span, type string)` for `let` bindings, hint appears immediately after the binding name
    pub inlay_hints: Vec<(Span, String)>,
    pub document_symbols: Vec<DocumentSymbol>,
    /// Resolves the global spans above to concrete files and line/column.
    pub source_map: SourceMap,
    /// whether the project analysed into hir, recovered diagnostics do not clear it,
    /// the feature data above stays valid alongside them. when false (a parse or module
    /// failure) the feature data is empty and an editor should keep its previous
    /// results rather than blank them
    pub ok: bool,
}

/// A top-level declared symbol for the document outline
#[derive(Debug)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
}

/// A hover result
///
/// the inferred type and, when it has a runtime layout, its size and alignment in bytes
#[derive(Debug, Clone)]
pub struct HoverInfo {
    /// fully-qualified container of the hovered item (`project::util`, or
    /// `project::util::Point` for members), shown above the declaration
    pub path: Option<String>,
    pub ty: String,
    pub layout: Option<(u32, u32)>,
    pub docs: Option<String>,
}

struct Walker<'a, 'h> {
    typeck: &'a TypeckResults,
    locals: &'a IndexVec<LocalId, Local>,
    hir: &'a Hir<'h>,
    /// resolves a callee by its signature id, [Function::id] is not the
    /// position in [Hir::functions], so positional indexing is wrong
    functions: &'a HashMap<FunctionId, &'a Function<'h>>,
    /// resolves a spliced constant use back to its declaration
    constants: &'a HashMap<SymbolId, &'a Constant<'h>>,
    /// the enclosing function's declared generic parameter names
    generics: &'a [SymbolId],
    layouts: &'a LayoutTable,
    map: &'a SourceMap,
    modules: &'a HashMap<FileId, String>,
    hover: &'a mut Vec<(Span, HoverInfo)>,
    defs: &'a mut HashMap<Span, Span>,
    hints: &'a mut Vec<(Span, String)>,
}

#[derive(Debug, Clone, Copy)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Constant,
}

/// How many fields/variants a hover shows before truncating with `/* … */`
const MAX_HOVER_ITEMS: usize = 5;

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

    /// Execute the semantic analysis and return the results
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
        let std_root = module::resolve_std_root();
        let std_root = std_root.canonicalize().unwrap_or(std_root);

        let arena = bumpalo::Bump::new();
        let loader = module::ModuleLoader::with_file_system(
            name.clone(),
            root.clone(),
            std_root.clone(),
            module::OverlayFS { overlay: self.overlays },
            &arena,
        )
        .recovering();

        let result = loader.load(&self.entry);
        let source_map = crate::diagnostic::take_source_map();

        let mut analysis = match result {
            // recovery keeps a (partial) HIR even with errors: surface every
            // recovered diagnostic while still serving features for what resolved
            Ok(hir) => {
                let modules = module_paths(&source_map, &name, &root, &std_root);
                let mut analysis = walk_hir(&hir, &source_map, &modules);
                analysis.diagnostics = hir.diagnostics;
                analysis
            },
            Err((mut diagnostics, e)) => {
                let span = e.span().unwrap_or_default();
                diagnostics.push(e.rich(span));
                SemanticAnalysis { diagnostics, ..Default::default() }
            },
        };
        analysis.source_map = source_map;
        analysis
    }
}

impl<'hir> Hir<'hir> {
    #[inline]
    fn doc_text(&self, decl_span: Span) -> Option<String> {
        self.docs.get(&decl_span).map(|docs| docs.to_string())
    }
}

impl<'a, 'h> Walker<'a, 'h> {
    fn block(&mut self, block: &Block<'h>) {
        for stmt in block.statements {
            self.stmt(stmt);
        }
    }

    #[inline]
    fn binding(&mut self, id: LocalId) {
        let (typ, span) = {
            let local = &self.locals[id];
            (local.typ, local.decl_span)
        };

        self.hints.push((span, format_type(typ, self.hir, self.generics)));
        self.hover.push((span, self.hover_info(typ)));
    }

    #[inline]
    fn hover_info(&self, typ: Type) -> HoverInfo {
        let layout = match is_open(typ, self.hir) {
            true => None,
            false => layout_of(self.layouts, typ),
        };

        HoverInfo {
            path: None,
            ty: format_type(typ, self.hir, self.generics),
            layout,
            docs: None,
        }
    }

    fn stmt(&mut self, stmt: &Statement<'h>) {
        match stmt {
            Statement::LetInit { id, init } => {
                self.binding(*id);
                self.expr(init);
            },
            Statement::LetUninit { id } => self.binding(*id),
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
            Statement::Return(None) => {},
        }
    }

    fn expr(&mut self, expr: &hir::Expression<'h>) {
        if let Some(symbol) = self.typeck.const_use(expr.id)
            && let Some(constant) = self.constants.get(&symbol)
        {
            self.hover.push((
                expr.span,
                const_hover(constant, self.hir, self.layouts, self.map, self.modules),
            ));
            if constant.decl_span != Span::default() {
                self.defs.insert(expr.span, constant.decl_span);
            }
            return;
        }

        let resolved = match &expr.kind {
            ExpressionKind::Call { .. } | ExpressionKind::MethodCall { .. } => self
                .typeck
                .type_dependent_def(expr.id)
                .and_then(Res::function)
                .and_then(|id| self.functions.get(&id).copied()),
            _ => None,
        };

        let hover = match (resolved, &expr.kind) {
            (Some(target), _) => fn_hover(target, self.hir, self.map, self.modules),
            // expressions that name a type: struct literals, paths, and enum
            // variant references (lowered to enum-typed literals) show its
            // full declaration, like the declaration site does
            (
                None,
                ExpressionKind::Struct { .. }
                | ExpressionKind::Path(_)
                | ExpressionKind::Literal(_),
            ) => {
                let typ = self.typeck.type_of(expr.id);
                type_hover(typ, self.hir, self.layouts, self.map, self.modules)
                    .unwrap_or_else(|| self.hover_info(typ))
            },
            _ => self.hover_info(self.typeck.type_of(expr.id)),
        };

        self.hover.push((expr.span, hover));

        match &expr.kind {
            ExpressionKind::Local(id) => {
                self.defs.insert(expr.span, self.locals[*id].decl_span);
            },
            ExpressionKind::Call { callee, args } => {
                if let Some(target) = resolved {
                    self.defs.insert(callee.span, target.decl_span);
                }
                for arg in *args {
                    self.expr(arg);
                }
            },
            ExpressionKind::MethodCall { receiver, args, .. } => {
                if let Some(target) = resolved {
                    self.defs.insert(expr.span, target.decl_span);
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

fn walk_hir(hir: &Hir<'_>, map: &SourceMap, modules: &HashMap<FileId, String>) -> SemanticAnalysis {
    let mut hover_types = Vec::new();
    let mut goto_definitions = HashMap::new();
    let mut inlay_hints = Vec::new();
    let layouts = LayoutTable::build(&hir.structs, &hir.enums);
    let functions: HashMap<_, _> =
        hir.functions.iter().map(|function| (function.id, function)).collect();
    let constants: HashMap<_, _> =
        hir.constants.iter().map(|constant| (constant.name, constant)).collect();

    for func in &hir.functions {
        if func.decl_span != Span::default() {
            hover_types.push((func.decl_span, fn_hover(func, hir, map, modules)));
        }
        for param in &func.params {
            let local = &func.locals[param.id];
            if local.decl_span != Span::default() {
                let layout = layout_of(&layouts, param.typ);
                hover_types.push((
                    local.decl_span,
                    HoverInfo {
                        path: None,
                        ty: format_type(param.typ, hir, &func.generics),
                        layout,
                        docs: None,
                    },
                ));
            }
        }

        Walker {
            typeck: &func.typeck,
            locals: &func.locals,
            hir,
            functions: &functions,
            constants: &constants,
            generics: &func.generics,
            layouts: &layouts,
            map,
            modules,
            hover: &mut hover_types,
            defs: &mut goto_definitions,
            hints: &mut inlay_hints,
        }
        .block(&func.body);
    }

    for (idx, structure) in hir.structs.iter().enumerate() {
        if structure.decl_span != Span::default()
            && let Some(info) =
                type_hover(Type::structure(StructId(idx as u32)), hir, &layouts, map, modules)
        {
            hover_types.push((structure.decl_span, info));
        }
    }
    for enumeration in &hir.enums {
        if enumeration.decl_span != Span::default()
            && let Some(info) =
                type_hover(Type::enumerable(enumeration.id), hir, &layouts, map, modules)
        {
            hover_types.push((enumeration.decl_span, info));
        }
    }
    for constant in &hir.constants {
        if constant.decl_span != Span::default() {
            hover_types
                .push((constant.decl_span, const_hover(constant, hir, &layouts, map, modules)));
        }
    }

    hover_types.sort_unstable_by_key(|(span, _)| span.start.offset());

    let mut symbols = Vec::new();
    let sym = &hir.symbols;
    let (fns, structs, enums, consts) = (&hir.functions, &hir.structs, &hir.enums, &hir.constants);
    collect_symbols(fns, SymbolKind::Function, sym, &mut symbols, |f| f.name, |f| f.decl_span);
    collect_symbols(structs, SymbolKind::Struct, sym, &mut symbols, |s| s.name, |s| s.decl_span);
    collect_symbols(enums, SymbolKind::Enum, sym, &mut symbols, |e| e.name, |e| e.decl_span);
    collect_symbols(consts, SymbolKind::Constant, sym, &mut symbols, |c| c.name, |c| c.decl_span);
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
    pretty_args(qualified.rsplit("::").next().unwrap_or(qualified))
}

#[inline]
fn pretty_args(name: &str) -> String {
    match name.split_once('$') {
        Some((base, args)) => format!("{base}<{}>", args.replace('$', ", ")),
        None => name.to_owned(),
    }
}

fn is_open(typ: Type, hir: &Hir<'_>) -> bool {
    let carries_generic = |typ: Type| match typ.kind() {
        TypeKind::GenericParam(_) => true,
        TypeKind::Ref { to, .. } => matches!(to.kind(), TypeKind::GenericParam(_)),
        _ => false,
    };

    match typ.kind() {
        TypeKind::Struct(id) => hir.structs[id].fields.iter().any(|f| carries_generic(f.typ)),
        TypeKind::Enum(id) => {
            hir.enums[id].variants.iter().any(|v| v.payload.is_some_and(carries_generic))
        },
        _ => false,
    }
}

#[inline(always)]
fn layout_of(layouts: &LayoutTable, typ: Type) -> Option<(u32, u32)> {
    match typ.kind() {
        TypeKind::Unit
        | TypeKind::Never
        | TypeKind::SelfType
        | TypeKind::GenericParam(_)
        | TypeKind::Error => None,
        _ => Some(layouts.type_layout(typ)),
    }
}

/// The module path of every registered file, in `use`-path form: files under
/// the project root become `project::dir::file` (`main.nyx` is the root itself),
/// files under the std root become `std::file`
fn module_paths(
    map: &SourceMap,
    project: &str,
    root: &Path,
    std_root: &Path,
) -> HashMap<FileId, String> {
    map.files()
        .map(|file| {
            let module = match file.name.strip_prefix(root) {
                Ok(relative) => module_path(project, relative),
                Err(_) => match file.name.strip_prefix(std_root) {
                    Ok(relative) => module_path("std", relative),
                    Err(_) => file
                        .name
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| project.to_owned()),
                },
            };
            (file.id, module)
        })
        .collect()
}

fn module_path(root: &str, relative: &Path) -> String {
    let mut segments = vec![root.to_owned()];
    let relative = relative.with_extension("");
    segments.extend(relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()));

    // the entry file is its directory's module
    if segments.len() > 1 && segments.last().is_some_and(|segment| segment == "main") {
        segments.pop();
    }

    segments.join("::")
}

#[inline]
fn module_of(map: &SourceMap, modules: &HashMap<FileId, String>, span: Span) -> Option<String> {
    match span == Span::default() {
        true => None,
        false => modules.get(&map.span_data(span).file).cloned(),
    }
}

fn type_hover(
    typ: Type,
    hir: &Hir<'_>,
    layouts: &LayoutTable,
    map: &SourceMap,
    modules: &HashMap<FileId, String>,
) -> Option<HoverInfo> {
    let (path, ty, docs) = match typ.kind() {
        TypeKind::Struct(id) => {
            let structure = &hir.structs[id];
            let path = module_of(map, modules, structure.decl_span);
            (path, struct_def(structure, hir), hir.doc_text(structure.decl_span))
        },
        TypeKind::Enum(id) => {
            let enumeration = &hir.enums[id];
            let path = module_of(map, modules, enumeration.decl_span);
            (path, enum_def(enumeration, hir), hir.doc_text(enumeration.decl_span))
        },
        _ => return None,
    };

    let layout = match is_open(typ, hir) {
        true => None,
        false => layout_of(layouts, typ),
    };

    Some(HoverInfo { path, ty, layout, docs })
}

fn const_hover(
    constant: &Constant<'_>,
    hir: &Hir<'_>,
    layouts: &LayoutTable,
    map: &SourceMap,
    modules: &HashMap<FileId, String>,
) -> HoverInfo {
    let qualified = hir.symbols.get(constant.name);
    let implementor = implementor_from_name(qualified);
    let path = module_of(map, modules, constant.decl_span).map(|module| match &implementor {
        Some(implementor) => format!("{module}::{implementor}"),
        None => module,
    });

    let mut ty = String::new();
    if constant.is_pub {
        ty.push_str("pub ");
    }
    ty.push_str("const ");
    ty.push_str(&short_name(qualified));
    ty.push_str(": ");
    ty.push_str(&format_type(constant.typ, hir, &[]));
    if let Some(value) = const_value(constant, hir, layouts) {
        ty.push_str(" = ");
        ty.push_str(&value);
    }

    HoverInfo {
        path,
        ty,
        layout: None,
        docs: hir.doc_text(constant.decl_span),
    }
}

// TODO: those things should be better integrated with the compiler
// in the future instead of ad-hoc resolution here

fn const_value(constant: &Constant<'_>, hir: &Hir<'_>, layouts: &LayoutTable) -> Option<String> {
    use crate::parser::expression::UnaryOperator;

    match &constant.value.kind {
        ExpressionKind::Literal(Literal::Float(value)) => Some(value.to_string()),
        ExpressionKind::Literal(Literal::Bool(value)) => Some(value.to_string()),
        ExpressionKind::Literal(Literal::Char(value)) => Some(format!("'{value}'")),
        ExpressionKind::Literal(Literal::Str(symbol)) => {
            Some(format!("\"{}\"", hir.symbols.get(*symbol)))
        },
        ExpressionKind::Unary { operator: UnaryOperator::Neg, expr }
            if matches!(expr.kind, ExpressionKind::Literal(Literal::Float(_))) =>
        {
            let ExpressionKind::Literal(Literal::Float(value)) = expr.kind else {
                return None;
            };
            Some((-value).to_string())
        },
        _ => {
            let value = eval_const_int(constant.value, layouts)?;
            Some(render_int(value, constant.typ))
        },
    }
}

fn eval_const_int(expr: &hir::Expression<'_>, layouts: &LayoutTable) -> Option<i128> {
    use crate::parser::expression::{BinaryOperator, TypeIntrinsicKind, UnaryOperator};

    match &expr.kind {
        ExpressionKind::Literal(Literal::Int(value)) => Some(*value as i128),
        ExpressionKind::Unary { operator: UnaryOperator::Neg, expr } => {
            eval_const_int(expr, layouts).map(i128::wrapping_neg)
        },
        ExpressionKind::Unary { operator: UnaryOperator::Not, expr } => {
            eval_const_int(expr, layouts).map(|value| !value)
        },
        ExpressionKind::Cast { from, .. } => eval_const_int(from, layouts),
        ExpressionKind::TypeIntrinsic { kind, typ } => {
            let (size, align) = layout_of(layouts, *typ)?;
            Some(match kind {
                TypeIntrinsicKind::SizeOf => size as i128,
                TypeIntrinsicKind::AlignOf => align as i128,
            })
        },
        ExpressionKind::Binary { operator, left, right } => {
            let left = eval_const_int(left, layouts)?;
            let right = eval_const_int(right, layouts)?;
            match operator {
                BinaryOperator::Add => left.checked_add(right),
                BinaryOperator::Sub => left.checked_sub(right),
                BinaryOperator::Mul => left.checked_mul(right),
                BinaryOperator::Div => left.checked_div(right),
                BinaryOperator::Shl => left.checked_shl(u32::try_from(right).ok()?),
                BinaryOperator::Shr => left.checked_shr(u32::try_from(right).ok()?),
                BinaryOperator::BitAnd => Some(left & right),
                BinaryOperator::BitOr => Some(left | right),
                BinaryOperator::BitXor => Some(left ^ right),
                _ => None,
            }
        },
        _ => None,
    }
}

fn render_int(value: i128, typ: Type) -> String {
    let bits = match typ.kind() {
        TypeKind::I8 | TypeKind::U8 => 8,
        TypeKind::I16 | TypeKind::U16 => 16,
        TypeKind::I32 | TypeKind::U32 | TypeKind::Char => 32,
        _ => 64,
    };
    let unsigned = matches!(
        typ.kind(),
        TypeKind::U8 | TypeKind::U16 | TypeKind::U32 | TypeKind::U64 | TypeKind::Uptr
    );

    let truncated = (value as u128) & (u128::MAX >> (128 - bits));
    match unsigned {
        true => truncated.to_string(),
        false => {
            let signed = ((truncated << (128 - bits)) as i128) >> (128 - bits);
            match signed < 0 {
                true => format!("{signed} (0x{truncated:X})"),
                false => signed.to_string(),
            }
        },
    }
}

fn fn_hover(
    func: &Function<'_>,
    hir: &Hir<'_>,
    map: &SourceMap,
    modules: &HashMap<FileId, String>,
) -> HoverInfo {
    let implementor = implementor_of(func, hir);
    let mut path = module_of(map, modules, func.decl_span);
    let mut ty = signature(func, hir);

    if let Some(implementor) = implementor {
        path = path.map(|module| format!("{module}::{implementor}"));
        ty = format!("impl {implementor}\n{ty}");
    }

    HoverInfo { path, ty, layout: None, docs: hir.doc_text(func.decl_span) }
}

#[inline]
fn implementor_of(func: &Function<'_>, hir: &Hir<'_>) -> Option<String> {
    match &func.kind {
        FunctionKind::Method(method) => Some(format_type(method.receiver, hir, &func.generics)),
        _ => implementor_from_name(hir.symbols.get(func.name)),
    }
}

#[inline]
fn implementor_from_name(qualified: &str) -> Option<String> {
    let mut segments = qualified.split("::");
    let (_, scope, item) = (segments.next()?, segments.next()?, segments.next());
    item.is_some().then(|| pretty_args(scope))
}

fn signature(func: &Function<'_>, hir: &Hir<'_>) -> String {
    let mut out = String::new();
    let flags = [(func.is_pub, "pub "), (func.inline, "inline "), (func.is_const, "const ")];

    out.extend(flags.into_iter().filter_map(|(flag, word)| flag.then_some(word)));

    out.push_str("fn ");
    out.push_str(&short_name(hir.symbols.get(func.name)));

    let params: Vec<_> = func
        .params
        .iter()
        .map(|p| match hir.symbols.get(func.locals[p.id].name) {
            "self" => match p.typ.kind() {
                TypeKind::Ref { mutable: true, .. } => "&mut self".into(),
                TypeKind::Ref { .. } => "&self".into(),
                _ => "self".into(),
            },
            name => format!("{name}: {}", format_type(p.typ, hir, &func.generics)),
        })
        .collect();

    out.push('(');
    out.push_str(&params.join(", "));
    out.push(')');

    let ret = format_type(func.return_type, hir, &func.generics);
    if ret != "()" {
        out.push_str(": ");
        out.push_str(&ret);
    }

    out
}

fn struct_def(structure: &Struct, hir: &Hir<'_>) -> String {
    let name = nominal_name(structure.name, &structure.generics, hir);
    if structure.fields.is_empty() {
        return format!("struct {name}");
    }

    let fields = structure.fields.iter().map(|f| {
        format!(
            "    {}: {},",
            hir.symbols.get(f.name),
            format_type(f.typ, hir, &structure.generics)
        )
    });

    format!("struct {name} {{\n{}\n}}", truncated(fields, structure.fields.len()))
}

fn enum_def(enumeration: &Enum, hir: &Hir<'_>) -> String {
    let name = nominal_name(enumeration.name, &enumeration.generics, hir);
    if enumeration.variants.is_empty() {
        return format!("enum {name}");
    }

    let variants = enumeration.variants.iter().map(|v| match v.payload {
        Some(typ) => format!(
            "    {}({}),",
            hir.symbols.get(v.name),
            format_type(typ, hir, &enumeration.generics)
        ),
        None => format!("    {},", hir.symbols.get(v.name)),
    });

    format!("enum {name} {{\n{}\n}}", truncated(variants, enumeration.variants.len()))
}

/// join the first [`MAX_HOVER_ITEMS`] lines, eliding the rest with `/* … */`
fn truncated(lines: impl Iterator<Item = String>, total: usize) -> String {
    let mut lines: Vec<_> = lines.take(MAX_HOVER_ITEMS).collect();
    if total > MAX_HOVER_ITEMS {
        lines.push("    /* … */".to_owned());
    }

    lines.join("\n")
}

fn format_type(typ: Type, hir: &Hir<'_>, generics: &[SymbolId]) -> String {
    match typ.kind() {
        TypeKind::Unit => "()".to_owned(),
        TypeKind::Str => "str".to_owned(),
        TypeKind::GenericParam(i) => generics
            .get(i as usize)
            .map(|&name| hir.symbols.get(name).to_owned())
            .unwrap_or_else(|| format!("T{i}")),
        TypeKind::Struct(id) => nominal_name(hir.structs[id].name, &hir.structs[id].generics, hir),
        TypeKind::Enum(id) => nominal_name(hir.enums[id].name, &hir.enums[id].generics, hir),
        TypeKind::Ref { mutable, to } => {
            let typ = format_type(Type::new(to.kind()), hir, generics);
            match mutable {
                true => format!("&mut {typ}"),
                _ => format!("&{typ}"),
            }
        },
        kind => kind.to_string(),
    }
}

fn nominal_name(name: SymbolId, generics: &[SymbolId], hir: &Hir<'_>) -> String {
    let raw = hir.symbols.get(name);
    if generics.is_empty() {
        return short_name(raw);
    }

    let base = raw.rsplit("::").next().unwrap_or(raw);
    let base = base.split('$').next().unwrap_or(base);
    let names: Vec<_> = generics.iter().map(|&g| hir.symbols.get(g)).collect();

    format!("{base}<{}>", names.join(", "))
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

    #[test]
    fn unknown_param_type_keeps_features_alive() {
        let a =
            analyse("param", "fn poisoned(a: Nonexistent): i32 { 1 }\nfn main() { let x = 232; }");
        assert!(a.ok, "recovery must still produce a HIR with live features");
        assert_eq!(a.diagnostics.len(), 1, "exactly the unknown type: {:?}", a.diagnostics);
        assert!(a.inlay_hints.iter().any(|(_, ty)| ty == "i32"), "main still gets its hint");
        assert!(
            a.hover_types.iter().any(|(_, h)| h.ty.contains("fn poisoned")),
            "the poisoned function still hovers as a signature"
        );
    }

    #[test]
    fn errors_in_two_functions_are_both_reported() {
        let a = analyse(
            "two_fns",
            r#"
            fn first(): i32 { true }
            fn second() { let x: bool = 232; }
            fn main() { let y = 1; }
            "#,
        );
        assert!(a.ok, "recovery must still produce a HIR with live features");
        assert_eq!(a.diagnostics.len(), 2, "one error per function: {:?}", a.diagnostics);
        assert!(a.inlay_hints.iter().any(|(_, ty)| ty == "i32"), "main still gets its hint");
    }

    #[test]
    fn unknown_struct_field_type_still_registers_the_struct() {
        let a = analyse(
            "struct_field",
            r#"
            struct Holder { value: Missing, count: i32 }
            fn main() { let h = 1; }
            "#,
        );
        assert!(a.ok, "recovery must still produce a HIR with live features");
        assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
        assert!(
            a.hover_types.iter().any(|(_, h)| h.ty.contains("struct Holder")),
            "the struct must survive a poisoned field"
        );
        assert!(
            a.document_symbols.iter().any(|s| s.name == "Holder"),
            "the outline still lists the struct"
        );
    }

    #[test]
    fn doc_comments_surface_on_item_hover() {
        let a = analyse(
            "docs",
            r#"
            /// Adds two numbers.
            fn add(a: i32, b: i32): i32 { a + b }

            /// A 2D point.
            struct Point { x: i32, y: i32 }

            /// The answer.
            const ANSWER: i32 = 42;

            fn main() {
                let p = Point { x: 1, y: 2 };
                let _ = add(p.x, p.y) + ANSWER;
            }
            "#,
        );
        assert!(a.ok, "{:?}", a.diagnostics);

        let doc_of = |needle: &str| {
            a.hover_types
                .iter()
                .find(|(_, hover)| hover.ty.contains(needle))
                .and_then(|(_, hover)| hover.docs.as_deref())
        };

        assert_eq!(doc_of("fn add"), Some("Adds two numbers."));
        assert_eq!(doc_of("struct Point"), Some("A 2D point."));
        assert_eq!(doc_of("const ANSWER"), Some("The answer."));
        assert_eq!(doc_of("fn main"), None, "an undocumented item has no docs");
    }

    #[test]
    fn impl_method_docs_surface_on_hover() {
        let a = analyse(
            "impl_docs",
            r#"
            struct Point { x: i32 }
            impl Point {
                /// the horizontal coordinate
                fn get(&self): i32 { self.x }
            }
            fn main() {
                let p = Point { x: 1 };
                let _ = p.get();
            }
            "#,
        );
        assert!(a.ok, "{:?}", a.diagnostics);

        let doc = a
            .hover_types
            .iter()
            .find(|(_, hover)| hover.ty.contains("fn get"))
            .and_then(|(_, hover)| hover.docs.as_deref());
        assert_eq!(doc, Some("the horizontal coordinate"));
    }

    #[test]
    fn fieldless_enums_auto_size_while_payload_enums_keep_the_tag() {
        let a = analyse(
            "enum_repr",
            r#"
            enum Direction { North, West, East, South }
            enum Tiny { No, Yes(bool) }
            fn main() {
                let _ = Direction::North;
                let _ = Tiny::No;
            }
            "#,
        );
        assert!(a.ok, "{:?}", a.diagnostics);

        let layout_of = |needle: &str| {
            a.hover_types
                .iter()
                .find_map(|(_, hover)| hover.ty.contains(needle).then_some(hover.layout))
                .flatten()
        };

        assert_eq!(layout_of("enum Direction"), Some((1, 1)));
        assert_eq!(layout_of("enum Tiny"), Some((8, 4)));
    }

    #[test]
    fn broken_initialiser_keeps_the_binding_alive() {
        let a = analyse("broken_init", "fn main() { let d = unknown_fn(); let e = d; }");
        assert!(a.ok, "recovery must still produce a HIR with live features");
        assert_eq!(a.diagnostics.len(), 1, "only the unknown call, once: {:?}", a.diagnostics);
        assert!(
            a.inlay_hints.iter().any(|(_, ty)| ty == "{unknown}"),
            "d stays declared with a poison hint: {:?}",
            a.inlay_hints
        );
    }

    #[test]
    fn duplicate_functions_report_without_killing_analysis() {
        let a = analyse("dup_fn", "fn twice() {}\nfn twice() {}\nfn main() { let z = 42; }");
        assert!(a.ok, "recovery must still produce a HIR with live features");
        assert!(!a.diagnostics.is_empty());
        assert!(a.inlay_hints.iter().any(|(_, ty)| ty == "i32"), "main still gets its hint");
    }
}
