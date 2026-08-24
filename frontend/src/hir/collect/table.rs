use crate::{
    diagnostic,
    hir::{
        self, AdtDef, AdtId, ArrayId, ArrayType, Constant, FnDef, FunctionId, Owner, Static,
        StaticId, SymbolId, SymbolTable, TyInterner, Type, TypeKind, def,
        diagnostics::Diagnostics,
        error::{HirError, HirErrorKind, hir_error},
        ids::IndexVec,
        symbols::Mangler,
        type_resolver::{self, TypeResolver},
    },
    lexer::{Spanned, token::Span},
    parser::statement,
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    ops::{Deref, DerefMut, Index},
};

/// Mutable declaration builder
///
/// Consuming it is the only production path to an [ItemTable], establishing
/// the boundary before body lowering starts
pub(in crate::hir) struct ItemCollector<'hir> {
    table: ItemTable<'hir>,
}

/// The single accumulated namespace for a compilation
///
/// Grows incrementally as modules are loaded: structs and function
/// fn_defs are assigned monotonically increasing IDs across all modules
pub(in crate::hir) struct ItemTable<'hir> {
    pub arena: &'hir bumpalo::Bump,
    pub types: TyInterner<'hir>,
    pub mangler: Mangler<'hir>,
    pub symbols: SymbolTable,
    pub functions: FunctionNamespace<'hir>,
    pub adts: AdtNamespace<'hir>,
    /// fixed-size array types, shared by type resolution
    /// and expression lowering so equal `[T; N]` always map to the same [ArrayId]
    pub arrays: ArrayTable<'hir>,
    pub interfaces: InterfaceNamespace<'hir>,
    pub values: ValueNamespace<'hir>,
    pub editor: EditorIndex<'hir>,
    /// bounds that could not be checked where they were written, because the `impl`
    /// discharging them may only be collected later, or in a later module
    pub pending_bounds: RefCell<Vec<PendingBound<'hir>>>,
    /// Whether the module currently being collected/lowered belongs to std,
    /// set per module by the loader, gates intrinsics and `syscall`
    pub in_std: Cell<bool>,
    pub diagnostics: RefCell<Diagnostics>,

    #[cfg(test)]
    pub reported_errors: RefCell<Vec<HirError<'hir>>>,
}

/// The struct/enum namespace: definitions plus the by-name lookups used to
/// resolve a nominal type or an enum variant back to its [AdtId]
#[derive(Default)]
pub(in crate::hir) struct AdtNamespace<'hir> {
    pub defs: IndexVec<AdtId, AdtDef<'hir>>,
    pub struct_map: Structs,
    pub enum_map: Enums,
    pub variants: EnumVariants,
}

/// The function namespace: definitions plus the by-name and by-receiver
/// lookups used to resolve a call to its [FunctionId]
#[derive(Default)]
pub(in crate::hir) struct FunctionNamespace<'hir> {
    pub defs: IndexVec<FunctionId, FnDef<'hir>>,
    pub by_name: Functions,
    pub methods: Methods<'hir>,
    pub interface_methods: InterfaceItems,
    pub interface_functions: InterfaceItems,
}

/// The interface namespace: declared interfaces, which types implement which
/// of them, and every implementation's associated-type bindings
#[derive(Default)]
pub(in crate::hir) struct InterfaceNamespace<'hir> {
    pub defs: Interfaces<'hir>,
    pub impls: InterfaceImpls<'hir>,
    /// `(implementing type, interface) -> the interface's type arguments`, so a bound
    /// written `T: Interface<U>` can be held to the `U` the implementation chose
    pub impl_args: HashMap<(Type<'hir>, SymbolId), Vec<Type<'hir>>>,
    /// `(implementing type, associated name) -> bound type`, the `type Output = T;`
    /// of every implementation, keyed so `Self::Output` resolves per receiver
    pub associated_types: HashMap<(Type<'hir>, SymbolId), Type<'hir>>,
}

/// The value namespace: module-level constants and statics, plus the
/// by-name lookup used to resolve a static reference to its [StaticId]
#[derive(Default)]
pub struct ValueNamespace<'hir> {
    pub constants: HashMap<SymbolId, &'hir Constant<'hir>>,
    pub statics: IndexVec<StaticId, Static<'hir>>,
    pub static_map: HashMap<SymbolId, StaticId>,
}

