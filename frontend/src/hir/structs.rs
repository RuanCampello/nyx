use crate::{
    hir::{
        AdtDef, AdtId, AdtKind, ArrayId, ArrayType, FieldDef, Layout, SymbolId, SymbolTable,
        TyInterner, Type, TypeKind,
        collect::{ArrayTable, Enums, GenericEnv, Structs},
        diagnostics::Diagnostics,
        error::{HirError, hir_error},
        ids::IndexVec,
        type_resolver,
    },
    parser::statement::{self, StructRepr, StructReprKind},
};
use std::collections::{HashMap, HashSet};

struct Lowering<'a, 'h, 'hir> {
    declarations: &'a [(SymbolId, &'a statement::Struct<'h>)],
    struct_map: &'a Structs,
    enum_map: &'a Enums,
    adts: &'a IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &'a ArrayTable<'hir>,
    types: &'a TyInterner<'hir>,
    symbols: &'a SymbolTable,
    local_ids: HashMap<AdtId, usize>,
    states: Vec<Visit>,
}

struct LayoutEngine<'a, 'hir> {
    adts: &'a IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &'a IndexVec<ArrayId, ArrayType<'hir>>,
    layouts: Vec<Option<ComputedLayout>>,
    states: Vec<Visit>,
}

#[derive(Clone)]
struct ComputedLayout {
    summary: Layout,
    offsets: Vec<u32>,
    payload_offset: u32,
}

#[derive(Clone, Copy)]
struct PendingField<'hir> {
    typ: Type<'hir>,
    declared_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Visit {
    Unvisited,
    Visiting,
    Visited,
}

pub(in crate::hir) fn lower_structs<'h, 'hir>(
    declarations: &[(SymbolId, &statement::Struct<'h>)],
    struct_map: &Structs,
    enum_map: &Enums,
    adts: &IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &ArrayTable<'hir>,
    types: &TyInterner<'hir>,
    symbols: &SymbolTable,
    lowered: &mut [Option<AdtDef<'hir>>],
    sink: &mut Diagnostics,
) -> Result<(), HirError<'h>> {
    for (_, declaration) in declarations {
        for field in &declaration.fields {
            symbols.insert(field.name);
        }
    }

    let mut lowering = Lowering {
        declarations,
        struct_map,
        enum_map,
        adts,
        arrays,
        types,
        symbols,
        local_ids: declarations
            .iter()
            .enumerate()
            .map(|(index, (symbol, _))| (struct_map[symbol], index))
            .collect(),
        states: vec![Visit::Unvisited; declarations.len()],
    };

    for id in 0..declarations.len() {
        lowering.lower_struct(id, lowered, sink)?;
    }
    Ok(())
}

pub(in crate::hir) fn compute_layouts<'hir>(
    adts: &mut IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &IndexVec<ArrayId, ArrayType<'hir>>,
) {
    let computed = LayoutEngine::new(adts, arrays).compute();
    for (definition, computed) in adts.iter_mut().zip(computed) {
        definition.layout = computed.summary;
        match &mut definition.kind {
            AdtKind::Struct { fields, .. } => {
                for (field, offset) in fields.iter_mut().zip(computed.offsets) {
                    field.offset = offset;
                }
            },
            AdtKind::Enum { payload_offset, .. } => *payload_offset = computed.payload_offset,
        }
    }
}

pub fn type_layout<'hir>(
    typ: Type<'hir>,
    types: &TyInterner<'hir>,
    adts: &IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &ArrayTable<'hir>,
) -> (u32, u32) {
    let (size, align, _) = type_field_info(typ, types, adts, arrays);
    (size, align)
}

pub fn struct_field<'hir>(
    origin: Type<'hir>,
    field: SymbolId,
    types: &TyInterner<'hir>,
    adts: &IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &ArrayTable<'hir>,
) -> FieldDef<'hir> {
    let (id, args) = match origin.kind() {
        TypeKind::Adt(id, args) => (id, args),
        TypeKind::Ref { to, .. } => match to.kind() {
            TypeKind::Adt(id, args) => (id, args),
            _ => unreachable!("field projection on non-struct"),
        },
        _ => unreachable!("field projection on non-struct"),
    };
    let index = adts[id]
        .fields()
        .iter()
        .position(|candidate| candidate.name == field)
        .expect("field must exist after HIR validation");
    if args.is_empty() {
        return adts[id].fields()[index];
    }
    let layout = concrete_adt_layout(id, args, types, adts, arrays);
    let mut field = adts[id].fields()[index];
    field.typ = field.typ.subst(types, arrays, args);
    field.offset = layout.offsets[index];
    field
}

