use crate::analysis::{Snapshot, short_name, walker::Binding};
use frontend::hir::{self, AdtId, LocalId, Owner, SymbolId};
use frontend::{lexer::token::Span, source_map::SourceMap};

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

/// What a span names, resolved against a [`Snapshot`] only when a hover asks
///
/// Bodies hold far more spans than any session will ever hover, so nothing here
/// is rendered up front
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HoverTarget<'hir> {
    /// an expression, shown as the type it inferred to
    Type {
        typ: hir::Type<'hir>,
        function: u32,
    },
    /// a type named in source, shown as the declaration it resolves to
    Nominal {
        typ: hir::Type<'hir>,
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
    Struct(AdtId),
    Field {
        structure: AdtId,
        field: u32,
    },
    Enum(AdtId),
    Variant {
        enumeration: AdtId,
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

/// How many fields/variants a hover shows before truncating with `// ...`
const MAX_HOVER_ITEMS: usize = 5;

impl<'hir> Snapshot<'hir> {
    /// render what `target` names
    pub(super) fn hover(&self, target: HoverTarget<'hir>, map: &SourceMap) -> Option<HoverInfo> {
        use HoverTarget as H;

        let generics = |function: Option<u32>| match function {
            Some(at) => self.functions[at as usize].generics.as_slice(),
            None => &[],
        };

        let info = match target {
            H::Type { typ, function } => self.type_info(typ, generics(Some(function))),
            H::Nominal { typ, function } => self
                .nominal_hover(typ, map)
                .unwrap_or_else(|| self.type_info(typ, generics(function))),
            H::Local { function, local, form } => self.local_hover(function, local, form),
            H::Function(at) => self.fn_hover(&self.functions[at as usize], map),
            H::Constant(at) => self.const_hover(&self.constants[at as usize], map),
            H::Struct(id) => self.nominal_hover_of(id, map),
            H::Field { structure, field } => self.field_hover(structure, field, map),
            H::Enum(id) => self.nominal_hover_of(id, map),
            H::Variant { enumeration, variant } => self.variant_hover(enumeration, variant, map),
            H::Interface(at) => self.interface_hover(at as usize, map),
            H::InterfaceMethod { interface, method } => {
                self.interface_method_hover(interface as usize, method as usize, map)
            },
            H::InterfaceConstant { interface, constant } => {
                self.interface_constant_hover(interface as usize, constant as usize, map)
            },
        };

        Some(info)
    }

    /// the type an inlay hint annotates a binding with
    pub(super) fn hint(&self, typ: hir::Type<'hir>, function: u32) -> String {
        format_type(typ, self, &self.functions[function as usize].generics)
    }

    #[inline]
    pub(super) fn docs(&self, name_span: Span) -> Option<String> {
        self.docs.get(&name_span).map(|docs| docs.to_string())
    }

    /// The module path of the file `span` falls in, `none` for synthetic spans
    #[inline]
    pub(super) fn module_of(&self, map: &SourceMap, span: Span) -> Option<String> {
        match span == Span::default() {
            true => None,
            false => self.modules.get(&map.span_data(span).file).cloned(),
        }
    }

    fn type_info(&self, typ: hir::Type<'hir>, generics: &[SymbolId]) -> HoverInfo {
        let layout = match is_open(typ, self) {
            true => None,
            false => layout_of(self, typ),
        };

        HoverInfo { path: None, ty: format_type(typ, self, generics), layout, docs: None }
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

    fn field_hover(&self, id: AdtId, field: u32, map: &SourceMap) -> HoverInfo {
        let def = &self.adts[id];
        let field = &def.fields()[field as usize];
        let owner = nominal_name(def.name, &generic_names(def), self);
        let path = self.module_of(map, def.decl_span).map(|module| format!("{module}::{owner}"));

        HoverInfo {
            path,
            ty: format!(
                "{}: {}",
                self.symbols.get(field.name),
                format_type(field.typ, self, &generic_names(def))
            ),
            layout: layout_of(self, field.typ),
            docs: self.docs(field.name_span),
        }
    }

    fn variant_hover(&self, id: AdtId, variant: u32, map: &SourceMap) -> HoverInfo {
        let def = &self.adts[id];
        let variant = &def.variants()[variant as usize];
        let owner = nominal_name(def.name, &generic_names(def), self);
        let path = self.module_of(map, def.decl_span).map(|module| format!("{module}::{owner}"));

        let name = self.symbols.get(variant.name);
        let ty = match &variant.payload {
            Some(payload) => {
                format!("{owner}::{name}({})", format_type(*payload, self, &generic_names(def)))
            },
            None => format!("{owner}::{name} = {}", variant.value),
        };

        // the variant's own layout, not the enum's: a fieldless one carries
        // nothing, so it is zero-sized like an empty struct
        let layout = match &variant.payload {
            Some(payload) if is_open(*payload, self) => None,
            Some(payload) => layout_of(self, *payload),
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

    /// a nominal type shown as its own declaration, `none` for anything without one to show
    fn nominal_hover(&self, typ: hir::Type<'hir>, map: &SourceMap) -> Option<HoverInfo> {
        match typ.kind() {
            hir::TypeKind::Adt(id, _) => Some(self.nominal_hover_of(id, map)),
            _ => None,
        }
    }

    /// a struct or enum shown as its own declaration, keyed directly by id
    /// rather than by a (possibly synthetic) [`hir::Type`] handle
    fn nominal_hover_of(&self, id: AdtId, map: &SourceMap) -> HoverInfo {
        let def = &self.adts[id];
        let path = self.module_of(map, def.decl_span);
        let ty = match def.is_struct() {
            true => struct_def(def, self),
            false => enum_def(def, self),
        };

        let layout = match is_open_adt(def) {
            true => None,
            false => Some(def.layout.into()),
        };

        HoverInfo { path, ty, layout, docs: self.docs(def.decl_span) }
    }

    fn const_hover(&self, constant: &hir::Constant<'hir>, map: &SourceMap) -> HoverInfo {
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
        if let Some(value) = super::eval::const_value(constant, self) {
            ty.push_str(" = ");
            ty.push_str(&value);
        }

        HoverInfo { path, ty, layout: None, docs: self.docs(constant.decl_span) }
    }

    fn fn_hover(&self, func: &hir::Function<'hir>, map: &SourceMap) -> HoverInfo {
        let implementor = implementor(func, self);
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

/// generic parameter names declared on an ADT, by their symbol
fn generic_names(def: &hir::AdtDef<'_>) -> Vec<SymbolId> {
    def.generics.iter().map(|generic| generic.name).collect()
}

pub(super) fn implementor<'hir>(
    func: &hir::Function<'hir>,
    hir: &Snapshot<'hir>,
) -> Option<String> {
    implementor_of(func.owner, hir, &func.generics)
}

/// the type a `.` call reaches this through, `none` for an associated function
///
/// [`hir::FunctionKind`] cannot answer this: an intrinsic method takes a receiver
/// without being a [`hir::FunctionKind::Method`]
pub(super) fn receiver<'hir>(
    func: &hir::Function<'hir>,
    hir: &Snapshot<'hir>,
) -> Option<hir::Type<'hir>> {
    let first = func.params.first()?;
    (hir.symbols.get(func.locals[first.id].name) == "self").then_some(first.typ)
}

#[inline]
pub(super) fn interface_const_signature(
    constant: &hir::InterfaceConstSignature<'_>,
    interface: &hir::InterfaceSignature<'_>,
    hir: &Snapshot<'_>,
) -> String {
    format!(
        "const {}: {}",
        short_name(hir.symbols.get(constant.name)),
        format_type(constant.typ, hir, &interface.generic_params)
    )
}

pub(super) fn interface_signature(
    method: &hir::InterfaceMethodSignature<'_>,
    interface: &hir::InterfaceSignature<'_>,
    hir: &Snapshot<'_>,
) -> String {
    let generics = &interface.generic_params;
    let mut params: Vec<_> = Vec::with_capacity(method.params.len() + 1);

    if method.has_receiver {
        params.push(match method.receiver_mut {
            true => "&mut self".to_owned(),
            false => "&self".to_owned(),
        });
    }
    params.extend(method.params.iter().map(|&typ| format_type(typ, hir, generics)));

    let mut out = format!("fn {}({})", short_name(hir.symbols.get(method.name)), params.join(", "));
    let ret = format_type(method.return_type, hir, generics);
    if ret != "()" {
        out.push_str(": ");
        out.push_str(&ret);
    }

    out
}

#[inline]
/// the type an item is declared on, rendered as it is written
pub(super) fn implementor_of(
    owner: Owner<'_>,
    hir: &Snapshot<'_>,
    generics: &[SymbolId],
) -> Option<String> {
    match owner {
        Owner::Free => None,
        Owner::Inherent(on) | Owner::Interface { on, .. } => Some(format_type(on, hir, generics)),
    }
}

pub(super) fn signature(func: &hir::Function<'_>, hir: &Snapshot<'_>) -> String {
    let mut out = String::new();
    // markers sit on their own line above the signature, as they are written
    if func.is_unsafe {
        out.push_str("@unsafe\n");
    }
    if matches!(func.kind, hir::FunctionKind::Intrinsic(_)) {
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
                hir::TypeKind::Ref { mutable: true, .. } => "&mut self".into(),
                hir::TypeKind::Ref { .. } => "&self".into(),
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

fn struct_def(def: &hir::AdtDef<'_>, hir: &Snapshot<'_>) -> String {
    let generics = generic_names(def);
    let name = nominal_name(def.name, &generics, hir);
    let fields = def.fields();
    if fields.is_empty() {
        return format!("struct {name}");
    }

    let lines = fields.iter().map(|f| {
        format!("    {}: {},", hir.symbols.get(f.name), format_type(f.typ, hir, &generics))
    });

    format!("struct {name} {{\n{}\n}}", truncated(lines, fields.len()))
}

fn enum_def(def: &hir::AdtDef<'_>, hir: &Snapshot<'_>) -> String {
    let generics = generic_names(def);
    let name = nominal_name(def.name, &generics, hir);
    let variants = def.variants();
    if variants.is_empty() {
        return format!("enum {name}");
    }

    let lines = variants.iter().map(|v| match &v.payload {
        Some(typ) => format!("    {}({}),", hir.symbols.get(v.name), format_type(*typ, hir, &generics)),
        None => format!("    {},", hir.symbols.get(v.name)),
    });

    format!("enum {name} {{\n{}\n}}", truncated(lines, variants.len()))
}

/// join the first [`MAX_HOVER_ITEMS`] lines, eliding the rest with `// …`
fn truncated(lines: impl Iterator<Item = String>, total: usize) -> String {
    let mut lines: Vec<_> = lines.take(MAX_HOVER_ITEMS).collect();
    if total > MAX_HOVER_ITEMS {
        lines.push("    // …".to_owned());
    }

    lines.join("\n")
}

pub(super) fn format_type(typ: hir::Type<'_>, hir: &Snapshot<'_>, generics: &[SymbolId]) -> String {
    use hir::TypeKind::*;

    match typ.kind() {
        Unit => "()".to_owned(),
        Str => "str".to_owned(),
        GenericParam(i) => generics
            .get(i as usize)
            .map(|&name| hir.symbols.get(name).to_owned())
            .unwrap_or_else(|| format!("T{i}")),
        Adt(id, args) => {
            let def = &hir.adts[id];
            let declared = generic_names(def);
            match args.is_empty() {
                true => nominal_name(def.name, &declared, hir),
                false => format!(
                    "{}<{}>",
                    hir.symbols.get(def.name),
                    args.iter()
                        .map(|&arg| format_type(arg, hir, generics))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        },
        Ref { mutable, to } => {
            let typ = format_type(to, hir, generics);
            match mutable {
                true => format!("&mut {typ}"),
                _ => format!("&{typ}"),
            }
        },
        Raw { mutable, to } => {
            let typ = format_type(to, hir, generics);
            match mutable {
                true => format!("*mut {typ}"),
                _ => format!("*{typ}"),
            }
        },
        Array(id) => {
            let array = &hir.arrays[id];
            format!("[{}; {}]", format_type(array.element, hir, generics), array.len)
        },
        Slice { mutable, element } => {
            let element = format_type(element, hir, generics);
            match mutable {
                true => format!("&mut [{element}]"),
                _ => format!("&[{element}]"),
            }
        },
        kind => kind.to_string(),
    }
}

/// whether a function is one concrete specialisation of a template
pub(super) fn is_generic_instance(func: &hir::Function<'_>, hir: &Snapshot<'_>) -> bool {
    let qualified = hir.symbols.get(func.name);
    let tail = qualified.rsplit("::").next().unwrap_or(qualified);

    tail.contains('$') && func.generics.is_empty()
}

fn function_name(func: &hir::Function<'_>, hir: &Snapshot<'_>) -> String {
    let qualified = hir.symbols.get(func.name);
    let declares_generics = matches!(func.owner, Owner::Free) && !func.generics.is_empty();
    match declares_generics {
        true => nominal_name(func.name, &func.generics, hir),
        _ => short_name(qualified),
    }
}

pub(super) fn nominal_name(name: SymbolId, generics: &[SymbolId], hir: &Snapshot<'_>) -> String {
    let raw = hir.symbols.get(name);
    if generics.is_empty() {
        return short_name(raw);
    }

    let base = raw.rsplit("::").next().unwrap_or(raw);
    let base = base.split('$').next().unwrap_or(base);
    let names: Vec<_> = generics.iter().map(|&g| hir.symbols.get(g)).collect();

    format!("{base}<{}>", names.join(", "))
}

pub(super) fn is_open(typ: hir::Type<'_>, hir: &Snapshot<'_>) -> bool {
    match typ.kind() {
        hir::TypeKind::Adt(id, _) => is_open_adt(&hir.adts[id]),
        _ => false,
    }
}

fn is_open_adt(def: &hir::AdtDef<'_>) -> bool {
    let carries_generic = |typ: hir::Type<'_>| match typ.kind() {
        hir::TypeKind::GenericParam(_) => true,
        hir::TypeKind::Ref { to, .. } => matches!(to.kind(), hir::TypeKind::GenericParam(_)),
        _ => false,
    };

    match def.is_struct() {
        true => def.fields().iter().any(|field| carries_generic(field.typ)),
        false => def.variants().iter().any(|variant| variant.payload.is_some_and(carries_generic)),
    }
}

#[inline(always)]
pub(super) fn layout_of(hir: &Snapshot<'_>, typ: hir::Type<'_>) -> Option<(u32, u32)> {
    use hir::TypeKind::*;

    Some(match typ.kind() {
        Unit | Never | SelfType | GenericParam(_) | Error | Infer(_) => return None,
        I8 | U8 | Bool => (1, 1),
        I16 | U16 => (2, 2),
        I32 | U32 | F32 | Char => (4, 4),
        I64 | U64 | F64 | Iptr | Uptr | Ref { .. } | Raw { .. } => (8, 8),
        Str | Slice { .. } => (16, 8),
        String => (24, 8),
        Adt(id, _) => hir.adts[id].layout.into(),
        Array(id) => {
            let array = &hir.arrays[id];
            let (size, align) = layout_of(hir, array.element)?;
            (size * array.len, align)
        },
    })
}

/// the type a field access reaches through, references being transparent
#[inline]
pub(super) fn through_reference(typ: hir::Type<'_>) -> hir::Type<'_> {
    match typ.kind() {
        hir::TypeKind::Ref { to, .. } | hir::TypeKind::Raw { to, .. } => through_reference(to),
        _ => typ,
    }
}

/// Split a `Qualifier::name` span into the qualifier and the trailing name,
/// `none` when the path has no qualifier
pub(super) fn split_path(map: &SourceMap, path: Span) -> Option<(Span, Span)> {
    let (file, range) = map.local_range(path);
    let at = map.source(file).get(range)?.rfind("::")?;

    let start = path.start.0;
    let qualifier = Span::new(path.start, frontend::BytePos(start + at as u32));
    let name = Span::new(frontend::BytePos(start + at as u32 + 2), path.end);

    Some((qualifier, name))
}

#[inline]
pub(super) fn nominal_name_span(typ: hir::Type<'_>, hir: &Snapshot<'_>) -> Option<Span> {
    match typ.kind() {
        hir::TypeKind::Adt(id, _) => hir.adts.get(id).map(|def| def.name_span),
        _ => None,
    }
}