/// Editor-facing indices, unused by lowering itself: hover, goto-definition
/// and find-references read these instead of walking the HIR
#[derive(Default)]
pub struct EditorIndex<'hir> {
    /// Rendered `///` documentation per item, keyed by its `decl_span`
    pub docs: HashMap<Span, Box<str>>,
    /// `(span, item name)` for every item named in a `use` declaration
    pub imports: Vec<(Span, SymbolId)>,
    /// The type every named type annotation resolved to, keyed by its span
    pub type_refs: RefCell<HashMap<Span, Type<'hir>>>,
}

/// A deduplicating interner for fixed-size array types
///
/// Uses interior mutability so it can be shared immutably with the type resolver,
/// which only ever holds `&ResolveCtx`
///
/// Equal `(element, len)` pairs always yield
/// the same [ArrayId], keeping [Type] equality sound
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ArrayTable<'hir> {
    types: RefCell<IndexVec<ArrayId, ArrayType<'hir>>>,
    lookup: RefCell<HashMap<(Type<'hir>, u32), ArrayId>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceSignature<'hir> {
    pub name: SymbolId,
    pub is_pub: bool,
    pub superinterfaces: Vec<SymbolId>,
    pub methods: Vec<InterfaceMethodSignature<'hir>>,
    pub constants: Vec<InterfaceConstSignature<'hir>>,
    pub generic_params: Vec<SymbolId>,
    /// each parameter's declared default, positional with [Self::generic_params]
    /// an absent one stands for `Self`, the type carrying the bound
    pub generic_defaults: Vec<Option<Type<'hir>>>,
    /// declaration order of `type X;`, which is the order their implicit parameter
    /// slots follow [Self::generic_params]
    pub associated_types: Vec<SymbolId>,
    pub decl_span: Span,
    /// the declared name alone, where goto-definition lands
    pub name_span: Span,
}

/// One `T: Bound` obligation, held until every implementation is known
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingBound<'hir> {
    pub typ: Type<'hir>,
    pub bound: SymbolId,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InterfaceConstSignature<'hir> {
    pub name: SymbolId,
    pub typ: Type<'hir>,
    pub decl_span: Span,
    pub name_span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceMethodSignature<'hir> {
    pub name: SymbolId,
    pub params: Vec<Type<'hir>>,
    pub return_type: Type<'hir>,
    pub has_receiver: bool,
    pub receiver_mut: bool,
    /// when set, every implementation of this method has to be `const` too
    pub is_const: bool,
    pub decl_span: Span,
    /// the declared name alone, where goto-definition lands
    pub name_span: Span,
}

/// [TypeResolver] over the item table: instantiates generic templates on demand
struct ScopeResolver<'a, 'hir> {
    scope: &'a mut ItemTable<'hir>,
    self_type: Option<Type<'hir>>,
    env: Option<&'a GenericEnv<'hir>>,
}