pub fn enum_payload_offset<'hir>(
    typ: Type<'hir>,
    types: &TyInterner<'hir>,
    adts: &IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &ArrayTable<'hir>,
) -> u32 {
    let (id, args) = match typ.kind() {
        TypeKind::Adt(id, args) => (id, args),
        TypeKind::Ref { to, .. } => match to.kind() {
            TypeKind::Adt(id, args) => (id, args),
            _ => unreachable!("payload offset requested for non-enum"),
        },
        _ => unreachable!("payload offset requested for non-enum"),
    };
    match args.is_empty() {
        true => adts[id].payload_offset(),
        false => concrete_adt_layout(id, args, types, adts, arrays).payload_offset,
    }
}

pub fn type_contains_float<'hir>(
    typ: Type<'hir>,
    types: &TyInterner<'hir>,
    adts: &IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &ArrayTable<'hir>,
) -> bool {
    type_field_info(typ, types, adts, arrays).2
}

fn type_field_info<'hir>(
    typ: Type<'hir>,
    types: &TyInterner<'hir>,
    adts: &IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &ArrayTable<'hir>,
) -> (u32, u32, bool) {
    if let Some((size, align)) = scalar_layout(typ) {
        return (size, align, matches!(typ.kind(), TypeKind::F32 | TypeKind::F64));
    }
    match typ.kind() {
        TypeKind::Adt(id, args) if args.is_empty() => {
            let layout = adts[id].layout;
            let (size, align) = layout.into();
            (size, align, layout.contains_float())
        },
        TypeKind::Adt(id, args) => {
            let layout = concrete_adt_layout(id, args, types, adts, arrays).summary;
            let (size, align) = layout.into();
            (size, align, layout.contains_float())
        },
        TypeKind::Array(id) => {
            let array = arrays.get(id);
            let (size, align, contains_float) = type_field_info(array.element, types, adts, arrays);
            (size * array.len, align, contains_float)
        },
        TypeKind::SelfType | TypeKind::GenericParam(_) | TypeKind::Error => (0, 1, false),
        _ => unreachable!("type has no runtime layout"),
    }
}

fn concrete_adt_layout<'hir>(
    id: AdtId,
    args: &[Type<'hir>],
    types: &TyInterner<'hir>,
    adts: &IndexVec<AdtId, AdtDef<'hir>>,
    arrays: &ArrayTable<'hir>,
) -> ComputedLayout {
    match &adts[id].kind {
        AdtKind::Struct { fields, repr } => {
            let field_types: Vec<_> =
                fields.iter().map(|field| field.typ.subst(types, arrays, args)).collect();
            layout_struct_fields(&field_types, *repr, |typ| {
                type_field_info(typ, types, adts, arrays)
            })
        },

        AdtKind::Enum { variants, repr, .. } => {
            let payloads: Vec<_> = variants
                .iter()
                .filter_map(|variant| variant.payload)
                .map(|payload| payload.subst(types, arrays, args))
                .collect();
            layout_enum_variants(&payloads, *repr, |typ| type_field_info(typ, types, adts, arrays))
        },
    }
}

