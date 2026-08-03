use frontend::hir::module;
use frontend::hir::{
    self, ArrayId, ArrayType, Block, Constant, Enum, EnumId, ExpressionKind, Function, FunctionId,
    FunctionKind, Hir, InterfaceConstSignature, InterfaceMethodSignature, InterfaceSignature,
    Literal, Local, LocalId, Owner, Parameter, Res, Statement, Struct, StructId, SymbolId,
    SymbolTable, Type, TypeKind, TypeckResults, index_vec::IndexVec,
};
use frontend::{
    diagnostic::AsDiagnostic,
    lexer::token::Span,
    source_map::{FileId, SourceMap},
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Analysis {
    entry: PathBuf,
    overlays: HashMap<PathBuf, String>,
}

/// Hover and go-to-definition data extracted from a HIR pass
#[derive(Debug, Default)]
pub struct SemanticAnalysis {
    pub diagnostics: Vec<CheckError>,
    /// `(span, what it names)` sorted by `span.start.offset()` for binary search
    /// from a cursor position
    pub hover_types: Vec<(Span, HoverTarget)>,
    /// identifier-use span to definition-site span
    pub goto_definitions: HashMap<Span, Span>,
    /// `(name_span, type, owning function)` for every binding, the hint appears
    /// immediately after the binding name
    pub inlay_hints: Vec<(Span, Type, u32)>,
    pub document_symbols: Vec<DocumentSymbol>,
    /// Everything the completion provider can offer, precomputed while the HIR
    /// is still alive
    pub completions: Completions,
    /// `(body span, locals declared in it)` for every function, so a position
    /// inside a body can offer the names that body has in scope
    pub scopes: Vec<(Span, Vec<Completion>)>,
    /// What the tables above name, outliving the arena the HIR was lowered into
    pub index: Index,
    /// Resolves the global spans above to concrete files and line/column.
    pub source_map: SourceMap,
    /// whether the project analysed into a hir at all: syntax and type errors are
    /// recovered from and leave this set, with the feature data above still valid
    /// for whatever resolved
    pub ok: bool,
}

/// The declarations every request renders from
#[derive(Debug, Default)]
pub struct Index {
    pub symbols: SymbolTable,
    pub structs: IndexVec<StructId, Struct>,
    pub enums: IndexVec<EnumId, Enum>,
    pub arrays: IndexVec<ArrayId, ArrayType>,
    pub interfaces: Vec<InterfaceSignature>,
    pub functions: Vec<FnInfo>,
    pub constants: Vec<ConstInfo>,
    /// rendered `///` documentation, keyed by the span of the name it sits above
    pub docs: HashMap<Span, Box<str>>,
    /// the `use`-path form of each file, for the container line above a hover
    pub modules: HashMap<FileId, String>,
}

/// A function without the body and inference results the arena owns
#[derive(Debug)]
pub struct FnInfo {
    pub name: SymbolId,
    pub owner: Owner,
    pub kind: FunctionKind,
    pub params: Vec<Parameter>,
    pub locals: IndexVec<LocalId, Local>,
    pub return_type: Type,
    pub generics: Vec<SymbolId>,
    pub is_const: bool,
    pub is_pub: bool,
    pub inline: bool,
    pub is_unsafe: bool,
    pub decl_span: Span,
    pub name_span: Span,
}

/// A constant with its initialiser already folded, the expression tree being
/// the only thing about it the arena owns
#[derive(Debug)]
pub struct ConstInfo {
    pub name: SymbolId,
    pub owner: Owner,
    pub typ: Type,
    pub value: Option<String>,
    pub is_pub: bool,
    pub decl_span: Span,
    pub name_span: Span,
}

/// The candidates a completion request can draw on
#[derive(Debug, Default)]
pub struct Completions {
    /// members reachable through `.`, keyed by the receiver's nominal type name
    pub members: HashMap<String, Vec<Completion>>,
    /// items reachable through `::`, keyed by a type name (`Point`) or by a
    /// module path (`std::mem`)
    pub associated: HashMap<String, Vec<Completion>>,
    /// every item nameable without a qualifier
    pub globals: Vec<Completion>,
}

/// One offered name
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub label: String,
    pub kind: CompletionKind,
    /// the signature or type shown beside the label
    pub detail: String,
    pub docs: Option<String>,
    /// for a value, the nominal type whose members it exposes through `.`
    pub type_key: Option<String>,
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
    index: &'a Index,
    /// resolves a path span back to its text, the only place segment boundaries
    /// survive: the hir keeps one span for the whole path
    map: &'a SourceMap,
    /// position of the function being walked within [Index::functions]
    function: u32,
    /// resolves a callee by its signature id, [Function::id] is not the
    /// position in [Hir::functions], so positional indexing is wrong
    functions: &'a HashMap<FunctionId, (u32, &'a Function<'h>)>,
    /// resolves a spliced constant use back to its declaration
    constants: &'a HashMap<SymbolId, (u32, &'a Constant<'h>)>,
    hover: &'a mut Vec<(Span, HoverTarget)>,
    defs: &'a mut HashMap<Span, Span>,
    hints: &'a mut Vec<(Span, Type, u32)>,
    /// how each name in this body was declared, so a use hovers as its
    /// declaration rather than as a bare type
    forms: HashMap<LocalId, Binding>,
}

struct CompletionCollector<'a> {
    hir: &'a Index,
    map: &'a SourceMap,
    imported_names: &'a HashSet<String>,
    out: Completions,
}

#[derive(Debug, Clone, Copy)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Constant,
}

/// A top-level declaration a `use` can name, as its position in the [Index]
#[derive(Debug, Clone, Copy)]
enum Importable {
    Function(u32),
    Type(Type),
    Constant(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    Module,
    Function,
    Method,
    Field,
    Variant,
    Struct,
    Enum,
    Interface,
    Primitive,
    Constant,
    Variable,
}

/// What a span names, resolved against an [Index] only when a hover asks
///
/// Bodies hold far more spans than any session will ever hover, so nothing here
/// is rendered up front
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HoverTarget {
    /// an expression, shown as the type it inferred to
    Type {
        typ: Type,
        function: u32,
    },
    /// a type named in source, shown as the declaration it resolves to
    Nominal {
        typ: Type,
        function: Option<u32>,
    },
    /// a name a body introduced, shown as it was declared
    Local {
        function: u32,
        local: LocalId,
        form: Binding,
    },
    Function(u32),
    Constant(u32),
    Struct(StructId),
    Field {
        structure: StructId,
        field: u32,
    },
    Enum(EnumId),
    Variant {
        enumeration: EnumId,
        variant: u32,
    },
    Interface(u32),
    InterfaceMethod {
        interface: u32,
        method: u32,
    },
    InterfaceConstant {
        interface: u32,
        constant: u32,
    },
}

/// How a name entered scope, which decides how its hover reads back
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// a `let`, shown with its keyword and mutability
    Let,
    /// destructured by a pattern, which has no keyword of its own
    Pattern,
}

/// How many fields/variants a hover shows before truncating with `// …`
const MAX_HOVER_ITEMS: usize = 5;

/// A single compile-time error in structured form so consumers can render it as richly as the CLI
type CheckError = frontend::diagnostic::RichDiagnostic;

impl Analysis {
    /// Create a new analysis builder starting at the given entry path.
    pub fn new(entry: impl Into<PathBuf>) -> Self {
        Self { entry: entry.into(), overlays: HashMap::new() }
    }

    /// Add a single in-memory file overlay (e.g. unsaved editor buffer).
    #[cfg(test)]
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
        let source_map = frontend::diagnostic::take_source_map();

