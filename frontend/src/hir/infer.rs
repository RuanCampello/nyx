use crate::hir::ty::{EnumRepr, TyInterner, Type};

/// Union-find table of integer inference variables for a single body
///
/// Each variable arises from an un-annotated integer literal and is unified
/// with a concrete integral type as its uses are lowered
/// Variables that are never constrained default to `i32` at resolution time
#[derive(Debug, Default)]
pub(in crate::hir) struct InferTable<'hir> {
    vars: Vec<IntVar<'hir>>,
}

#[derive(Debug, Clone, Copy)]
struct IntVar<'hir> {
    parent: u32,
    /// The concrete integral type of this class, set once unified
    /// Only meaningful on a class root
    value: Option<Type<'hir>>,
}

impl<'hir> InferTable<'hir> {
    #[inline]
    pub(crate) fn fresh(&mut self, types: &TyInterner<'hir>) -> Type<'hir> {
        let id = self.vars.len() as u32;
        self.vars.push(IntVar { parent: id, value: None });
        types.infer(id)
    }

    #[inline]
    pub(crate) fn resolve_shallow(&mut self, ty: Type<'hir>) -> Type<'hir> {
        match ty.infer_var() {
            Some(vid) => self.value_of(vid).unwrap_or(ty),
            None => ty,
        }
    }

    #[inline]
    pub(crate) fn resolve_or_default(&mut self, ty: Type<'hir>) -> Type<'hir> {
        let resolved = self.resolve_shallow(ty);
        match resolved.is_infer() {
            true => EnumRepr::I32.typ(),
            false => resolved,
        }
    }

    /// unifies two types, constraining any inference variable involved
    pub(crate) fn unify(&mut self, a: Type<'hir>, b: Type<'hir>) -> Result<(), ()> {
        match (a.infer_var(), b.infer_var()) {
            (Some(va), Some(vb)) => self.union(va, vb),
            (Some(va), None) => self.constrain(va, b),
            (None, Some(vb)) => self.constrain(vb, a),
            (None, None) => (a == b).then_some(()).ok_or(()),
        }
    }

    fn value_of(&mut self, vid: u32) -> Option<Type<'hir>> {
        let root = self.root(vid);
        self.vars[root as usize].value
    }

    fn root(&mut self, vid: u32) -> u32 {
        let mut current = vid;
        while self.vars[current as usize].parent != current {
            let grandparent = self.vars[self.vars[current as usize].parent as usize].parent;
            self.vars[current as usize].parent = grandparent;
            current = grandparent;
        }
        current
    }

    /// pins a variable's class to a concrete type
    ///
    /// a divergent type leaves the class open, an integral type sets it, anything else conflicts
    fn constrain(&mut self, vid: u32, concrete: Type<'hir>) -> Result<(), ()> {
        if concrete.diverges() {
            return Ok(());
        }
        if !concrete.is_integer() {
            return Err(());
        }

        let root = self.root(vid);
        match self.vars[root as usize].value {
            Some(existing) if existing != concrete => Err(()),
            value => {
                if value.is_none() {
                    self.vars[root as usize].value = Some(concrete);
                }
                Ok(())
            },
        }
    }

    fn union(&mut self, a: u32, b: u32) -> Result<(), ()> {
        let (ra, rb) = (self.root(a), self.root(b));
        if ra == rb {
            return Ok(());
        }

        match (self.vars[ra as usize].value, self.vars[rb as usize].value) {
            (Some(x), Some(y)) if x != y => Err(()),
            (Some(_), _) => {
                self.vars[rb as usize].parent = ra;
                Ok(())
            },
            _ => {
                self.vars[ra as usize].parent = rb;
                Ok(())
            },
        }
    }
}
