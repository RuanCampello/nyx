//! Match exhaustiveness checking
//!
//! Everything reduces to **usefulness**: a pattern is useful against a matrix of
//! patterns when some value matches it and none of the rows, so a match is
//! exhaustive exactly when a bare wildcard is *not* useful against its arms, and
//! the values making it useful are the witness. Patterns are modelled as a
//! constructor applied to fixed fields, and checking recurses on columns:
//! the matrix is *specialised* by each constructor the scrutinee admits,
//! dropping rows whose head cannot produce it. Domains too large to list are
//! split rather than enumerated, and anything unenumerable counts as covered
//! only by a wildcard, so the answer is never exhaustive when it should not be.
//!
//! The algorithm is Luc Maranget's, *Warnings for pattern matching* 2007 [paper],
//! written in the shape rustc gives it in [rustc_pattern_analysis], which is
//! what this was built from
//!
//! [paper]: (https://cambium.inria.fr/~maranget/papers/warn/warn.pdf)
//! [rustc_pattern_analysis]: (https://doc.rust-lang.org/nightly/nightly-rustc/rustc_pattern_analysis/usefulness/index.html)

use crate::hir::def::AdtKind;
use crate::hir::ids::IndexVec;
use crate::hir::{
    AdtDef, AdtId, Literal, Pattern, PatternKind, SymbolId, SymbolTable, Type, TypeKind,
};
use std::fmt::Write;

/// Everything the checker needs to look through a type
pub(in crate::hir) struct Context<'a, 'hir> {
    pub adts: &'a IndexVec<AdtId, AdtDef<'hir>>,
    pub symbols: &'a SymbolTable,
}

/// The patterns of one arm, and whether a guard stands between it and its body
pub(in crate::hir) struct Row<'a, 'hir> {
    pub pattern: &'a Pattern<'hir>,
    pub guarded: bool,
}

/// One counterexample: a value the arms leave unmatched
pub struct Witness(pub String);

/// A pattern rewritten into constructor-and-fields form
#[derive(Debug, Clone)]
struct Patt<'hir> {
    typ: Type<'hir>,
    kind: PattKind<'hir>,
}

/// A pattern's head constructor, and the unit the column split works in
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Constructor {
    /// The single constructor of a struct, whose fields are its own
    Single,
    /// One variant of an enum, by its index in the declaration
    Variant(usize),
    /// A closed interval over the integer encoding of a scalar
    Range { lo: i128, hi: i128 },
    /// A domain the checker cannot enumerate, so only a wildcard covers it
    Opaque,
}

