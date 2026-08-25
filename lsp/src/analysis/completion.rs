use crate::analysis::{
    HoverTarget, Snapshot, base_name,
    hover::{
        self, format_type, implementor_of, is_generic_instance, nominal_name, through_reference,
    },
};
use frontend::hir::{self, AdtId, Owner};
use frontend::lexer::token::Span;
use frontend::source_map::SourceMap;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// The candidates a completion request can draw on
#[derive(Debug, Default)]
pub struct Completions<'a> {
    /// members reachable through `.`, keyed by the receiver's type name with any
    /// generic arguments stripped, since one template owns every instantiation
    pub members: HashMap<&'a str, Vec<Completion<'a>>>,
    /// items reachable through `::`, keyed by a type name or by a module path
    pub associated: HashMap<&'a str, Vec<Completion<'a>>>,
    /// every item nameable without a qualifier
    pub globals: Vec<Completion<'a>>,
    /// the generic parameters of each [members](Self::members) key, so a receiver
    /// written with concrete arguments shows those instead of the parameters
    pub generics: HashMap<&'a str, GenericSlots<'a>>,
}

/// One offered name
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Completion<'a> {
    pub label: &'a str,
    pub kind: CompletionKind,
    /// the signature or type shown beside the label
    pub detail: &'a str,
    pub docs: Option<&'a str>,
    /// for a value, the nominal type whose members it exposes through `.`
    pub type_key: Option<&'a str>,
}

#[derive(Debug, Default)]
pub struct GenericSlots<'a> {
    pub arity: usize,
    positions: HashMap<&'a str, usize>,
}

pub(super) struct CompletionCollector<'a, 'hir> {
    hir: &'a Snapshot<'hir>,
    map: &'a SourceMap,
    imported_names: &'a HashSet<String>,
    arena: &'hir bumpalo::Bump,
    out: Completions<'hir>,
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

/// A top-level declaration a `use` can name, as its position in a [`Snapshot`]
#[derive(Debug, Clone, Copy)]
pub(super) enum Importable {
    Function(u32),
    Type(AdtId),
    Constant(u32),
}

impl<'a, 'hir> CompletionCollector<'a, 'hir> {
    pub(super) fn new(
        hir: &'a Snapshot<'hir>,
        map: &'a SourceMap,
        imported_names: &'a HashSet<String>,
        arena: &'hir bumpalo::Bump,
    ) -> Self {
        Self { hir, map, imported_names, arena, out: Completions::default() }
    }

    pub(super) fn collect(mut self) -> Completions<'hir> {
        let (hir, arena) = (self.hir, self.arena);
        self.register_modules();

        self.out
            .globals
            .extend(frontend::PRIMITIVE_TYPES.iter().map(|&name| Completion {
                label: name,
                kind: CompletionKind::Primitive,
                detail: keep(arena, &format!("primitive type {name}")),
                docs: None,
                type_key: None,
            }));

        for def in hir.adts.iter() {
            let name = keep(arena, &base_name(hir.symbols.get(def.name)));
            let generics: Vec<_> = def.generics.iter().map(|generic| generic.name).collect();
            let key = member_key(arena, &nominal_name(def.name, &generics, hir));
            self.declare_generics(key, generics.len(), &generics);

            match def.is_struct() {
                true => {
                    let fields = self.out.members.entry(key).or_default();
                    for field in def.fields() {
                        fields.push(Completion {
                            label: keep(arena, hir.symbols.get(field.name)),
                            kind: CompletionKind::Field,
                            detail: keep(arena, &format_type(field.typ, hir, &generics)),
                            docs: hir.docs(field.name_span).map(|docs| keep(arena, &docs)),
                            type_key: type_key(arena, field.typ, hir),
                        });
                    }
                },
                _ => {
                    let variants = self.out.associated.entry(name).or_default();
                    for variant in def.variants() {
                        variants.push(Completion {
                            label: keep(arena, hir.symbols.get(variant.name)),
                            kind: CompletionKind::Variant,
                            detail: keep(
                                arena,
                                &match &variant.payload {
                                    Some(payload) => format_type(*payload, hir, &generics),
                                    None => variant.value.to_string(),
                                },
                            ),
                            docs: hir.docs(variant.name_span).map(|docs| keep(arena, &docs)),
                            type_key: None,
                        });
                    }
                },
            }

            if def.decl_span != Span::default() {
                let nominal = nominal_name(def.name, &generics, hir);
                let (kind, keyword) = match def.is_struct() {
                    true => (CompletionKind::Struct, "struct"),
                    _ => (CompletionKind::Enum, "enum"),
                };

                let candidate = Completion {
                    label: name,
                    kind,
                    detail: keep(arena, &format!("{keyword} {nominal}")),
                    docs: hir.docs(def.decl_span).map(|docs| keep(arena, &docs)),
                    type_key: None,
                };

                self.export_by_module(def.decl_span, def.is_pub, candidate);
            }
        }

