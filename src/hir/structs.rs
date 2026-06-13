//! Struct lowering: AST -> HIR with topological field-type resolution

use crate::hir::{
    self, Struct, StructField, StructId, SymbolId, SymbolTable, Type, TypeKind,
    diagnostics::Diagnostics,
    error::{HirError, hir_error},
    scope::{Enums, Structs},
    type_resolver,
};
use crate::parser::statement::{self, StructRepr};
use std::collections::HashSet;

/// One module's struct batch being lowered in topological field order
struct Lowering<'a, 'h> {
    declarations: &'a [(SymbolId, &'a statement::Struct<'h>)],
    map: &'a Structs,
    enum_map: &'a Enums,
    symbols: &'a SymbolTable,
    states: Vec<Visit>,
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
        symbols,
        states: vec![Visit::Unvisited; declarations.len()],
    };

    for id in 0..declarations.len() {
        lowering.lower_struct(id, lowered, sink.as_deref_mut())?;
    }

    Ok(())
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

            let ctx = type_resolver::ResolveCtx::root(self.symbols, self.map, self.enum_map);
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

            fields.push(StructField { name: field_symbol, typ });
        }

        let repr = StructRepr { kind: declaration.repr.kind, align: declaration.repr.align };
        let decl_span = declaration.span;
        lowered[id] = Some(Struct {
            id: StructId(id as u32),
            name,
            decl_span,
            docs: hir::join_docs(&declaration.docs),
            fields,
            repr,
            generics: Vec::new(),
        });
        self.states[id] = Visit::Visited;

        Ok(())
    }
}
