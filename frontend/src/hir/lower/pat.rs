use crate::{
    hir::{
        Literal, Pattern, PatternKind, Type, TypeKind,
        error::{HirError, hir_error},
        lower::FunctionBuilder,
        symbols::qualified,
    },
    lexer::token::Span,
    parser::statement::{self, PatternLit},
};
use std::collections::HashSet;

impl<'s, 'f, 'hir, 'src> FunctionBuilder<'s, 'f, 'hir, 'src>
where
    'src: 'hir,
{
    pub(super) fn lower_pattern(
        &mut self,
        scrutinee_type: Type<'hir>,
        pattern: &statement::Pattern<'src>,
        span: Span,
    ) -> Result<Pattern<'hir>, HirError<'hir>> {
        use PatternLit as Lit;
        use statement::Pattern as Patt;

        match pattern {
            Patt::Wildcard => Ok(Pattern { kind: PatternKind::Wildcard, span }),
            Patt::Literal(lit) => {
                let kind = match lit {
                    Lit::Int(n) => PatternKind::Literal(Literal::Int(*n)),
                    Lit::Float(f) => PatternKind::Literal(Literal::Float(*f)),
                    Lit::Bool(b) => PatternKind::Literal(Literal::Bool(*b)),
                    Lit::Char(c) => PatternKind::Literal(Literal::Char(*c)),
                };
                Ok(Pattern { kind, span })
            },

            Patt::Range { start, end, inclusive } => {
                let is_int = scrutinee_type.is_integer()
                    || matches!(scrutinee_type.kind(), TypeKind::Infer(_));

                let endpoints = match (start, end) {
                    (Lit::Int(a), Lit::Int(b)) if is_int => {
                        Some((Literal::Int(*a), Literal::Int(*b), *a, *b))
                    },
                    (Lit::Char(a), Lit::Char(b)) if scrutinee_type.kind() == TypeKind::Char => {
                        Some((Literal::Char(*a), Literal::Char(*b), *a as i64, *b as i64))
                    },
                    _ => None,
                };

                let Some((start, end, low, high)) = endpoints else {
                    return Err(hir_error!(span, InvalidRangeType { typ: scrutinee_type }));
                };

                if low > high || (!inclusive && low == high) {
                    return Err(hir_error!(span, EmptyRange));
                }

                Ok(Pattern {
                    kind: PatternKind::Range { start, end, inclusive: *inclusive },
                    span,
                })
            },

            Patt::Binding { name, sub } => {
                let symbol = self.scope.symbols.insert(name);
                let local = self.declare_local(symbol, scrutinee_type, false, span)?;
                let lowered = self.lower_pattern(scrutinee_type, sub.value_ref(), sub.span())?;
                Ok(Pattern {
                    kind: PatternKind::Bind { local, sub: self.arena.alloc(lowered) },
                    span,
                })
            },

            Patt::Struct { name, fields, rest } => {
                let struct_sym = self.scope.symbols.get_id(name);
                let named_id =
                    struct_sym.and_then(|sym| self.scope.adts.struct_map.get(&sym).copied());

                let (id, generic_args) = match scrutinee_type.kind() {
                    TypeKind::Adt(id, args) if self.scope[id].is_struct() => (id, args),
                    _ => {
                        let found = named_id
                            .map(|id| self.scope.types.adt(id, &[]))
                            .ok_or_else(|| hir_error!(span, UnknownType { name }))?;
                        return Err(hir_error!(
                            span,
                            TypeMismatch { expected: scrutinee_type, found }
                        ));
                    },
                };

                let matches_scrutinee = struct_sym
                    .is_some_and(|sym| self.scope.adts.struct_map.get(&sym).copied() == Some(id));

                if !matches_scrutinee {
                    return match named_id {
                        Some(other) => Err(hir_error!(
                            span,
                            TypeMismatch {
                                expected: scrutinee_type,
                                found: self.scope.types.adt(other, &[]),
                            }
                        )),
                        _ => Err(hir_error!(span, UnknownType { name })),
                    };
                }

                let definition_name = self.scope[id].name;
                let struct_name = self.arena.alloc_str(self.scope.symbols.get(definition_name));

                let mut seen = HashSet::with_capacity(fields.len());
                let mut lowered = Vec::with_capacity(fields.len());

                for field in fields {
                    let field_symbol = self.scope.symbols.insert(field.name);
                    if !seen.insert(field_symbol) {
                        return Err(hir_error!(field.span, DuplicateField { name: field.name }));
                    }

                    let expected =
                        self.scope[id].fields().iter().find(|f| f.name == field_symbol).copied();
                    let Some(expected) = expected else {
                        return Err(hir_error!(
                            field.span,
                            UnknownField { struct_name, field: field.name }
                        ));
                    };

                    let expected_type =
                        expected.typ.subst(&self.scope.types, &self.scope.arrays, generic_args);
                    let sub = match &field.pattern {
                        Some(sub) => {
                            self.lower_pattern(expected_type, sub.value_ref(), sub.span())?
                        },
                        _ => {
                            let local =
                                self.declare_local(field_symbol, expected_type, false, field.span)?;
                            Pattern { kind: PatternKind::Binding(local), span: field.span }
                        },
                    };

                    lowered.push((field_symbol, &*self.arena.alloc(sub)));
                }

                if !rest
                    && let Some(missing) = self.scope[id]
                        .fields()
                        .iter()
                        .find(|f| !seen.contains(&f.name))
                        .map(|f| f.name)
                {
                    return Err(hir_error!(
                        span,
                        MissingField {
                            struct_name,
                            field: self.arena.alloc_str(self.scope.symbols.get(missing)),
                        }
                    ));
                }

                let fields = self.arena.alloc_slice_copy(&lowered);
                Ok(Pattern { kind: PatternKind::Struct { id, fields }, span })
            },

            Patt::Or(alts) => {
                let lowered: Vec<Pattern<'hir>> = alts
                    .iter()
                    .map(|alt| self.lower_pattern(scrutinee_type, alt.value_ref(), alt.span()))
                    .collect::<Result<_, _>>()?;
                let slice = self.arena.alloc_slice_copy(&lowered);
                Ok(Pattern { kind: PatternKind::Or(slice), span })
            },

            Patt::Ident(name) => {
                if let TypeKind::Adt(id, _) = scrutinee_type.kind()
                    && self.scope[id].is_enum()
                {
                    let (id, _) = self.enum_type(scrutinee_type, span)?;
                    let enum_def = &self.scope[id];

                    if let Some(idx) = enum_def
                        .variants()
                        .iter()
                        .position(|v| self.scope.symbols.get(v.name) == *name)
                    {
                        let variant = enum_def.variants()[idx];
                        if variant.payload.is_some() {
                            return Err(hir_error!(
                                span,
                                TypeMismatch {
                                    expected: scrutinee_type,
                                    found: self.scope.types.common.unit,
                                }
                            ));
                        }

                        return Ok(Pattern {
                            kind: PatternKind::Variant { id, variant_idx: idx, sub: None },
                            span,
                        });
                    }
                }

                let symbol = self.scope.symbols.insert(name);
                let local_id = self.declare_local(symbol, scrutinee_type, false, span)?;

                Ok(Pattern { kind: PatternKind::Binding(local_id), span })
            },

            Patt::Variant { qualifier, name, sub } => {
                let (id, generic_args) = self.enum_type(scrutinee_type, span)?;
                let enum_def = &self.scope[id];

                if let Some(qualifier) = qualifier {
                    let matches = self.scope.symbols.get_id(qualifier).is_some_and(|enum_symbol| {
                        self.scope.adts.enum_map.get(&enum_symbol).copied() == Some(id)
                    });

                    if !matches {
                        let name = qualified(self.arena, qualifier, name);
                        return Err(hir_error!(span, UnknownType { name }));
                    }
                }

                let variant_idx = enum_def
                    .variants()
                    .iter()
                    .position(|v| self.scope.symbols.get(v.name) == *name)
                    .ok_or_else(|| hir_error!(span, UnknownType { name }))?;

                let variant = enum_def.variants()[variant_idx];
                let payload = variant.payload.map(|payload| {
                    payload.subst(&self.scope.types, &self.scope.arrays, generic_args)
                });

                let sub = match (sub, payload) {
                    (Some(pat), Some(payload)) => {
                        let lowered = self.lower_pattern(payload, pat.value_ref(), pat.span())?;
                        Some(&*self.arena.alloc(lowered))
                    },
                    (None, None) => None,
                    _ => {
                        return Err(hir_error!(
                            span,
                            TypeMismatch {
                                expected: scrutinee_type,
                                found: self.scope.types.common.unit,
                            }
                        ));
                    },
                };

                Ok(Pattern { kind: PatternKind::Variant { id, variant_idx, sub }, span })
            },
        }
    }
}