pub(in crate::hir) type Functions = HashMap<SymbolId, FunctionId>;
pub(in crate::hir) type Structs = HashMap<SymbolId, AdtId>;
pub(in crate::hir) type Enums = HashMap<SymbolId, AdtId>;
pub(in crate::hir) type EnumVariants = HashMap<(SymbolId, SymbolId), (AdtId, i64)>;
pub(in crate::hir) type Methods<'hir> = HashMap<(Type<'hir>, SymbolId), FunctionId>;
/// `(interface, short item name) -> every FunctionId implementing it, across all impls`
/// one interface+name pair legitimately has one implementor per concrete type, so this
/// stays a filtered list rather than a single winner
pub(in crate::hir) type InterfaceItems = HashMap<(SymbolId, SymbolId), Vec<FunctionId>>;
pub(in crate::hir) type Interfaces<'hir> = HashMap<SymbolId, InterfaceSignature<'hir>>;
pub(in crate::hir) type InterfaceImpls<'hir> = HashSet<(Type<'hir>, SymbolId)>;
/// Maps a generic parameter name (`T`) to the concrete type it was instantiated
/// with, when re-lowering a generic template body for a concrete instance
/// Empty for ordinary (non-instance) lowering
pub(in crate::hir) type GenericEnv<'hir> = HashMap<String, Type<'hir>>;

/// Reserved nominal name for `impl [T]` blocks, which target the structural
/// slice type
/// Kept in sync with the parser, which tags slice impls with it
pub const SLICE_IMPL_NAME: &str = "[]";

impl<'hir> ItemTable<'hir> {
    pub fn new(arena: &'hir bumpalo::Bump) -> Self {
        Self {
            arena,
            types: TyInterner::new(arena),
            mangler: Mangler::default(),
            symbols: SymbolTable::new(),
            functions: FunctionNamespace::default(),
            adts: AdtNamespace::default(),
            arrays: ArrayTable::default(),
            interfaces: InterfaceNamespace::default(),
            values: ValueNamespace::default(),
            editor: EditorIndex::default(),
            pending_bounds: RefCell::default(),
            in_std: Cell::new(false),
            diagnostics: RefCell::new(Diagnostics::default()),
            #[cfg(test)]
            reported_errors: RefCell::new(Vec::new()),
        }
    }

    #[inline]
    pub(in crate::hir) fn method(
        &self,
        receiver: Type<'hir>,
        name: SymbolId,
    ) -> Option<FunctionId> {
        self.functions.methods.get(&(receiver, name)).copied().or_else(|| {
            self.functions.methods.iter().find_map(|(&(pattern, candidate), &id)| {
                (candidate == name && type_pattern_matches(pattern, receiver)).then_some(id)
            })
        })
    }

    pub(in crate::hir) fn implements_interface(
        &self,
        typ: Type<'hir>,
        interface: SymbolId,
    ) -> bool {
        self.interfaces.impls.contains(&(typ, interface))
            || self.interfaces.impls.iter().any(|&(pattern, candidate)| {
                candidate == interface && type_pattern_matches(pattern, typ)
            })
    }

    pub(in crate::hir) fn free_impl_function(
        &self,
        concrete: Type<'hir>,
        interface: SymbolId,
        name: SymbolId,
    ) -> Option<FunctionId> {
        let short_name = self.symbols.get(name);
        self.functions.defs.iter().enumerate().find_map(|(index, definition)| {
            (definition.owner == Owner::Interface { on: concrete, interface }
                && !definition.has_receiver
                && self.symbols.get(definition.name).rsplit("::").next() == Some(short_name))
            .then_some(FunctionId(index as u32))
        })
    }

    pub(in crate::hir) fn interface_constant(
        &self,
        concrete: Type<'hir>,
        interface: SymbolId,
        name: &str,
    ) -> Option<&'hir Constant<'hir>> {
        self.values.constants.values().copied().find(|constant| {
            matches!(
                constant.owner,
                Owner::Interface { on, interface: candidate }
                    if on == concrete && candidate == interface
            ) && self.symbols.get(constant.name).rsplit("::").next() == Some(name)
        })
    }