        for interface in &hir.interfaces {
            let name = keep(arena, &base_name(hir.symbols.get(interface.name)));
            let methods = self.out.associated.entry(name).or_default();
            for method in &interface.methods {
                methods.push(Completion {
                    label: keep(arena, &base_name(hir.symbols.get(method.name))),
                    kind: CompletionKind::Method,
                    detail: keep(arena, &hover::interface_signature(method, interface, hir)),
                    docs: hir.docs(method.decl_span).map(|docs| keep(arena, &docs)),
                    type_key: None,
                });
            }

            for constant in &interface.constants {
                methods.push(Completion {
                    label: keep(arena, &base_name(hir.symbols.get(constant.name))),
                    kind: CompletionKind::Constant,
                    detail: keep(
                        arena,
                        &hover::interface_const_signature(constant, interface, hir),
                    ),
                    docs: hir.docs(constant.decl_span).map(|docs| keep(arena, &docs)),
                    type_key: type_key(arena, constant.typ, hir),
                });
            }
            let nominal = nominal_name(interface.name, &interface.generic_params, hir);

            let candidate = Completion {
                label: name,
                kind: CompletionKind::Interface,
                detail: keep(arena, &format!("interface {nominal}")),
                docs: hir.docs(interface.decl_span).map(|docs| keep(arena, &docs)),
                type_key: None,
            };
            self.export_by_module(interface.decl_span, interface.is_pub, candidate);
        }

        let templates: HashSet<_> = hir
            .functions
            .iter()
            .filter(|func| !is_generic_instance(func, hir))
            .map(|func| base_name(hir.symbols.get(func.name)))
            .collect();

        for func in hir.functions.iter() {
            let qualified = hir.symbols.get(func.name);
            if is_generic_instance(func, hir) && templates.contains(&base_name(qualified)) {
                continue;
            }

            let receiver = hover::receiver(func, hir);
            let candidate = Completion {
                label: keep(arena, &base_name(qualified)),
                kind: match receiver {
                    Some(_) => CompletionKind::Method,
                    None => CompletionKind::Function,
                },
                detail: keep(arena, &hover::signature(func, hir)),
                docs: hir.docs(func.decl_span).map(|docs| keep(arena, &docs)),
                type_key: type_key(arena, func.return_type, hir),
            };

            match (receiver, implementor_of(func.owner, hir, &[])) {
                (Some(receiver), _) => {
                    let key =
                        member_key(arena, &format_type(through_reference(receiver), hir, &[]));
                    let arity = self.out.generics.get(key).map_or(0, |slots| slots.arity);
                    self.declare_generics(
                        key,
                        arity,
                        &func.generics[..arity.min(func.generics.len())],
                    );
                    self.out.members.entry(key).or_default().push(candidate);
                },
                (_, Some(implementor)) => {
                    let key = keep(arena, &base_name(&implementor));
                    self.out.associated.entry(key).or_default().push(candidate);
                },
                _ => self.export_by_module(func.decl_span, func.is_pub, candidate),
            }
        }

