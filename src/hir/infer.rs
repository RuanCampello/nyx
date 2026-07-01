use crate::hir::types::{Type, TypeKind};

/// Union-find table of integer inference variables for a single body
///
/// Each variable arises from an un-annotated integer literal and is unified
/// with a concrete integral type as its uses are lowered
/// Variables that are never constrained default to `i32` at resolution time
#[derive(Debug, Default)]
pub(crate) struct InferTable {
    vars: Vec<IntVar>,
}

#[derive(Debug, Clone, Copy)]
struct IntVar {
    parent: u32,
    /// The concrete integral type of this class, set once unified
    /// Only meaningful on a class root
    value: Option<Type>,
}

impl InferTable {
    #[inline]
    pub(crate) fn fresh(&mut self) -> Type {
        let id = self.vars.len() as u32;
        self.vars.push(IntVar { parent: id, value: None });
        Type::infer(id)
    }

    #[inline]
    pub(crate) fn resolve_shallow(&mut self, ty: Type) -> Type {
        match ty.infer_var() {
            Some(vid) => self.value_of(vid).unwrap_or(ty),
            None => ty,
        }
    }

    #[inline]
    pub(crate) fn resolve_or_default(&mut self, ty: Type) -> Type {
        let resolved = self.resolve_shallow(ty);
        match resolved.is_infer() {
            true => Type::new(TypeKind::I32),
            false => resolved,
        }
    }

    /// unifies two types, constraining any inference variable involved
    pub(crate) fn unify(&mut self, a: Type, b: Type) -> Result<(), ()> {
        match (a.infer_var(), b.infer_var()) {
            (Some(va), Some(vb)) => self.union(va, vb),
            (Some(va), None) => self.constrain(va, b),
            (None, Some(vb)) => self.constrain(vb, a),
            (None, None) => (a == b).then_some(()).ok_or(()),
        }
    }

    fn value_of(&mut self, vid: u32) -> Option<Type> {
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
    fn constrain(&mut self, vid: u32, concrete: Type) -> Result<(), ()> {
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
            (None, _) => {
                self.vars[ra as usize].parent = rb;
                Ok(())
            },
        }
    }
}
