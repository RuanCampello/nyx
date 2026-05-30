use crate::{
    hir::{RefTargetKind, Struct, StructId, SymbolId, Type, TypeKind, Visit, index_vec::IndexVec},
    mir::Layout,
    parser::statement::{StructRepr, StructReprKind},
};

#[derive(Debug, Clone)]
pub(crate) struct LayoutTable {
    structs: Vec<StructLayout>,
}

struct LayoutEngine<'s> {
    structs: &'s IndexVec<StructId, Struct>,
    layouts: Vec<Option<StructLayout>>,
    states: Vec<Visit>,
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
    pub(crate) fn build(structs: &IndexVec<StructId, Struct>) -> Self {
        Self { structs: LayoutEngine::new(structs).compute() }
    }

    pub(crate) fn summaries(&self) -> Vec<Layout> {
        self.structs.iter().map(|layout| layout.layout).collect()
    }

    pub(crate) fn type_layout(&self, typ: Type) -> (u32, u32) {
        match scalar_layout(typ) {
            Some(layout) => layout,
            None => match typ.kind() {
                TypeKind::Struct(id) => self.structs[id.0 as usize].layout.into(),
                TypeKind::Enum(id) => id.repr().layout(),
                TypeKind::SelfType | TypeKind::GenericParam(_) => {
                    unreachable!("MIR layout requires concrete types")
                },
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
}

impl<'s> LayoutEngine<'s> {
    fn new(structs: &'s IndexVec<StructId, Struct>) -> Self {
        Self {
            structs,
            layouts: vec![None; structs.len()],
            states: vec![Visit::Unvisited; structs.len()],
        }
    }

    fn compute(mut self) -> Vec<StructLayout> {
        for id in 0..self.structs.len() {
            self.compute_struct(StructId(id as u32));
        }

        self.layouts
            .into_iter()
            .map(|layout| layout.expect("struct layout must be computed"))
            .collect()
    }

    fn compute_struct(&mut self, id: StructId) {
        let idx = id.0 as usize;
        match self.states[idx] {
            Visit::Visited => return,
            Visit::Visiting => unreachable!("HIR rejects recursive by-value struct layout"),
            Visit::Unvisited => {},
        }

        self.states[idx] = Visit::Visiting;
        let definition = &self.structs[id];
        for field in &definition.fields {
            if let TypeKind::Struct(dep) = field.typ.kind() {
                self.compute_struct(dep);
            }
        }

        let layout = self.layout_struct(definition);
        self.layouts[idx] = Some(layout);
        self.states[idx] = Visit::Visited;
    }

    fn layout_struct(&self, definition: &Struct) -> StructLayout {
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

    fn order_fields(&self, fields: &mut [PendingField], repr: StructRepr) {
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
        &self,
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

    fn field_layout(&self, typ: Type, repr: StructRepr) -> (u32, u32) {
        let (size, align) = self.layout_of(typ);
        match repr.kind {
            StructReprKind::Packed => {
                let max_align = repr.align.map(|align| align.get()).unwrap_or(1);
                (size, align.min(max_align))
            },
            StructReprKind::Default | StructReprKind::Extern => (size, align),
        }
    }

    fn layout_of(&self, typ: Type) -> (u32, u32) {
        match scalar_layout(typ) {
            Some(layout) => layout,
            None => match typ.kind() {
                TypeKind::Struct(id) => self.layouts[id.0 as usize]
                    .as_ref()
                    .expect("dependent struct layout must be computed")
                    .layout
                    .into(),
                TypeKind::Enum(id) => id.repr().layout(),
                TypeKind::SelfType | TypeKind::GenericParam(_) => {
                    unreachable!("MIR layout requires concrete types")
                },
                _ => unreachable!("type has no runtime layout"),
            },
        }
    }

    fn contains_float(&self, typ: Type) -> bool {
        match typ.kind() {
            TypeKind::F32 | TypeKind::F64 => true,
            TypeKind::Struct(id) => self.layouts[id.0 as usize]
                .as_ref()
                .expect("dependent struct layout must be computed")
                .layout
                .contains_float(),
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
        TypeKind::Unit => Some((0, 1)),
        _ => None,
    }
}

#[inline(always)]
const fn align_to(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}
