use crate::{
    hir::{
        Enum, EnumId, RefTargetKind, Struct, StructId, SymbolId, Type, TypeKind, Visit,
        index_vec::IndexVec,
    },
    mir::Layout,
    parser::statement::{StructRepr, StructReprKind},
};

#[derive(Debug, Clone)]
pub(crate) struct LayoutTable {
    structs: Vec<StructLayout>,
    enums: Vec<EnumLayout>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EnumLayout {
    #[allow(unused)]
    pub(crate) tag_layout: Layout,
    pub(crate) payload_offset: u32,
    pub(crate) layout: Layout,
}

struct LayoutEngine<'s> {
    structs: &'s IndexVec<StructId, Struct>,
    enums: &'s IndexVec<EnumId, Enum>,
    struct_layouts: Vec<Option<StructLayout>>,
    enum_layouts: Vec<Option<EnumLayout>>,
    struct_states: Vec<Visit>,
    enum_states: Vec<Visit>,
}

#[derive(Debug, Clone)]
struct StructLayout {
    fields: Vec<FieldLayout>,
    layout: Layout,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FieldLayout {
    name: SymbolId,
    pub(crate) typ: Type,
    pub(crate) offset: u32,
}

#[derive(Debug, Clone, Copy)]
struct PendingField {
    name: SymbolId,
    typ: Type,
    declared_index: usize,
}

impl LayoutTable {
    pub(crate) fn build(
        structs: &IndexVec<StructId, Struct>,
        enums: &IndexVec<EnumId, Enum>,
    ) -> Self {
        let (structs, enums) = LayoutEngine::new(structs, enums).compute();
        Self { structs, enums }
    }

    pub(crate) fn struct_summaries(&self) -> Vec<Layout> {
        self.structs.iter().map(|layout| layout.layout).collect()
    }

    pub(crate) fn enum_summaries(&self) -> Vec<Layout> {
        self.enums.iter().map(|layout| layout.layout).collect()
    }

    pub(crate) fn type_layout(&self, typ: Type) -> (u32, u32) {
        match scalar_layout(typ) {
            Some(layout) => layout,
            None => match typ.kind() {
                TypeKind::Struct(id) => self.structs[id.0 as usize].layout.into(),
                TypeKind::Enum(id) => self.enums[id.id() as usize].layout.into(),
                TypeKind::SelfType | TypeKind::GenericParam(_) => (0, 1),
                _ => unreachable!("type has no runtime layout"),
            },
        }
    }

    pub(crate) fn field(&self, origin: Type, field: SymbolId) -> FieldLayout {
        let id = match origin.kind() {
            TypeKind::Struct(id) => id,
            TypeKind::Ref { to, .. } => match to.kind() {
                RefTargetKind::Struct(id) => id,
                _ => unreachable!("field projection on non-struct"),
            },
            _ => unreachable!("field projection on non-struct"),
        };

        self.structs[id.0 as usize]
            .fields
            .iter()
            .find(|layout| layout.name == field)
            .copied()
            .expect("field layout must exist after HIR validation")
    }

    pub(crate) fn enum_layout(&self, id: EnumId) -> &EnumLayout {
        &self.enums[id.id() as usize]
    }
}

impl<'s> LayoutEngine<'s> {
    fn new(structs: &'s IndexVec<StructId, Struct>, enums: &'s IndexVec<EnumId, Enum>) -> Self {
        Self {
            structs,
            enums,
            struct_layouts: vec![None; structs.len()],
            enum_layouts: vec![None; enums.len()],
            struct_states: vec![Visit::Unvisited; structs.len()],
            enum_states: vec![Visit::Unvisited; enums.len()],
        }
    }

    fn compute(mut self) -> (Vec<StructLayout>, Vec<EnumLayout>) {
        for id in 0..self.structs.len() {
            self.compute_struct(StructId(id as u32));
        }
        for id in 0..self.enums.len() {
            let enum_def = &self.enums[id];
            self.compute_enum(enum_def.id);
        }

        let structs = self
            .struct_layouts
            .into_iter()
            .map(|layout| layout.expect("struct layout must be computed"))
            .collect();
        let enums = self
            .enum_layouts
            .into_iter()
            .map(|layout| layout.expect("enum layout must be computed"))
            .collect();

        (structs, enums)
    }

    fn compute_struct(&mut self, id: StructId) {
        let idx = id.0 as usize;
        match self.struct_states[idx] {
            Visit::Visited => return,
            Visit::Visiting => unreachable!("HIR rejects recursive by-value struct layout"),
            Visit::Unvisited => {},
        }

        self.struct_states[idx] = Visit::Visiting;
        let definition = &self.structs[id];
        for field in &definition.fields {
            if let TypeKind::Struct(dep) = field.typ.kind() {
                self.compute_struct(dep);
            }
        }

        let layout = self.layout_struct(definition);
        self.struct_layouts[idx] = Some(layout);
        self.struct_states[idx] = Visit::Visited;
    }

    fn compute_enum(&mut self, id: EnumId) {
        let idx = id.id() as usize;
        match self.enum_states[idx] {
            Visit::Visited => return,
            Visit::Visiting => unreachable!("HIR rejects recursive by-value enum layout"),
            Visit::Unvisited => {},
        }

        self.enum_states[idx] = Visit::Visiting;
        let definition = &self.enums[idx];

        let (tag_size, tag_align) = definition.repr.layout();

        let mut max_payload_size = 0;
        let mut max_payload_align = 1;
        let mut contains_float = false;

        for variant in &definition.variants {
            if let Some(payload_ty) = variant.payload {
                let (p_size, p_align) = self.layout_of(payload_ty);
                max_payload_size = max_payload_size.max(p_size);
                max_payload_align = max_payload_align.max(p_align);
                contains_float |= self.contains_float(payload_ty);
            }
        }

        let enum_alignment = tag_align.max(max_payload_align);
        let payload_offset = align_to(tag_size, max_payload_align);

        let size = align_to(payload_offset + max_payload_size, enum_alignment);

        self.enum_layouts[idx] = Some(EnumLayout {
            tag_layout: Layout::new(tag_size, tag_align, false),
            payload_offset,
            layout: Layout::new(size, enum_alignment, contains_float),
        });

        self.enum_states[idx] = Visit::Visited;
    }

