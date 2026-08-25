use crate::mir::{self, Function};
use frontend::hir::{self, IndexVec, TyInterner, Type, TypeKind};
use std::collections::HashMap;

pub(in crate::mir::lower) struct CollectedLayouts<'hir> {
    pub adts: mir::Layouts<'hir>,
    pub arrays: Vec<mir::Layout>,
}

pub(in crate::mir::lower) struct LayoutCollector<'a, 'hir> {
    types: &'a TyInterner<'hir>,
    adts: &'a IndexVec<hir::AdtId, hir::AdtDef<'hir>>,
    arrays: &'a hir::ArrayTable<'hir>,
    layouts: HashMap<Type<'hir>, mir::Layout>,
}

impl<'a, 'hir> LayoutCollector<'a, 'hir> {
    pub(super) fn new(
        types: &'a TyInterner<'hir>,
        adts: &'a IndexVec<hir::AdtId, hir::AdtDef<'hir>>,
        arrays: &'a hir::ArrayTable<'hir>,
    ) -> Self {
        Self { types, adts, arrays, layouts: HashMap::new() }
    }

    pub(super) fn collect(
        mut self,
        functions: &[Function<'hir>],
        statics: &IndexVec<hir::StaticId, hir::Static<'hir>>,
    ) -> CollectedLayouts<'hir> {
        for function in functions {
            self.collect_type(function.return_type);
            for &(_, typ) in &function.locals {
                self.collect_type(typ);
            }
        }
        for item in statics {
            self.collect_type(item.typ);
        }

        let arrays = self
            .arrays
            .snapshot()
            .iter()
            .map(|array| {
                let Self { types, adts, arrays, .. } = self;
                let (size, align) = hir::type_layout(array.element, types, adts, arrays);
                let contains_float = hir::type_contains_float(array.element, types, adts, arrays);
                mir::Layout::new(size * array.len, align, contains_float)
            })
            .collect();

        CollectedLayouts { adts: self.layouts, arrays }
    }

    fn collect_type(&mut self, typ: Type<'hir>) {
        match typ.kind() {
            TypeKind::Adt(id, args) => {
                if self.layouts.contains_key(&typ) {
                    return;
                }
                let Self { types, arrays, adts, .. } = self;
                let (size, align) = hir::type_layout(typ, types, adts, arrays);
                let contains_float = hir::type_contains_float(typ, types, adts, arrays);
                self.layouts.insert(typ, mir::Layout::new(size, align, contains_float));

                match &self.adts[id].kind {
                    hir::AdtKind::Struct { fields, .. } => {
                        for field in fields {
                            self.collect_type(field.typ.subst(self.types, self.arrays, args));
                        }
                    },
                    hir::AdtKind::Enum { variants, .. } => {
                        for payload in variants.iter().filter_map(|variant| variant.payload) {
                            self.collect_type(payload.subst(self.types, self.arrays, args));
                        }
                    },
                }
            },
            TypeKind::Array(id) => self.collect_type(self.arrays.get(id).element),
            TypeKind::Ref { to, .. } | TypeKind::Raw { to, .. } => self.collect_type(to),
            TypeKind::Slice { element, .. } => self.collect_type(element),
            _ => {},
        }
    }
}