    /// Assembles the final [Hir](hir::Hir), consuming everything this
    /// table collected
    pub(in crate::hir) fn into_hir(
        self,
        functions: IndexVec<FunctionId, hir::Function<'hir>>,
        diagnostics: Vec<diagnostic::RichDiagnostic>,
    ) -> hir::Hir<'hir> {
        hir::Hir {
            types: self.types,
            symbols: self.symbols,
            adts: self.adts.defs,
            arrays: self.arrays,
            functions,
            statics: self.values.statics,
            constants: self.values.constants.into_values().cloned().collect(),
            interfaces: self.interfaces.defs.into_values().collect(),
            docs: self.editor.docs,
            imports: self.editor.imports,
            type_refs: self.editor.type_refs.into_inner(),
            diagnostics,
            #[cfg(test)]
            reported_errors: self.reported_errors.into_inner(),
        }
    }

    /// Record `error` and yield a poison [Type<'hir>] so analysis can continue
    pub(in crate::hir) fn poison(&self, error: HirError<'hir>) -> Type<'hir> {
        #[cfg(test)]
        self.reported_errors.borrow_mut().push(error);
        Type::error(self.diagnostics.borrow_mut().emit(error.into()))
    }

    /// Record `error` and continue lowering
    pub(in crate::hir) fn soft(&self, error: HirError<'hir>) {
        #[cfg(test)]
        self.reported_errors.borrow_mut().push(error);
        self.diagnostics.borrow_mut().emit(error.into());
    }

    pub(in crate::hir) fn declare_or_error(
        &mut self,
        exists: bool,
        error: impl FnOnce(&mut Self) -> HirError<'hir>,
    ) -> bool {
        if exists {
            let error = error(self);
            self.soft(error);
        }

        exists
    }

    /// note that `span` names `typ`, for editor navigation
    #[inline]
    pub(in crate::hir) fn record_type_ref(&self, span: Span, typ: Type<'hir>) {
        self.editor.type_refs.borrow_mut().entry(span).or_insert(typ);
    }

    pub(in crate::hir) fn function_id<'s>(
        &self,
        function: &statement::Function<'s>,
        hint: Option<&'s str>,
        error_kind: impl FnOnce(&'s str) -> HirErrorKind<'s>,
    ) -> Result<FunctionId, HirError<'s>> {
        let impl_type = function.impl_type.or(hint);

        match (function.receiver, impl_type) {
            (Some(_), Some(impl_type)) => {
                let receiver_type = self.lookup_named_type(impl_type).ok_or(HirError {
                    kind: HirErrorKind::UnknownType { name: impl_type },
                    span: function.span,
                })?;

                // a method that exists always has its name interned already
                self.symbols
                    .get_id(function.name)
                    .and_then(|method_symbol| self.method(receiver_type, method_symbol))
                    .ok_or_else(|| HirError {
                        kind: error_kind(function.name),
                        span: function.span,
                    })
            },
            (Some(_), _) => Err(HirError { kind: error_kind(function.name), span: function.span }),
            (_, Some(impl_type)) => self
                .resolve_qualified_call(impl_type, function.name)
                .ok_or_else(|| HirError { kind: error_kind(function.name), span: function.span }),
            (_, _) => self
                .resolve_function(|m| m.item(function.name))
                .ok_or_else(|| HirError { kind: error_kind(function.name), span: function.span }),
        }
    }

    #[inline]
    pub(in crate::hir) fn resolve_return_type<'h>(
        &mut self,
        return_type: Option<&Spanned<statement::Type<'h>>>,
        self_type: Option<Type<'hir>>,
        env: Option<&GenericEnv<'hir>>,
    ) -> Result<Type<'hir>, HirError<'hir>>
    where
        'h: 'hir,
    {
        Ok(return_type.map_or(self.types.common.unit, |s| {
            self.resolve_type(s.value_ref(), s.span(), self_type, env)
                .unwrap_or_else(|error| self.poison(error))
        }))
    }

    #[inline]
    pub(in crate::hir) fn resolve_params<'h>(
        &mut self,
        params: &[statement::Parameter<'h>],
        self_type: Option<Type<'hir>>,
        env: Option<&GenericEnv<'hir>>,
    ) -> Result<Vec<Type<'hir>>, HirError<'hir>>
    where
        'h: 'hir,
    {
        let mut out = Vec::with_capacity(params.len());
        for p in params {
            let typ = self
                .resolve_type(p.typ.value_ref(), p.typ.span(), self_type, env)
                .unwrap_or_else(|error| self.poison(error));
            out.push(typ);
        }
        Ok(out)
    }

    #[inline]
    pub(in crate::hir) fn resolve_signature<'h>(
        &mut self,
        params: &[statement::Parameter<'h>],
        return_type: Option<&Spanned<statement::Type<'h>>>,
        self_type: Option<Type<'hir>>,
        env: Option<&GenericEnv<'hir>>,
    ) -> Result<(Vec<Type<'hir>>, Type<'hir>), HirError<'hir>>
    where
        'h: 'hir,
    {
        let params = self.resolve_params(params, self_type, env)?;
        let return_type = self.resolve_return_type(return_type, self_type, env)?;
        Ok((params, return_type))
    }

    pub(in crate::hir) fn resolve_type<'h>(
        &mut self,
        typ: &statement::Type<'h>,
        span: Span,
        self_type: Option<Type<'hir>>,
        env: Option<&GenericEnv<'hir>>,
    ) -> Result<Type<'hir>, HirError<'hir>>
    where
        'h: 'hir,
    {
        let mut resolver = ScopeResolver { scope: self, self_type, env };
        type_resolver::resolve(&mut resolver, typ, span)
    }

    pub(in crate::hir) fn generic_adt(
        &self,
        name: &str,
        args: &[Type<'hir>],
        span: Span,
    ) -> Result<Type<'hir>, HirError<'hir>> {
        let symbol = self
            .symbols
            .get_id(name)
            .ok_or_else(|| hir_error!(span, UnknownType { name: self.arena.alloc_str(name) }))?;
        let id = match (
            self.adts.struct_map.get(&symbol).copied(),
            self.adts.enum_map.get(&symbol).copied(),
        ) {
            (Some(id), _) | (_, Some(id)) => id,
            _ => return Err(hir_error!(span, UnknownType { name: self.arena.alloc_str(name) })),
        };

        let mut filled = Vec::new();
        let args =
            self.complete_generic_args(self.arena.alloc_str(name), id, args, &mut filled, span)?;

        Ok(self.types.adt(id, args))
    }

    /// checks `args` against the parameters of `id`, filling in the defaults of any the caller left off
    fn complete_generic_args<'a>(
        &self,
        name: &'hir str,
        id: AdtId,
        args: &'a [Type<'hir>],
        filled: &'a mut Vec<Type<'hir>>,
        span: Span,
    ) -> Result<&'a [Type<'hir>], HirError<'hir>> {
        let generics = &self.adts.defs[id].generics;
        let args = def::complete_generic_args(generics, args, filled, &self.types, &self.arrays)
            .map_err(|expected| {
                let found = args.len();
                hir_error!(span, ArityMismatch { name, expected, found, decl: None })
            })?;

        for (param, &typ) in generics.iter().zip(args) {
            if matches!(typ.kind(), TypeKind::GenericParam(_)) {
                continue;
            }

            let pending = param.bounds.iter().map(|&bound| PendingBound { typ, bound, span });
            self.pending_bounds.borrow_mut().extend(pending);
        }

        Ok(args)
    }

    pub(in crate::hir) fn instance_symbol(&self, base: &str, args: &[Type<'hir>]) -> String {
        let mut mangled = String::from(base);
        for &arg in args {
            mangled.push('$');
            mangled.push_str(&self.mangle_component(arg));
        }

        mangled
    }

    fn mangle_component(&self, typ: Type<'hir>) -> String {
        match typ.kind() {
            TypeKind::Adt(id, args) => {
                let mut name = self.symbols.get(self.adts.defs[id].name).to_string();
                for &arg in args {
                    name.push('_');
                    name.push_str(&self.mangle_component(arg));
                }
                name
            },
            TypeKind::Ref { to, .. } => format!("ref_{}", self.mangle_component(Type::from(to))),
            TypeKind::GenericParam(i) => format!("T{i}"),
            other => other.mangled().to_string(),
        }
    }

    pub(in crate::hir) fn is_generic_function(&self, function: &statement::Function<'_>) -> bool {
        if !function.generics.is_empty() {
            return true;
        }

        function.impl_type.is_some_and(|impl_type| {
            impl_type == SLICE_IMPL_NAME
                || self.symbols.get_id(impl_type).is_some_and(|sym| {
                    self.nominal_type(sym).is_some_and(|typ| match typ.kind() {
                        TypeKind::Adt(id, _) => !self.adts.defs[id].generics.is_empty(),
                        _ => false,
                    })
                })
        })
    }

    #[inline]
    pub(in crate::hir) fn push_signature(&mut self, signature: FnDef<'hir>) -> FunctionId {
        self.functions.defs.push(signature)
    }

    #[inline]
    pub(in crate::hir) fn nominal_type(&self, symbol: SymbolId) -> Option<Type<'hir>> {
        nominal_type(&self.types, &self.adts.struct_map, &self.adts.enum_map, symbol)
    }

    #[inline]
    pub(in crate::hir) fn lookup_named_type(&self, name: &str) -> Option<Type<'hir>> {
        resolve_primitive_type(&self.types, name)
            .or_else(|| self.symbols.get_id(name).and_then(|s| self.nominal_type(s)))
    }

    #[inline]
    pub(in crate::hir) fn nominal_name(&self, typ: Type<'hir>) -> Option<&str> {
        match typ.strip_reference().kind() {
            TypeKind::Adt(id, _) => Some(self.symbols.get(self.adts.defs[id].name)),
            _ => None,
        }
    }

    pub(in crate::hir) fn resolve_function<F>(&self, operation: F) -> Option<FunctionId>
    where
        F: FnOnce(&Mangler) -> String,
    {
        let mangled = operation(&self.mangler);
        self.symbols
            .get_id(&mangled)
            .and_then(|s| self.functions.by_name.get(&s).copied())
    }

    pub(in crate::hir) fn resolve_qualified_call(
        &self,
        qualifier: &str,
        name: &str,
    ) -> Option<FunctionId> {
        if let Some(id) = self.resolve_function(|m| m.scoped_item(qualifier, name)) {
            return Some(id);
        }

        let receiver_type = self.lookup_named_type(qualifier)?;
        self.interfaces.impls.iter().filter(|&&(t, _)| t == receiver_type).find_map(
            |&(_, interface_sym)| {
                let interface_name = self.symbols.get(interface_sym);
                self.resolve_function(|m| m.interface_item(qualifier, interface_name, name))
            },
        )
    }

    #[inline]
    pub(in crate::hir) fn resolve_function_call(
        &self,
        qualifier: Option<&str>,
        name: &str,
    ) -> Option<FunctionId> {
        if let Some(qualifier) = qualifier
            && let Some(id) = self.resolve_qualified_call(qualifier, name)
        {
            return Some(id);
        }

        self.resolve_function(|m| m.item(name))
    }

    pub(in crate::hir) fn resolve_qualified_function_call(
        &self,
        path: &[&str],
        name: &str,
    ) -> Option<FunctionId> {
        let qualifier = path.join("::");
        self.resolve_function_call(Some(&qualifier), name)
            .or_else(|| self.resolve_function_call(None, name))
    }
}

