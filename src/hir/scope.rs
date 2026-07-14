use crate::{
    hir::{
        ArrayId, ArrayType, Constant, Enum, EnumId, EnumRepr, EnumVariant, FunctionId,
        FunctionKind, Intrinsic, Layout, Method, Struct, StructField, StructId, SymbolId,
        SymbolTable, Type, TypeKind,
        diagnostics::Diagnostics,
        error::{HirError, HirErrorKind, hir_error},
        index_vec::IndexVec,
        symbols::Mangler,
        type_resolver::{self, TypeResolver},
    },
    lexer::{Spanned, token::Span},
    parser::statement,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Index,
};

/// The single accumulated namespace for a compilation
///
/// Grows incrementally as modules are loaded: structs and function
/// signatures are assigned monotonically increasing IDs across all modules
pub struct Scope<'hir> {
    pub(in crate::hir) arena: &'hir bumpalo::Bump,
    pub(in crate::hir) mangler: Mangler<'hir>,
    pub symbols: SymbolTable,
    pub signatures: IndexVec<FunctionId, FunctionSignature>,
    pub functions: Functions,
    pub methods: Methods,
    pub structs: IndexVec<StructId, Struct>,
    pub struct_map: Structs,
    pub enums: IndexVec<EnumId, Enum>,
    pub enum_map: Enums,
    /// fixed-size array types, shared by type resolution
    /// and expression lowering so equal `[T; N]` always map to the same [ArrayId]
    pub arrays: ArrayTable,
    pub enum_variants: EnumVariants,
    pub interfaces: Interfaces,
    pub interface_impls: InterfaceImpls,
    pub constants: HashMap<SymbolId, &'hir Constant<'hir>>,
    /// Rendered `///` documentation per item, keyed by its `decl_span`
    pub docs: HashMap<Span, Box<str>>,

    pub generic_structs: HashMap<SymbolId, statement::Struct<'hir>>,
    pub generic_enums: HashMap<SymbolId, statement::Enum<'hir>>,
    /// Generic free-function templates keyed by the [`FunctionId`] of their (open) signature
    pub generic_fns: HashMap<FunctionId, statement::Function<'hir>>,
    pub generic_fn_envs: HashMap<FunctionId, GenericEnv>,
    pub generic_impls: Vec<statement::Impl<'hir>>,

    pub(in crate::hir) specialized_slices: HashSet<Type>,

    /// Whether the module currently being collected/lowered belongs to std,
    /// set per module by the loader, gates intrinsics and `syscall`
    pub(in crate::hir) in_std: bool,

    /// When set, recoverable lowering errors are accumulated in [diagnostics]
    /// and the offending nodes are poisoned instead of aborting the whole pass
    ///
    /// [diagnostics]: Scope::diagnostics
    pub(in crate::hir) recover: bool,
    pub(in crate::hir) diagnostics: Diagnostics,
}

/// A deduplicating interner for fixed-size array types
///
/// Uses interior mutability so it can be shared immutably with the type resolver,
/// which only ever holds `&ResolveCtx`. Equal `(element, len)` pairs always yield
/// the same [ArrayId], keeping [Type] equality sound
#[derive(Debug, Default)]
pub struct ArrayTable {
    types: RefCell<IndexVec<ArrayId, ArrayType>>,
    lookup: RefCell<HashMap<(Type, u32), ArrayId>>,
}

#[derive(Debug, Clone)]
pub(in crate::hir) struct FunctionSignature {
    pub name: SymbolId,
    pub params: Vec<Type>,
    pub return_type: Type,
    pub kind: FunctionKind,
    pub is_const: bool,
}

#[derive(Debug)]
pub(in crate::hir) struct InterfaceSignature {
    #[allow(unused)]
    pub name: SymbolId,
    pub superinterfaces: Vec<SymbolId>,
    pub methods: Vec<InterfaceMethodSignature>,
    pub generic_params: Vec<SymbolId>,
}