#[derive(Debug, Clone)]
enum PattKind<'hir> {
    Wildcard,
    Ctor { ctor: Constructor, fields: Vec<Patt<'hir>> },
    Or(Vec<Patt<'hir>>),
}

impl<'a, 'hir> Context<'a, 'hir> {
    /// values of `scrutinee` that no arm matches, at most `limit` of them
    ///
    /// An empty result means the match is exhaustive
    /// A guarded arm is left out of the matrix: its guard can fail at runtime, so it covers nothing on its own
    pub fn missing_patterns(
        &self,
        scrutinee: Type<'hir>,
        arms: &[Row<'_, 'hir>],
        limit: usize,
    ) -> Vec<Witness> {
        let scrutinee = peel(scrutinee);
        if !is_checkable(scrutinee) {
            return Vec::new();
        }

        let matrix: Vec<_> = arms
            .iter()
            .filter(|arm| !arm.guarded)
            .map(|arm| vec![self.lower(scrutinee, arm.pattern)])
            .collect();
        let query = vec![Patt { typ: scrutinee, kind: PattKind::Wildcard }];

        self.usefulness(&matrix, &query)
            .into_iter()
            .take(limit)
            .map(|witness| {
                let mut rendered = String::new();
                render(self, witness.first().unwrap_or(&query[0]), &mut rendered);

                Witness(rendered)
            })
            .collect()
    }

    fn lower(&self, typ: Type<'hir>, pattern: &Pattern<'hir>) -> Patt<'hir> {
        let typ = peel(typ);
        let kind = match pattern.kind {
            // a binding tests nothing, it only names what it matched
            PatternKind::Wildcard | PatternKind::Binding(_) => PattKind::Wildcard,
            PatternKind::Bind { sub, .. } => return self.lower(typ, sub),
            PatternKind::Or(alternatives) => PattKind::Or(
                alternatives.iter().map(|alternative| self.lower(typ, alternative)).collect(),
            ),
            PatternKind::Literal(literal) => match encode(literal) {
                Some(value) => PattKind::Ctor {
                    ctor: Constructor::Range { lo: value, hi: value },
                    fields: Vec::new(),
                },
                None => PattKind::Ctor { ctor: Constructor::Opaque, fields: Vec::new() },
            },
            PatternKind::Range { start, end, inclusive } => {
                match (encode(start), encode(end)) {
                    (Some(lo), Some(hi)) => {
                        let hi = match inclusive {
                            true => hi,
                            _ => hi - 1,
                        };

                        match lo <= hi {
                            true => PattKind::Ctor {
                                ctor: Constructor::Range { lo, hi },
                                fields: Vec::new(),
                            },
                            // an empty range matches nothing at all
                            _ => PattKind::Ctor { ctor: Constructor::Opaque, fields: Vec::new() },
                        }
                    },
                    _ => PattKind::Ctor { ctor: Constructor::Opaque, fields: Vec::new() },
                }
            },
            PatternKind::Variant { id, variant_idx, sub } => {
                let fields = match (sub, self.payload_type(id, variant_idx)) {
                    (Some(sub), Some(payload)) => vec![self.lower(payload, sub)],
                    _ => Vec::new(),
                };

                PattKind::Ctor { ctor: Constructor::Variant(variant_idx), fields }
            },
            PatternKind::Struct { id, fields } => {
                let declared = self.struct_fields(id);
                let lowered = declared
                    .iter()
                    .map(|(name, field_type)| {
                        match fields.iter().find(|(bound, _)| bound == name) {
                            Some((_, pattern)) => self.lower(*field_type, pattern),
                            // a field the pattern leaves out matches anything
                            None => Patt { typ: *field_type, kind: PattKind::Wildcard },
                        }
                    })
                    .collect();

                PattKind::Ctor { ctor: Constructor::Single, fields: lowered }
            },
        };

        Patt { typ, kind }
    }

    /// the values matching `query` that no row of `matrix` matches
    ///
    /// an empty result means `query` is redundant against the matrix
    /// for a wildcard query that is exactly what exhaustiveness means
    fn usefulness(&self, matrix: &[Vec<Patt<'hir>>], query: &[Patt<'hir>]) -> Vec<Vec<Patt<'hir>>> {
        let Some(head) = query.first() else {
            // no columns left: the query is useful only if nothing matched it
            return match matrix.is_empty() {
                true => vec![Vec::new()],
                false => Vec::new(),
            };
        };

        let matrix = expand(matrix);
        let queries = expand(std::slice::from_ref(&query.to_vec()));

        let column: Vec<_> = matrix
            .iter()
            .filter_map(|row| match row.first().map(|pat| &pat.kind) {
                Some(PattKind::Ctor { ctor, .. }) => Some(*ctor),
                _ => None,
            })
            .collect();

        let mut witnesses = Vec::new();

        for ctor in self.split(head.typ, &column) {
            for query in &queries {
                let Some(specialised_query) = self.specialise(ctor, query) else {
                    continue;
                };

                let specialised: Vec<_> =
                    matrix.iter().filter_map(|row| self.specialise(ctor, row)).collect();

                for witness in self.usefulness(&specialised, &specialised_query) {
                    witnesses.push(self.unspecialise(head.typ, ctor, witness));
                }
            }
        }

        witnesses
    }

    /// rebuilds the constructor a specialisation peeled off, so a witness found
    /// deep in the recursion comes back out as a whole pattern
    fn unspecialise(
        &self,
        typ: Type<'hir>,
        ctor: Constructor,
        witness: Vec<Patt<'hir>>,
    ) -> Vec<Patt<'hir>> {
        let arity = self.fields_of(typ, ctor).len();
        let mut witness = witness;
        let rest = witness.split_off(arity.min(witness.len()));

        let mut rebuilt = vec![Patt { typ, kind: PattKind::Ctor { ctor, fields: witness } }];
        rebuilt.extend(rest);

        rebuilt
    }

    /// how many fields `ctor` carries for a value of `typ`, and of what type
    fn fields_of(&self, typ: Type<'hir>, ctor: Constructor) -> Vec<Type<'hir>> {
        match ctor {
            Constructor::Variant(index) => match typ.kind() {
                TypeKind::Adt(id, _) => {
                    self.payload_type(id, index).map(peel).into_iter().collect()
                },
                _ => Vec::new(),
            },
            Constructor::Single => match typ.kind() {
                TypeKind::Adt(id, _) => {
                    self.struct_fields(id).into_iter().map(|(_, typ)| peel(typ)).collect()
                },
                _ => Vec::new(),
            },
            Constructor::Range { .. } | Constructor::Opaque => Vec::new(),
        }
    }

    /// the constructors a value of `typ` can have, cut so that none of them
    /// straddles a boundary drawn by the patterns already in the column
    fn split(&self, typ: Type<'hir>, column: &[Constructor]) -> Vec<Constructor> {
        if let TypeKind::Adt(id, _) = typ.kind() {
            return match &self.adts[id].kind {
                // every variant is offered, so a missing one becomes the witness
                AdtKind::Enum { variants, .. } => {
                    (0..variants.len()).map(Constructor::Variant).collect()
                },
                AdtKind::Struct { .. } => vec![Constructor::Single],
            };
        }

        match domain(typ) {
            Some((lo, hi)) => split_ranges(lo, hi, column),
            _ => vec![Constructor::Opaque],
        }
    }

    /// replaces the head of `row` with the fields `ctor` exposes, or drops the row
    fn specialise(&self, ctor: Constructor, row: &[Patt<'hir>]) -> Option<Vec<Patt<'hir>>> {
        let (head, rest) = row.split_first()?;

        let mut specialised = match &head.kind {
            PattKind::Wildcard => self
                .fields_of(head.typ, ctor)
                .into_iter()
                .map(|typ| Patt { typ, kind: PattKind::Wildcard })
                .collect(),

            PattKind::Ctor { ctor: head_ctor, fields } => match covers(*head_ctor, ctor) {
                true => fields.clone(),
                false => return None,
            },

            // rows are expanded before they reach here
            PattKind::Or(_) => {
                unreachable!("an or-pattern is expanded into one row per alternative")
            },
        };

        specialised.extend_from_slice(rest);

        Some(specialised)
    }

    fn payload_type(&self, id: AdtId, variant_idx: usize) -> Option<Type<'hir>> {
        match &self.adts[id].kind {
            AdtKind::Enum { variants, .. } => variants.get(variant_idx).and_then(|v| v.payload),
            AdtKind::Struct { .. } => None,
        }
    }

    fn struct_fields(&self, id: AdtId) -> Vec<(SymbolId, Type<'hir>)> {
        match &self.adts[id].kind {
            AdtKind::Struct { fields, .. } => {
                fields.iter().map(|field| (field.name, field.typ)).collect()
            },
            AdtKind::Enum { .. } => Vec::new(),
        }
    }
}

/// whether the scrutinee is concrete enough to reason about
#[inline(always)]
const fn is_checkable(typ: Type<'_>) -> bool {
    !matches!(
        typ.kind(),
        TypeKind::GenericParam(_) | TypeKind::Infer(_) | TypeKind::Error | TypeKind::SelfType
    )
}

/// a match looks straight through a reference to the value behind it, so the
/// constructors that matter are the pointee's
fn peel(typ: Type<'_>) -> Type<'_> {
    let mut typ = typ;

    while let TypeKind::Ref { to, .. } | TypeKind::Raw { to, .. } = typ.kind() {
        typ = to;
    }

    typ
}

/// the integer a scalar literal stands for, when it has one
#[inline(always)]
const fn encode(literal: Literal) -> Option<i128> {
    match literal {
        Literal::Int(value) => Some(value as i128),
        Literal::Bool(value) => Some(value as i128),
        Literal::Char(value) => Some(value as i128),
        // floats have no successor, so no interval split can enumerate them
        Literal::Float(_) | Literal::Unit | Literal::Str(_) => None,
    }
}

/// the closed interval a scalar type ranges over
#[inline]
const fn domain(typ: Type<'_>) -> Option<(i128, i128)> {
    let bounds = match typ.kind() {
        TypeKind::Bool => (0, 1),
        TypeKind::Char => (0, 0x0010_FFFF),
        TypeKind::I8 => (i8::MIN as i128, i8::MAX as i128),
        TypeKind::U8 => (0, u8::MAX as i128),
        TypeKind::I16 => (i16::MIN as i128, i16::MAX as i128),
        TypeKind::U16 => (0, u16::MAX as i128),
        TypeKind::I32 => (i32::MIN as i128, i32::MAX as i128),
        TypeKind::U32 => (0, u32::MAX as i128),
        TypeKind::I64 | TypeKind::Iptr => (i64::MIN as i128, i64::MAX as i128),
        TypeKind::U64 | TypeKind::Uptr => (0, u64::MAX as i128),
        _ => return None,
    };

    Some(bounds)
}

/// cuts `lo..=hi` at every boundary the column mentions
///
/// the result is disjoint, covers the whole domain, and no interval in it is
/// partly inside and partly outside a pattern already present, which is what
/// lets a single interval stand for every value it holds
fn split_ranges(lo: i128, hi: i128, column: &[Constructor]) -> Vec<Constructor> {
    let mut boundaries = vec![lo];

    for ctor in column {
        let Constructor::Range { lo: start, hi: end } = *ctor else {
            continue;
        };

        if start > lo && start <= hi {
            boundaries.push(start);
        }
        if end >= lo && end < hi {
            boundaries.push(end + 1);
        }
    }

    boundaries.sort_unstable();
    boundaries.dedup();

    let mut split = Vec::with_capacity(boundaries.len());
    for (index, &start) in boundaries.iter().enumerate() {
        let end = match boundaries.get(index + 1) {
            Some(&next) => next - 1,
            _ => hi,
        };

        split.push(Constructor::Range { lo: start, hi: end });
    }

    split
}

/// whether a row headed by `head` survives specialisation by `ctor`
#[inline]
const fn covers(head: Constructor, ctor: Constructor) -> bool {
    use Constructor::*;
    match (head, ctor) {
        (Single, Single) => true,
        (Variant(left), Variant(right)) => left == right,
        // the split guarantees `ctor` lies wholly inside or wholly outside
        (Range { lo, hi }, Range { lo: start, hi: end }) => lo <= start && end <= hi,
        // nothing is known to cover an unenumerable domain
        _ => false,
    }
}

/// one row per alternative, so the recursion never meets an or-pattern head
fn expand<'hir>(rows: &[Vec<Patt<'hir>>]) -> Vec<Vec<Patt<'hir>>> {
    let mut expanded = Vec::with_capacity(rows.len());

    for row in rows {
        match row.split_first() {
            Some((Patt { kind: PattKind::Or(alternatives), typ }, rest)) => {
                for alternative in alternatives {
                    let mut replaced = vec![Patt { typ: *typ, kind: alternative.kind.clone() }];
                    replaced.extend_from_slice(rest);

                    expanded.extend(expand(&[replaced]));
                }
            },
            _ => expanded.push(row.clone()),
        }
    }

    expanded
}