        for constant in &hir.constants {
            let qualified = hir.symbols.get(constant.name);
            let candidate = Completion {
                label: keep(arena, &base_name(qualified)),
                kind: CompletionKind::Constant,
                detail: keep(arena, &format_type(constant.typ, hir, &[])),
                docs: hir.docs(constant.decl_span).map(|docs| keep(arena, &docs)),
                type_key: type_key(arena, constant.typ, hir),
            };

            match implementor_of(constant.owner, hir, &[]) {
                Some(implementor) => {
                    let key = keep(arena, &base_name(&implementor));
                    self.out.associated.entry(key).or_default().push(candidate);
                },
                _ => self.export_by_module(constant.decl_span, constant.is_pub, candidate),
            }
        }

        for list in self.out.members.values_mut().chain(self.out.associated.values_mut()) {
            dedup_by_label(list);
        }
        dedup_by_label(&mut self.out.globals);

        self.out
    }

    fn declare_generics(&mut self, key: &'hir str, arity: usize, names: &[hir::SymbolId]) {
        if arity == 0 {
            return;
        }

        let (symbols, arena) = (&self.hir.symbols, self.arena);
        let slots = self.out.generics.entry(key).or_default();
        slots.arity = arity;
        for (at, &name) in names.iter().enumerate() {
            slots.positions.insert(keep(arena, symbols.get(name)), at);
        }
    }

    fn export_by_module(&mut self, decl_span: Span, is_pub: bool, candidate: Completion<'hir>) {
        if let Some(module) = self.hir.module_of(self.map, decl_span)
            && is_pub
        {
            let module = keep(self.arena, &module);
            self.out.associated.entry(module).or_default().push(candidate);
        }

        if self.is_open_name(decl_span, candidate.label) {
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
        let (hir, arena) = (self.hir, self.arena);
        for path in hir.modules.values() {
            let mut prefix: Option<&'hir str> = None;

            for segment in path.split("::") {
                let full = match prefix {
                    Some(prefix) => keep(arena, &format!("{prefix}::{segment}")),
                    _ => keep(arena, segment),
                };
                let candidate = Completion {
                    label: keep(arena, segment),
                    kind: CompletionKind::Module,
                    detail: keep(arena, &format!("mod {full}")),
                    docs: None,
                    type_key: None,
                };

                match prefix {
                    Some(prefix) => self.out.associated.entry(prefix).or_default().push(candidate),
                    _ => self.out.globals.push(candidate),
                }

                prefix = Some(full);
            }
        }
    }
}

impl GenericSlots<'_> {
    pub fn substitute<'t>(&self, text: &'t str, args: &[&'t str]) -> Cow<'t, str> {
        if let Some(argument) = self.argument(text, args) {
            return Cow::Borrowed(argument);
        }

        match words(text).any(|word| self.argument(word, args).is_some()) {
            true => Cow::Owned(self.rewrite(text, args)),
            _ => Cow::Borrowed(text),
        }
    }

    pub fn argument<'t>(&self, word: &str, args: &[&'t str]) -> Option<&'t str> {
        self.position(word).and_then(|at| args.get(at).copied())
    }

    fn rewrite(&self, text: &str, args: &[&str]) -> String {
        let mut out = String::with_capacity(text.len());
        let mut word = String::new();

        for c in text.chars() {
            match is_name_char(c) {
                true => word.push(c),
                _ => {
                    self.push_word(&mut out, &mut word, args);
                    out.push(c);
                },
            }
        }
        self.push_word(&mut out, &mut word, args);

        out
    }

    fn push_word(&self, out: &mut String, word: &mut String, args: &[&str]) {
        if word.is_empty() {
            return;
        }

        match self.argument(word, args) {
            Some(argument) => out.push_str(argument),
            _ => out.push_str(word),
        }
        word.clear();
    }

    fn position(&self, word: &str) -> Option<usize> {
        match self.positions.get(word) {
            Some(&at) => Some(at),
            _ => word
                .strip_prefix('T')
                .and_then(|rest| rest.parse::<usize>().ok())
                .filter(|&at| at < self.arity),
        }
    }
}

impl Importable {
    pub(super) fn name_span(self, hir: &Snapshot<'_>) -> Span {
        match self {
            Self::Function(at) => hir.functions[hir::FunctionId(at)].name_span,
            Self::Constant(at) => hir.constants[at as usize].name_span,
            Self::Type(id) => hir.adts.get(id).map(|def| def.name_span).unwrap_or_default(),
        }
    }