impl<'hir> ItemCollector<'hir> {
    pub(in crate::hir) fn new(arena: &'hir bumpalo::Bump) -> Self {
        Self { table: ItemTable::new(arena) }
    }

    pub(in crate::hir) fn freeze(self) -> ItemTable<'hir> {
        self.table
    }
}

pub(in crate::hir) fn type_pattern_matches(pattern: Type<'_>, concrete: Type<'_>) -> bool {
    match (pattern.kind(), concrete.kind()) {
        (TypeKind::GenericParam(_), _) => true,
        (TypeKind::Adt(pattern_id, pattern_args), TypeKind::Adt(concrete_id, concrete_args)) => {
            pattern_id == concrete_id
                && pattern_args.len() == concrete_args.len()
                && pattern_args
                    .iter()
                    .zip(concrete_args)
                    .all(|(&pattern, &concrete)| type_pattern_matches(pattern, concrete))
        },
        (TypeKind::Slice { element: pattern, .. }, TypeKind::Slice { element: concrete, .. }) => {
            type_pattern_matches(pattern.into(), concrete.into())
        },
        (
            TypeKind::Ref { to: pattern, .. } | TypeKind::Raw { to: pattern, .. },
            TypeKind::Ref { to: concrete, .. } | TypeKind::Raw { to: concrete, .. },
        ) => type_pattern_matches(pattern.into(), concrete.into()),
        _ => pattern == concrete,
    }
}

