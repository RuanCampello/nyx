//! Struct lowering: AST -> HIR with topological field-type resolution

use crate::hir::{
    ArrayId, ArrayType, Enum, EnumId, Layout, Struct, StructField, StructId, SymbolId, SymbolTable,
    Type, TypeKind,
    diagnostics::Diagnostics,
    error::{HirError, hir_error},
    index_vec::IndexVec,
    scope::{ArrayTable, Enums, Structs},
    type_resolver,
};
use crate::parser::statement::{self, StructRepr, StructReprKind};
use std::collections::HashSet;

/// One module's struct batch being lowered in topological field order
struct Lowering<'a, 'h> {
    declarations: &'a [(SymbolId, &'a statement::Struct<'h>)],
    map: &'a Structs,
    enum_map: &'a Enums,
    arrays: &'a ArrayTable,
    symbols: &'a SymbolTable,
    states: Vec<Visit>,
}

/// Lays out every struct and enum together, resolving their mutual
/// dependencies in a single topological walk
struct LayoutEngine<'s> {
    structs: &'s IndexVec<StructId, Struct>,
    enums: &'s IndexVec<EnumId, Enum>,
    arrays: &'s IndexVec<ArrayId, ArrayType>,
    struct_layouts: Vec<Option<StructLayout>>,
    enum_layouts: Vec<Option<EnumLayout>>,
    struct_states: Vec<Visit>,
    enum_states: Vec<Visit>,
}

#[derive(Clone)]
struct StructLayout {
    summary: Layout,
    /// field offsets in source declaration order
    // TODO: this can probably be a slice so it could be copy
    offsets: Vec<u32>,
}

#[derive(Clone, Copy)]
struct EnumLayout {
    summary: Layout,
    payload_offset: u32,
}

#[derive(Clone, Copy)]
struct PendingField {
    typ: Type,
    declared_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Visit {
    Unvisited,
    Visiting,
    Visited,
}

/// Lower every struct in `declarations`, writing the result into `lowered`
/// at the matching index
///
/// Resolves field types and recurses on by-value struct fields so the dependency graph is laid out in
/// topological order. Fails on cycles or duplicate fields, unless a recovery `sink`
/// is given, then offending fields are poisoned (or skipped) and every struct still lowers,
/// keeping the id <-> index relation dense
pub(in crate::hir) fn lower_structs<'h>(
    declarations: &[(SymbolId, &statement::Struct<'h>)],
    map: &Structs,
    enum_map: &Enums,
    arrays: &ArrayTable,
    symbols: &mut SymbolTable,
    lowered: &mut [Option<Struct>],
    mut sink: Option<&mut Diagnostics>,
) -> Result<(), HirError<'h>> {
    for (_, declaration) in declarations {
        for field in &declaration.fields {
            symbols.insert(field.name);
        }
    }

    let mut lowering = Lowering {
        declarations,
        map,
        enum_map,
        arrays,
        symbols,
        states: vec![Visit::Unvisited; declarations.len()],
    };

    for id in 0..declarations.len() {
        lowering.lower_struct(id, lowered, sink.as_deref_mut())?;
    }

    Ok(())
}

/// compute and cache the byte layout of every nominal type
pub(in crate::hir) fn compute_layouts(
    structs: &mut IndexVec<StructId, Struct>,
    enums: &mut IndexVec<EnumId, Enum>,
    arrays: &IndexVec<ArrayId, ArrayType>,
) {
    let (struct_layouts, enum_layouts) = LayoutEngine::new(structs, enums, arrays).compute();

    for (definition, computed) in structs.iter_mut().zip(struct_layouts) {
        definition.layout = computed.summary;
        for (field, offset) in definition.fields.iter_mut().zip(computed.offsets) {
            field.offset = offset;
        }
    }

    for (definition, computed) in enums.iter_mut().zip(enum_layouts) {
        definition.layout = computed.summary;
        definition.payload_offset = computed.payload_offset;
    }
}

/// size and alignment of any runtime type, reading the cached nominal layouts
pub fn type_layout(
    typ: Type,
    structs: &IndexVec<StructId, Struct>,
    enums: &IndexVec<EnumId, Enum>,
    arrays: &IndexVec<ArrayId, ArrayType>,
) -> (u32, u32) {
    match scalar_layout(typ) {
        Some(layout) => layout,
        None => match typ.kind() {
            TypeKind::Struct(id) => structs[id].layout.into(),
            TypeKind::Enum(id) => enums[id].layout.into(),
            TypeKind::Array(id) => {
                let array = arrays[id];
                let (size, align) = type_layout(array.element, structs, enums, arrays);
                (size * array.len, align)
            },
            TypeKind::SelfType | TypeKind::GenericParam(_) | TypeKind::Error => (0, 1),
            _ => unreachable!("type has no runtime layout"),
        },
    }
}

/// the field named `field` of `origin`, following a single reference
pub(crate) fn struct_field(
    origin: Type,
    field: SymbolId,
    structs: &IndexVec<StructId, Struct>,
) -> &StructField {
    let id = match origin.kind() {
        TypeKind::Struct(id) => id,
        TypeKind::Ref { to, .. } => match to.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!("field projection on non-struct"),
        },
        _ => unreachable!("field projection on non-struct"),
    };