    fn layout_struct(&mut self, definition: &Struct) -> StructLayout {
        let mut fields: Vec<_> = definition
            .fields
            .iter()
            .enumerate()
            .map(|(declared_index, field)| PendingField {
                name: field.name,
                typ: field.typ,
                declared_index,
            })
            .collect();

        self.order_fields(&mut fields, definition.repr);
        let (fields, summary) = self.assign_offsets(fields, definition.repr);

        StructLayout { fields, layout: summary }
    }

    fn order_fields(&mut self, fields: &mut [PendingField], repr: StructRepr) {
        match repr.kind {
            StructReprKind::Default => {
                fields.sort_unstable_by(|a, b| {
                    let (a_size, a_align) = self.layout_of(a.typ);
                    let (b_size, b_align) = self.layout_of(b.typ);

                    b_align
                        .cmp(&a_align)
                        .then_with(|| b_size.cmp(&a_size))
                        .then_with(|| a.declared_index.cmp(&b.declared_index))
                });
            },
            StructReprKind::Packed => {
                let max_align = repr.align.map(|align| align.get()).unwrap_or(1);
                fields.sort_unstable_by(|a, b| {
                    let (a_size, a_align) = self.layout_of(a.typ);
                    let (b_size, b_align) = self.layout_of(b.typ);

                    b_align
                        .min(max_align)
                        .cmp(&a_align.min(max_align))
                        .then_with(|| b_size.cmp(&a_size))
                        .then_with(|| a.declared_index.cmp(&b.declared_index))
                });
            },
            StructReprKind::Extern => {},
        }
    }

    fn assign_offsets(
        &mut self,
        fields: Vec<PendingField>,
        repr: StructRepr,
    ) -> (Vec<FieldLayout>, Layout) {
        let mut offset = 0;
        let mut struct_align = 1;
        let mut contains_float = false;
        let mut laid_out = Vec::with_capacity(fields.len());

        for field in fields {
            let (size, align) = self.field_layout(field.typ, repr);
            struct_align = struct_align.max(align);
            contains_float |= self.contains_float(field.typ);
            offset = align_to(offset, align);
            laid_out.push(FieldLayout { name: field.name, typ: field.typ, offset });
            offset += size;
        }

        if repr.kind != StructReprKind::Packed {
            if let Some(align) = repr.align {
                struct_align = struct_align.max(align.get());
            }
        }

        let size = align_to(offset, struct_align);
        (laid_out, Layout::new(size, struct_align, contains_float))
    }

    fn field_layout(&mut self, typ: Type, repr: StructRepr) -> (u32, u32) {
        let (size, align) = self.layout_of(typ);
        match repr.kind {
            StructReprKind::Packed => {
                let max_align = repr.align.map(|align| align.get()).unwrap_or(1);
                (size, align.min(max_align))
            },
            StructReprKind::Default | StructReprKind::Extern => (size, align),
        }
    }

    fn layout_of(&mut self, typ: Type) -> (u32, u32) {
        match scalar_layout(typ) {
            Some(layout) => layout,
            None => match typ.kind() {
                TypeKind::Struct(id) => {
                    self.compute_struct(id);
                    self.struct_layouts[id.0 as usize]
                        .as_ref()
                        .expect("dependent struct layout must be computed")
                        .layout
                        .into()
                },
                TypeKind::Enum(id) => {
                    self.compute_enum(id);
                    self.enum_layouts[id.id() as usize]
                        .as_ref()
                        .expect("dependent enum layout must be computed")
                        .layout
                        .into()
                },
                TypeKind::SelfType | TypeKind::GenericParam(_) => (0, 1),
                _ => unreachable!("type has no runtime layout"),
            },
        }
    }

    fn contains_float(&mut self, typ: Type) -> bool {
        match typ.kind() {
            TypeKind::F32 | TypeKind::F64 => true,
            TypeKind::Struct(id) => {
                self.compute_struct(id);
                self.struct_layouts[id.0 as usize]
                    .as_ref()
                    .expect("dependent struct layout must be computed")
                    .layout
                    .contains_float()
            },
            TypeKind::Enum(id) => {
                self.compute_enum(id);
                self.enum_layouts[id.id() as usize]
                    .as_ref()
                    .expect("dependent enum layout must be computed")
                    .layout
                    .contains_float()
            },
            _ => false,
        }
    }
}

#[inline]
const fn scalar_layout(typ: Type) -> Option<(u32, u32)> {
    match typ.kind() {
        TypeKind::I8 | TypeKind::U8 | TypeKind::Bool => Some((1, 1)),
        TypeKind::I16 | TypeKind::U16 => Some((2, 2)),
        TypeKind::I32 | TypeKind::U32 | TypeKind::F32 | TypeKind::Char => Some((4, 4)),
        TypeKind::I64 | TypeKind::U64 | TypeKind::Iptr | TypeKind::Uptr | TypeKind::F64 => {
            Some((8, 8))
        },
        TypeKind::Ref { .. } => Some((8, 8)),
        TypeKind::Str => Some((16, 8)),
        TypeKind::String => Some((24, 8)),
        TypeKind::Unit | TypeKind::Never => Some((0, 1)),
        _ => None,
    }
}

#[inline(always)]
const fn align_to(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}
