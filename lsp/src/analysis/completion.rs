use crate::analysis::{
    HoverTarget, Snapshot, base_name,
    hover::{
        self, format_type, implementor_of, is_generic_instance, nominal_name, through_reference,
    },
};
use frontend::hir::{self, AdtId, Owner};
use frontend::lexer::token::Span;
use frontend::source_map::SourceMap;
use std::collections::{HashMap, HashSet};

/// The candidates a completion request can draw on
#[derive(Debug, Default)]
pub struct Completions {
    /// members reachable through `.`, keyed by the receiver's nominal type name
    pub members: HashMap<String, Vec<Completion>>,
    /// items reachable through `::`, keyed by a type name or by a module path
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

pub(super) struct CompletionCollector<'a, 'hir> {
    hir: &'a Snapshot<'hir>,
    map: &'a SourceMap,
    imported_names: &'a HashSet<String>,
    out: Completions,
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
    ) -> Self {
        Self { hir, map, imported_names, out: Completions::default() }
    }

    pub(super) fn collect(mut self) -> Completions {
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

        for def in hir.adts.iter() {
            let name = base_name(hir.symbols.get(def.name));
            let generics: Vec<_> = def.generics.iter().map(|generic| generic.name).collect();
            let key = base_name(&nominal_name(def.name, &generics, hir));

            match def.is_struct() {
                true => {
                    let fields = self.out.members.entry(key).or_default();
                    for field in def.fields() {
                        fields.push(Completion {
                            label: hir.symbols.get(field.name).to_owned(),
                            kind: CompletionKind::Field,
                            detail: format_type(field.typ, hir, &generics),
                            docs: hir.docs(field.name_span),
                            type_key: type_key(field.typ, hir),
                        });
                    }
                },
                _ => {
                    let variants = self.out.associated.entry(name.clone()).or_default();
                    for variant in def.variants() {
                        variants.push(Completion {
                            label: hir.symbols.get(variant.name).to_owned(),
                            kind: CompletionKind::Variant,
                            detail: match &variant.payload {
                                Some(payload) => format_type(*payload, hir, &generics),
                                None => variant.value.to_string(),
                            },
                            docs: hir.docs(variant.name_span),
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
                    detail: format!("{keyword} {nominal}"),
                    docs: hir.docs(def.decl_span),
                    type_key: None,
                };

                self.export_by_module(def.decl_span, candidate);
            }
        }

        for interface in &hir.interfaces {
            let name = base_name(hir.symbols.get(interface.name));
            let methods = self.out.associated.entry(name.clone()).or_default();
            for method in &interface.methods {
                methods.push(Completion {
                    label: base_name(hir.symbols.get(method.name)),
                    kind: CompletionKind::Method,
                    detail: hover::interface_signature(method, interface, hir),
                    docs: hir.docs(method.decl_span),
                    type_key: None,
                });
            }

            for constant in &interface.constants {
                methods.push(Completion {
                    label: base_name(hir.symbols.get(constant.name)),
                    kind: CompletionKind::Constant,
                    detail: hover::interface_const_signature(constant, interface, hir),
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
            self.export_by_module(interface.decl_span, candidate);
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
                label: base_name(qualified),
                kind: match receiver {
                    Some(_) => CompletionKind::Method,
                    None => CompletionKind::Function,
                },
                detail: hover::signature(func, hir),
                docs: hir.docs(func.decl_span),
                type_key: type_key(func.return_type, hir),
            };

            match (receiver, implementor_of(func.owner, hir, &[])) {
                (Some(receiver), _) => {
                    let key = base_name(&format_type(through_reference(receiver), hir, &[]));
                    self.out.members.entry(key).or_default().push(candidate);
                },
                (_, Some(implementor)) => {
                    self.out.associated.entry(base_name(&implementor)).or_default().push(candidate);
                },
                _ => {
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
                _ => {
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

    fn export_by_module(&mut self, decl_span: Span, candidate: Completion) {
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
                    _ => format!("{prefix}::{segment}"),
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
                    _ => self.out.associated.entry(prefix.clone()).or_default().push(candidate),
                }

                prefix = full;
            }
        }
    }
}

impl Importable {
    pub(super) fn name_span(self, hir: &Snapshot<'_>) -> Span {
        match self {
            Self::Function(at) => hir.functions[at as usize].name_span,
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

/// The name a type's members are indexed under, references being transparent
/// because a method call auto-references its receiver
#[inline]
fn type_key(typ: hir::Type<'_>, hir: &Snapshot<'_>) -> Option<String> {
    let typ = through_reference(typ);
    match typ.kind() {
        hir::TypeKind::Infer(_)
        | hir::TypeKind::Error
        | hir::TypeKind::Unit
        | hir::TypeKind::Never => None,
        _ => Some(base_name(&format_type(typ, hir, &[]))),
    }
}

/// collect every name completion can offer, keyed by how it is reached
#[inline]
pub(super) fn completions(
    hir: &Snapshot<'_>,
    map: &SourceMap,
    imported_names: &HashSet<String>,
) -> Completions {
    CompletionCollector::new(hir, map, imported_names).collect()
}

/// drop repeats a monomorphised template leaves behind, keeping source order
pub(super) fn dedup_by_label(list: &mut Vec<Completion>) {
    let mut seen = HashSet::new();
    list.retain(|item| seen.insert((item.label.clone(), item.kind)));
    list.sort_by(|a, b| a.label.cmp(&b.label));
}

pub(super) fn scope_of(func: &hir::Function<'_>, hir: &Snapshot<'_>) -> Vec<Completion> {
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