#[derive(Debug)]
pub(in crate::hir) struct InterfaceMethodSignature {
    pub name: SymbolId,
    pub params: Vec<Type>,
    pub return_type: Type,
    pub(in crate::hir) has_receiver: bool,
    pub(in crate::hir) receiver_mut: bool,
}

/// [TypeResolver] over the item table: instantiates generic templates on demand
struct ScopeResolver<'a, 'hir> {
    scope: &'a mut Scope<'hir>,
    self_type: Option<Type>,
    env: Option<&'a GenericEnv>,
}

pub(in crate::hir) type Functions = HashMap<SymbolId, FunctionId>;
pub(in crate::hir) type Structs = HashMap<SymbolId, StructId>;
pub(in crate::hir) type Enums = HashMap<SymbolId, EnumId>;
pub(in crate::hir) type EnumVariants = HashMap<(SymbolId, SymbolId), (EnumId, i64)>;
pub(in crate::hir) type Methods = HashMap<(Type, SymbolId), FunctionId>;
pub(in crate::hir) type Interfaces = HashMap<SymbolId, InterfaceSignature>;
pub(in crate::hir) type InterfaceImpls = HashSet<(Type, SymbolId)>;
/// Maps a generic parameter name (`T`) to the concrete type it was instantiated
/// with, when re-lowering a generic template body for a concrete instance
/// Empty for ordinary (non-instance) lowering
pub(in crate::hir) type GenericEnv = HashMap<String, Type>;

/// Reserved nominal name for `impl [T]` blocks, which target the structural
/// slice type
/// Kept in sync with the parser, which tags slice impls with it
pub(crate) const SLICE_IMPL_NAME: &str = "[]";

impl<'hir> Scope<'hir> {
    pub fn new(arena: &'hir bumpalo::Bump) -> Self {
        Self {
            arena,
            mangler: Mangler::default(),
            symbols: SymbolTable::new(),
            signatures: IndexVec::new(),
            functions: HashMap::new(),
            methods: HashMap::new(),
            structs: IndexVec::new(),
            struct_map: HashMap::new(),
            enums: IndexVec::new(),
            enum_map: HashMap::new(),
            arrays: ArrayTable::default(),
            enum_variants: HashMap::new(),
            interfaces: HashMap::new(),
            interface_impls: HashSet::new(),
            constants: HashMap::new(),
            docs: HashMap::new(),
            generic_structs: HashMap::new(),
            generic_enums: HashMap::new(),
            generic_fns: HashMap::new(),
            generic_fn_envs: HashMap::new(),
            generic_impls: Vec::new(),
            specialized_slices: HashSet::new(),
            in_std: false,
            recover: false,
            diagnostics: Diagnostics::default(),
        }
    }

    /// Record `error` and yield a poison [`Type`] so analysis can continue, or
    /// propagate it unchanged when not in recovery mode
    pub(in crate::hir) fn poison(&mut self, error: HirError<'hir>) -> Result<Type, HirError<'hir>> {
        self.recover
            .then(|| Type::error(self.diagnostics.emit(error.into())))
            .ok_or(error)
    }

    /// Record `error` and continue, or propagate it when not recovering
    pub(in crate::hir) fn soft(&mut self, error: HirError<'hir>) -> Result<(), HirError<'hir>> {
        self.recover
            .then(|| self.diagnostics.emit(error.into()))
            .ok_or(error)
            .map(|_| ())
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
                    .and_then(|method_symbol| {
                        self.methods.get(&(receiver_type, method_symbol)).copied()
                    })
                    .ok_or_else(|| HirError {
                        kind: error_kind(function.name),
                        span: function.span,
                    })
            },

            (Some(_), None) => {
                Err(HirError { kind: error_kind(function.name), span: function.span })
            },

            (None, Some(impl_type)) => self
                .resolve_qualified_call(impl_type, function.name)
                .ok_or_else(|| HirError { kind: error_kind(function.name), span: function.span }),
            (None, None) => self
                .resolve_function(|m| m.item(function.name))
                .ok_or_else(|| HirError { kind: error_kind(function.name), span: function.span }),
        }
    }