    pub(super) fn hover_target<'hir>(self, hir: &Snapshot<'hir>) -> HoverTarget<'hir> {
        match self {
            Self::Function(at) => HoverTarget::Function(at),
            Self::Constant(at) => HoverTarget::Constant(at),
            Self::Type(id) => match hir.adts[id].is_struct() {
                true => HoverTarget::Struct(id),
                false => HoverTarget::Enum(id),
            },
        }
    }
}

/// index every top-level declaration by the bare name a `use` would import it under
pub(super) fn importable_items<'a>(hir: &'a Snapshot<'_>) -> HashMap<&'a str, Importable> {
    let mut items = HashMap::new();

    for (id, def) in hir.adts.iter_enumerated() {
        items.insert(hir.symbols.get(def.name), Importable::Type(id));
    }
    for (at, func) in hir.functions.iter().enumerate() {
        if let Some(name) = importable_name(hir.symbols.get(func.name), func.owner) {
            items.insert(name, Importable::Function(at as u32));
        }
    }

    for (at, constant) in hir.constants.iter().enumerate() {
        if let Some(name) = importable_name(hir.symbols.get(constant.name), constant.owner) {
            items.insert(name, Importable::Constant(at as u32));
        }
    }

    items
}

#[inline]
fn importable_name<'a>(qualified: &'a str, owner: Owner<'_>) -> Option<&'a str> {
    matches!(owner, Owner::Free).then(|| qualified.rsplit("::").next().unwrap_or(qualified))
}

#[inline]
pub(super) fn member_key<'h>(arena: &'h bumpalo::Bump, rendered: &str) -> &'h str {
    let base = base_name(rendered);
    keep(arena, base.split_once('<').map_or(base.as_str(), |(head, _)| head))
}

#[inline]
fn keep<'h>(arena: &'h bumpalo::Bump, text: &str) -> &'h str {
    arena.alloc_str(text)
}

#[inline]
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[inline]
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !is_name_char(c)).filter(|word| !word.is_empty())
}

#[inline]
fn type_key<'h>(
    arena: &'h bumpalo::Bump,
    typ: hir::Type<'_>,
    hir: &Snapshot<'_>,
) -> Option<&'h str> {
    let typ = through_reference(typ);
    match typ.kind() {
        hir::TypeKind::Infer(_)
        | hir::TypeKind::Error
        | hir::TypeKind::Unit
        | hir::TypeKind::Never => None,
        _ => Some(keep(arena, &base_name(&format_type(typ, hir, &[])))),
    }
}

/// collect every name completion can offer, keyed by how it is reached
#[inline]
pub(super) fn completions<'hir>(
    hir: &Snapshot<'hir>,
    map: &SourceMap,
    imported_names: &HashSet<String>,
    arena: &'hir bumpalo::Bump,
) -> Completions<'hir> {
    CompletionCollector::new(hir, map, imported_names, arena).collect()
}

/// drop repeats a monomorphised template leaves behind, keeping source order
pub(super) fn dedup_by_label(list: &mut Vec<Completion<'_>>) {
    let mut seen = HashSet::new();
    list.retain(|item| seen.insert((item.label, item.kind)));
    list.sort_by(|a, b| a.label.cmp(b.label));
}

pub(super) fn scope_of<'hir>(
    func: &hir::Function<'_>,
    hir: &Snapshot<'_>,
    arena: &'hir bumpalo::Bump,
) -> Vec<Completion<'hir>> {
    let mut locals: Vec<_> = func
        .locals
        .iter()
        .filter(|local| local.decl_span != Span::default())
        .map(|local| Completion {
            label: keep(arena, hir.symbols.get(local.name)),
            kind: CompletionKind::Variable,
            detail: keep(arena, &format_type(local.typ, hir, &func.generics)),
            docs: None,
            type_key: type_key(arena, local.typ, hir),
        })
        .collect();

    dedup_by_label(&mut locals);
    locals
}