impl<'hir> ArrayTable<'hir> {
    pub fn intern(&self, element: Type<'hir>, len: u32) -> ArrayId {
        let cacheable = !self.type_has_infer(element);
        if cacheable && let Some(&id) = self.lookup.borrow().get(&(element, len)) {
            return id;
        }

        let mut types = self.types.borrow_mut();
        let id = types.push(ArrayType { element, len });
        diagnostic::register_array_name(id.0, &format!("[{element}; {len}]"));

        if cacheable {
            self.lookup.borrow_mut().insert((element, len), id);
        }

        id
    }

    #[inline]
    pub fn get(&self, id: ArrayId) -> ArrayType<'hir> {
        self.types.borrow()[id]
    }

    fn type_has_infer(&self, typ: Type<'hir>) -> bool {
        match typ.kind() {
            TypeKind::Infer(_) => true,
            TypeKind::Ref { to, .. } | TypeKind::Raw { to, .. } => self.type_has_infer(to),
            TypeKind::Slice { element, .. } => self.type_has_infer(element),
            TypeKind::Adt(_, args) => args.iter().any(|&arg| self.type_has_infer(arg)),
            TypeKind::Array(id) => self.type_has_infer(self.get(id).element),
            _ => false,
        }
    }

    #[inline]
    pub fn resolve(&self, id: ArrayId, element: Type<'hir>) {
        self.types.borrow_mut()[id].element = element;
    }

    #[inline]
    pub fn snapshot(&self) -> IndexVec<ArrayId, ArrayType<'hir>> {
        self.types.borrow().clone()
    }
}

