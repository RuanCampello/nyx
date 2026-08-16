//! Item collection
//!
//! per-module phase that extends the [ItemTable] namespace with
//! declarations before any body is lowered

mod adts;
mod signatures;
mod table;

pub use table::*;

use crate::{
    hir::{
        self, Function, FunctionId, FunctionKind, Owner, SymbolId, constants,
        declarations::Declarations, error::HirErrorKind, ids::IndexVec, interfaces, lower, statics,
    },
    lexer::token::Span,
    parser::statement,
};
use std::collections::{HashMap, HashSet};

impl<'hir> ItemTable<'hir> {
    pub(in crate::hir) fn extend<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        arena: &'hir bumpalo::Bump,
    ) where
        's: 'hir,
    {
        self.extend_types(declarations);
        self.extend_items(declarations, arena);
    }

    pub(in crate::hir) fn extend_types<'d, 's>(&mut self, declarations: &Declarations<'d, 's>)
    where
        's: 'hir,
    {
        self.collect_docs(declarations);
        self.collect_imports(declarations);
        let structs = self.declare_structs(declarations).unwrap_or_else(|error| {
            self.soft(error);
            Vec::new()
        });
        let enums = self.declare_enums(declarations).unwrap_or_else(|error| {
            self.soft(error);
            Vec::new()
        });
        if let Err(error) = self.lower_structs(&structs) {
            self.soft(error);
        }
        if let Err(error) = self.lower_enums(&enums) {
            self.soft(error);
        }
    }

    pub(in crate::hir) fn extend_items<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        arena: &'hir bumpalo::Bump,
    ) where
        's: 'hir,
    {
        if let Err(error) = self.extend_interfaces(declarations) {
            self.soft(error);
        }
        if let Err(error) = self.extend_signatures(declarations) {
            self.soft(error);
        }
        if let Err(error) = constants::extend(self, declarations, arena) {
            self.soft(error);
        }
        if let Err(error) = statics::extend(self, declarations, arena) {
            self.soft(error);
        }
        if let Err(error) = interfaces::validate(self, declarations) {
            self.soft(error);
        }
    }

    fn collect_docs(&mut self, declarations: &Declarations<'_, '_>) {
        for (span, lines) in &declarations.docs {
            if let Some(joined) = join_docs(lines) {
                self.editor.docs.insert(*span, joined);
            }
        }
    }

    fn collect_imports(&mut self, declarations: &Declarations<'_, '_>) {
        for declaration in &declarations.uses {
            let statement::UseItems::Named(items) = &declaration.items else {
                continue;
            };
            for item in items {
                let name = self.symbols.insert(item.name);
                self.editor.imports.push((item.span, name));
            }
        }
    }

    pub(in crate::hir) fn lower_matching_functions<'d, 's>(
        &self,
        declarations: &Declarations<'d, 's>,
        mut should_lower: impl FnMut(FunctionId) -> bool,
        retain_intrinsics: bool,
        arena: &'hir bumpalo::Bump,
    ) -> IndexVec<FunctionId, Function<'hir>>
    where
        's: 'hir,
    {
        let mut lowered = IndexVec::new();
        let mut seen = HashSet::new();

        for function in declarations.functions() {
            if self.is_generic_function(function) {
                continue;
            }

            let id = match self
                .function_id(function, None, |name| HirErrorKind::UnknownFunction { name })
            {
                Ok(id) => id,
                Err(e) => {
                    self.soft(e);
                    continue;
                },
            };

            let skip_intrinsic = matches!(self.functions.defs[id].kind, FunctionKind::Intrinsic(_))
                && !retain_intrinsics;

            if !should_lower(id) || skip_intrinsic || !seen.insert(id) {
                continue;
            }

            match lower::FunctionBuilder::new(self, id, function, arena).lower() {
                Ok(function) => {
                    lowered.push(function);
                },
                Err(error) => self.soft(error),
            }
        }

        lowered
    }

    pub(in crate::hir) fn lower_generic_templates(
        &self,
        roots: &IndexVec<FunctionId, Function<'hir>>,
        arena: &'hir bumpalo::Bump,
        include_unreachable: bool,
    ) -> HashMap<FunctionId, Function<'hir>> {
        let mut worklist = Vec::new();
        for function in roots {
            self.collect_generic_callees(function, &mut worklist);
        }
        if include_unreachable {
            worklist.extend(self.functions.defs.iter().enumerate().filter_map(
                |(index, definition)| definition.body.as_ref().map(|_| FunctionId(index as u32)),
            ));
        }

        let mut lowered = HashMap::new();
        while let Some(id) = worklist.pop() {
            if lowered.contains_key(&id) {
                continue;
            }
            let Some(function) = self.functions.defs[id].body.clone() else {
                continue;
            };

            let mut env = self.functions.defs[id].generic_env.clone();
            extend_generic_env(&mut env, &self.types, &function.generics);
            let impl_type = match self.functions.defs[id].owner {
                Owner::Inherent(on) | Owner::Interface { on, .. } => {
                    self.nominal_name(on).map(str::to_owned)
                },
                Owner::Free => None,
            };

            let impl_type = impl_type.as_deref().map(|name| &*arena.alloc_str(name));

            match lower::FunctionBuilder::new_instance(self, id, &function, arena, env, impl_type)
                .lower()
            {
                Ok(function) => {
                    self.collect_generic_callees(&function, &mut worklist);
                    lowered.insert(id, function);
                },
                Err(error) => self.soft(error),
            }
        }
        lowered
    }

    fn collect_generic_callees(&self, function: &Function<'hir>, out: &mut Vec<FunctionId>) {
        use hir::Res;

        for resolution in function.typeck.type_dependent_defs.values() {
            match *resolution {
                Res::Function(id) if self.functions.defs[id].body.is_some() => out.push(id),
                Res::ParamMethod { interface, name, .. } => {
                    out.extend(self.functions.methods.iter().filter_map(
                        |(&(_, candidate_name), &id)| {
                            let fn_def = &self.functions.defs[id];

                            let same_name = candidate_name == name;
                            let has_body = fn_def.body.is_some();
                            let matches_interface = matches!(
                                fn_def.owner,
                                Owner::Interface { interface: cand, .. } if cand == interface
                            );

                            (same_name && has_body && matches_interface).then_some(id)
                        },
                    ));
                },
                Res::ParamFunction { interface, name, .. } => {
                    out.extend(self.functions.defs.iter().enumerate().filter_map(
                        |(index, definition)| {
                            let has_body = definition.body.is_some();
                            let is_standalone = !definition.has_receiver;
                            let same_name = self.symbols.get(definition.name).rsplit("::").next()
                                == Some(self.symbols.get(name));
                            let matches_interface = matches!(
                                definition.owner,
                                Owner::Interface {
                                    interface: candidate,
                                    ..
                                } if candidate == interface
                            );

                            let is_valid_callee =
                                has_body && is_standalone && same_name && matches_interface;
                            is_valid_callee.then_some(FunctionId(index as u32))
                        },
                    ));
                },
                _ => {},
            }
        }
    }

    fn nominal_decl_span(&self, symbol: SymbolId) -> Option<Span> {
        self.adts
            .struct_map
            .get(&symbol)
            .and_then(|&id| self.adts.defs.get(id).map(|adt| adt.decl_span))
            .or_else(|| {
                self.adts
                    .enum_map
                    .get(&symbol)
                    .and_then(|&id| self.adts.defs.get(id).map(|adt| adt.decl_span))
            })
            .and_then(source_span)
    }
}

#[inline]
pub(in crate::hir) fn source_span(span: Span) -> Option<Span> {
    (span != Span::default()).then_some(span)
}

fn join_docs(lines: &[&str]) -> Option<Box<str>> {
    if lines.is_empty() {
        return None;
    }

    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(line.strip_prefix(' ').unwrap_or(line));
    }

    Some(out.into_boxed_str())
}