    structs[id]
        .fields
        .iter()
        .find(|candidate| candidate.name == field)
        .expect("field must exist after HIR validation")
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
        TypeKind::Str | TypeKind::Slice { .. } => Some((16, 8)),
        TypeKind::String => Some((24, 8)),
        TypeKind::Unit | TypeKind::Never => Some((0, 1)),
        _ => None,
    }
}

#[inline(always)]
const fn align_to(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}

impl<'a, 'h> Lowering<'a, 'h> {
    fn lower_struct(
        &mut self,
        id: usize,
        lowered: &mut [Option<Struct>],
        mut sink: Option<&mut Diagnostics>,
    ) -> Result<(), HirError<'h>> {
        match self.states[id] {
            Visit::Visited => return Ok(()),
            Visit::Visiting => {
                let (_, declaration) = self.declarations[id];
                return Err(hir_error!(
                    declaration.span,
                    CircularStruct { name: declaration.name }
                ));
            },
            Visit::Unvisited => {},
        }

        self.states[id] = Visit::Visiting;
        let (name, declaration) = self.declarations[id];
        let mut seen = HashSet::new();
        let mut fields = Vec::with_capacity(declaration.fields.len());

        for field in &declaration.fields {
            let field_symbol = self.symbols.get_id(field.name).unwrap();
            if !seen.insert(field_symbol) {
                let error = hir_error!(field.span, DuplicateField { name: field.name });
                match sink.as_deref_mut() {
                    Some(sink) => {
                        sink.emit(error.into());
                        continue;
                    },
                    None => return Err(error),
                }
            }

            let ctx =
                type_resolver::ResolveCtx::root(self.symbols, self.map, self.enum_map, self.arrays);
            let mut typ =
                match type_resolver::resolve_annotation(&ctx, &field.typ.value(), field.typ.span())
                {
                    Ok(typ) => typ,
                    Err(error) => match sink.as_deref_mut() {
                        Some(sink) => Type::error(sink.emit(error.into())),
                        None => return Err(error),
                    },
                };

            if let TypeKind::Struct(dep) = typ.kind()
                && let Err(error) = self.lower_struct(dep.0 as usize, lowered, sink.as_deref_mut())
            {
                // poisoning the back-edge field breaks the by-value cycle, so the
                // layout engine never recurses through it
                match sink.as_deref_mut() {
                    Some(sink) => typ = Type::error(sink.emit(error.into())),
                    None => return Err(error),
                }
            }

            fields.push(StructField { name: field_symbol, typ, offset: 0 });
        }