impl<'a, 'hir> TypeResolver<'hir, 'hir> for ScopeResolver<'a, 'hir> {
    fn named(&mut self, name: &'hir str, span: Span) -> Result<Type<'hir>, HirError<'hir>> {
        if let Some(env) = self.env
            && let Some(&t) = env.get(name)
        {
            return Ok(t);
        }

        let typ = self
            .scope
            .symbols
            .get_id(name)
            .and_then(|symbol| self.scope.nominal_type(symbol))
            .ok_or_else(|| hir_error!(span, UnknownType { name }))?;
        self.scope.record_type_ref(span, typ);

        Ok(typ)
    }

    fn generic(
        &mut self,
        name: &'hir str,
        args: &[Type<'hir>],
        span: Span,
    ) -> Result<Type<'hir>, HirError<'hir>> {
        let typ = self.scope.generic_adt(name, args, span)?;
        self.scope.record_type_ref(span, typ);

        Ok(typ)
    }

    fn self_type(&mut self, _span: Span) -> Result<Type<'hir>, HirError<'hir>> {
        Ok(self.self_type.unwrap_or(self.scope.types.common.self_type))
    }

    fn associated(
        &mut self,
        qualifier: Option<Type<'hir>>,
        name: &'hir str,
        span: Span,
    ) -> Result<Type<'hir>, HirError<'hir>> {
        if qualifier.is_none() {
            if let Some(&typ) = self.env.and_then(|env| env.get(&associated_key(name))) {
                return Ok(typ);
            }
        }

        let Some(owner) = qualifier.or(self.self_type) else {
            return Err(hir_error!(span, UnknownAssociatedType { name }));
        };

        let symbol = self
            .scope
            .symbols
            .get_id(name)
            .ok_or_else(|| hir_error!(span, UnknownAssociatedType { name }))?;

        self.scope
            .interfaces
            .associated_types
            .get(&(owner.strip_reference(), symbol))
            .copied()
            .ok_or_else(|| hir_error!(span, UnknownAssociatedType { name }))
    }

    fn arrays(&self) -> &ArrayTable<'hir> {
        &self.scope.arrays
    }

