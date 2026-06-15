use crate::{
    hir::{
        self, ArrayId, ArrayType, Constant, Enum, EnumId, EnumRepr, EnumVariant, Function,
        FunctionId, FunctionKind, Intrinsic, Layout, Method, RefTarget, Struct, StructField,
        StructId, SymbolId, SymbolTable, Type, TypeKind, constants,
        declarations::Declarations,
        diagnostics::Diagnostics,
        error::{HirError, HirErrorKind, hir_error},
        index_vec::IndexVec,
        interfaces,
        lower::{self},
        structs,
        symbols::{Mangler, qualified},
    },
    lexer::{Spanned, token::Span},
    parser::statement,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Index,
    str::FromStr,
};

/// The single accumulated namespace for a compilation
///
/// Grows incrementally as modules are loaded: structs and function
/// signatures are assigned monotonically increasing IDs across all modules
pub struct Scope<'hir> {
    pub(in crate::hir) arena: &'hir bumpalo::Bump,
    pub(in crate::hir) mangler: Mangler<'hir>,
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
    pub constants: HashMap<SymbolId, Constant<'hir>>,
    /// Rendered `///` documentation per item, keyed by its `decl_span`
    pub docs: HashMap<Span, Box<str>>,

    pub generic_structs: HashMap<SymbolId, statement::Struct<'hir>>,
    pub generic_enums: HashMap<SymbolId, statement::Enum<'hir>>,
    /// Generic free-function templates keyed by the [`FunctionId`] of their (open) signature
    pub generic_fns: HashMap<FunctionId, statement::Function<'hir>>,
    pub generic_fn_envs: HashMap<FunctionId, GenericEnv>,
    pub generic_impls: Vec<statement::Impl<'hir>>,

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

impl<'hir> Scope<'hir> {
    pub fn new(arena: &'hir bumpalo::Bump) -> Self {
        Self {
            arena,
            mangler: Mangler::default(),
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

    /// Analyse `declarations` and extend this scope with their declarations
    ///
    /// Ids are assigned relative to what is already in the scope so this can be called
    /// once per module in dependency order
    pub fn extend<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
        in_std: bool,
        arena: &'hir bumpalo::Bump,
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        self.collect_docs(declarations);
        self.extend_enums(declarations, symbols)?;
        self.extend_structs(declarations, symbols)?;
        self.extend_interfaces(declarations, symbols)?;
        self.extend_signatures(declarations, symbols, in_std)?;
        constants::extend(self, declarations, symbols, in_std, arena)?;
        interfaces::validate(self, declarations, symbols)?;

        Ok(())
    }

    fn collect_docs(&mut self, declarations: &Declarations<'_, '_>) {
        for (span, lines) in &declarations.docs {
            if let Some(joined) = hir::join_docs(lines) {
                self.docs.insert(*span, joined);
            }
        }
    }

    pub fn lower_functions<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
        in_std: bool,
        arena: &'hir bumpalo::Bump,
    ) -> Result<IndexVec<FunctionId, Function<'hir>>, HirError<'hir>>
    where
        's: 'hir,
    {
        self.lower_matching_functions(declarations, symbols, in_std, |_| true, arena)
    }

