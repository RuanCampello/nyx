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
        declarations::Declarations,
        error::{HirErrorKind, hir_error},
        ids::IndexVec,
        interfaces, lower, statics,
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

        let enum_defaults = enums
            .iter()
            .map(|(id, _, _, declaration)| (*id, declaration.generics.as_slice()));
        let defaults = structs
            .iter()
            .map(|(id, declaration)| (*id, declaration.generics.as_slice()))
            .chain(enum_defaults);
        self.resolve_generic_defaults(defaults);

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

    /// reports every bound that collection deferred, once no further `impl` can appear
    pub(in crate::hir) fn settle_bounds(&mut self) {
        self.check_generic_defaults();

        for pending in self.pending_bounds.take() {
            if self.implements_interface(pending.typ, pending.bound) {
                continue;
            }

            let bound_name = self.arena.alloc_str(self.symbols.get(pending.bound));
            let (type_name, span) = (pending.typ, pending.span);
            self.soft(hir_error!(span, UnsatisfiedBound { type_name, bound_name }));
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
            let function =
                lower::FunctionBuilder::new_instance(self, id, &function, arena, env, impl_type)
                    .lower();

            match function {
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
        use hir::Res::*;

        for resolution in function.typeck.type_dependent_defs.values() {
            match *resolution {
                Function(id) if self.functions.defs[id].body.is_some() => out.push(id),
                ParamMethod { interface, name, .. } => {
                    let methods = &self.functions.interface_methods;
                    self.extend_interface_callees(methods, interface, name, out);
                },
                ParamFunction { interface, name, .. } => {
                    let functions = &self.functions.interface_functions;
                    self.extend_interface_callees(functions, interface, name, out);
                },
                _ => {},
            }
        }
    }

    fn extend_interface_callees(
        &self,
        index: &InterfaceItems,
        interface: SymbolId,
        name: SymbolId,
        out: &mut Vec<FunctionId>,
    ) {
        let Some(candidates) = index.get(&(interface, name)) else {
            return;
        };

        out.extend(candidates.iter().copied().filter(|&id| self.functions.defs[id].body.is_some()));
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