    #[inline]
    pub(in crate::hir) fn resolve_return_type<'h>(
        &mut self,
        return_type: Option<&Spanned<statement::Type<'h>>>,
        self_type: Option<Type>,
        env: Option<&GenericEnv>,
    ) -> Result<Type, HirError<'hir>>
    where
        'h: 'hir,
    {
        return_type.map_or(Ok(Type::default()), |s| {
            self.resolve_type(s.value_ref(), s.span(), self_type, env)
                .or_else(|error| self.poison(error))
        })
    }

    #[inline]
    pub(in crate::hir) fn resolve_params<'h>(
        &mut self,
        params: &[statement::Parameter<'h>],
        self_type: Option<Type>,
        env: Option<&GenericEnv>,
    ) -> Result<Vec<Type>, HirError<'hir>>
    where
        'h: 'hir,
    {
        let mut out = Vec::with_capacity(params.len());
        for p in params {
            let typ = self
                .resolve_type(p.typ.value_ref(), p.typ.span(), self_type, env)
                .or_else(|error| self.poison(error))?;
            out.push(typ);
        }
        Ok(out)
    }

    /// Resolve an AST type annotation to a HIR [`Type`], instantiating generic struct/enum templates on-demand
    pub(in crate::hir) fn resolve_type<'h>(
        &mut self,
        typ: &statement::Type<'h>,
        span: Span,
        self_type: Option<Type>,
        env: Option<&GenericEnv>,
    ) -> Result<Type, HirError<'hir>>
    where
        'h: 'hir,
    {
        let mut resolver = ScopeResolver { scope: self, self_type, env };
        type_resolver::resolve(&mut resolver, typ, span)
    }

    /// Specialise a generic struct/enum template for the concrete `args`, returning the resulting nominal [`Type`]
    /// Specialisations are cached by mangled name
    pub(in crate::hir) fn instantiate_generic(
        &mut self,
        name: &str,
        args: &[Type],
        span: Span,
    ) -> Result<Type, HirError<'hir>> {
        let mangled = self.mangle_generic(name, args);
        let mangled_sym = self.symbols.insert(&mangled);

        // already specialised, reuse it
        if let Some(typ) = self.nominal_type(mangled_sym) {
            return Ok(typ);
        }

        let template_sym = self.symbols.get_id(name);
        if let Some(sym) = template_sym {
            if let Some(template) = self.generic_enums.get(&sym).cloned() {
                return self.instantiate_enum(&template, mangled_sym, args, span);
            }
            if let Some(template) = self.generic_structs.get(&sym).cloned() {
                return self.instantiate_struct(&template, mangled_sym, args, span);
            }
        }

        Err(hir_error!(span, UnknownType { name: self.arena.alloc_str(name) }))
    }

    fn instantiate_enum(
        &mut self,
        template: &statement::Enum<'hir>,
        mangled_sym: SymbolId,
        args: &[Type],
        span: Span,
    ) -> Result<Type, HirError<'hir>> {
        self.check_generic_args(template.name, &template.generics, args, span)?;

        let repr = match &template.repr {
            None => EnumRepr::minimal_for(&template.variants),
            Some(explicit) => EnumRepr::try_from(explicit.value()).map_err(|_| {
                hir_error!(
                    explicit.span(),
                    TypeMismatch {
                        expected: TypeKind::I32.into(),
                        found: Type::from_primitive_ast(&explicit.value()).unwrap_or_default(),
                    }
                )
            })?,
        };
        let id = EnumId::new(self.enums.len() as u32, repr);

        self.enum_map.insert(mangled_sym, id);
        self.enums.push(Enum {
            id,
            name: mangled_sym,
            decl_span: Span::default(),
            variants: Vec::new(),
            repr,
            layout: Layout::default(),
            payload_offset: 0,
            generics: declared_names(&template.generics, args, &mut self.symbols),
        });

        let env = build_substitution(&template.generics, args);
        let mut seen = HashSet::new();
        let mut next_value = 0;
        let mut variants = Vec::with_capacity(template.variants.len());

        for variant in &template.variants {
            let variant_symbol = self.symbols.insert(variant.name);
            if !seen.insert(variant_symbol) {
                return Err(hir_error!(variant.span, DuplicateVariant { name: variant.name }));
            }

            let value = variant.value.unwrap_or(next_value);
            next_value = value + 1;

            let payload = match variant.payload.as_ref() {
                None => None,
                Some(typ) => Some(
                    self.resolve_type(typ.value_ref(), typ.span(), None, Some(&env))
                        .or_else(|error| self.poison(error))?,
                ),
            };

            self.enum_variants.insert((mangled_sym, variant_symbol), (id, value));
            variants.push(EnumVariant { name: variant_symbol, value, payload });
        }

        self.enums[id].variants = variants;

        let receiver_type = Type::enumerable(id);
        self.specialize_impls(template.name, mangled_sym, receiver_type, args)?;

        Ok(receiver_type)
    }

    fn instantiate_struct(
        &mut self,
        template: &statement::Struct<'hir>,
        mangled_sym: SymbolId,
        args: &[Type],
        span: Span,
    ) -> Result<Type, HirError<'hir>> {
        self.check_generic_args(template.name, &template.generics, args, span)?;

        let id = StructId(self.structs.len() as u32);
        self.struct_map.insert(mangled_sym, id);
        self.structs.push(Struct {
            id,
            name: mangled_sym,
            decl_span: Span::default(),
            fields: Vec::new(),
            repr: template.repr,
            layout: Layout::default(),
            generics: declared_names(&template.generics, args, &mut self.symbols),
        });

        let env = build_substitution(&template.generics, args);
        let mut fields = Vec::with_capacity(template.fields.len());
        for field in &template.fields {
            let typ = self
                .resolve_type(field.typ.value_ref(), field.typ.span(), None, Some(&env))
                .or_else(|error| self.poison(error))?;
            fields.push(StructField { name: self.symbols.insert(field.name), typ, offset: 0 });
        }
        self.structs[id].fields = fields;

        let receiver_type = Type::structure(id);
        self.specialize_impls(template.name, mangled_sym, receiver_type, args)?;

        Ok(receiver_type)
    }

    /// Validate that `args` has the right arity and satisfies every bound declared on `generics`.
    fn check_generic_args(
        &self,
        name: &'hir str,
        generics: &[statement::GenericBound<'hir>],
        args: &[Type],
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        if generics.len() != args.len() {
            return Err(hir_error!(
                span,
                ArityMismatch { name, expected: generics.len(), found: args.len() }
            ));
        }
        for (param, &concrete_type) in generics.iter().zip(args) {
            for bound in &param.bounds {
                let interface_name = match bound.value_ref() {
                    statement::Type::Named(n) => n,
                    statement::Type::Generic(n, _) => n,
                    _ => continue,
                };
                let satisfied = matches!(concrete_type.kind(), TypeKind::GenericParam(_))
                    || self
                        .symbols
                        .get_id(interface_name)
                        .is_some_and(|sym| self.interface_impls.contains(&(concrete_type, sym)));
                if !satisfied {
                    return Err(hir_error!(
                        span,
                        UnsatisfiedBound { type_name: concrete_type, bound_name: interface_name }
                    ));
                }
            }
        }
        Ok(())
    }

    /// Instantiate every `impl Name<T>` block from `generic_impls` that matches `template_name`
    /// into `receiver_type`, using `args` as the substitution
    fn specialize_impls(
        &mut self,
        template_name: &str,
        mangled_sym: SymbolId,
        receiver_type: Type,
        args: &[Type],
    ) -> Result<(), HirError<'hir>> {
        let matching: Vec<_> =
            self.generic_impls.iter().filter(|i| i.name == template_name).cloned().collect();
        for implementation in &matching {
            let impl_env = build_impl_substitution(implementation, args);

            if let Some(interface_name) = implementation.interface {
                self.interface_impls
                    .insert((receiver_type, self.symbols.insert(interface_name)));
            }

            for method in &implementation.methods {
                let method_symbol = self.symbols.insert(method.name);
                let receiver_name = self.symbols.get(mangled_sym);
                let mangled = match implementation.interface {
                    Some(iface) => self.symbols.insert(&self.mangler.interface_item(
                        receiver_name,
                        iface,
                        method.name,
                    )),
                    None => {
                        self.symbols.insert(&self.mangler.scoped_item(receiver_name, method.name))
                    },
                };

                let mut method_env = impl_env.clone();
                method_env.extend(generic_param_env(&method.generics));

                let mut params = Vec::new();
                if let Some(receiver) = method.receiver {
                    params.push(receiver_param_type(receiver_type, receiver.mutable));
                }
                params.extend(self.resolve_params(
                    &method.params,
                    Some(receiver_type),
                    Some(&method_env),
                )?);
                let return_type = self.resolve_return_type(
                    method.return_type.as_ref(),
                    Some(receiver_type),
                    Some(&method_env),
                )?;

                let intrinsic = intrinsic_method(false, implementation.name, method.name);
                let kind = match (intrinsic, method.receiver) {
                    (Some(i), _) => FunctionKind::Intrinsic(i),
                    (None, Some(r)) => FunctionKind::Method(Method {
                        receiver: receiver_type,
                        name: method_symbol,
                        mutable: r.mutable,
                    }),
                    (None, None) => FunctionKind::Free,
                };

                let sig_id = self.push_signature(FunctionSignature {
                    name: mangled,
                    params,
                    return_type,
                    kind,
                    is_const: method.is_const,
                });

                if method.receiver.is_some() {
                    self.methods.insert((receiver_type, method_symbol), sig_id);
                } else {
                    self.functions.insert(mangled, sig_id);
                }

                if intrinsic.is_none() {
                    self.generic_fns.insert(sig_id, method.clone());
                    self.generic_fn_envs.insert(sig_id, impl_env.clone());
                }
            }
        }

        Ok(())
    }

    pub(in crate::hir) fn specialize_slice_impls(
        &mut self,
        slice_type: Type,
    ) -> Result<(), HirError<'hir>> {
        let TypeKind::Slice { element, .. } = slice_type.kind() else {
            return Ok(());
        };

        let canonical = Type::slice(element, false);
        if !self.specialized_slices.insert(canonical) {
            return Ok(());
        }

        let element = Type::from(element);
        let mangled = self.mangle_generic("slice", &[element]);
        let mangled_sym = self.symbols.insert(&mangled);
        self.specialize_impls(SLICE_IMPL_NAME, mangled_sym, canonical, &[element])
    }

    pub(in crate::hir) fn mangle_generic(&self, base: &str, args: &[Type]) -> String {
        let mut mangled = String::from(base);
        for &arg in args {
            mangled.push('$');
            mangled.push_str(&self.mangle_component(arg));
        }
        mangled
    }

    fn mangle_component(&self, typ: Type) -> String {
        match typ.kind() {
            TypeKind::Struct(id) => self.symbols.get(self.structs[id].name).to_string(),
            TypeKind::Enum(id) => self.symbols.get(self.enums[id].name).to_string(),
            TypeKind::Ref { to, .. } => {
                format!("ref_{}", self.mangle_component(Type::from(to)))
            },
            TypeKind::GenericParam(i) => format!("T{i}"),
            other => other.mangled().to_string(),
        }
    }

    /// Whether a function/method is a generic template that must not be lowered directly
    pub(in crate::hir) fn is_generic_function(&self, function: &statement::Function<'_>) -> bool {
        if !function.generics.is_empty() {
            return true;
        }

        function.impl_type.is_some_and(|impl_type| {
            // `impl [T]` methods are generic over the element and specialised on demand
            impl_type == SLICE_IMPL_NAME
                || self.symbols.get_id(impl_type).is_some_and(|sym| {
                    self.generic_enums.contains_key(&sym) || self.generic_structs.contains_key(&sym)
                })
        })
    }

    /// push a new function signature and returns its assigned [`FunctionId`]
    #[inline]
    pub(in crate::hir) fn push_signature(&mut self, signature: FunctionSignature) -> FunctionId {
        let id = FunctionId(self.signatures.len() as u32);
        self.signatures.push(signature);
        id
    }

    #[inline]
    pub(in crate::hir) fn nominal_type(&self, symbol: SymbolId) -> Option<Type> {
        nominal_type(&self.struct_map, &self.enum_map, symbol)
    }

    #[inline]
    pub(in crate::hir) fn lookup_named_type(&self, name: &str) -> Option<Type> {
        resolve_primitive_type(name)
            .or_else(|| self.symbols.get_id(name).and_then(|s| self.nominal_type(s)))
    }

    pub(in crate::hir) fn resolve_function<F>(&self, operation: F) -> Option<FunctionId>
    where
        F: FnOnce(&Mangler) -> String,
    {
        let mangled = operation(&self.mangler);
        self.symbols.get_id(&mangled).and_then(|s| self.functions.get(&s).copied())
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
        self.interface_impls.iter().filter(|&&(t, _)| t == receiver_type).find_map(
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
}

impl ArrayTable {
    /// intern `[element; len]`, reusing the existing id when the type is already known
    ///
    /// an inferred element is never cached: its variable is meaningful only within
    /// the body that minted it, so a fresh id keeps bodies from aliasing each other
    pub fn intern(&self, element: Type, len: u32) -> ArrayId {
        let cacheable = !element.is_infer();
        if cacheable && let Some(&id) = self.lookup.borrow().get(&(element, len)) {
            return id;
        }

        let mut types = self.types.borrow_mut();
        let id = ArrayId(types.len() as u32);
        types.push(ArrayType { element, len });
        if cacheable {
            self.lookup.borrow_mut().insert((element, len), id);
        }

        id
    }

    #[inline]
    pub fn get(&self, id: ArrayId) -> ArrayType {
        self.types.borrow()[id]
    }

    /// pins the element of an inferred array once its variable is resolved
    ///
    /// safe only for uncached (inferred) arrays, which belong to a single body
    /// concrete arrays are shared and must never be mutated under another's feet
    #[inline]
    pub fn resolve(&self, id: ArrayId, element: Type) {
        self.types.borrow_mut()[id].element = element;
    }

    #[inline]
    pub fn snapshot(&self) -> IndexVec<ArrayId, ArrayType> {
        self.types.borrow().clone()
    }
}

impl FunctionSignature {
    #[inline]
    pub(in crate::hir) fn has_receiver(&self) -> bool {
        matches!(self.kind, FunctionKind::Method(_))
    }

    #[inline]
    pub(in crate::hir) fn receiver_type(&self) -> Option<Type> {
        self.has_receiver().then(|| self.params[0])
    }

    #[inline]
    pub(in crate::hir) fn receiver_mutable(&self) -> bool {
        self.receiver_type()
            .is_some_and(|t| matches!(t.kind(), TypeKind::Ref { mutable: true, .. }))
    }

    #[inline]
    pub(in crate::hir) fn explicit_params(&self) -> &[Type] {
        &self.params[self.has_receiver() as usize..]
    }
}

impl<'s> Index<StructId> for Scope<'s> {
    type Output = Struct;
    fn index(&self, id: StructId) -> &Struct {
        &self.structs[id]
    }
}

impl<'s> Index<EnumId> for Scope<'s> {
    type Output = Enum;
    fn index(&self, id: EnumId) -> &Enum {
        &self.enums[id]
    }
}

impl<'a, 'hir> TypeResolver<'hir> for ScopeResolver<'a, 'hir> {
    fn named(&mut self, name: &'hir str, span: Span) -> Result<Type, HirError<'hir>> {
        if let Some(env) = self.env
            && let Some(&t) = env.get(name)
        {
            return Ok(t);
        }

        self.scope
            .symbols
            .get_id(name)
            .and_then(|symbol| self.scope.nominal_type(symbol))
            .ok_or_else(|| hir_error!(span, UnknownType { name }))
    }

    fn generic(
        &mut self,
        name: &'hir str,
        args: &[Type],
        span: Span,
    ) -> Result<Type, HirError<'hir>> {
        self.scope.instantiate_generic(name, args, span)
    }

    fn self_type(&mut self, _span: Span) -> Result<Type, HirError<'hir>> {
        Ok(self.self_type.unwrap_or(TypeKind::SelfType.into()))
    }

    fn arrays(&self) -> &ArrayTable {
        &self.scope.arrays
    }
}