        let repr = StructRepr { kind: declaration.repr.kind, align: declaration.repr.align };
        let decl_span = declaration.span;
        lowered[id] = Some(Struct {
            id: StructId(id as u32),
            name,
            decl_span,
            fields,
            repr,
            layout: Layout::default(),
            generics: Vec::new(),
        });
        self.states[id] = Visit::Visited;

        Ok(())
    }
}

impl<'s> LayoutEngine<'s> {
    fn new(
        structs: &'s IndexVec<StructId, Struct>,
        enums: &'s IndexVec<EnumId, Enum>,
        arrays: &'s IndexVec<ArrayId, ArrayType>,
    ) -> Self {
        Self {
            structs,
            enums,
            arrays,
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
            let enum_id = self.enums[id].id;
            self.compute_enum(enum_id);
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
            if let Some(payload) = variant.payload {
                let (size, align) = self.layout_of(payload);
                max_payload_size = max_payload_size.max(size);
                max_payload_align = max_payload_align.max(align);
                contains_float |= self.contains_float(payload);
            }
        }

        let alignment = tag_align.max(max_payload_align);
        let payload_offset = align_to(tag_size, max_payload_align);
        let size = align_to(payload_offset + max_payload_size, alignment);

        self.enum_layouts[idx] = Some(EnumLayout {
            summary: Layout::new(size, alignment, contains_float),
            payload_offset,
        });
        self.enum_states[idx] = Visit::Visited;
    }

    fn layout_struct(&mut self, definition: &Struct) -> StructLayout {
        let mut fields: Vec<_> = definition
            .fields
            .iter()
            .enumerate()
            .map(|(declared_index, field)| PendingField { typ: field.typ, declared_index })
            .collect();

        self.order_fields(&mut fields, definition.repr);
        self.assign_offsets(fields, definition.repr)
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

    fn assign_offsets(&mut self, fields: Vec<PendingField>, repr: StructRepr) -> StructLayout {
        let mut offset = 0;
        let mut struct_align = 1;
        let mut contains_float = false;
        let mut offsets = vec![0u32; fields.len()];

        for field in fields {
            let (size, align) = self.field_layout(field.typ, repr);
            struct_align = struct_align.max(align);
            contains_float |= self.contains_float(field.typ);
            offset = align_to(offset, align);
            offsets[field.declared_index] = offset;
            offset += size;
        }

        if repr.kind != StructReprKind::Packed
            && let Some(align) = repr.align
        {
            struct_align = struct_align.max(align.get());
        }

        let size = align_to(offset, struct_align);
        StructLayout {
            summary: Layout::new(size, struct_align, contains_float),
            offsets,
        }
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
                        .summary
                        .into()
                },
                TypeKind::Enum(id) => {
                    self.compute_enum(id);
                    self.enum_layouts[id.id() as usize]
                        .as_ref()
                        .expect("dependent enum layout must be computed")
                        .summary
                        .into()
                },
                TypeKind::Array(id) => {
                    let array = self.arrays[id];
                    let (size, align) = self.layout_of(array.element);
                    (size * array.len, align)
                },
                TypeKind::SelfType | TypeKind::GenericParam(_) | TypeKind::Error => (0, 1),
                _ => unreachable!("type has no runtime layout"),
            },
        }
    }

    fn contains_float(&mut self, typ: Type) -> bool {
        match typ.kind() {
            TypeKind::F32 | TypeKind::F64 => true,
            TypeKind::Array(id) => self.contains_float(self.arrays[id].element),
            TypeKind::Struct(id) => {
                self.compute_struct(id);
                self.struct_layouts[id.0 as usize]
                    .as_ref()
                    .expect("dependent struct layout must be computed")
                    .summary
                    .contains_float()
            },
            TypeKind::Enum(id) => {
                self.compute_enum(id);
                self.enum_layouts[id.id() as usize]
                    .as_ref()
                    .expect("dependent enum layout must be computed")
                    .summary
                    .contains_float()
            },
            _ => false,
        }
    }
}
