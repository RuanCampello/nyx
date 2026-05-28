//! Struct lowering: AST → HIR with topological field-type resolution and
//! repr-driven memory layout.
//!
//! This is an independent pass: it doesn't touch functions, expressions, or
//! `FunctionBuilder` state. Living in its own module keeps `lower.rs` focused
//! on function-body lowering.

use crate::hir::{
    Struct, StructField, StructId, SymbolId, SymbolTable, TypeKind,
    error::{HirError, hir_error},
    scope::{Enums, Structs},
    type_resolver,
};
use crate::parser::statement::{self, StructRepr, StructReprKind};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hir) enum Visit {
    Unvisited,
    Visiting,
    Visited,
}

/// Lower every struct in `declarations`, writing the result into `lowered`
/// at the matching index
///
/// Resolves field types and recurses on by-value struct fields so the dependency graph is laid out in
/// topological order. Fails on cycles or duplicate fields
pub(in crate::hir) fn lower_structs<'h>(
    declarations: &[(SymbolId, &statement::Struct<'h>)],
    map: &Structs,
    enum_map: &Enums,
    symbols: &mut SymbolTable,
    lowered: &mut [Option<Struct>],
) -> Result<(), HirError<'h>> {
    for (_, declaration) in declarations {
        for field in &declaration.fields {
            symbols.insert(field.name);
        }
    }

    let mut states = vec![Visit::Unvisited; declarations.len()];
    let symbols = &*symbols;
    for id in 0..declarations.len() {
        lower_struct(id, declarations, map, enum_map, symbols, lowered, &mut states)?;
    }

    Ok(())
}

pub(in crate::hir) fn lower_struct<'h>(
    id: usize,
    declarations: &[(SymbolId, &statement::Struct<'h>)],
    map: &Structs,
    enum_map: &Enums,
    symbols: &SymbolTable,
    lowered: &mut [Option<Struct>],
    states: &mut [Visit],
) -> Result<(), HirError<'h>> {
    match states[id] {
        Visit::Visited => return Ok(()),
        Visit::Visiting => {
            let (_, declaration) = declarations[id];
            return Err(hir_error!(
                declaration.span,
                CircularStruct { name: declaration.name.into() }
            ));
        },
        Visit::Unvisited => {},
    }

    states[id] = Visit::Visiting;
    let (name, declaration) = declarations[id];
    let mut seen = HashSet::new();
    let mut fields = Vec::with_capacity(declaration.fields.len());

    for (idx, field) in declaration.fields.iter().enumerate() {
        let field_symbol = symbols.get_id(field.name).unwrap();
        if !seen.insert(field_symbol) {
            return Err(hir_error!(field.span, DuplicateField { name: field.name.into() }));
        }

        let ctx = type_resolver::ResolveCtx::root(symbols, map, enum_map);
        let typ =
            type_resolver::resolve_annotation(&ctx, &field.typ.value(), field.typ.span())?;
        if let TypeKind::Struct(dep) = typ.kind() {
            lower_struct(dep.0 as usize, declarations, map, enum_map, symbols, lowered, states)?;
        }

        fields.push(StructField {
            name: field_symbol,
            typ,
            offset: 0,
            declared_index: idx as u32,
        });
    }

    let repr = StructRepr { kind: declaration.repr.kind, align: declaration.repr.align };
    let (fields, size, align) = layout_fields(fields, lowered, repr);
    lowered[id] = Some(Struct { id: StructId(id as u32), name, fields, size, align, repr });
    states[id] = Visit::Visited;

    Ok(())
}

/// Apply repr-driven field reordering and assign byte offsets.
/// Returns `(fields_in_layout_order, struct_size, struct_align)`.
fn layout_fields(
    mut fields: Vec<StructField>,
    structs: &[Option<Struct>],
    repr: StructRepr,
) -> (Vec<StructField>, u32, u32) {
    // PERFORMANCE: field reordering is a small stable sort by layout class
    match repr.kind {
        StructReprKind::Default => {
            fields.sort_by(|a, b| {
                let (a_size, a_align) = &a.typ.layout(structs);
                let (b_size, b_align) = &b.typ.layout(structs);

                b_align
                    .cmp(&a_align)
                    .then_with(|| b_size.cmp(&a_size))
                    .then_with(|| a.declared_index.cmp(&b.declared_index))
            });
        },
        StructReprKind::Packed => {
            let max_align = repr.align.map(|a| a.get()).unwrap_or(1);
            fields.sort_by(|a, b| {
                let (a_size, mut a_align) = a.typ.layout(structs);
                let (b_size, mut b_align) = b.typ.layout(structs);
                a_align = a_align.min(max_align);
                b_align = b_align.min(max_align);

                b_align
                    .cmp(&a_align)
                    .then_with(|| b_size.cmp(&a_size))
                    .then_with(|| a.declared_index.cmp(&b.declared_index))
            });
        },
        _ => {},
    }

    let mut offset = 0;
    let mut struct_align = 1;

    match repr.kind == StructReprKind::Packed {
        true => {
            let max_align = repr.align.map(|a| a.get()).unwrap_or(1);
            for field in &mut fields {
                let (size, mut align) = field.typ.layout(structs);
                align = align.min(max_align);
                struct_align = struct_align.max(align);
                offset = align_to(offset, align);
                field.offset = offset;
                offset += size;
            }
        },
        _ => {
            for field in &mut fields {
                let (size, align) = field.typ.layout(structs);
                struct_align = struct_align.max(align);
                offset = align_to(offset, align);
                field.offset = offset;
                offset += size;
            }

            if let Some(align) = repr.align {
                struct_align = struct_align.max(align.get());
            }
        },
    };
    let size = align_to(offset, struct_align);

    (fields, size, struct_align)
}

#[inline(always)]
const fn align_to(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}