#[inline(always)]
pub(in crate::hir) fn resolve_primitive_type(name: &str) -> Option<Type> {
    statement::Type::from_str(name).and_then(|ast_ty| Type::from_primitive_ast(&ast_ty))
}

#[inline]
pub(in crate::hir) fn nominal_type(
    struct_map: &Structs,
    enum_map: &Enums,
    symbol: SymbolId,
) -> Option<Type> {
    struct_map
        .get(&symbol)
        .copied()
        .map(Type::structure)
        .or_else(|| enum_map.get(&symbol).copied().map(Type::enumerable))
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

fn build_substitution(generics: &[statement::GenericBound<'_>], args: &[Type]) -> GenericEnv {
    generics.iter().zip(args).map(|(g, &arg)| (g.name.to_string(), arg)).collect()
}

/// The declared parameter names of an *identity* instantiation (every argument
/// still an open `GenericParam`), so displays can say `Result<S, F>` instead of
/// positional `T0`/`T1` names. Empty for concrete instantiations
fn declared_names(
    generics: &[statement::GenericBound<'_>],
    args: &[Type],
    symbols: &mut SymbolTable,
) -> Vec<SymbolId> {
    let identity =
        !args.is_empty() && args.iter().all(|arg| matches!(arg.kind(), TypeKind::GenericParam(_)));

    match identity {
        true => generics.iter().map(|g| symbols.insert(g.name)).collect(),
        false => Vec::new(),
    }
}

pub(in crate::hir) fn receiver_param_type(receiver: Type, mutable: bool) -> Type {
    match receiver.kind() {
        TypeKind::Slice { element, .. } => Type::slice(element, mutable),
        _ => Type::receiver_ref(receiver, mutable),
    }
}

fn build_impl_substitution(implementation: &statement::Impl<'_>, args: &[Type]) -> GenericEnv {
    if !implementation.generics.is_empty() {
        return build_substitution(&implementation.generics, args);
    }

    if let statement::Type::Slice(element, _) = implementation.receiver.value_ref()
        && let statement::Type::Named(name) = element.as_ref()
        && let Some(&concrete) = args.first()
    {
        return HashMap::from([(name.to_string(), concrete)]);
    }

    let statement::Type::Generic(_, receiver_args) = implementation.receiver.value_ref() else {
        return HashMap::new();
    };

    receiver_args
        .iter()
        .zip(args)
        .filter_map(|(arg, &concrete)| {
            let statement::Type::Named(name) = arg.value_ref() else {
                return None;
            };
            Some((name.to_string(), concrete))
        })
        .collect()
}

#[inline(always)]
pub(in crate::hir) fn intrinsic_method(
    in_std: bool,
    receiver: &str,
    method: &str,
) -> Option<Intrinsic> {
    match (in_std, receiver, method) {
        (true, "str", "len") | (_, SLICE_IMPL_NAME, "len") => Some(Intrinsic::Len),
        (true, _, "wrapping_add") => Some(Intrinsic::WrappingAdd),
        (true, _, "wrapping_sub") => Some(Intrinsic::WrappingSub),
        (true, _, "wrapping_mul") => Some(Intrinsic::WrappingMul),
        _ => None,
    }
}
pub(in crate::hir) fn generic_param_env(generics: &[statement::GenericBound<'_>]) -> GenericEnv {
    generics
        .iter()
        .enumerate()
        .map(|(i, g)| (g.name.to_string(), Type::generic_param(i as u8)))
        .collect()
}