    pub(in crate::hir) fn lower_matching_functions<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
        in_std: bool,
        mut should_lower: impl FnMut(FunctionId) -> bool,
        arena: &'hir bumpalo::Bump,
    ) -> Result<IndexVec<FunctionId, Function<'hir>>, HirError<'hir>>
    where
        's: 'hir,
    {
        let mut lowered = IndexVec::new();
        // under recovery a duplicate declaration resolves to the surviving id
        // lowering it again would clone the function
        let mut seen = HashSet::new();
        for function in declarations.functions() {
            if self.is_generic_function(function, symbols) {
                continue;
            }

            let id = match self
                .function_id(function, symbols, None, |name| HirErrorKind::UnknownFunction { name })
            {
                Ok(id) => id,
                Err(e) => {
                    self.soft(e)?;
                    continue;
                },
            };

            let is_intrinsic = matches!(self.signatures[id].kind, FunctionKind::Intrinsic(_));

            if !should_lower(id) || is_intrinsic || !seen.insert(id) {
                continue;
            }

            match lower::FunctionBuilder::new(self, symbols, id, function, in_std, arena).lower() {
                Ok(function) => lowered.push(function),
                // anything that escaped body-level recovery taints only this function
                Err(error) => self.soft(error)?,
            }
        }

        Ok(lowered)
    }

    fn extend_structs<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        let offset = self.structs.len() as u32;
        let mut local_map = Structs::new();
        let mut local_declarations = Vec::new();

        for struct_decl in &declarations.structs {
            let symbol = symbols.insert(struct_decl.name);

            let already_exists = self.struct_map.contains_key(&symbol)
                || self.enum_map.contains_key(&symbol)
                || local_map.contains_key(&symbol);

            if already_exists {
                self.soft(hir_error!(
                    struct_decl.span,
                    DuplicateStruct { name: struct_decl.name }
                ))?;
                continue;
            }

            if !struct_decl.generics.is_empty() {
                self.generic_structs.insert(symbol, (*struct_decl).clone());
                continue;
            }

            let local_id = StructId(local_declarations.len() as u32);
            local_map.insert(symbol, local_id);
            local_declarations.push((symbol, *struct_decl));
        }

        if local_declarations.is_empty() {
            return Ok(());
        }

        let mut lowered = vec![None; local_declarations.len()];

        structs::lower_structs(
            &local_declarations,
            &local_map,
            &self.enum_map,
            &self.arrays,
            symbols,
            &mut lowered,
            self.recover.then_some(&mut self.diagnostics),
        )?;

        for mut s in lowered.into_iter().map(|s| s.expect("every struct must be lowered")) {
            s.id = StructId(s.id.0 + offset);
            for field in &mut s.fields {
                if let TypeKind::Struct(mut id) = field.typ.kind() {
                    id.0 += offset;
                    field.typ = Type::structure(id);
                }
            }
            self.struct_map.insert(s.name, s.id);
            self.structs.push(s);
        }

        Ok(())
    }

    fn extend_enums<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        for enum_decl in &declarations.enums {
            let symbol = symbols.insert(enum_decl.name);
            if self.enum_map.contains_key(&symbol)
                || self.struct_map.contains_key(&symbol)
                || self.generic_enums.contains_key(&symbol)
                || declarations.structs.iter().any(|s| s.name == enum_decl.name)
            {
                self.soft(hir_error!(enum_decl.span, DuplicateEnum { name: enum_decl.name }))?;
                continue;
            }

            if !enum_decl.generics.is_empty() {
                self.generic_enums.insert(symbol, (*enum_decl).clone());
                continue;
            }

            let repr = match &enum_decl.repr {
                None => EnumRepr::minimal_for(&enum_decl.variants),
                Some(explicit) => match EnumRepr::try_from(explicit.value()) {
                    Ok(repr) => repr,
                    _ => {
                        self.soft(hir_error!(
                            explicit.span(),
                            TypeMismatch {
                                expected: TypeKind::I32.into(),
                                found: Type::from_primitive_ast(&explicit.value())
                                    .unwrap_or_default(),
                            }
                        ))?;

                        continue;
                    },
                },
            };
            let id = EnumId::new(self.enums.len() as u32, repr);

            self.enum_map.insert(symbol, id);
            self.enums.push(Enum {
                id,
                name: symbol,
                decl_span: enum_decl.span,
                variants: Vec::new(),
                repr,
                layout: Layout::default(),
                payload_offset: 0,
                generics: Vec::new(),
            });

            let mut seen = HashSet::new();
            let mut next_value = 0;
            let mut variants = Vec::with_capacity(enum_decl.variants.len());

            for variant in &enum_decl.variants {
                let variant_symbol = symbols.insert(variant.name);
                if !seen.insert(variant_symbol) {
                    self.soft(hir_error!(variant.span, DuplicateVariant { name: variant.name }))?;
                    continue;
                }

                let value = variant.value.unwrap_or(next_value);
                next_value = value + 1;

                // resolve a tagged-union variant payload (e.g. `Some(T)`), a concrete
                // `Type::Generic` here instantiates its template on-demand
                let payload = match variant.payload.as_ref() {
                    None => None,
                    Some(typ) => Some(
                        self.resolve_type(typ.value_ref(), typ.span(), symbols, None, None)
                            .or_else(|error| self.poison(error))?,
                    ),
                };

                self.enum_variants.insert((symbol, variant_symbol), (id, value));
                variants.push(EnumVariant { name: variant_symbol, value, payload });
            }

            self.enums[id].variants = variants;
        }

        Ok(())
    }

    fn extend_interfaces<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        for interface in &declarations.interfaces {
            let name = symbols.insert(interface.name);
            if self.interfaces.contains_key(&name) {
                self.soft(hir_error!(interface.span, DuplicateInterface { name: interface.name }))?;
                continue;
            }

            let superinterfaces =
                interface.superinterfaces.iter().map(|name| symbols.insert(name)).collect();

            let generic_params: Vec<SymbolId> =
                interface.generics.iter().map(|g| symbols.insert(g.name)).collect();

            let param_env: GenericEnv = interface
                .generics
                .iter()
                .enumerate()
                .map(|(i, g)| (g.name.to_owned(), Type::generic_param(i as u8)))
                .collect();

            let env = (!param_env.is_empty()).then_some(&param_env);
            let mut methods = Vec::with_capacity(interface.methods.len());
            for method in &interface.methods {
                let method_name = symbols.insert(method.name);
                let has_receiver = method.receiver.is_some();
                let receiver_mut = method.receiver.map(|r| r.mutable).unwrap_or(false);

                let params = self.resolve_params(&method.params, symbols, None, env)?;
                let return_type =
                    self.resolve_return_type(method.return_type.as_ref(), symbols, None, env)?;

                methods.push(InterfaceMethodSignature {
                    name: method_name,
                    params,
                    return_type,
                    has_receiver,
                    receiver_mut,
                });
            }

            self.interfaces.insert(
                name,
                InterfaceSignature { name, superinterfaces, methods, generic_params },
            );
        }

        Ok(())
    }

    fn extend_signatures<'d, 'h>(
        &mut self,
        declarations: &Declarations<'d, 'h>,
        symbols: &mut SymbolTable,
        in_std: bool,
    ) -> Result<(), HirError<'hir>>
    where
        'h: 'hir,
    {
        // register generic impls up-front: a free-function signature like
        // `fn f(b: &Box<T>)` instantiates `Box$T0` and specialises its methods
        // from `generic_impls`, which must already be populated
        for implementation in declarations.impls.iter().copied() {
            if is_generic_impl(implementation) {
                self.generic_impls.push(implementation.clone());
            }
        }

        for function in &declarations.functions {
            if function.receiver.is_some() {
                self.soft(hir_error!(function.span, ReceiverOutsideImpl))?;
                continue;
            }

            let symbol = symbols.insert(&self.mangler.item(function.name));
            if self.functions.contains_key(&symbol) {
                self.soft(hir_error!(function.span, DuplicateFunction { name: function.name }))?;
                continue;
            }

            if !function.generics.is_empty() {
                let env = generic_param_env(&function.generics);
                let params = self.resolve_params(&function.params, symbols, None, Some(&env))?;
                let return_type = self.resolve_return_type(
                    function.return_type.as_ref(),
                    symbols,
                    None,
                    Some(&env),
                )?;

                let sig = FunctionSignature {
                    name: symbol,
                    params,
                    return_type,
                    kind: FunctionKind::Free,
                    is_const: function.is_const,
                };
                let id = self.push_signature(sig);
                self.functions.insert(symbol, id);
                self.generic_fns.insert(id, (*function).clone());

                continue;
            }

            let params = self.resolve_params(&function.params, symbols, None, None)?;
            let return_type =
                self.resolve_return_type(function.return_type.as_ref(), symbols, None, None)?;
            let intrinsic = in_std.then(|| Intrinsic::from_str(function.name).ok()).flatten();
            let kind = match intrinsic {
                Some(i) => FunctionKind::Intrinsic(i),
                None => FunctionKind::Free,
            };

            let sig = FunctionSignature {
                name: symbol,
                params,
                return_type,
                kind,
                is_const: function.is_const,
            };
            let id = self.push_signature(sig);
            self.functions.insert(symbol, id);
        }

        // when compiling std, inject a built-in 'syscall' signature so std modules
        // don't need to declare it
        if in_std {
            let syscall_sym = symbols.insert(&self.mangler.item("syscall"));
            if !self.functions.contains_key(&syscall_sym) {
                let id = FunctionId(self.signatures.len() as u32);
                self.functions.insert(syscall_sym, id);
                self.signatures.push(FunctionSignature {
                    name: syscall_sym,
                    params: vec![],
                    return_type: TypeKind::Iptr.into(),
                    kind: FunctionKind::Intrinsic(Intrinsic::Syscall),
                    is_const: false,
                });
            }
        }

        self.extend_impl_signatures(declarations, symbols, in_std)?;

        Ok(())
    }

    fn extend_impl_signatures<'d, 'h>(
        &mut self,
        declarations: &Declarations<'d, 'h>,
        symbols: &mut SymbolTable,
        in_std: bool,
    ) -> Result<(), HirError<'hir>>
    where
        'h: 'hir,
    {
        for implementation in declarations.impls.iter().copied() {
            // generic impls were collected in `extend_signatures`; specialised on demand
            if is_generic_impl(implementation) {
                continue;
            }

            let receiver_type =
                match self.impl_receiver_type(implementation, declarations, symbols, in_std) {
                    Ok(receiver_type) => receiver_type,
                    Err(error) => {
                        self.soft(error)?;
                        continue;
                    },
                };

            let impl_param_env =
                self.build_impl_param_env(implementation, receiver_type, symbols)?;
            let impl_env_ref = impl_param_env.as_ref();

            for method in &implementation.methods {
                let method_symbol = symbols.insert(method.name);
                let mangled = match implementation.interface {
                    Some(interface) => symbols.insert(&self.mangler.interface_item(
                        implementation.name,
                        interface,
                        method.name,
                    )),
                    _ => {
                        symbols.insert(&self.mangler.scoped_item(implementation.name, method.name))
                    },
                };

                match method.receiver {
                    Some(receiver) => {
                        if self.methods.contains_key(&(receiver_type, method_symbol)) {
                            self.soft(hir_error!(
                                method.span,
                                DuplicateMethod {
                                    struct_name: implementation.name,
                                    name: method.name,
                                }
                            ))?;
                            continue;
                        }

                        let id = FunctionId(self.signatures.len() as u32);
                        self.methods.insert((receiver_type, method_symbol), id);

                        let mut params = Vec::with_capacity(method.params.len() + 1);
                        params.push(Type::receiver_ref(receiver_type, receiver.mutable));
                        params.extend(self.resolve_params(
                            &method.params,
                            symbols,
                            Some(receiver_type),
                            impl_env_ref,
                        )?);
                        let return_type = self.resolve_return_type(
                            method.return_type.as_ref(),
                            symbols,
                            Some(receiver_type),
                            impl_env_ref,
                        )?;

                        let intrinsic = intrinsic_method(in_std, implementation.name, method.name);

                        let kind = match intrinsic {
                            Some(i) => FunctionKind::Intrinsic(i),
                            None => FunctionKind::Method(Method {
                                receiver: receiver_type,
                                name: method_symbol,
                                mutable: receiver.mutable,
                            }),
                        };
                        self.signatures.push(FunctionSignature {
                            name: mangled,
                            params,
                            return_type,
                            kind,
                            is_const: method.is_const,
                        });
                    },

                    None => {
                        if self.functions.contains_key(&mangled) {
                            let name = qualified(self.arena, implementation.name, method.name);
                            self.soft(hir_error!(method.span, DuplicateFunction { name }))?;
                            continue;
                        }

                        let id = FunctionId(self.signatures.len() as u32);
                        self.functions.insert(mangled, id);

                        let params = self.resolve_params(
                            &method.params,
                            symbols,
                            Some(receiver_type),
                            impl_env_ref,
                        )?;
                        let return_type = self.resolve_return_type(
                            method.return_type.as_ref(),
                            symbols,
                            Some(receiver_type),
                            impl_env_ref,
                        )?;

                        self.signatures.push(FunctionSignature {
                            name: mangled,
                            params,
                            return_type,
                            is_const: method.is_const,
                            kind: FunctionKind::Free,
                        });
                    },
                }
            }
        }
        Ok(())
    }

    /// Resolve the receiver type an `impl` block attaches to, enforcing the orphan rule
    fn impl_receiver_type<'h>(
        &mut self,
        implementation: &statement::Impl<'h>,
        declarations: &Declarations<'_, 'h>,
        symbols: &mut SymbolTable,
        in_std: bool,
    ) -> Result<Type, HirError<'hir>>
    where
        'h: 'hir,
    {
        if let Some(primitive) = resolve_primitive_type(implementation.name) {
            return match in_std {
                true => Ok(primitive),
                false => {
                    Err(hir_error!(implementation.span, OrphanImpl { name: implementation.name }))
                },
            };
        }

        let is_local = declarations.structs.iter().any(|s| s.name == implementation.name)
            || declarations.enums.iter().any(|e| e.name == implementation.name);
        let symbol = symbols.insert(implementation.name);

        match (self.nominal_type(symbol), is_local) {
            (Some(typ), true) => Ok(typ),
            (Some(_), false) => {
                Err(hir_error!(implementation.span, OrphanImpl { name: implementation.name }))
            },
            (None, _) => {
                Err(hir_error!(implementation.span, UnknownType { name: implementation.name }))
            },
        }
    }

    /// Register a `impl T with Iface` relation and, if `Iface` is generic,
    /// build the param-name → concrete-type map used when resolving method
    /// annotations.
    fn build_impl_param_env<'h>(
        &mut self,
        implementation: &statement::Impl<'h>,
        receiver_type: Type,
        symbols: &mut SymbolTable,
    ) -> Result<Option<GenericEnv>, HirError<'hir>>
    where
        'h: 'hir,
    {
        let Some(interface_name) = implementation.interface else {
            return Ok(None);
        };

        let interface_sym = symbols.insert(interface_name);
        self.interface_impls.insert((receiver_type, interface_sym));

        let Some(interface) = self
            .interfaces
            .get(&interface_sym)
            .filter(|interface| !interface.generic_params.is_empty())
        else {
            return Ok(None);
        };
        let generic_params = interface.generic_params.clone();

        let explicit_args = match implementation.interface_type.as_ref().map(|s| s.value()) {
            Some(statement::Type::Generic(_, args)) => {
                let mut resolved = Vec::with_capacity(args.len());
                for arg in &args {
                    let typ = self
                        .resolve_type(
                            arg.value_ref(),
                            arg.span(),
                            symbols,
                            Some(receiver_type),
                            None,
                        )
                        .or_else(|error| self.poison(error))?;
                    resolved.push(typ);
                }
                resolved
            },
            _ => Vec::new(),
        };

        let env = generic_params
            .into_iter()
            .enumerate()
            .map(|(i, sym)| {
                let name = symbols.get(sym).to_owned();
                let typ = explicit_args.get(i).copied().unwrap_or(receiver_type);
                (name, typ)
            })
            .collect();

        Ok(Some(env))
    }

    #[inline]
    pub(in crate::hir) fn function_id<'s>(
        &self,
        function: &statement::Function<'s>,
        symbols: &mut SymbolTable,
        hint: Option<&'s str>,
        error_kind: impl FnOnce(&'s str) -> HirErrorKind<'s>,
    ) -> Result<FunctionId, HirError<'s>> {
        let impl_type = function.impl_type.or(hint);

        match (function.receiver, impl_type) {
            (Some(_), Some(impl_type)) => {
                let receiver_type = self.lookup_named_type(impl_type, symbols).ok_or(HirError {
                    kind: HirErrorKind::UnknownType { name: impl_type },
                    span: function.span,
                })?;

                let method_symbol = symbols.insert(function.name);
                self.methods
                    .get(&(receiver_type, method_symbol))
                    .copied()
                    .ok_or_else(|| HirError {
                        kind: error_kind(function.name),
                        span: function.span,
                    })
            },

            (Some(_), None) => {
                Err(HirError { kind: error_kind(function.name), span: function.span })
            },

            (None, Some(impl_type)) => self
                .resolve_qualified_call(impl_type, function.name, symbols)
                .ok_or_else(|| HirError { kind: error_kind(function.name), span: function.span }),
            (None, None) => self
                .resolve_function(symbols, |m| m.item(function.name))
                .ok_or_else(|| HirError { kind: error_kind(function.name), span: function.span }),
        }
    }

    #[inline]
    pub(in crate::hir) fn resolve_return_type<'h>(
        &mut self,
        return_type: Option<&Spanned<statement::Type<'h>>>,
        symbols: &mut SymbolTable,
        self_type: Option<Type>,
        env: Option<&GenericEnv>,
    ) -> Result<Type, HirError<'hir>>
    where
        'h: 'hir,
    {
        return_type.map_or(Ok(Type::default()), |s| {
            self.resolve_type(s.value_ref(), s.span(), symbols, self_type, env)
                .or_else(|error| self.poison(error))
        })
    }

    #[inline]
    pub(in crate::hir) fn resolve_params<'h>(
        &mut self,
        params: &[statement::Parameter<'h>],
        symbols: &mut SymbolTable,
        self_type: Option<Type>,
        env: Option<&GenericEnv>,
    ) -> Result<Vec<Type>, HirError<'hir>>
    where
        'h: 'hir,
    {
        let mut out = Vec::with_capacity(params.len());
        for p in params {
            let typ = self
                .resolve_type(p.typ.value_ref(), p.typ.span(), symbols, self_type, env)
                .or_else(|error| self.poison(error))?;
            out.push(typ);
        }
        Ok(out)
    }

    /// Resolve an AST type annotation to a HIR [`Type`], instantiating generic struct/enum templates on-demand
    fn resolve_type<'h>(
        &mut self,
        typ: &statement::Type<'h>,
        span: Span,
        symbols: &mut SymbolTable,
        self_type: Option<Type>,
        env: Option<&GenericEnv>,
    ) -> Result<Type, HirError<'hir>>
    where
        'h: 'hir,
    {
        match typ {
            statement::Type::Named(name) => {
                if let Some(env) = env
                    && let Some(&t) = env.get(*name)
                {
                    return Ok(t);
                }
                symbols
                    .get_id(name)
                    .and_then(|symbol| self.nominal_type(symbol))
                    .ok_or_else(|| hir_error!(span, UnknownType { name }))
            },

            statement::Type::Ref(inner) => {
                let inner = self.resolve_type(inner, span, symbols, self_type, env)?;
                let to = RefTarget::try_from(inner).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch {
                            expected: Type::structure(Default::default()),
                            found: inner
                        }
                    )
                })?;
                Ok(Type::refer(to, false))
            },

            statement::Type::SelfType => Ok(self_type.unwrap_or(TypeKind::SelfType.into())),
            statement::Type::RefSelf => self_type.map_or(
                Ok(Type::refer(RefTarget::new(TypeKind::SelfType), false)),
                |self_typ| {
                    let to = RefTarget::try_from(self_typ).map_err(|_| {
                        hir_error!(
                            span,
                            TypeMismatch {
                                expected: Type::structure(Default::default()),
                                found: self_typ,
                            }
                        )
                    })?;
                    Ok(Type::refer(to, false))
                },
            ),

            statement::Type::Generic(name, args) => {
                let mut resolved = Vec::with_capacity(args.len());
                for arg in args {
                    resolved.push(self.resolve_type(
                        arg.value_ref(),
                        arg.span(),
                        symbols,
                        self_type,
                        env,
                    )?);
                }
                self.instantiate_generic(name, &resolved, span, symbols)
            },

            statement::Type::Array(element, len) => {
                let element = self.resolve_type(element, span, symbols, self_type, env)?;
                Ok(Type::array(self.arrays.intern(element, *len as u32)))
            },

            statement::Type::Slice(element) => {
                let element = self.resolve_type(element, span, symbols, self_type, env)?;
                let element = RefTarget::try_from(element).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch { expected: Type::structure(Default::default()), found: element }
                    )
                })?;
                Ok(Type::slice(element, false))
            },

            // `Generic` is handled above; this arm only sees primitives, which `from_primitive_ast`
            // always resolves, so the error branch is effectively unreachable
            other => Type::from_primitive_ast(other)
                .ok_or_else(|| hir_error!(span, UnknownType { name: "<unsupported type>" })),
        }
    }

    /// Specialise a generic struct/enum template for the concrete `args`, returning the resulting nominal [`Type`]
    /// Specialisations are cached by mangled name
    pub(in crate::hir) fn instantiate_generic(
        &mut self,
        name: &str,
        args: &[Type],
        span: Span,
        symbols: &mut SymbolTable,
    ) -> Result<Type, HirError<'hir>> {
        let mangled = self.mangle_generic(name, args, symbols);
        let mangled_sym = symbols.insert(&mangled);

        // already specialised, reuse it
        if let Some(typ) = self.nominal_type(mangled_sym) {
            return Ok(typ);
        }

        let template_sym = symbols.get_id(name);
        if let Some(sym) = template_sym {
            if let Some(template) = self.generic_enums.get(&sym).cloned() {
                return self.instantiate_enum(&template, mangled_sym, args, span, symbols);
            }
            if let Some(template) = self.generic_structs.get(&sym).cloned() {
                return self.instantiate_struct(&template, mangled_sym, args, span, symbols);
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
        symbols: &mut SymbolTable,
    ) -> Result<Type, HirError<'hir>> {
        self.check_generic_args(template.name, &template.generics, args, span, symbols)?;

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
            generics: declared_names(&template.generics, args, symbols),
        });

        let env = build_substitution(&template.generics, args);
        let mut seen = HashSet::new();
        let mut next_value = 0;
        let mut variants = Vec::with_capacity(template.variants.len());

        for variant in &template.variants {
            let variant_symbol = symbols.insert(variant.name);
            if !seen.insert(variant_symbol) {
                return Err(hir_error!(variant.span, DuplicateVariant { name: variant.name }));
            }

            let value = variant.value.unwrap_or(next_value);
            next_value = value + 1;

            let payload = match variant.payload.as_ref() {
                None => None,
                Some(typ) => Some(
                    self.resolve_type(typ.value_ref(), typ.span(), symbols, None, Some(&env))
                        .or_else(|error| self.poison(error))?,
                ),
            };

            self.enum_variants.insert((mangled_sym, variant_symbol), (id, value));
            variants.push(EnumVariant { name: variant_symbol, value, payload });
        }

        self.enums[id].variants = variants;

        let receiver_type = Type::enumerable(id);
        self.specialize_impls(template.name, mangled_sym, receiver_type, args, symbols)?;

        Ok(receiver_type)
    }

    fn instantiate_struct(
        &mut self,
        template: &statement::Struct<'hir>,
        mangled_sym: SymbolId,
        args: &[Type],
        span: Span,
        symbols: &mut SymbolTable,
    ) -> Result<Type, HirError<'hir>> {
        self.check_generic_args(template.name, &template.generics, args, span, symbols)?;

        let id = StructId(self.structs.len() as u32);
        self.struct_map.insert(mangled_sym, id);
        self.structs.push(Struct {
            id,
            name: mangled_sym,
            decl_span: Span::default(),
            fields: Vec::new(),
            repr: template.repr,
            layout: Layout::default(),
            generics: declared_names(&template.generics, args, symbols),
        });

        let env = build_substitution(&template.generics, args);
        let mut fields = Vec::with_capacity(template.fields.len());
        for field in &template.fields {
            let typ = self
                .resolve_type(field.typ.value_ref(), field.typ.span(), symbols, None, Some(&env))
                .or_else(|error| self.poison(error))?;
            fields.push(StructField { name: symbols.insert(field.name), typ, offset: 0 });
        }
        self.structs[id].fields = fields;

        let receiver_type = Type::structure(id);
        self.specialize_impls(template.name, mangled_sym, receiver_type, args, symbols)?;

        Ok(receiver_type)
    }

    /// Validate that `args` has the right arity and satisfies every bound declared on `generics`.
    fn check_generic_args(
        &self,
        name: &'hir str,
        generics: &[statement::GenericBound<'hir>],
        args: &[Type],
        span: Span,
        symbols: &SymbolTable,
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
                    || symbols
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
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'hir>> {
        let matching: Vec<_> =
            self.generic_impls.iter().filter(|i| i.name == template_name).cloned().collect();
        for implementation in &matching {
            let impl_env = build_impl_substitution(implementation, args);

            if let Some(interface_name) = implementation.interface {
                self.interface_impls.insert((receiver_type, symbols.insert(interface_name)));
            }

            for method in &implementation.methods {
                let method_symbol = symbols.insert(method.name);
                let receiver_name = symbols.get(mangled_sym);
                let mangled = match implementation.interface {
                    Some(iface) => symbols.insert(&self.mangler.interface_item(
                        receiver_name,
                        iface,
                        method.name,
                    )),
                    None => symbols.insert(&self.mangler.scoped_item(receiver_name, method.name)),
                };

                let mut method_env = impl_env.clone();
                method_env.extend(generic_param_env(&method.generics));

                let mut params = Vec::new();
                if let Some(receiver) = method.receiver {
                    params.push(Type::receiver_ref(receiver_type, receiver.mutable));
                }
                params.extend(self.resolve_params(
                    &method.params,
                    symbols,
                    Some(receiver_type),
                    Some(&method_env),
                )?);
                let return_type = self.resolve_return_type(
                    method.return_type.as_ref(),
                    symbols,
                    Some(receiver_type),
                    Some(&method_env),
                )?;

                let kind = match method.receiver {
                    Some(r) => FunctionKind::Method(Method {
                        receiver: receiver_type,
                        name: method_symbol,
                        mutable: r.mutable,
                    }),
                    None => FunctionKind::Free,
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

                self.generic_fns.insert(sig_id, method.clone());
                self.generic_fn_envs.insert(sig_id, impl_env.clone());
            }
        }
        Ok(())
    }

    pub(in crate::hir) fn mangle_generic(
        &self,
        base: &str,
        args: &[Type],
        symbols: &SymbolTable,
    ) -> String {
        let mut mangled = String::from(base);
        for &arg in args {
            mangled.push('$');
            mangled.push_str(&self.mangle_component(arg, symbols));
        }
        mangled
    }

    fn mangle_component(&self, typ: Type, symbols: &SymbolTable) -> String {
        match typ.kind() {
            TypeKind::Struct(id) => symbols.get(self.structs[id].name).to_string(),
            TypeKind::Enum(id) => symbols.get(self.enums[id].name).to_string(),
            TypeKind::Ref { to, .. } => {
                format!("ref_{}", self.mangle_component(Type::from(to), symbols))
            },
            TypeKind::GenericParam(i) => format!("T{i}"),
            other => other.mangled().to_string(),
        }
    }

    /// Whether a function/method is a generic template that must not be lowered directly
    fn is_generic_function(
        &self,
        function: &statement::Function<'_>,
        symbols: &SymbolTable,
    ) -> bool {
        if !function.generics.is_empty() {
            return true;
        }

        function
            .impl_type
            .and_then(|impl_type| symbols.get_id(impl_type))
            .is_some_and(|sym| {
                self.generic_enums.contains_key(&sym) || self.generic_structs.contains_key(&sym)
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
    pub(in crate::hir) fn lookup_named_type(
        &self,
        name: &str,
        symbols: &SymbolTable,
    ) -> Option<Type> {
        resolve_primitive_type(name)
            .or_else(|| symbols.get_id(name).and_then(|s| self.nominal_type(s)))
    }

    pub(in crate::hir) fn resolve_function<F>(
        &self,
        symbols: &SymbolTable,
        operation: F,
    ) -> Option<FunctionId>
    where
        F: FnOnce(&Mangler) -> String,
    {
        let mangled = operation(&self.mangler);
        symbols.get_id(&mangled).and_then(|s| self.functions.get(&s).copied())
    }

    pub(in crate::hir) fn resolve_qualified_call(
        &self,
        qualifier: &str,
        name: &str,
        symbols: &SymbolTable,
    ) -> Option<FunctionId> {
        if let Some(id) = self.resolve_function(symbols, |m| m.scoped_item(qualifier, name)) {
            return Some(id);
        }

        let receiver_type = self.lookup_named_type(qualifier, symbols)?;
        self.interface_impls.iter().filter(|&&(t, _)| t == receiver_type).find_map(
            |&(_, interface_sym)| {
                let interface_name = symbols.get(interface_sym);
                self.resolve_function(symbols, |m| {
                    m.interface_item(qualifier, interface_name, name)
                })
            },
        )
    }

    #[inline]
    pub(in crate::hir) fn resolve_function_call(
        &self,
        qualifier: Option<&str>,
        name: &str,
        symbols: &SymbolTable,
    ) -> Option<FunctionId> {
        if let Some(qualifier) = qualifier
            && let Some(id) = self.resolve_qualified_call(qualifier, name, symbols)
        {
            return Some(id);
        }

        self.resolve_function(symbols, |m| m.item(name))
    }
}

impl ArrayTable {
    /// intern `[element; len]`, reusing the existing id when the type is already known
    pub fn intern(&self, element: Type, len: u32) -> ArrayId {
        if let Some(&id) = self.lookup.borrow().get(&(element, len)) {
            return id;
        }

        let mut types = self.types.borrow_mut();
        let id = ArrayId(types.len() as u32);
        types.push(ArrayType { element, len });
        self.lookup.borrow_mut().insert((element, len), id);
        id
    }

    #[inline]
    pub fn get(&self, id: ArrayId) -> ArrayType {
        self.types.borrow()[id]
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
fn is_generic_impl(imp: &statement::Impl<'_>) -> bool {
    !imp.generics.is_empty() || matches!(imp.receiver.value_ref(), statement::Type::Generic(..))
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

fn build_impl_substitution(implementation: &statement::Impl<'_>, args: &[Type]) -> GenericEnv {
    if !implementation.generics.is_empty() {
        return build_substitution(&implementation.generics, args);
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

fn intrinsic_method(in_std: bool, receiver: &str, method: &str) -> Option<Intrinsic> {
    (in_std && receiver == "str" && method == "len").then_some(Intrinsic::Len)
}
pub(in crate::hir) fn generic_param_env(generics: &[statement::GenericBound<'_>]) -> GenericEnv {
    generics
        .iter()
        .enumerate()
        .map(|(i, g)| (g.name.to_string(), Type::generic_param(i as u8)))
        .collect()
}