fn render(context: &Context<'_, '_>, pat: &Patt<'_>, out: &mut String) {
    let PattKind::Ctor { ctor, fields } = &pat.kind else {
        out.push('_');
        return;
    };

    match *ctor {
        Constructor::Opaque => out.push('_'),
        Constructor::Single => match pat.typ.kind() {
            TypeKind::Adt(id, _) => {
                let _ = write!(out, "{} {{ .. }}", context.symbols.get(context.adts[id].name));
            },
            _ => out.push('_'),
        },
        Constructor::Variant(index) => {
            let TypeKind::Adt(id, _) = pat.typ.kind() else {
                out.push('_');
                return;
            };

            let definition = &context.adts[id];
            let AdtKind::Enum { variants, .. } = &definition.kind else {
                out.push('_');
                return;
            };

            let name = variants
                .get(index)
                .map(|variant| context.symbols.get(variant.name))
                .unwrap_or("_");
            let _ = write!(out, "{}::{}", context.symbols.get(definition.name), name);

            if let Some(payload) = fields.first() {
                out.push('(');
                render(context, payload, out);
                out.push(')');
            }
        },

        Constructor::Range { lo, hi } => render_range(pat.typ, lo, hi, out),
    }
}

fn render_range(typ: Type<'_>, lo: i128, hi: i128, out: &mut String) {
    let scalar = |value: i128, out: &mut String| match typ.kind() {
        TypeKind::Bool => out.push_str(match value {
            0 => "false",
            _ => "true",
        }),
        TypeKind::Char => match u32::try_from(value).ok().and_then(char::from_u32) {
            Some(character) => {
                let _ = write!(out, "{character:?}");
            },
            _ => out.push('_'),
        },
        _ => {
            let _ = write!(out, "{value}");
        },
    };

    scalar(lo, out);

    if lo != hi {
        out.push_str("..=");
        scalar(hi, out);
    }
}