fn layout_struct_fields<'hir>(
    types: &[Type<'hir>],
    repr: StructRepr,
    mut field_info: impl FnMut(Type<'hir>) -> (u32, u32, bool),
) -> ComputedLayout {
    use StructReprKind::*;

    let mut fields: Vec<_> = types
        .iter()
        .enumerate()
        .map(|(declared_index, &typ)| PendingField { typ, declared_index })
        .collect();
    order_fields_by(&mut fields, repr, &mut field_info);

    let mut offset = 0;
    let mut alignment = 1;
    let mut contains_float = false;
    let mut offsets = vec![0; fields.len()];

    for field in fields {
        let (size, natural_align, has_float) = field_info(field.typ);
        let align = match repr.kind {
            Packed => natural_align.min(repr.align.map(|align| align.get()).unwrap_or(1)),
            Default | Extern => natural_align,
        };
        alignment = alignment.max(align);
        contains_float |= has_float;
        offset = align_to(offset, align);
        offsets[field.declared_index] = offset;
        offset += size;
    }

    if repr.kind != Packed
        && let Some(explicit) = repr.align
    {
        alignment = alignment.max(explicit.get());
    }

    ComputedLayout {
        summary: Layout::new(align_to(offset, alignment), alignment, contains_float),
        offsets,
        payload_offset: 0,
    }
}

fn layout_enum_variants<'hir>(
    payloads: &[Type<'hir>],
    repr: crate::hir::EnumRepr,
    mut field_info: impl FnMut(Type<'hir>) -> (u32, u32, bool),
) -> ComputedLayout {
    let (tag_size, tag_align) = repr.layout();
    let (mut payload_size, mut payload_align) = (0, 1);
    let mut contains_float = false;

    for &payload in payloads {
        let (size, align, has_float) = field_info(payload);
        payload_size = payload_size.max(size);
        payload_align = payload_align.max(align);
        contains_float |= has_float;
    }

    let alignment = tag_align.max(payload_align);
    let payload_offset = align_to(tag_size, payload_align);
    ComputedLayout {
        summary: Layout::new(
            align_to(payload_offset + payload_size, alignment),
            alignment,
            contains_float,
        ),
        offsets: Vec::new(),
        payload_offset,
    }
}

fn order_fields_by<'hir>(
    fields: &mut [PendingField<'hir>],
    repr: StructRepr,
    field_info: &mut impl FnMut(Type<'hir>) -> (u32, u32, bool),
) {
    if repr.kind == StructReprKind::Extern {
        return;
    }

    let max_align = repr.align.map(|align| align.get()).unwrap_or(1);
    fields.sort_unstable_by(|a, b| {
        let (a_size, mut a_align, _) = field_info(a.typ);
        let (b_size, mut b_align, _) = field_info(b.typ);
        if repr.kind == StructReprKind::Packed {
            a_align = a_align.min(max_align);
            b_align = b_align.min(max_align);
        }

        b_align
            .cmp(&a_align)
            .then_with(|| b_size.cmp(&a_size))
            .then_with(|| a.declared_index.cmp(&b.declared_index))
    });
}

#[inline(always)]
const fn scalar_layout(typ: Type<'_>) -> Option<(u32, u32)> {
    use TypeKind::*;
    match typ.kind() {
        I8 | U8 | Bool => Some((1, 1)),
        I16 | U16 => Some((2, 2)),
        I32 | U32 | F32 | Char => Some((4, 4)),
        I64 | U64 | Iptr | Uptr | F64 => Some((8, 8)),
        Ref { .. } | Raw { .. } => Some((8, 8)),
        Str | Slice { .. } => Some((16, 8)),
        String => Some((24, 8)),
        Unit | Never => Some((0, 1)),
        _ => None,
    }
}

#[inline(always)]
const fn align_to(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}

impl<'a, 'h, 'hir> Lowering<'a, 'h, 'hir> {
    fn lower_struct(
        &mut self,
        id: usize,
        lowered: &mut [Option<AdtDef<'hir>>],
        sink: &mut Diagnostics,
    ) -> Result<(), HirError<'h>> {
        match self.states[id] {
            Visit::Visited => return Ok(()),
            Visit::Unvisited => {},
            Visit::Visiting => {
                let (_, declaration) = self.declarations[id];
                return Err(hir_error!(
                    declaration.span,
                    CircularStruct { name: declaration.name }
                ));
            },
        }

        self.states[id] = Visit::Visiting;
        let (name, declaration) = self.declarations[id];
        let mut seen = HashSet::new();
        let mut fields = Vec::with_capacity(declaration.fields.len());

        for field in &declaration.fields {
            let field_symbol = self.symbols.get_id(field.name).expect("field name is interned");
            if !seen.insert(field_symbol) {
                sink.emit(hir_error!(field.span, DuplicateField { name: field.name }).into());
                continue;
            }

            let env: GenericEnv<'hir> = declaration
                .generics
                .iter()
                .enumerate()
                .map(|(index, generic)| {
                    (generic.name.to_owned(), self.types.generic_param(index as u8))
                })
                .collect();

            let mut context = type_resolver::ResolveCtx::root(
                self.symbols,
                self.struct_map,
                self.enum_map,
                self.adts,
                self.arrays,
                self.types,
            );
            context.env = Some(&env);

            let mut typ = match type_resolver::resolve_annotation(
                &context,
                &field.typ.value(),
                field.typ.span(),
            ) {
                Ok(typ) => typ,
                Err(error) => Type::error(sink.emit(error.into())),
            };

            if let TypeKind::Adt(dependency, _) = typ.kind()
                && let Some(&local_id) = self.local_ids.get(&dependency)
                && let Err(error) = self.lower_struct(local_id, lowered, sink)
            {
                typ = Type::error(sink.emit(error.into()));
            }
            fields.push(FieldDef {
                name: field_symbol,
                typ,
                offset: 0,
                name_span: field.name_span,
            });
        }

        lowered[id] = Some(AdtDef {
            name,
            is_pub: declaration.is_pub,
            decl_span: declaration.span,
            name_span: declaration.name_span,
            kind: AdtKind::Struct { fields, repr: declaration.repr },
            layout: Layout::default(),
            generics: self.adts[self.struct_map[&name]].generics.clone(),
        });

        self.states[id] = Visit::Visited;

        Ok(())
    }
}

impl<'a, 'hir> LayoutEngine<'a, 'hir> {
    fn new(
        adts: &'a IndexVec<AdtId, AdtDef<'hir>>,
        arrays: &'a IndexVec<ArrayId, ArrayType<'hir>>,
    ) -> Self {
        Self {
            adts,
            arrays,
            layouts: vec![None; adts.len()],
            states: vec![Visit::Unvisited; adts.len()],
        }
    }

    fn compute(mut self) -> Vec<ComputedLayout> {
        for index in 0..self.adts.len() {
            self.compute_adt(AdtId(index as u32));
        }
        self.layouts
            .into_iter()
            .map(|layout| layout.expect("ADT layout must be computed"))
            .collect()
    }

    fn compute_adt(&mut self, id: AdtId) {
        match self.states[id.0 as usize] {
            Visit::Visited => return,
            Visit::Visiting => unreachable!("HIR rejects recursive by-value ADT layout"),
            Visit::Unvisited => {},
        }
        self.states[id.0 as usize] = Visit::Visiting;

        let computed = match &self.adts[id].kind {
            AdtKind::Struct { fields, repr } => {
                let field_types: Vec<_> = fields.iter().map(|field| field.typ).collect();
                layout_struct_fields(&field_types, *repr, |typ| self.field_info(typ))
            },
            AdtKind::Enum { variants, repr, .. } => {
                let payloads: Vec<_> =
                    variants.iter().filter_map(|variant| variant.payload).collect();
                layout_enum_variants(&payloads, *repr, |typ| self.field_info(typ))
            },
        };

        self.layouts[id.0 as usize] = Some(computed);
        self.states[id.0 as usize] = Visit::Visited;
    }

    fn field_info(&mut self, typ: Type<'hir>) -> (u32, u32, bool) {
        if let Some((size, align)) = scalar_layout(typ) {
            return (size, align, matches!(typ.kind(), TypeKind::F32 | TypeKind::F64));
        }

        match typ.kind() {
            TypeKind::Adt(id, _) => {
                self.compute_adt(id);
                let layout = self.layouts[id.0 as usize]
                    .as_ref()
                    .expect("dependent ADT layout must be computed")
                    .summary;
                let (size, align) = layout.into();
                (size, align, layout.contains_float())
            },
            TypeKind::Array(id) => {
                let array = self.arrays[id];
                let (size, align, contains_float) = self.field_info(array.element);
                (size * array.len, align, contains_float)
            },
            TypeKind::SelfType | TypeKind::GenericParam(_) | TypeKind::Error => (0, 1, false),
            _ => unreachable!("type has no runtime layout"),
        }
    }
}
