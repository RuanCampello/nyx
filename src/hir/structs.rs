//! Struct lowering: AST → HIR with topological field-type resolution.
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
use crate::parser::statement::{self, StructRepr};
use std::collections::HashSet;

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

    for field in &declaration.fields {
        let field_symbol = symbols.get_id(field.name).unwrap();
        if !seen.insert(field_symbol) {
            return Err(hir_error!(field.span, DuplicateField { name: field.name.into() }));
        }

        let ctx = type_resolver::ResolveCtx::root(symbols, map, enum_map);
        let typ = type_resolver::resolve_annotation(&ctx, &field.typ.value(), field.typ.span())?;
        if let TypeKind::Struct(dep) = typ.kind() {
            lower_struct(dep.0 as usize, declarations, map, enum_map, symbols, lowered, states)?;
        }

        fields.push(StructField { name: field_symbol, typ });
    }

    let repr = StructRepr { kind: declaration.repr.kind, align: declaration.repr.align };
    lowered[id] = Some(Struct { id: StructId(id as u32), name, fields, repr });
    states[id] = Visit::Visited;

    Ok(())
}
