use crate::hir::{
    collect::ArrayTable,
    ty::{TyInterner, Type, TypeKind},
};

impl<'hir> Type<'hir> {
    pub fn subst(
        self,
        types: &TyInterner<'hir>,
        arrays: &ArrayTable<'hir>,
        args: &[Type<'hir>],
    ) -> Self {
        match self.kind() {
            TypeKind::GenericParam(index) => args.get(index as usize).copied().unwrap_or(self),
            TypeKind::Ref { mutable, to } => types.refer(to.subst(types, arrays, args), mutable),
            TypeKind::Raw { mutable, to } => types.raw(to.subst(types, arrays, args), mutable),
            TypeKind::Slice { mutable, element } => {
                types.slice(element.subst(types, arrays, args), mutable)
            },
            TypeKind::Adt(id, current) => {
                let current: Vec<_> =
                    current.iter().map(|typ| typ.subst(types, arrays, args)).collect();
                types.adt(id, &current)
            },
            TypeKind::Array(id) => {
                let array = arrays.get(id);
                let element = array.element.subst(types, arrays, args);
                types.array(arrays.intern(element, array.len))
            },
            _ => self,
        }
    }
}