        let mut analysis = match result {
            // recovery keeps a (partial) HIR even with errors: surface every
            // recovered diagnostic while still serving features for what resolved
            Ok(mut hir) => {
                let modules = module_paths(&source_map, &name, &root, &std_root);
                let diagnostics = std::mem::take(&mut hir.diagnostics);
                let mut analysis = walk_hir(hir, &source_map, modules);
                analysis.diagnostics = diagnostics;
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

impl Index {
    /// render what `target` names
    pub fn hover(&self, target: HoverTarget, map: &SourceMap) -> Option<HoverInfo> {
        let generics = |function: Option<u32>| match function {
            Some(at) => self.functions[at as usize].generics.as_slice(),
            None => &[],
        };

        let info = match target {
            HoverTarget::Type { typ, function } => self.type_info(typ, generics(Some(function))),
            HoverTarget::Nominal { typ, function } => self
                .nominal_hover(typ, map)
                .unwrap_or_else(|| self.type_info(typ, generics(function))),
            HoverTarget::Local { function, local, form } => self.local_hover(function, local, form),
            HoverTarget::Function(at) => self.fn_hover(&self.functions[at as usize], map),
            HoverTarget::Constant(at) => self.const_hover(&self.constants[at as usize], map),
            HoverTarget::Struct(id) => self.nominal_hover(Type::structure(id), map)?,
            HoverTarget::Field { structure, field } => self.field_hover(structure, field, map),
            HoverTarget::Enum(id) => self.nominal_hover(Type::enumerable(id), map)?,
            HoverTarget::Variant { enumeration, variant } => {
                self.variant_hover(enumeration, variant, map)
            },
            HoverTarget::Interface(at) => self.interface_hover(at as usize, map),
            HoverTarget::InterfaceMethod { interface, method } => {
                self.interface_method_hover(interface as usize, method as usize, map)
            },
            HoverTarget::InterfaceConstant { interface, constant } => {
                self.interface_constant_hover(interface as usize, constant as usize, map)
            },
        };

        Some(info)
    }

    /// the type an inlay hint annotates a binding with
    #[inline]
    pub fn hint(&self, typ: Type, function: u32) -> String {
        format_type(typ, self, &self.functions[function as usize].generics)
    }

    #[inline]
    fn docs(&self, name_span: Span) -> Option<String> {
        self.docs.get(&name_span).map(|docs| docs.to_string())
    }

    /// The module path of the file `span` falls in, `none` for synthetic spans
    #[inline]
    fn module_of(&self, map: &SourceMap, span: Span) -> Option<String> {
        match span == Span::default() {
            true => None,
            false => self.modules.get(&map.span_data(span).file).cloned(),
        }
    }

    fn type_info(&self, typ: Type, generics: &[SymbolId]) -> HoverInfo {
        let layout = match is_open(typ, self) {
            true => None,
            false => layout_of(self, typ),
        };

        HoverInfo {
            path: None,
            ty: format_type(typ, self, generics),
            layout,
            docs: None,
        }
    }

    /// a binding read back as it was declared, so it shows its `let`, its
    /// mutability and its inferred type at once
    fn local_hover(&self, function: u32, local: LocalId, form: Binding) -> HoverInfo {
        let func = &self.functions[function as usize];
        let local = &func.locals[local];
        let prefix = match (form, local.mutable) {
            (Binding::Let, true) => "let mut ",
            (Binding::Let, false) => "let ",
            (Binding::Pattern, _) => "",
        };

        HoverInfo {
            ty: format!(
                "{prefix}{}: {}",
                self.symbols.get(local.name),
                format_type(local.typ, self, &func.generics)
            ),
            ..self.type_info(local.typ, &func.generics)
        }
    }
}

impl FnInfo {
    #[inline]
    fn implementor(&self, index: &Index) -> Option<String> {
        implementor_of(self.owner, index, &self.generics)
    }

    /// the type a `.` call reaches this through, `none` for an associated function
    ///
    /// [FunctionKind] cannot answer this: an intrinsic method takes a receiver
    /// without being a [FunctionKind::Method]
    fn receiver(&self, index: &Index) -> Option<Type> {
        let first = self.params.first()?;
        (index.symbols.get(self.locals[first.id].name) == "self").then_some(first.typ)
    }
}

impl ConstInfo {
    fn of(constant: &Constant<'_>, index: &Index) -> Self {
        Self {
            name: constant.name,
            owner: constant.owner,
            typ: constant.typ,
            value: const_value(constant, index),
            is_pub: constant.is_pub,
            decl_span: constant.decl_span,
            name_span: constant.name_span,
        }
    }
}

impl<'a, 'h> Walker<'a, 'h> {
    fn qualified(&mut self, path: Span, callee: HoverTarget, name_span: Span, owner: Type) {
        let Some((qualifier, name)) = split_path(self.map, path) else {
            return;
        };

        self.hover.push((name, callee));
        if name_span != Span::default() {
            self.defs.insert(name, name_span);
        }

        self.hover
            .push((qualifier, HoverTarget::Nominal { typ: owner, function: None }));
        match nominal_name_span(owner, self.index) {
            Some(target) if target != Span::default() => {
                self.defs.insert(qualifier, target);
            },
            _ => {},
        }
    }

    fn block(&mut self, block: &Block<'h>) {
        for stmt in block.statements {
            self.stmt(stmt);
        }
    }

    /// Record the hint and hover for a name a body introduces
    fn binding(&mut self, id: LocalId, form: Binding) {
        let span = self.locals[id].decl_span;

        self.hints.push((span, self.locals[id].typ, self.function));
        self.hover
            .push((span, HoverTarget::Local { function: self.function, local: id, form }));
        self.forms.insert(id, form);
    }

    fn pattern(&mut self, pattern: &hir::Pattern<'h>) {
        match &pattern.kind {
            hir::PatternKind::Binding(id) => self.binding(*id, Binding::Pattern),
            hir::PatternKind::Bind { local, sub } => {
                self.binding(*local, Binding::Pattern);
                self.pattern(sub);
            },
            hir::PatternKind::Variant { sub: Some(sub), .. } => self.pattern(sub),
            hir::PatternKind::Struct { fields, .. } => {
                for (_, sub) in *fields {
                    self.pattern(sub);
                }
            },
            hir::PatternKind::Or(alternatives) => {
                for alternative in *alternatives {
                    self.pattern(alternative);
                }
            },
            hir::PatternKind::Wildcard
            | hir::PatternKind::Variant { sub: None, .. }
            | hir::PatternKind::Literal(_)
            | hir::PatternKind::Range { .. } => {},
        }
    }

    fn stmt(&mut self, stmt: &Statement<'h>) {
        match stmt {
            Statement::LetInit { id, init } => {
                self.binding(*id, Binding::Let);
                self.expr(init);
            },
            Statement::LetUninit { id } => self.binding(*id, Binding::Let),
            Statement::Expr(e) | Statement::Return(Some(e)) => self.expr(e),
            Statement::If { condition, then_block, else_block } => {
                self.expr(condition);
                self.block(then_block);
                if let Some(eb) = else_block {
                    self.block(eb);
                }
            },
            Statement::Loop { kind, body } => {
                match kind {
                    hir::LoopKind::Infinite => {},
                    hir::LoopKind::Range { start, end, .. } => {
                        self.expr(start);
                        self.expr(end);
                    },
                    hir::LoopKind::Iterable { iterable, .. } => self.expr(iterable),
                }
                self.block(body);
            },
            Statement::Block(b) => self.block(b),
            Statement::Return(None) | Statement::Break | Statement::Continue => {},
        }
    }

    fn expr(&mut self, expr: &hir::Expression<'h>) {
        if let Some(symbol) = self.typeck.const_use(expr.id)
            && let Some(&(position, constant)) = self.constants.get(&symbol)
        {
            self.hover.push((expr.span, HoverTarget::Constant(position)));
            if constant.name_span != Span::default() {
                self.defs.insert(expr.span, constant.name_span);
            }
            return;
        }

        // a variant constructor lowers to a call, so both share the resolution
        // recorded against the call expression :D
        let variant = match &expr.kind {
            ExpressionKind::Call { .. } => match self.typeck.type_dependent_def(expr.id) {
                Some(Res::Variant { id, index }) => self.index.enums[id]
                    .variants
                    .get(index)
                    .map(|variant| (id, index as u32, variant.name_span)),
                _ => None,
            },
            _ => None,
        };

        if let Some((enumeration, index, name_span)) = variant {
            if name_span != Span::default() {
                self.defs.insert(expr.span, name_span);
            }
            if let ExpressionKind::Call { callee, .. } = &expr.kind {
                let target = HoverTarget::Variant { enumeration, variant: index };
                self.qualified(callee.span, target, name_span, Type::enumerable(enumeration));
            }
        }

        let resolved = match &expr.kind {
            ExpressionKind::Call { .. } | ExpressionKind::MethodCall { .. } => self
                .typeck
                .type_dependent_def(expr.id)
                .and_then(Res::function)
                .and_then(|id| self.functions.get(&id).copied()),
            _ => None,
        };

        // a field access names the field's declaration, not just its type
        let field = match &expr.kind {
            ExpressionKind::Field { base, field } => {
                match through_reference(self.typeck.type_of(base.id)).kind() {
                    TypeKind::Struct(id) => self.index.structs[id]
                        .fields
                        .iter()
                        .position(|f| f.name == *field)
                        .map(|at| (id, at as u32)),
                    _ => None,
                }
            },
            _ => None,
        };

        if let Some((structure, at)) = field {
            let name_span = self.index.structs[structure].fields[at as usize].name_span;
            if name_span != Span::default() {
                self.defs.insert(expr.span, name_span);
            }
        }

        let function = self.function;
        let hover = match (variant, resolved, &expr.kind) {
            (Some((enumeration, variant, _)), ..) => HoverTarget::Variant { enumeration, variant },
            _ if field.is_some() => {
                let (structure, field) = field.expect("just checked");
                HoverTarget::Field { structure, field }
            },
            (None, Some((position, _)), _) => HoverTarget::Function(position),
            // expressions that name a type: struct literals, paths, and enum
            // variant references (lowered to enum-typed literals) show its
            // full declaration, like the declaration site does
            (
                None,
                None,
                ExpressionKind::Struct { .. }
                | ExpressionKind::Path(_)
                | ExpressionKind::Literal(_),
            ) => {
                HoverTarget::Nominal { typ: self.typeck.type_of(expr.id), function: Some(function) }
            },
            (None, None, ExpressionKind::Local(id)) => match self.forms.get(id) {
                Some(&form) => HoverTarget::Local { function, local: *id, form },
                None => HoverTarget::Type { typ: self.typeck.type_of(expr.id), function },
            },
            _ => HoverTarget::Type { typ: self.typeck.type_of(expr.id), function },
        };

        self.hover.push((expr.span, hover));

        match &expr.kind {
            ExpressionKind::Local(id) => {
                self.defs.insert(expr.span, self.locals[*id].decl_span);
            },
            // a leaf here: the value tree belongs to the definition site and
            // lives in the constant's own ExprId space
            ExpressionKind::Const(constant) => {
                self.defs.insert(expr.span, constant.name_span);
            },
            ExpressionKind::Call { callee, args } => {
                if let Some((position, target)) = resolved {
                    self.defs.insert(callee.span, target.name_span);
                    if let Owner::Inherent(typ) | Owner::Interface { on: typ, .. } = target.owner {
                        let hover = HoverTarget::Function(position);
                        self.qualified(callee.span, hover, target.name_span, typ);
                    }
                }
                for arg in *args {
                    self.expr(arg);
                }
            },
            ExpressionKind::MethodCall { receiver, args, .. } => {
                if let Some((_, target)) = resolved {
                    self.defs.insert(expr.span, target.name_span);
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
            ExpressionKind::Array { elements } => {
                for element in *elements {
                    self.expr(element);
                }
            },
            ExpressionKind::ArrayRepeat { value, .. } => self.expr(value),
            ExpressionKind::Index { base, index } => {
                self.expr(base);
                self.expr(index);
            },
            ExpressionKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in *arms {
                    self.pattern(arm.pattern);
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

impl<'a> CompletionCollector<'a> {
    fn new(hir: &'a Index, map: &'a SourceMap, imported_names: &'a HashSet<String>) -> Self {
        Self { hir, map, imported_names, out: Completions::default() }
    }

    fn collect(mut self) -> Completions {
        let hir = self.hir;
        self.register_modules();

        self.out
            .globals
            .extend(frontend::PRIMITIVE_TYPES.iter().map(|&name| Completion {
                label: name.to_owned(),
                kind: CompletionKind::Primitive,
                detail: format!("primitive type {name}"),
                docs: None,
                type_key: None,
            }));

        for (idx, structure) in hir.structs.iter().enumerate() {
            let name = base_name(hir.symbols.get(structure.name));
            let typ = Type::structure(StructId(idx as u32));
            let key = base_name(&format_type(typ, hir, &[]));

            let fields = self.out.members.entry(key).or_default();
            for field in &structure.fields {
                fields.push(Completion {
                    label: hir.symbols.get(field.name).to_owned(),
                    kind: CompletionKind::Field,
                    detail: format_type(field.typ, hir, &structure.generics),
                    docs: hir.docs(field.name_span),
                    type_key: type_key(field.typ, hir),
                });
            }

            if structure.decl_span != Span::default() {
                let nominal = nominal_name(structure.name, &structure.generics, hir);
                let candidate = Completion {
                    label: name,
                    kind: CompletionKind::Struct,
                    detail: format!("struct {nominal}"),
                    docs: hir.docs(structure.decl_span),
                    type_key: None,
                };
                self.export(structure.decl_span, candidate);
            }
        }

        for enumeration in hir.enums.iter() {
            let name = base_name(hir.symbols.get(enumeration.name));
            let variants = self.out.associated.entry(name.clone()).or_default();
            for variant in &enumeration.variants {
                variants.push(Completion {
                    label: hir.symbols.get(variant.name).to_owned(),
                    kind: CompletionKind::Variant,
                    detail: match variant.payload {
                        Some(payload) => format_type(payload, hir, &enumeration.generics),
                        None => variant.value.to_string(),
                    },
                    docs: hir.docs(variant.name_span),
                    type_key: None,
                });
            }

            if enumeration.decl_span != Span::default() {
                let nominal = nominal_name(enumeration.name, &enumeration.generics, hir);
                let candidate = Completion {
                    label: name,
                    kind: CompletionKind::Enum,
                    detail: format!("enum {nominal}"),
                    docs: hir.docs(enumeration.decl_span),
                    type_key: None,
                };
                self.export(enumeration.decl_span, candidate);
            }
        }

        for interface in &hir.interfaces {
            let name = base_name(hir.symbols.get(interface.name));
            let methods = self.out.associated.entry(name.clone()).or_default();
            for method in &interface.methods {
                methods.push(Completion {
                    label: base_name(hir.symbols.get(method.name)),
                    kind: CompletionKind::Method,
                    detail: interface_signature(method, interface, hir),
                    docs: hir.docs(method.decl_span),
                    type_key: None,
                });
            }
            for constant in &interface.constants {
                methods.push(Completion {
                    label: base_name(hir.symbols.get(constant.name)),
                    kind: CompletionKind::Constant,
                    detail: interface_const_signature(constant, interface, hir),
                    docs: hir.docs(constant.decl_span),
                    type_key: type_key(constant.typ, hir),
                });
            }
            let nominal = nominal_name(interface.name, &interface.generic_params, hir);

            let candidate = Completion {
                label: name,
                kind: CompletionKind::Interface,
                detail: format!("interface {nominal}"),
                docs: hir.docs(interface.decl_span),
                type_key: None,
            };
            self.export(interface.decl_span, candidate);
        }

        for func in &hir.functions {
            let qualified = hir.symbols.get(func.name);
            let receiver = func.receiver(hir);
            let candidate = Completion {
                label: base_name(qualified),
                kind: match receiver {
                    Some(_) => CompletionKind::Method,
                    None => CompletionKind::Function,
                },
                detail: signature(func, hir),
                docs: hir.docs(func.decl_span),
                type_key: type_key(func.return_type, hir),
            };

            match (receiver, implementor_of(func.owner, hir, &[])) {
                (Some(receiver), _) => {
                    let key = base_name(&format_type(through_reference(receiver), hir, &[]));
                    self.out.members.entry(key).or_default().push(candidate);
                },
                (None, Some(implementor)) => {
                    self.out.associated.entry(base_name(&implementor)).or_default().push(candidate);
                },
                (None, None) => {
                    if let Some(module) = hir.module_of(self.map, func.decl_span) {
                        self.out.associated.entry(module).or_default().push(candidate.clone());
                    }
                    if self.is_open_name(func.decl_span, &candidate.label) {
                        self.out.globals.push(candidate);
                    }
                },
            }
        }

        for constant in &hir.constants {
            let qualified = hir.symbols.get(constant.name);
            let candidate = Completion {
                label: base_name(qualified),
                kind: CompletionKind::Constant,
                detail: format_type(constant.typ, hir, &[]),
                docs: hir.docs(constant.decl_span),
                type_key: type_key(constant.typ, hir),
            };

            match implementor_of(constant.owner, hir, &[]) {
                Some(implementor) => {
                    self.out.associated.entry(base_name(&implementor)).or_default().push(candidate);
                },
                None => {
                    if let Some(module) = hir.module_of(self.map, constant.decl_span) {
                        self.out.associated.entry(module).or_default().push(candidate.clone());
                    }
                    if self.is_open_name(constant.decl_span, &candidate.label) {
                        self.out.globals.push(candidate);
                    }
                },
            }
        }

        for list in self.out.members.values_mut().chain(self.out.associated.values_mut()) {
            dedup_by_label(list);
        }
        dedup_by_label(&mut self.out.globals);

        self.out
    }

    fn export(&mut self, decl_span: Span, candidate: Completion) {
        if let Some(module) = self.hir.module_of(self.map, decl_span) {
            self.out.associated.entry(module).or_default().push(candidate.clone());
        }
        if self.is_open_name(decl_span, &candidate.label) {
            self.out.globals.push(candidate);
        }
    }

    fn is_open_name(&self, decl_span: Span, label: &str) -> bool {
        match self.hir.module_of(self.map, decl_span).as_deref() {
            Some(module) if module.starts_with("std::") => self.imported_names.contains(label),
            _ => true,
        }
    }

    fn register_modules(&mut self) {
        for path in self.hir.modules.values() {
            let mut prefix = String::new();

            for segment in path.split("::") {
                let full = match prefix.is_empty() {
                    true => segment.to_owned(),
                    false => format!("{prefix}::{segment}"),
                };
                let candidate = Completion {
                    label: segment.to_owned(),
                    kind: CompletionKind::Module,
                    detail: format!("mod {full}"),
                    docs: None,
                    type_key: None,
                };

                match prefix.is_empty() {
                    true => self.out.globals.push(candidate),
                    false => self.out.associated.entry(prefix.clone()).or_default().push(candidate),
                }

                prefix = full;
            }
        }
    }
}

impl From<Function<'_>> for FnInfo {
    fn from(value: Function<'_>) -> Self {
        Self {
            name: value.name,
            owner: value.owner,
            kind: value.kind,
            params: value.params,
            locals: value.locals,
            return_type: value.return_type,
            generics: value.generics,
            is_const: value.is_const,
            is_pub: value.is_pub,
            inline: value.inline,
            is_unsafe: value.is_unsafe,
            decl_span: value.decl_span,
            name_span: value.name_span,
        }
    }
}

fn walk_hir(hir: Hir<'_>, map: &SourceMap, modules: HashMap<FileId, String>) -> SemanticAnalysis {
    let Hir {
        symbols,
        structs,
        enums,
        arrays,
        functions,
        constants,
        interfaces,
        docs,
        imports,
        type_refs,
        ..
    } = hir;

    let mut index = Index {
        symbols,
        structs,
        enums,
        arrays,
        interfaces,
        docs,
        modules,
        functions: Vec::new(),
        constants: Vec::new(),
    };

    let mut hover_types = Vec::new();
    let mut goto_definitions = HashMap::new();
    let mut inlay_hints = Vec::new();

    // a signature id is not a position in the function table, so a callee can
    // only be resolved through this
    let by_id: HashMap<_, _> = functions
        .iter()
        .enumerate()
        .map(|(at, function)| (function.id, (at as u32, function)))
        .collect();
    let by_name: HashMap<_, _> = constants
        .iter()
        .enumerate()
        .map(|(at, constant)| (constant.name, (at as u32, constant)))
        .collect();

    for (at, func) in functions.iter().enumerate() {
        let at = at as u32;
        if func.decl_span != Span::default() {
            hover_types.push((func.decl_span, HoverTarget::Function(at)));
        }

        let mut forms = HashMap::new();
        for param in &func.params {
            let local = &func.locals[param.id];
            forms.insert(param.id, Binding::Pattern);

            if local.decl_span != Span::default() {
                hover_types.push((
                    local.decl_span,
                    HoverTarget::Local { function: at, local: param.id, form: Binding::Pattern },
                ));
            }
        }

        Walker {
            typeck: &func.typeck,
            locals: &func.locals,
            index: &index,
            map,
            function: at,
            functions: &by_id,
            constants: &by_name,
            hover: &mut hover_types,
            defs: &mut goto_definitions,
            hints: &mut inlay_hints,
            forms,
        }
        .block(&func.body);
    }

    for (at, structure) in index.structs.iter().enumerate() {
        if structure.decl_span == Span::default() {
            continue;
        }
        let id = StructId(at as u32);
        hover_types.push((structure.decl_span, HoverTarget::Struct(id)));
        for (at, field) in structure.fields.iter().enumerate() {
            if field.name_span != Span::default() {
                hover_types.push((
                    field.name_span,
                    HoverTarget::Field { structure: id, field: at as u32 },
                ));
            }
        }
    }
    for enumeration in index.enums.iter() {
        if enumeration.decl_span == Span::default() {
            continue;
        }
        hover_types.push((enumeration.decl_span, HoverTarget::Enum(enumeration.id)));
        for (at, variant) in enumeration.variants.iter().enumerate() {
            if variant.name_span != Span::default() {
                hover_types.push((
                    variant.name_span,
                    HoverTarget::Variant { enumeration: enumeration.id, variant: at as u32 },
                ));
            }
        }
    }
    for (at, constant) in constants.iter().enumerate() {
        if constant.decl_span != Span::default() {
            hover_types.push((constant.decl_span, HoverTarget::Constant(at as u32)));
        }
    }
    for (at, interface) in index.interfaces.iter().enumerate() {
        if interface.decl_span == Span::default() {
            continue;
        }
        hover_types.push((interface.decl_span, HoverTarget::Interface(at as u32)));
        for (method, signature) in interface.methods.iter().enumerate() {
            if signature.name_span != Span::default() {
                hover_types.push((
                    signature.name_span,
                    HoverTarget::InterfaceMethod { interface: at as u32, method: method as u32 },
                ));
            }
        }
        for (constant, signature) in interface.constants.iter().enumerate() {
            if signature.name_span != Span::default() {
                hover_types.push((
                    signature.name_span,
                    HoverTarget::InterfaceConstant {
                        interface: at as u32,
                        constant: constant as u32,
                    },
                ));
            }
        }
    }

    for (&span, &typ) in &type_refs {
        hover_types.push((span, HoverTarget::Nominal { typ, function: None }));
        match nominal_name_span(typ, &index) {
            Some(target) if target != Span::default() => {
                goto_definitions.insert(span, target);
            },
            _ => {},
        }
    }

    index.constants = constants.iter().map(|constant| ConstInfo::of(constant, &index)).collect();
    index.functions = functions.into_iter().map(FnInfo::from).collect();

    let imported_names: HashSet<_> =
        imports.iter().map(|(_, name)| index.symbols.get(*name).to_owned()).collect();
    let importable = importable_items(&index);
    for (span, name) in imports {
        let Some(&item) = importable.get(index.symbols.get(name)) else {
            continue;
        };
        let name_span = item.name_span(&index);
        hover_types.push((span, item.into()));
        if name_span != Span::default() {
            goto_definitions.insert(span, name_span);
        }
    }

    hover_types.sort_unstable_by_key(|(span, _)| span.start.offset());

    let scopes = index
        .functions
        .iter()
        .filter(|func| func.decl_span != Span::default())
        .map(|func| (func.decl_span, scope_of(func, &index)))
        .collect();

    let mut symbols = Vec::new();
    let sym = &index.symbols;
    let (fns, structs, enums) = (&index.functions, &index.structs, &index.enums);
    let consts = &index.constants;
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
        completions: completions(&index, map, &imported_names),
        scopes,
        index,
        source_map: SourceMap::default(),
        ok: true,
    }
}

fn scope_of(func: &FnInfo, hir: &Index) -> Vec<Completion> {
    let mut locals: Vec<_> = func
        .locals
        .iter()
        .filter(|local| local.decl_span != Span::default())
        .map(|local| Completion {
            label: hir.symbols.get(local.name).to_owned(),
            kind: CompletionKind::Variable,
            detail: format_type(local.typ, hir, &func.generics),
            docs: None,
            type_key: type_key(local.typ, hir),
        })
        .collect();

    dedup_by_label(&mut locals);
    locals
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
fn base_name(qualified: &str) -> String {
    let tail = qualified.rsplit("::").next().unwrap_or(qualified);
    tail.split('$').next().unwrap_or(tail).to_owned()
}

#[inline]
fn pretty_args(name: &str) -> String {
    match name.split_once('$') {
        Some((base, args)) => format!("{base}<{}>", args.replace('$', ", ")),
        None => name.to_owned(),
    }
}

fn is_open(typ: Type, hir: &Index) -> bool {
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
fn layout_of(hir: &Index, typ: Type) -> Option<(u32, u32)> {
    match typ.kind() {
        TypeKind::Unit
        | TypeKind::Never
        | TypeKind::SelfType
        | TypeKind::GenericParam(_)
        | TypeKind::Error => None,
        _ => Some(hir::type_layout(typ, &hir.structs, &hir.enums, &hir.arrays)),
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
            // std answers for its own files first: a project whose root
            // contains them (nyx itself) would otherwise claim them
            let module = match file.name.strip_prefix(std_root) {
                Ok(relative) => module_path("std", relative),
                Err(_) => match file.name.strip_prefix(root) {
                    Ok(relative) => module_path(project, relative),
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

impl Importable {
    fn name_span(self, index: &Index) -> Span {
        match self {
            Self::Function(at) => index.functions[at as usize].name_span,
            Self::Constant(at) => index.constants[at as usize].name_span,
            Self::Type(typ) => nominal_name_span(typ, index).unwrap_or_default(),
        }
    }
}

impl From<Importable> for HoverTarget {
    fn from(value: Importable) -> Self {
        match value {
            Importable::Function(at) => Self::Function(at),
            Importable::Constant(at) => Self::Constant(at),
            Importable::Type(typ) => Self::Nominal { typ, function: None },
        }
    }
}

/// index every top-level declaration by the bare name a `use` would import it under
fn importable_items(index: &Index) -> HashMap<&str, Importable> {
    let mut items = HashMap::new();

    for (at, structure) in index.structs.iter().enumerate() {
        let name = index.symbols.get(structure.name);
        items.insert(name, Importable::Type(Type::structure(StructId(at as u32))));
    }
    for enumeration in index.enums.iter() {
        let name = index.symbols.get(enumeration.name);
        items.insert(name, Importable::Type(Type::enumerable(enumeration.id)));
    }
    for (at, func) in index.functions.iter().enumerate() {
        if let Some(name) = importable_name(index.symbols.get(func.name), func.owner) {
            items.insert(name, Importable::Function(at as u32));
        }
    }
    for (at, constant) in index.constants.iter().enumerate() {
        if let Some(name) = importable_name(index.symbols.get(constant.name), constant.owner) {
            items.insert(name, Importable::Constant(at as u32));
        }
    }

    items
}

#[inline]
fn importable_name(qualified: &str, owner: Owner) -> Option<&str> {
    matches!(owner, Owner::Free).then(|| qualified.rsplit("::").next().unwrap_or(qualified))
}

/// The name a type's members are indexed under, references being transparent
/// because a method call auto-references its receiver
#[inline]
fn type_key(typ: Type, hir: &Index) -> Option<String> {
    let typ = through_reference(typ);
    match typ.kind() {
        TypeKind::Infer(_) | TypeKind::Error | TypeKind::Unit | TypeKind::Never => None,
        _ => Some(base_name(&format_type(typ, hir, &[]))),
    }
}

/// collect every name completion can offer, keyed by how it is reached
#[inline]
fn completions(hir: &Index, map: &SourceMap, imported_names: &HashSet<String>) -> Completions {
    CompletionCollector::new(hir, map, imported_names).collect()
}

/// drop repeats a monomorphised template leaves behind, keeping source order
fn dedup_by_label(list: &mut Vec<Completion>) {
    let mut seen = std::collections::HashSet::new();
    list.retain(|item| seen.insert((item.label.clone(), item.kind)));
    list.sort_by(|a, b| a.label.cmp(&b.label));
}

/// the type a field access reaches through, references being transparent
///
/// [RefTarget](frontend::hir::RefTarget) forbids nesting, so one hop always suffices
#[inline]
fn through_reference(typ: Type) -> Type {
    match typ.kind() {
        TypeKind::Ref { to, .. } | TypeKind::Raw { to, .. } => Type::new(to.kind()),
        _ => typ,
    }
}

/// Split a `Qualifier::name` span into the qualifier and the trailing name,
/// `none` when the path has no qualifier
fn split_path(map: &SourceMap, path: Span) -> Option<(Span, Span)> {
    let (file, range) = map.local_range(path);
    let at = map.source(file).get(range)?.rfind("::")?;

    let start = path.start.0;
    let qualifier = Span::new(path.start, frontend::BytePos(start + at as u32));
    let name = Span::new(frontend::BytePos(start + at as u32 + 2), path.end);

    Some((qualifier, name))
}

#[inline]
fn nominal_name_span(typ: Type, hir: &Index) -> Option<Span> {
    match typ.kind() {
        TypeKind::Struct(id) => Some(hir.structs[id].name_span),
        TypeKind::Enum(id) => Some(hir.enums[id].name_span),
        _ => None,
    }
}

impl Index {
    fn field_hover(&self, id: StructId, field: u32, map: &SourceMap) -> HoverInfo {
        let structure = &self.structs[id];
        let field = &structure.fields[field as usize];
        let owner = nominal_name(structure.name, &structure.generics, self);
        let path = self
            .module_of(map, structure.decl_span)
            .map(|module| format!("{module}::{owner}"));

        HoverInfo {
            path,
            ty: format!(
                "{}: {}",
                self.symbols.get(field.name),
                format_type(field.typ, self, &structure.generics)
            ),
            layout: layout_of(self, field.typ),
            docs: self.docs(field.name_span),
        }
    }

    fn variant_hover(&self, id: EnumId, variant: u32, map: &SourceMap) -> HoverInfo {
        let enumeration = &self.enums[id];
        let variant = &enumeration.variants[variant as usize];
        let owner = nominal_name(enumeration.name, &enumeration.generics, self);
        let path = self
            .module_of(map, enumeration.decl_span)
            .map(|module| format!("{module}::{owner}"));

        let name = self.symbols.get(variant.name);
        let ty = match variant.payload {
            Some(payload) => {
                format!("{owner}::{name}({})", format_type(payload, self, &enumeration.generics))
            },
            None => format!("{owner}::{name} = {}", variant.value),
        };

        // the variant's own layout, not the enum's: a fieldless one carries
        // nothing, so it is zero-sized like an empty struct
        let layout = match variant.payload {
            Some(payload) if is_open(payload, self) => None,
            Some(payload) => layout_of(self, payload),
            None => Some((0, 1)),
        };

        HoverInfo { path, ty, layout, docs: self.docs(variant.name_span) }
    }

    fn interface_hover(&self, at: usize, map: &SourceMap) -> HoverInfo {
        let interface = &self.interfaces[at];
        let name = nominal_name(interface.name, &interface.generic_params, self);
        let mut ty = format!("interface {name}");

        if !interface.superinterfaces.is_empty() {
            let bounds: Vec<_> = interface
                .superinterfaces
                .iter()
                .map(|&s| short_name(self.symbols.get(s)))
                .collect();
            ty.push_str(": ");
            ty.push_str(&bounds.join(" + "));
        }

        let item_count = interface.methods.len() + interface.constants.len();
        if item_count != 0 {
            let items = interface
                .constants
                .iter()
                .map(|constant| {
                    format!("    {};", interface_const_signature(constant, interface, self))
                })
                .chain(interface.methods.iter().map(|method| {
                    format!("    {};", interface_signature(method, interface, self))
                }));
            ty = format!("{ty} {{\n{}\n}}", truncated(items, item_count));
        }

        HoverInfo {
            path: self.module_of(map, interface.decl_span),
            ty,
            layout: None,
            docs: self.docs(interface.decl_span),
        }
    }

    fn interface_method_hover(&self, at: usize, method: usize, map: &SourceMap) -> HoverInfo {
        let interface = &self.interfaces[at];
        let method = &interface.methods[method];
        let owner = nominal_name(interface.name, &interface.generic_params, self);
        let path = self
            .module_of(map, interface.decl_span)
            .map(|module| format!("{module}::{owner}"));

        HoverInfo {
            path,
            ty: format!("interface {owner}\n{}", interface_signature(method, interface, self)),
            layout: None,
            docs: self.docs(method.decl_span),
        }
    }

    fn interface_constant_hover(&self, at: usize, constant: usize, map: &SourceMap) -> HoverInfo {
        let interface = &self.interfaces[at];
        let constant = &interface.constants[constant];
        let owner = nominal_name(interface.name, &interface.generic_params, self);
        let path = self
            .module_of(map, interface.decl_span)
            .map(|module| format!("{module}::{owner}"));

        HoverInfo {
            path,
            ty: format!(
                "interface {owner}\n{}",
                interface_const_signature(constant, interface, self)
            ),
            layout: layout_of(self, constant.typ),
            docs: self.docs(constant.decl_span),
        }
    }
}

#[inline]
fn interface_const_signature(
    constant: &InterfaceConstSignature,
    interface: &InterfaceSignature,
    hir: &Index,
) -> String {
    format!(
        "const {}: {}",
        short_name(hir.symbols.get(constant.name)),
        format_type(constant.typ, hir, &interface.generic_params)
    )
}

fn interface_signature(
    method: &InterfaceMethodSignature,
    interface: &InterfaceSignature,
    hir: &Index,
) -> String {
    let generics = &interface.generic_params;
    let mut params: Vec<String> = Vec::with_capacity(method.params.len() + 1);

    if method.has_receiver {
        params.push(match method.receiver_mut {
            true => "&mut self".to_owned(),
            false => "&self".to_owned(),
        });
    }
    // the receiver is not part of the declared parameter list here, unlike a
    // lowered function, whose params already carry it
    params.extend(method.params.iter().map(|&typ| format_type(typ, hir, generics)));

    let mut out = format!("fn {}({})", short_name(hir.symbols.get(method.name)), params.join(", "));
    let ret = format_type(method.return_type, hir, generics);
    if ret != "()" {
        out.push_str(": ");
        out.push_str(&ret);
    }

    out
}

impl Index {
    /// A nominal type shown as its own declaration, `none` for anything without
    /// one to show
    fn nominal_hover(&self, typ: Type, map: &SourceMap) -> Option<HoverInfo> {
        let (path, ty, docs) = match typ.kind() {
            TypeKind::Struct(id) => {
                let structure = &self.structs[id];
                let path = self.module_of(map, structure.decl_span);
                (path, struct_def(structure, self), self.docs(structure.decl_span))
            },
            TypeKind::Enum(id) => {
                let enumeration = &self.enums[id];
                let path = self.module_of(map, enumeration.decl_span);
                (path, enum_def(enumeration, self), self.docs(enumeration.decl_span))
            },
            _ => return None,
        };

        let layout = match is_open(typ, self) {
            true => None,
            false => layout_of(self, typ),
        };

        Some(HoverInfo { path, ty, layout, docs })
    }

    fn const_hover(&self, constant: &ConstInfo, map: &SourceMap) -> HoverInfo {
        let qualified = self.symbols.get(constant.name);
        let implementor = implementor_of(constant.owner, self, &[]);
        let path = self.module_of(map, constant.decl_span).map(|module| match &implementor {
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
        ty.push_str(&format_type(constant.typ, self, &[]));
        if let Some(value) = &constant.value {
            ty.push_str(" = ");
            ty.push_str(value);
        }

        HoverInfo { path, ty, layout: None, docs: self.docs(constant.decl_span) }
    }
}

// TODO: those things should be better integrated with the compiler
// in the future instead of ad-hoc resolution here

fn const_value(constant: &Constant<'_>, hir: &Index) -> Option<String> {
    use frontend::parser::expression::UnaryOperator;

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
            let value = eval_const_int(constant.value, hir)?;
            Some(render_int(value, constant.typ))
        },
    }
}

fn eval_const_int(expr: &hir::Expression<'_>, hir: &Index) -> Option<i128> {
    use frontend::parser::expression::{BinaryOperator, TypeIntrinsicKind, UnaryOperator};

    match &expr.kind {
        ExpressionKind::Literal(Literal::Int(value)) => Some(*value as i128),
        ExpressionKind::Unary { operator: UnaryOperator::Neg, expr } => {
            eval_const_int(expr, hir).map(i128::wrapping_neg)
        },
        ExpressionKind::Unary { operator: UnaryOperator::Not, expr } => {
            eval_const_int(expr, hir).map(|value| !value)
        },
        ExpressionKind::Cast { from, .. } => eval_const_int(from, hir),
        ExpressionKind::TypeIntrinsic { kind, typ } => {
            let (size, align) = layout_of(hir, *typ)?;
            Some(match kind {
                TypeIntrinsicKind::SizeOf => size as i128,
                TypeIntrinsicKind::AlignOf => align as i128,
            })
        },
        ExpressionKind::Binary { operator, left, right } => {
            let left = eval_const_int(left, hir)?;
            let right = eval_const_int(right, hir)?;
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

impl Index {
    fn fn_hover(&self, func: &FnInfo, map: &SourceMap) -> HoverInfo {
        let implementor = func.implementor(self);
        let mut path = self.module_of(map, func.decl_span);
        let mut ty = signature(func, self);

        if let Some(implementor) = implementor {
            path = path.map(|module| format!("{module}::{implementor}"));
            ty = match func.owner {
                Owner::Interface { interface, .. } => format!(
                    "impl {implementor} with {}\n{ty}",
                    short_name(self.symbols.get(interface))
                ),
                _ => format!("impl {implementor}\n{ty}"),
            };
        }

        HoverInfo { path, ty, layout: None, docs: self.docs(func.decl_span) }
    }
}

/// The type an item is declared on, rendered as it is written
#[inline]
fn implementor_of(owner: Owner, hir: &Index, generics: &[SymbolId]) -> Option<String> {
    match owner {
        Owner::Free => None,
        Owner::Inherent(on) | Owner::Interface { on, .. } => Some(format_type(on, hir, generics)),
    }
}

fn signature(func: &FnInfo, hir: &Index) -> String {
    let mut out = String::new();
    // markers sit on their own line above the signature, as they are written
    if func.is_unsafe {
        out.push_str("@unsafe\n");
    }
    if matches!(func.kind, FunctionKind::Intrinsic(_)) {
        out.push_str("@intrinsic\n");
    }

    let flags = [(func.is_pub, "pub "), (func.inline, "inline "), (func.is_const, "const ")];

    out.extend(flags.into_iter().filter_map(|(flag, word)| flag.then_some(word)));

    out.push_str("fn ");
    out.push_str(&function_name(func, hir));

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

fn struct_def(structure: &Struct, hir: &Index) -> String {
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

fn enum_def(enumeration: &Enum, hir: &Index) -> String {
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

/// join the first [`MAX_HOVER_ITEMS`] lines, eliding the rest with `// …`
fn truncated(lines: impl Iterator<Item = String>, total: usize) -> String {
    let mut lines: Vec<_> = lines.take(MAX_HOVER_ITEMS).collect();
    if total > MAX_HOVER_ITEMS {
        lines.push("    // …".to_owned());
    }

    lines.join("\n")
}

fn format_type(typ: Type, hir: &Index, generics: &[SymbolId]) -> String {
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
        TypeKind::Raw { mutable, to } => {
            let typ = format_type(Type::new(to.kind()), hir, generics);
            match mutable {
                true => format!("*mut {typ}"),
                _ => format!("*{typ}"),
            }
        },
        TypeKind::Array(id) => {
            let array = hir.arrays[id];
            format!("[{}; {}]", format_type(array.element, hir, generics), array.len)
        },
        TypeKind::Slice { mutable, element } => {
            let element = format_type(element.into(), hir, generics);
            match mutable {
                true => format!("&mut [{element}]"),
                _ => format!("&[{element}]"),
            }
        },
        kind => kind.to_string(),
    }
}

fn function_name(func: &FnInfo, hir: &Index) -> String {
    let qualified = hir.symbols.get(func.name);
    let tail = qualified.rsplit("::").next().unwrap_or(qualified);
    let named = !func.generics.is_empty() && tail.matches('$').count() == func.generics.len();

    match named {
        true => nominal_name(func.name, &func.generics, hir),
        false => short_name(qualified),
    }
}

fn nominal_name(name: SymbolId, generics: &[SymbolId], hir: &Index) -> String {
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

    fn rendered(a: &SemanticAnalysis) -> Vec<(Span, HoverInfo)> {
        a.hover_types
            .iter()
            .filter_map(|&(span, target)| Some((span, a.index.hover(target, &a.source_map)?)))
            .collect()
    }

    fn hints(a: &SemanticAnalysis) -> Vec<String> {
        a.inlay_hints.iter().map(|&(_, typ, at)| a.index.hint(typ, at)).collect()
    }

    fn analyse(tag: &str, content: &str) -> SemanticAnalysis {
        let entry = std::env::temp_dir().join(format!("nyx_analysis_{tag}.nyx"));
        std::fs::write(&entry, "").unwrap();
        let entry = std::fs::canonicalize(&entry).unwrap();
        let analysis = Analysis::new(entry.clone()).with_overlay(entry.clone(), content).run();
        std::fs::remove_file(&entry).ok();

        analysis
    }

    #[test]
    fn array_hint_renders_element_and_length() {
        let a = analyse("array_render", "fn main() { let arr = [0; 3]; let l = arr.len(); }");
        let hints = hints(&a);
        assert!(hints.iter().any(|h| h == "[i32; 3]"), "array renders as `[i32; 3]`: {hints:?}");
        assert!(hints.iter().any(|h| h == "uptr"), "len() result is uptr: {hints:?}");
    }

    #[test]
    fn array_element_infers_from_later_assignment() {
        let a = analyse(
            "array_infer",
            "fn main() { let mut arr = [0; 3]; let p: uptr = 1; arr[0] = p; }",
        );
        let hints = hints(&a);
        assert!(hints.iter().any(|h| h == "[uptr; 3]"), "element infers to uptr: {hints:?}");
    }

    #[test]
    fn array_element_infers_from_len_assignment() {
        let a = analyse(
            "array_len_assign",
            "fn main() { let mut arr = [0; 3]; arr[2] = arr.len(); let s = arr[0]; }",
        );
        assert!(a.diagnostics.is_empty(), "no type mismatch: {:?}", a.diagnostics);
        let hints = hints(&a);
        assert!(
            hints.iter().any(|h| h == "[uptr; 3]"),
            "element infers to uptr from len(): {hints:?}"
        );
    }

    #[test]
    fn valid_buffer_analyses_with_hints() {
        let a = analyse("valid", "fn main() { let x = 232; }");
        assert!(a.ok, "valid source must analyse into HIR");
        assert!(a.diagnostics.is_empty());
        assert!(hints(&a).iter().any(|ty| ty == "i32"), "expected the `: i32` hint");
    }

    #[test]
    fn enum_payload_can_reference_a_later_struct() {
        let a = analyse(
            "forward_enum_payload",
            r#"
                enum Msg { ChangeColour(Colour) }
                struct Colour { r: u8, g: u8, b: u8 }
                fn main() { }
            "#,
        );

        assert!(a.ok, "{:#?}", a.diagnostics);
        assert!(a.diagnostics.is_empty(), "{:#?}", a.diagnostics);
    }

    #[test]
    fn direct_qualified_std_call_has_no_diagnostics() {
        let a = analyse("qualified_std", "fn main() { std::io::println(\"ok\"); }");

        assert!(a.ok, "{:#?}", a.diagnostics);
        assert!(a.diagnostics.is_empty(), "{:#?}", a.diagnostics);
    }

    #[test]
    fn broken_buffer_still_reports_and_keeps_features() {
        let a = analyse("broken", "fn main() { let x = 1; let y = ");
        assert!(a.ok, "a syntax error is recovered, not fatal");
        assert!(!a.diagnostics.is_empty(), "the error is still reported");
        assert!(
            hints(&a).iter().any(|ty| ty == "i32"),
            "the sound binding keeps its hint: {:?}",
            hints(&a)
        );
    }

    #[test]
    fn a_syntax_error_does_not_hide_the_rest_of_the_file() {
        let a = analyse(
            "syntax_then_type",
            r#"
            struct Point {
                /// the horizontal coordinate
                x: i32,
                y: i32,
            }
            fn broken(: i32 { 1 }
            fn typed(): i32 { true }
            fn main() { let p = Point { x: 1, y: 2 }; }
            "#,
        );

        assert!(a.ok, "{:#?}", a.diagnostics);
        let messages: Vec<_> = a.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("Expected an identifier")),
            "the syntax error is reported: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("does not match the declared type")),
            "the type error after it is reported too: {messages:?}"
        );
        assert!(
            rendered(&a).iter().any(|(_, h)| h.ty.contains("struct Point")),
            "the struct still hovers"
        );
        assert!(
            a.document_symbols.iter().any(|s| s.name == "main"),
            "the outline still lists main: {:?}",
            a.document_symbols
        );
    }

    #[test]
    fn an_unterminated_body_keeps_the_following_function_analysable() {
        let a = analyse(
            "unterminated",
            "fn unfinished() { let z = 1; let w =\n\nfn main() { let total = 1 + 2; }",
        );

        assert!(a.ok, "{:#?}", a.diagnostics);
        assert!(!a.diagnostics.is_empty(), "the broken binding is reported");
        assert!(
            a.document_symbols.iter().any(|s| s.name == "main"),
            "main is still an item of its own: {:?}",
            a.document_symbols
        );
        assert!(
            hints(&a).iter().any(|ty| ty == "i32"),
            "and its bindings still get hints: {:?}",
            hints(&a)
        );
    }

    #[test]
    fn diagnostics_come_back_in_source_order() {
        let a = analyse(
            "ordered",
            "fn one(): i32 { true }\nfn two(): i32 { true }\nfn three(): i32 { true }",
        );

        let starts: Vec<_> = a
            .diagnostics
            .iter()
            .filter_map(|d| d.primary.as_ref().map(|label| label.span.start.0))
            .collect();
        assert_eq!(starts.len(), 3, "one per function: {:?}", a.diagnostics);
        assert!(starts.is_sorted(), "reported top to bottom: {starts:?}");
    }

    #[test]
    fn a_std_entry_reports_every_error_in_its_own_bodies() {
        let entry = std::fs::canonicalize("../std/alloc.nyx").expect("std/alloc.nyx must exist");
        let mut content = std::fs::read_to_string(&entry).expect("readable");
        content.push_str("\nfn first(): i32 { true }\nfn second() { nope(); }\n");

        let a = Analysis::new(entry.clone()).with_overlay(entry, content).run();
        let messages: Vec<_> = a.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert!(a.ok, "{messages:?}");
        assert!(
            messages.iter().any(|m| m.contains("does not match the declared type")),
            "a std entry gets its bodies checked: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("Cannot find function")),
            "and every later error too: {messages:?}"
        );
    }

    #[test]
    fn unknown_param_type_keeps_features_alive() {
        let a =
            analyse("param", "fn poisoned(a: Nonexistent): i32 { 1 }\nfn main() { let x = 232; }");
        assert!(a.ok, "recovery must still produce a HIR with live features");
        assert_eq!(a.diagnostics.len(), 1, "exactly the unknown type: {:?}", a.diagnostics);
        assert!(hints(&a).iter().any(|ty| ty == "i32"), "main still gets its hint");
        assert!(
            rendered(&a).iter().any(|(_, h)| h.ty.contains("fn poisoned")),
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
        assert!(hints(&a).iter().any(|ty| ty == "i32"), "main still gets its hint");
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
            rendered(&a).iter().any(|(_, h)| h.ty.contains("struct Holder")),
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
            struct Point {
                /// the horizontal coordinate
                x: i32,
                y: i32,
            }

            /// The answer.
            const ANSWER: i32 = 42;

            fn main() {
                let p = Point { x: 1, y: 2 };
                let _ = add(p.x, p.y) + ANSWER;
            }
            "#,
        );
        assert!(a.ok, "{:?}", a.diagnostics);

        let hovers = rendered(&a);
        let doc_of = |needle: &str| {
            hovers
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

        let hovers = rendered(&a);
        let doc = hovers
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
            rendered(&a)
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
            hints(&a).iter().any(|ty| ty == "{unknown}"),
            "d stays declared with a poison hint: {:?}",
            hints(&a)
        );
    }

    #[test]
    fn duplicate_functions_report_without_killing_analysis() {
        let a = analyse("dup_fn", "fn twice() {}\nfn twice() {}\nfn main() { let z = 42; }");
        assert!(a.ok, "recovery must still produce a HIR with live features");
        assert!(!a.diagnostics.is_empty());
        assert!(hints(&a).iter().any(|ty| ty == "i32"), "main still gets its hint");
    }

    fn entry_origin(a: &SemanticAnalysis, source: &str) -> u32 {
        a.source_map
            .files()
            .find(|file| file.src == source)
            .map(|file| file.start_pos.0)
            .expect("the analysed buffer is registered")
    }

    fn text_at(origin: u32, source: &str, span: Span) -> Option<&str> {
        let (start, end) = (span.start.0.checked_sub(origin)?, span.end.0.checked_sub(origin)?);
        source.get(start as usize..end as usize)
    }

    /// The tightest hover covering exactly `needle`, so a member wins over the
    /// declaration that contains it
    fn hover_on(a: &SemanticAnalysis, source: &str, needle: &str) -> HoverInfo {
        let origin = entry_origin(a, source);
        a.hover_types
            .iter()
            .filter(|(span, _)| text_at(origin, source, *span) == Some(needle))
            .min_by_key(|(span, _)| span.end.0 - span.start.0)
            .and_then(|&(_, target)| a.index.hover(target, &a.source_map))
            .unwrap_or_else(|| panic!("nothing hovers `{needle}`"))
    }

    /// The text the definition of `needle` lands on, `<std>` when it leaves the buffer
    fn definition_of(a: &SemanticAnalysis, source: &str, needle: &str) -> String {
        let origin = entry_origin(a, source);
        let (_, target) = a
            .goto_definitions
            .iter()
            .filter(|(use_span, _)| text_at(origin, source, **use_span) == Some(needle))
            .min_by_key(|(use_span, _)| use_span.end.0 - use_span.start.0)
            .unwrap_or_else(|| panic!("`{needle}` has no definition"));

        text_at(origin, source, *target).unwrap_or("<std>").to_owned()
    }

    const RICH: &str = r#"
        use std::mem::{size_of};

        /// A documented interface.
        interface Shape {
            /// the area of the shape
            fn area(&self): i32;
        }

        /// A point in space.
        struct Point {
            /// the horizontal coordinate
            x: i32,
            y: i32,
        }

        /// The kind of message.
        enum Msg {
            /// nothing to say
            Quiet,
            /// shouting, with a volume
            Loud(i32),
        }

        impl Point {
            /// make a point
            fn origin(): Point { Point { x: 0, y: 0 } }
        }

        impl Point with Shape {
            fn area(&self): i32 { self.x * self.y }
        }

        @unsafe
        fn danger(): i32 { 7 }

        fn take(p: Point, m: Msg): i32 { p.x }

        fn main() {
            let p = Point::origin();
            let total = p.area();
            let m = Msg::Loud(3);
            let size = size_of(i32);
        }
    "#;

    #[test]
    fn struct_fields_hover_with_their_docs() {
        let a = analyse("field_hover", RICH);
        assert!(a.ok, "{:?}", a.diagnostics);

        let x = hover_on(&a, RICH, "x");
        assert_eq!(x.ty, "x: i32");
        assert_eq!(x.docs.as_deref(), Some("the horizontal coordinate"));
        assert_eq!(x.layout, Some((4, 4)), "a field carries its own layout");
        assert!(x.path.as_deref().is_some_and(|p| p.ends_with("::Point")), "{:?}", x.path);
    }

    #[test]
    fn a_field_access_reaches_the_field_declaration() {
        let a = analyse("field_access", RICH);

        let access = hover_on(&a, RICH, "p.x");
        assert_eq!(access.ty, "x: i32", "the access shows the field, not just its type");
        assert_eq!(access.docs.as_deref(), Some("the horizontal coordinate"));
        assert_eq!(definition_of(&a, RICH, "p.x"), "x", "and jumps to the field's name");
    }

    #[test]
    fn enum_variants_hover_at_their_declaration_and_use() {
        let a = analyse("variant_hover", RICH);

        let quiet = hover_on(&a, RICH, "Quiet");
        assert_eq!(quiet.ty, "Msg::Quiet = 0", "a fieldless variant shows its discriminant");
        assert_eq!(quiet.docs.as_deref(), Some("nothing to say"));

        let used = hover_on(&a, RICH, "Msg::Loud(3)");
        assert_eq!(used.ty, "Msg::Loud(i32)", "a use shows the payload type");
        assert_eq!(used.docs.as_deref(), Some("shouting, with a volume"));
        assert_eq!(definition_of(&a, RICH, "Msg::Loud(3)"), "Loud");
    }

    #[test]
    fn interfaces_and_their_methods_hover() {
        let a = analyse("interface_hover", RICH);

        let shape = hover_on(
            &a,
            RICH,
            "interface Shape {\n    /// the area of the shape\n    fn area(&self): i32;\n}",
        );
        assert_eq!(shape.ty, "interface Shape {\n    fn area(&self): i32;\n}");
        assert_eq!(shape.docs.as_deref(), Some("A documented interface."));

        let area = hover_on(&a, RICH, "area");
        assert_eq!(area.ty, "interface Shape\nfn area(&self): i32");
        assert_eq!(area.docs.as_deref(), Some("the area of the shape"));
    }

    #[test]
    fn a_type_annotation_reaches_its_declaration() {
        let a = analyse("type_ref", RICH);

        let point = hover_on(&a, RICH, "Point");
        assert!(point.ty.starts_with("struct Point {"), "got {}", point.ty);
        assert_eq!(point.docs.as_deref(), Some("A point in space."));
        assert_eq!(definition_of(&a, RICH, "Point"), "Point", "and jumps to the declared name");
    }

    #[test]
    fn an_import_reaches_the_item_it_names() {
        let a = analyse("import_hover", RICH);

        let import = hover_on(&a, RICH, "size_of");
        assert!(
            import.ty.contains("fn size_of"),
            "the import shows the signature: {}",
            import.ty
        );
        assert_eq!(
            definition_of(&a, RICH, "size_of"),
            "<std>",
            "and jumps into the std module that declares it"
        );
    }

    #[test]
    fn markers_sit_above_the_signature_they_annotate() {
        let a = analyse("marker_hover", RICH);

        let danger = hover_on(&a, RICH, "fn danger(): i32 { 7 }");
        assert_eq!(danger.ty, "@unsafe\nfn danger(): i32");
    }

    #[test]
    fn destructuring_a_variant_hints_the_payload() {
        let source = r#"
            enum Msg { Quiet, Loud(i32), Named(Point) }
            struct Point { x: i32, y: i32 }
            fn describe(m: Msg): i32 {
                match m {
                    Msg::Loud(volume) -> volume,
                    Msg::Named(Point { x, y }) -> x + y,
                    other -> 0,
                }
            }
            fn main() { let _ = describe(Msg::Quiet); }
        "#;
        let a = analyse("destructure", source);
        assert!(a.ok, "{:?}", a.diagnostics);

        let origin = entry_origin(&a, source);
        let hint_on = |needle: &str| {
            a.inlay_hints
                .iter()
                .find(|&&(span, ..)| text_at(origin, source, span) == Some(needle))
                .map(|&(_, typ, at)| a.index.hint(typ, at))
        };

        assert_eq!(hint_on("volume").as_deref(), Some("i32"), "a payload binding is hinted");
        assert_eq!(hint_on("x").as_deref(), Some("i32"), "and so is a nested struct field binding");
        assert_eq!(
            hint_on("other").as_deref(),
            Some("Msg"),
            "a catch-all binds the scrutinee itself"
        );
    }

    fn offered(a: &SemanticAnalysis, source: &str, cursor: &str) -> Vec<String> {
        use crate::feature::completion;

        let offset = source.find(cursor).expect("the cursor marker") + cursor.len();
        let context = completion::context_at(source, offset);
        let position = frontend::BytePos(entry_origin(a, source) + offset as u32);

        completion::candidates(a, &context, completion::scope_at(a, position))
            .into_iter()
            .map(|item| item.label.clone())
            .collect()
    }

    #[test]
    fn a_dot_offers_fields_and_methods_of_the_receiver() {
        let a = analyse("complete_member", RICH);
        let offered = offered(&a, RICH, "let total = p.");

        assert!(offered.contains(&"x".to_owned()), "fields are offered: {offered:?}");
        assert!(offered.contains(&"y".to_owned()), "{offered:?}");
        assert!(offered.contains(&"area".to_owned()), "methods are offered: {offered:?}");
        assert!(!offered.contains(&"origin".to_owned()), "an associated fn is not: {offered:?}");
    }

    #[test]
    fn a_type_qualifier_offers_its_associated_items() {
        let a = analyse("complete_assoc", RICH);

        let on_point = offered(&a, RICH, "let p = Point::");
        assert!(on_point.contains(&"origin".to_owned()), "{on_point:?}");
        assert!(!on_point.contains(&"x".to_owned()), "a field is not associated: {on_point:?}");

        let on_msg = offered(&a, RICH, "let m = Msg::");
        assert!(on_msg.contains(&"Quiet".to_owned()), "{on_msg:?}");
        assert!(on_msg.contains(&"Loud".to_owned()), "{on_msg:?}");
    }

    #[test]
    fn a_module_path_offers_its_exports() {
        let a = analyse("complete_module", RICH);
        let offered = offered(&a, RICH, "use std::mem::");

        assert!(offered.contains(&"size_of".to_owned()), "{offered:?}");
    }

    #[test]
    fn a_root_offers_its_submodules() {
        let a = analyse("complete_submodule", RICH);

        let under_std = offered(&a, RICH, "use std::");
        assert!(under_std.contains(&"io".to_owned()), "an unimported module: {under_std:?}");
        assert!(under_std.contains(&"mem".to_owned()), "{under_std:?}");

        let roots = offered(&a, RICH, "    let size = ");
        assert!(roots.contains(&"std".to_owned()), "the root itself is nameable: {roots:?}");
    }

    #[test]
    fn unimported_standard_functions_are_not_offered_unqualified() {
        let source = "fn main() { let value = pri; }";
        let a = analyse("complete_unimported_std", source);
        let offered = offered(&a, source, "let value = pri");

        assert!(!offered.contains(&"print".to_owned()), "print needs an import: {offered:?}");
        assert!(!offered.contains(&"println".to_owned()), "println needs an import: {offered:?}");
    }

    #[test]
    fn imported_standard_functions_are_offered_unqualified() {
        let source = "use std::io::{print};\nfn main() { let value = pri; }";
        let a = analyse("complete_imported_std", source);
        let offered = offered(&a, source, "let value = pri");

        assert!(offered.contains(&"print".to_owned()), "the imported name is open: {offered:?}");
        assert!(
            !offered.contains(&"println".to_owned()),
            "other exports stay qualified: {offered:?}"
        );
    }

    #[test]
    fn a_module_path_offers_the_types_it_exports() {
        let a = analyse("complete_module_type", RICH);
        let exports: Vec<_> = a.completions.associated["std::optional"]
            .iter()
            .map(|item| item.label.as_str())
            .collect();

        assert!(
            exports.contains(&"Optional"),
            "a type is reachable through its module: {exports:?}"
        );
    }

    #[test]
    fn an_intrinsic_method_completes_and_hovers() {
        let source = "fn main() { let s = \"nyx\"; let n = s.len(); }";
        let a = analyse("intrinsic", source);
        assert!(a.ok, "{:?}", a.diagnostics);

        let offered = offered(&a, source, "let n = s.");
        assert!(offered.contains(&"len".to_owned()), "len is offered on a str: {offered:?}");

        let signature = a
            .completions
            .members
            .get("str")
            .and_then(|items| items.iter().find(|item| item.label == "len"))
            .map(|item| item.detail.clone())
            .expect("str::len is indexed");
        assert!(signature.starts_with("@intrinsic\n"), "the marker is shown: {signature}");
        assert!(signature.contains("fn len(&self): uptr"), "{signature}");
    }

    #[test]
    fn a_generic_signature_names_its_parameters_as_declared() {
        let a = analyse("generic_render", "use std::ptr;\nfn main() { }");
        let rendered = a
            .completions
            .associated
            .get("std::ptr")
            .expect("std::ptr is indexed")
            .iter()
            .find(|item| item.label == "add_mut")
            .map(|item| item.detail.clone())
            .expect("std::ptr::add_mut is indexed");

        assert!(
            rendered.contains("fn add_mut<T>(p: *mut T, count: uptr): *mut T"),
            "generics read back as written, not as the mangler numbered them: {rendered}"
        );
    }

    #[test]
    fn an_unqualified_position_offers_locals_and_globals() {
        let a = analyse("complete_open", RICH);
        let offered = offered(&a, RICH, "    let size = ");

        assert!(offered.contains(&"p".to_owned()), "a local in the same body: {offered:?}");
        assert!(offered.contains(&"Point".to_owned()), "a type: {offered:?}");
        assert!(offered.contains(&"describe".to_owned()) || offered.contains(&"take".to_owned()));
        assert!(offered.contains(&"Shape".to_owned()), "an interface: {offered:?}");
    }

    #[test]
    fn locals_of_another_body_are_not_offered() {
        let a = analyse("complete_scope", RICH);
        let outside = offered(&a, RICH, "fn take(p: Point, m: Msg): i32 { p");

        assert!(
            !outside.contains(&"total".to_owned()),
            "a local of main must not leak into take: {outside:?}"
        );
    }

    #[test]
    fn a_binding_hovers_as_the_declaration_it_was_written_as() {
        let source = r#"
            struct Point { x: i32, y: i32 }
            fn take(origin: Point): i32 {
                let mut total = 0;
                let fixed = origin.x;
                total = total + fixed;
                total
            }
            fn main() { let _ = take(Point { x: 1, y: 2 }); }
        "#;
        let a = analyse("binding_hover", source);
        assert!(a.ok, "{:?}", a.diagnostics);

        let total = hover_on(&a, source, "total");
        assert_eq!(total.ty, "let mut total: i32", "mutability is part of the declaration");
        assert_eq!(total.layout, Some((4, 4)), "with its size and alignment");

        assert_eq!(hover_on(&a, source, "fixed").ty, "let fixed: i32");
        assert_eq!(hover_on(&a, source, "origin").ty, "origin: Point", "a parameter has no let");
    }

    #[test]
    fn an_interface_implementation_names_the_interface_it_satisfies() {
        let a = analyse("impl_iface", RICH);

        let area = hover_on(&a, RICH, "fn area(&self): i32 { self.x * self.y }");
        assert_eq!(area.ty, "impl Point with Shape\nfn area(&self): i32");

        let origin = hover_on(&a, RICH, "fn origin(): Point { Point { x: 0, y: 0 } }");
        assert_eq!(origin.ty, "impl Point\nfn origin(): Point", "a plain impl names no interface");
    }

    #[test]
    fn definitions_land_on_the_name_not_the_keyword() {
        let a = analyse("goto_name", RICH);

        assert_eq!(definition_of(&a, RICH, "Point::origin()"), "origin");
        assert_eq!(definition_of(&a, RICH, "p.area()"), "area");
    }

    #[test]
    fn a_generic_impl_names_its_receiver_type() {
        let source = r#"
            struct Holder<T> { value: T }

            impl Holder<T> {
                fn get(&self): T { self.value }
            }

            fn main() {
                let h = Holder { value: 1 };
                let v = h.get();
            }
        "#;
        let a = analyse("generic_owner", source);
        assert!(a.ok, "{:?}", a.diagnostics);

        let got = hover_on(&a, source, "fn get(&self): T { self.value }");
        assert!(
            got.ty.starts_with("impl Holder<T>\n"),
            "the receiver type names the block, not a mangled segment: {}",
            got.ty
        );
    }

    #[test]
    fn every_recorded_target_still_resolves() {
        let a = analyse("targets_resolve", RICH);
        assert!(a.ok, "{:?}", a.diagnostics);

        assert!(!a.hover_types.is_empty(), "the fixture records hovers");
        assert_eq!(
            rendered(&a).len(),
            a.hover_types.len(),
            "every span a walk recorded must resolve against the index it was built with"
        );
    }

    #[test]
    fn a_hover_target_stays_a_handle() {
        // the walk records one of these per expression: it must stay a plain
        // handle, never grow a field that has to be rendered or allocated
        assert!(
            size_of::<HoverTarget>() <= 24,
            "a target is {} bytes, it should stay a handle",
            size_of::<HoverTarget>()
        );
    }
}