    fn types(&self) -> &TyInterner<'hir> {
        &self.scope.types
    }
}

impl<'s> Index<AdtId> for ItemTable<'s> {
    type Output = AdtDef<'s>;
    fn index(&self, id: AdtId) -> &AdtDef<'s> {
        &self.adts.defs[id]
    }
}

impl<'hir> Deref for ItemCollector<'hir> {
    type Target = ItemTable<'hir>;

    fn deref(&self) -> &Self::Target {
        &self.table
    }
}

impl DerefMut for ItemCollector<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.table
    }
}

#[inline(always)]
pub(in crate::hir) fn associated_key(name: &str) -> String {
    format!("Self::{name}")
}

#[inline(always)]
pub(in crate::hir) fn resolve_primitive_type<'hir>(
    types: &TyInterner<'hir>,
    name: &str,
) -> Option<Type<'hir>> {
    statement::Type::from_str(name).and_then(|ast_ty| types.from_primitive_ast(&ast_ty))
}

#[inline]
pub(in crate::hir) fn nominal_type<'hir>(
    types: &TyInterner<'hir>,
    struct_map: &Structs,
    enum_map: &Enums,
    symbol: SymbolId,
) -> Option<Type<'hir>> {
    struct_map
        .get(&symbol)
        .copied()
        .map(|id| types.adt(id, &[]))
        .or_else(|| enum_map.get(&symbol).copied().map(|id| types.adt(id, &[])))
}

#[inline]
pub(in crate::hir) fn is_generic_impl(imp: &statement::Impl<'_>) -> bool {
    !imp.generics.is_empty()
        || matches!(imp.receiver.value_ref(), statement::Type::Generic(..))
        || matches!(
            imp.receiver.value_ref(),
            statement::Type::Slice(element, _) if matches!(element.as_ref(), statement::Type::Named(_))
        )
}

pub(in crate::hir) fn receiver_param_type<'hir>(
    types: &TyInterner<'hir>,
    receiver: Type<'hir>,
    mutable: bool,
) -> Type<'hir> {
    match receiver.kind() {
        TypeKind::Slice { element, .. } => types.slice(element, mutable),
        _ => types.refer(receiver, mutable),
    }
}

pub(in crate::hir) fn generic_param_env<'hir>(
    types: &TyInterner<'hir>,
    generics: &[statement::GenericBound<'_>],
) -> GenericEnv<'hir> {
    generics
        .iter()
        .enumerate()
        .map(|(i, g)| (g.name.to_string(), types.generic_param(i as u8)))
        .collect()
}

pub(in crate::hir) fn extend_generic_env<'hir>(
    env: &mut GenericEnv<'hir>,
    types: &TyInterner<'hir>,
    generics: &[statement::GenericBound<'_>],
) {
    let mut next = env.len() as u8;
    for generic in generics {
        env.entry(generic.name.to_string()).or_insert_with(|| {
            let param = types.generic_param(next);
            next += 1;
            param
        });
    }
}
