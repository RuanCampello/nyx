use crate::{
    diagnostic,
    hir::{
        AdtDef, AdtId, AdtKind, EnumRepr, GenericParamDef, Layout, SymbolId, SymbolTable,
        VariantDef,
        collect::{GenericEnv, ItemTable},
        declarations::Declarations,
        error::{HirError, hir_error},
        structs,
    },
    parser::statement,
};
use std::collections::HashSet;

impl<'hir> ItemTable<'hir> {
    pub(super) fn declare_structs<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
    ) -> Result<Vec<(AdtId, &'d statement::Struct<'s>)>, HirError<'hir>>
    where
        's: 'hir,
    {
        let mut structs = Vec::new();

        for struct_decl in &declarations.structs {
            let symbol = self.symbols.insert(struct_decl.name);

            let already_exists = self.adts.struct_map.contains_key(&symbol)
                || self.adts.enum_map.contains_key(&symbol);

            if self.declare_or_error(already_exists, |this| {
                let previous = this.nominal_decl_span(symbol);
                hir_error!(struct_decl.span, DuplicateStruct { name: struct_decl.name, previous })
            }) {
                continue;
            }

            let generics = lower_adt_generics(&mut self.symbols, &struct_decl.generics);

            let id = self.adts.defs.push(AdtDef {
                name: symbol,
                is_pub: struct_decl.is_pub,
                decl_span: struct_decl.span,
                name_span: struct_decl.name_span,
                kind: AdtKind::Struct { fields: Vec::new(), repr: struct_decl.repr },
                layout: Layout::default(),
                generics,
            });
            diagnostic::register_adt_name(id.0, struct_decl.name);

            self.adts.struct_map.insert(symbol, id);
            structs.push((id, *struct_decl));
        }

        Ok(structs)
    }

    pub(super) fn lower_structs<'s>(
        &mut self,
        declarations: &[(AdtId, &statement::Struct<'s>)],
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        if declarations.is_empty() {
            return Ok(());
        }

        let local_declarations: Vec<_> = declarations
            .iter()
            .map(|(_, declaration)| {
                let symbol =
                    self.symbols.get_id(declaration.name).expect("declared struct is interned");
                (symbol, *declaration)
            })
            .collect();
        let mut lowered = vec![None; local_declarations.len()];

        structs::lower_structs(
            &local_declarations,
            &self.adts.struct_map,
            &self.adts.enum_map,
            &self.adts.defs,
            &self.arrays,
            &self.types,
            &self.symbols,
            &mut lowered,
            &mut self.diagnostics.borrow_mut(),
        )?;

        for ((id, _), definition) in declarations.iter().zip(lowered) {
            self.adts.defs[*id] = definition.expect("every struct must be lowered");
        }

        Ok(())
    }

    pub(super) fn declare_enums<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
    ) -> Result<Vec<(AdtId, SymbolId, EnumRepr, &'d statement::Enum<'s>)>, HirError<'hir>>
    where
        's: 'hir,
    {
        let mut enums = Vec::new();
        for enum_decl in &declarations.enums {
            let symbol = self.symbols.insert(enum_decl.name);
            let already_exists = self.adts.enum_map.contains_key(&symbol)
                || self.adts.struct_map.contains_key(&symbol);

            if self.declare_or_error(already_exists, |this| {
                let previous = this.nominal_decl_span(symbol);
                hir_error!(enum_decl.span, DuplicateEnum { name: enum_decl.name, previous })
            }) {
                continue;
            }

            let generics = lower_adt_generics(&mut self.symbols, &enum_decl.generics);

            let repr = match &enum_decl.repr {
                None => EnumRepr::minimal_for(&enum_decl.variants),
                Some(explicit) => match EnumRepr::try_from(explicit.value()) {
                    Ok(repr) => repr,
                    _ => {
                        self.soft(hir_error!(
                            explicit.span(),
                            TypeMismatch {
                                expected: self.types.common.i32,
                                found: self
                                    .types
                                    .from_primitive_ast(&explicit.value())
                                    .unwrap_or(self.types.common.error),
                            }
                        ));

                        continue;
                    },
                },
            };

            let id = self.adts.defs.push(AdtDef {
                name: symbol,
                is_pub: enum_decl.is_pub,
                decl_span: enum_decl.span,
                name_span: enum_decl.name_span,
                kind: AdtKind::Enum { variants: Vec::new(), repr, payload_offset: 0 },
                layout: Layout::default(),
                generics,
            });
            diagnostic::register_adt_name(id.0, enum_decl.name);

            self.adts.enum_map.insert(symbol, id);
            enums.push((id, symbol, repr, *enum_decl));
        }

        Ok(enums)
    }

    pub(super) fn lower_enums<'s>(
        &mut self,
        declarations: &[(AdtId, SymbolId, EnumRepr, &statement::Enum<'s>)],
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        for &(id, symbol, _, enum_decl) in declarations {
            let env: GenericEnv<'hir> = enum_decl
                .generics
                .iter()
                .enumerate()
                .map(|(index, generic)| {
                    (generic.name.to_owned(), self.types.generic_param(index as u8))
                })
                .collect();
            let mut seen = HashSet::new();
            let mut next_value = 0;
            let mut variants = Vec::with_capacity(enum_decl.variants.len());

            for variant in &enum_decl.variants {
                let variant_symbol = self.symbols.insert(variant.name);
                if !seen.insert(variant_symbol) {
                    self.soft(hir_error!(variant.span, DuplicateVariant { name: variant.name }));
                    continue;
                }

                let value = variant.value.unwrap_or(next_value);
                next_value = value + 1;

                let payload = variant.payload.as_ref().map(|typ| {
                    self.resolve_type(typ.value_ref(), typ.span(), None, Some(&env))
                        .unwrap_or_else(|error| self.poison(error))
                });

                self.adts.variants.insert((symbol, variant_symbol), (id, value));
                variants.push(VariantDef {
                    name: variant_symbol,
                    value,
                    payload,
                    name_span: variant.name_span,
                });
            }

            *self.adts.defs[id].variants_mut() = variants;
        }

        Ok(())
    }
}

fn lower_adt_generics(
    symbols: &mut SymbolTable,
    generics: &[statement::GenericBound<'_>],
) -> Vec<GenericParamDef> {
    generics
        .iter()
        .map(|generic| GenericParamDef {
            name: symbols.insert(generic.name),
            bounds: generic
                .bounds
                .iter()
                .filter_map(|bound| match bound.value_ref() {
                    statement::Type::Named(name) | statement::Type::Generic(name, _) => {
                        Some(symbols.insert(name))
                    },
                    _ => None,
                })
                .collect(),
        })
        .collect()
}
