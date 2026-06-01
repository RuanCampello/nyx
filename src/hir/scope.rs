use crate::{
    hir::{
        Constant, Enum, EnumId, EnumVariant, Function, FunctionId, FunctionKind, Intrinsic, Method,
        RefTarget, RefTargetKind, Struct, StructField, StructId, SymbolId, SymbolTable, Type,
        TypeKind, constants,
        declarations::Declarations,
        error::{HirError, HirErrorKind, hir_error},
        index_vec::IndexVec,
        interfaces,
        lower::{self},
        structs,
        symbols::Mangler,
    },
    lexer::{Spanned, token::Span},
    parser::statement,
};
use std::{
    collections::{HashMap, HashSet},
    ops::Index,
    str::FromStr,
};

/// The single accumulated namespace for a compilation
///
/// Grows incrementally as modules are loaded: structs and function
/// signatures are assigned monotonically increasing IDs across all modules
pub struct Scope<'hir> {
    pub(in crate::hir) mangler: Mangler<'hir>,
    pub signatures: IndexVec<FunctionId, FunctionSignature>,
    pub functions: Functions,
    pub methods: Methods,
    pub structs: IndexVec<StructId, Struct>,
    pub struct_map: Structs,
    pub enums: IndexVec<EnumId, Enum>,
    pub enum_map: Enums,
    pub enum_variants: EnumVariants,
    pub interfaces: Interfaces,
    pub interface_impls: InterfaceImpls,
    pub constants: HashMap<SymbolId, Constant<'hir>>,

    pub generic_structs: HashMap<SymbolId, statement::Struct<'hir>>,
    pub generic_enums: HashMap<SymbolId, statement::Enum<'hir>>,
    pub generic_fns: HashMap<SymbolId, statement::Function<'hir>>,
    pub generic_impls: Vec<statement::Impl<'hir>>,
}

#[derive(Debug)]
pub(in crate::hir) struct FunctionSignature {
    pub name: SymbolId,
    pub params: Vec<Type>,
    pub return_type: Type,
    pub kind: FunctionKind,
    pub is_const: bool,
    #[allow(unused)]
    pub inline: bool,
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

impl<'hir> Scope<'hir> {
    pub fn new() -> Self {
        Self {
            mangler: Mangler::default(),
            signatures: IndexVec::new(),
            functions: HashMap::new(),
            methods: HashMap::new(),
            structs: IndexVec::new(),
            struct_map: HashMap::new(),
            enums: IndexVec::new(),
            enum_map: HashMap::new(),
            enum_variants: HashMap::new(),
            interfaces: HashMap::new(),
            interface_impls: HashSet::new(),
            constants: HashMap::new(),
            generic_structs: HashMap::new(),
            generic_enums: HashMap::new(),
            generic_fns: HashMap::new(),
            generic_impls: Vec::new(),
        }
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
        self.extend_enums(declarations, symbols)?;
        self.extend_structs(declarations, symbols)?;
        self.extend_interfaces(declarations, symbols)?;
        self.extend_signatures(declarations, symbols, in_std)?;
        constants::extend(self, declarations, symbols, in_std, arena)?;
        interfaces::validate(self, declarations, symbols)?;

        Ok(())
    }

    pub fn lower_functions<'d, 's>(
        &self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
        in_std: bool,
        arena: &'hir bumpalo::Bump,
    ) -> Result<IndexVec<FunctionId, Function<'hir>>, HirError<'s>> {
        self.lower_matching_functions(declarations, symbols, in_std, |_| true, arena)
    }

    pub(in crate::hir) fn lower_matching_functions<'d, 's>(
        &self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
        in_std: bool,
        mut should_lower: impl FnMut(FunctionId) -> bool,
        arena: &'hir bumpalo::Bump,
    ) -> Result<IndexVec<FunctionId, Function<'hir>>, HirError<'s>> {
        declarations
            .functions()
            .filter_map(|function| {
                // generic templates carry open `GenericParam`s, they are not lowered here
                // but specialised on-demand during MIR monomorphisation
                if self.is_generic_function(function, symbols) {
                    return None;
                }

                let id = match self.function_id(function, symbols, None, |name| {
                    HirErrorKind::UnknownFunction { name }
                }) {
                    Ok(id) => id,
                    Err(err) => return Some(Err(err)),
                };

                if !should_lower(id) {
                    return None;
                }

                Some(
                    lower::FunctionBuilder::new(self, symbols, id, function, in_std, arena).lower(),
                )
            })
            .collect()
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
                return Err(hir_error!(
                    struct_decl.span,
                    DuplicateStruct { name: struct_decl.name.to_string() }
                ));
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
            symbols,
            &mut lowered,
        )?;

        for mut s in lowered.into_iter().map(|s| s.expect("every struct must be lowered")) {
            s.id = StructId(s.id.0 + offset);
            for field in &mut s.fields {
                if let TypeKind::Struct(mut id) = field.typ.kind() {
                    id.0 += offset;
                    field.typ = Type::new(TypeKind::Struct(id));
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
                return Err(hir_error!(
                    enum_decl.span,
                    DuplicateEnum { name: enum_decl.name.to_string() }
                ));
            }

            if !enum_decl.generics.is_empty() {
                self.generic_enums.insert(symbol, (*enum_decl).clone());
                continue;
            }

            let repr = enum_decl.repr.value().try_into().map_err(|_| {
                hir_error!(
                    enum_decl.repr.span(),
                    TypeMismatch {
                        expected: Type::new(TypeKind::I32),
                        found: Type::from_primitive_ast(&enum_decl.repr.value())
                            .unwrap_or_default(),
                    }
                )
            })?;
            let id = EnumId::new(self.enums.len() as u32, repr);

            self.enum_map.insert(symbol, id);
            self.enums.push(Enum { id, name: symbol, variants: Vec::new(), repr });

            let mut seen = HashSet::new();
            let mut next_value = 0;
            let mut variants = Vec::with_capacity(enum_decl.variants.len());

            for variant in &enum_decl.variants {
                let variant_symbol = symbols.insert(variant.name);
                if !seen.insert(variant_symbol) {
                    return Err(hir_error!(
                        variant.span,
                        DuplicateVariant { name: variant.name.to_string() }
                    ));
                }

                let value = variant.value.unwrap_or(next_value);
                next_value = value + 1;

                // resolve a tagged-union variant payload (e.g. `Some(T)`), a concrete
                // `Type::Generic` here instantiates its template on-demand
                let payload = match &variant.payload {
                    Some(typ) => {
                        Some(self.resolve_type(typ.value_ref(), typ.span(), symbols, None, None)?)
                    },
                    None => None,
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
    ) -> Result<(), HirError<'hir>> {
        for interface in &declarations.interfaces {
            let name = symbols.insert(interface.name);
            if self.interfaces.contains_key(&name) {
                return Err(hir_error!(
                    interface.span,
                    DuplicateInterface { name: interface.name.into() }
                ));
            }

            let superinterfaces =
                interface.superinterfaces.iter().map(|name| symbols.insert(name)).collect();

            let generic_params: Vec<SymbolId> =
                interface.generics.iter().map(|g| symbols.insert(g.name)).collect();

            let param_env: HashMap<String, Type> = interface
                .generics
                .iter()
                .enumerate()
                .map(|(i, g)| (g.name.to_owned(), Type::new(TypeKind::GenericParam(i as u8))))
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
        for function in &declarations.functions {
            if function.receiver.is_some() {
                return Err(hir_error!(function.span, ReceiverOutsideImpl));
            }

            let symbol = symbols.insert(&self.mangler.item(function.name));
            if self.functions.contains_key(&symbol) {
                return Err(hir_error!(
                    function.span,
                    DuplicateFunction { name: function.name.to_string() }
                ));
            }

            if !function.generics.is_empty() {
                self.generic_fns.insert(symbol, (*function).clone());
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
                inline: function.inline,
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
                    return_type: Type::new(TypeKind::Iptr),
                    kind: FunctionKind::Intrinsic(Intrinsic::Syscall),
                    is_const: false,
                    inline: false,
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
            if is_generic_impl(implementation) {
                self.generic_impls.push(implementation.clone());
                continue;
            }

            let receiver_type = match resolve_primitive_type(implementation.name) {
                Some(primitive) if in_std => primitive,
                Some(_) => {
                    return Err(hir_error!(
                        implementation.span,
                        OrphanImpl { name: implementation.name.to_string() }
                    ));
                },

                _ => {
                    let is_local =
                        declarations.structs.iter().any(|s| s.name == implementation.name)
                            || declarations.enums.iter().any(|e| e.name == implementation.name);
                    if !is_local {
                        let symbol = symbols.insert(implementation.name);
                        return match self.nominal_type(symbol) {
                            Some(_) => Err(hir_error!(
                                implementation.span,
                                OrphanImpl { name: implementation.name.to_string() }
                            )),

                            _ => Err(hir_error!(
                                implementation.span,
                                UnknownType { name: implementation.name.to_string() }
                            )),
                        };
                    }

                    let symbol = symbols.insert(implementation.name);
                    self.nominal_type(symbol).ok_or_else(|| HirError {
                        kind: HirErrorKind::UnknownType { name: implementation.name.to_string() },
                        span: implementation.span,
                    })?
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
                            return Err(hir_error!(
                                method.span,
                                DuplicateMethod {
                                    struct_name: implementation.name.to_string(),
                                    name: method.name.to_string(),
                                }
                            ));
                        }

                        let id = FunctionId(self.signatures.len() as u32);
                        self.methods.insert((receiver_type, method_symbol), id);

                        let mut params = Vec::with_capacity(method.params.len() + 1);
                        let first_param = Type::new(TypeKind::Ref {
                            mutable: receiver.mutable,
                            to: RefTarget::try_from(receiver_type)
                                .expect("receiver must be a reference target"),
                        });
                        params.push(first_param);
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

                        // FIXME: that's hardcoded af
                        // we can do it better but let it be
                        let intrinsic =
                            match in_std && implementation.name == "str" && method.name == "len" {
                                true => Some(Intrinsic::Len),
                                _ => None,
                            };

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
                            inline: method.inline,
                        });
                    },

                    None => {
                        if self.functions.contains_key(&mangled) {
                            let name = format!("{}::{}", implementation.name, method.name);
                            return Err(hir_error!(method.span, DuplicateFunction { name }));
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
                            inline: method.inline,
                            kind: FunctionKind::Free,
                        });
                    },
                }
            }
        }
        Ok(())
    }

    /// Register a `impl T with Iface` relation and, if `Iface` is generic,
    /// build the param-name → concrete-type map used when resolving method
    /// annotations.
    fn build_impl_param_env<'h>(
        &mut self,
        implementation: &statement::Impl<'h>,
        receiver_type: Type,
        symbols: &mut SymbolTable,
    ) -> Result<Option<HashMap<String, Type>>, HirError<'hir>> {
        let Some(interface_name) = implementation.interface else {
            return Ok(None);
        };

        let interface_sym = symbols.insert(interface_name);
        self.interface_impls.insert((receiver_type, interface_sym));

        let generic_params: Vec<SymbolId> = match self.interfaces.get(&interface_sym) {
            Some(interface) if !interface.generic_params.is_empty() => {
                interface.generic_params.clone()
            },
            _ => return Ok(None),
        };

        let explicit_args: Vec<_> = match implementation.interface_type.as_ref().map(|s| s.value())
        {
            Some(statement::Type::Generic(_, args)) => {
                let mut resolved = Vec::with_capacity(args.len());
                for arg in &args {
                    resolved.push(self.resolve_type(
                        arg.value_ref(),
                        arg.span(),
                        symbols,
                        Some(receiver_type),
                        None,
                    )?);
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
        hint: Option<&str>,
        error_kind: impl FnOnce(String) -> HirErrorKind<'s>,
    ) -> Result<FunctionId, HirError<'s>> {
        let impl_type = function.impl_type.or(hint);

        match (function.receiver, impl_type) {
            (Some(_), Some(impl_type)) => {
                let receiver_type =
                    self.lookup_named_type(impl_type, symbols).ok_or_else(|| HirError {
                        kind: HirErrorKind::UnknownType { name: impl_type.to_string() },
                        span: function.span,
                    })?;

                let method_symbol = symbols.insert(function.name);
                self.methods
                    .get(&(receiver_type, method_symbol))
                    .copied()
                    .ok_or_else(|| HirError {
                        kind: error_kind(function.name.to_string()),
                        span: function.span,
                    })
            },

            (Some(_), None) => Err(HirError {
                kind: error_kind(function.name.to_string()),
                span: function.span,
            }),

            (None, Some(impl_type)) => self
                .resolve_qualified_call(impl_type, function.name, symbols)
                .ok_or_else(|| HirError {
                    kind: error_kind(format!("{impl_type}::{}", function.name)),
                    span: function.span,
                }),
            (None, None) => {
                self.resolve_function(symbols, |m| m.item(function.name))
                    .ok_or_else(|| HirError {
                        kind: error_kind(function.name.to_string()),
                        span: function.span,
                    })
            },
        }
    }

    #[inline]
    fn resolve_return_type<'h>(
        &mut self,
        return_type: Option<&Spanned<statement::Type<'h>>>,
        symbols: &mut SymbolTable,
        self_type: Option<Type>,
        env: Option<&HashMap<String, Type>>,
    ) -> Result<Type, HirError<'hir>> {
        match return_type {
            Some(s) => self.resolve_type(s.value_ref(), s.span(), symbols, self_type, env),
            None => Ok(Type::default()),
        }
    }

    #[inline]
    fn resolve_params<'h>(
        &mut self,
        params: &[statement::Parameter<'h>],
        symbols: &mut SymbolTable,
        self_type: Option<Type>,
        env: Option<&HashMap<String, Type>>,
    ) -> Result<Vec<Type>, HirError<'hir>> {
        let mut out = Vec::with_capacity(params.len());
        for p in params {
            out.push(self.resolve_type(
                p.typ.value_ref(),
                p.typ.span(),
                symbols,
                self_type,
                env,
            )?);
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
        env: Option<&HashMap<String, Type>>,
    ) -> Result<Type, HirError<'hir>> {
        match typ {
            statement::Type::Named(name) => {
                if let Some(env) = env {
                    if let Some(&t) = env.get(*name) {
                        return Ok(t);
                    }
                }
                symbols
                    .get_id(name)
                    .and_then(|symbol| self.nominal_type(symbol))
                    .ok_or_else(|| hir_error!(span, UnknownType { name: name.to_string() }))
            },

            statement::Type::Ref(inner) => {
                let inner = self.resolve_type(inner, span, symbols, self_type, env)?;
                let to = RefTarget::try_from(inner).map_err(|_| {
                    hir_error!(
                        span,
                        TypeMismatch {
                            expected: Type::new(TypeKind::Struct(Default::default())),
                            found: inner,
                        }
                    )
                })?;
                Ok(Type::new(TypeKind::Ref { mutable: false, to }))
            },

            statement::Type::SelfType => Ok(self_type.unwrap_or(Type::new(TypeKind::SelfType))),
            statement::Type::RefSelf => match self_type {
                None => Ok(Type::new(TypeKind::Ref {
                    mutable: false,
                    to: RefTarget::new(RefTargetKind::SelfType),
                })),
                Some(self_typ) => {
                    let to = RefTarget::try_from(self_typ).map_err(|_| {
                        hir_error!(
                            span,
                            TypeMismatch {
                                expected: Type::new(TypeKind::Struct(Default::default())),
                                found: self_typ,
                            }
                        )
                    })?;
                    Ok(Type::new(TypeKind::Ref { mutable: false, to }))
                },
            },

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

            other => Type::from_primitive_ast(other)
                .ok_or_else(|| hir_error!(span, UnknownType { name: format!("{other:?}") })),
        }
    }

    /// Specialise a generic struct/enum template for the concrete `args`, returning the resulting nominal [`Type`]
    /// Specialisations are cached by mangled name
    fn instantiate_generic(
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

        Err(hir_error!(span, UnknownType { name: name.to_string() }))
    }

    fn instantiate_enum(
        &mut self,
        template: &statement::Enum<'hir>,
        mangled_sym: SymbolId,
        args: &[Type],
        span: Span,
        symbols: &mut SymbolTable,
    ) -> Result<Type, HirError<'hir>> {
        if template.generics.len() != args.len() {
            return Err(hir_error!(
                span,
                ArityMismatch {
                    name: template.name.to_string(),
                    expected: template.generics.len(),
                    found: args.len(),
                }
            ));
        }

        let repr = template.repr.value().try_into().map_err(|_| {
            hir_error!(
                template.repr.span(),
                TypeMismatch {
                    expected: Type::new(TypeKind::I32),
                    found: Type::from_primitive_ast(&template.repr.value()).unwrap_or_default(),
                }
            )
        })?;
        let id = EnumId::new(self.enums.len() as u32, repr);

        self.enum_map.insert(mangled_sym, id);
        self.enums.push(Enum { id, name: mangled_sym, variants: Vec::new(), repr });

        let env = build_substitution(&template.generics, args);
        let mut seen = HashSet::new();
        let mut next_value = 0;
        let mut variants = Vec::with_capacity(template.variants.len());

        for variant in &template.variants {
            let variant_symbol = symbols.insert(variant.name);
            if !seen.insert(variant_symbol) {
                return Err(hir_error!(
                    variant.span,
                    DuplicateVariant { name: variant.name.to_string() }
                ));
            }

            let value = variant.value.unwrap_or(next_value);
            next_value = value + 1;

            let payload = match &variant.payload {
                Some(typ) => Some(self.resolve_type(
                    typ.value_ref(),
                    typ.span(),
                    symbols,
                    None,
                    Some(&env),
                )?),
                None => None,
            };

            self.enum_variants.insert((mangled_sym, variant_symbol), (id, value));
            variants.push(EnumVariant { name: variant_symbol, value, payload });
        }

        self.enums[id].variants = variants;
        Ok(Type::new(TypeKind::Enum(id)))
    }

    fn instantiate_struct(
        &mut self,
        template: &statement::Struct<'hir>,
        mangled_sym: SymbolId,
        args: &[Type],
        span: Span,
        symbols: &mut SymbolTable,
    ) -> Result<Type, HirError<'hir>> {
        if template.generics.len() != args.len() {
            return Err(hir_error!(
                span,
                ArityMismatch {
                    name: template.name.to_string(),
                    expected: template.generics.len(),
                    found: args.len(),
                }
            ));
        }

        let id = StructId(self.structs.len() as u32);

        self.struct_map.insert(mangled_sym, id);
        self.structs.push(Struct {
            id,
            name: mangled_sym,
            fields: Vec::new(),
            repr: template.repr,
        });

        let env = build_substitution(&template.generics, args);
        let mut fields = Vec::with_capacity(template.fields.len());
        for field in &template.fields {
            let typ = self.resolve_type(
                field.typ.value_ref(),
                field.typ.span(),
                symbols,
                None,
                Some(&env),
            )?;
            fields.push(StructField { name: symbols.insert(field.name), typ });
        }

        self.structs[id].fields = fields;
        Ok(Type::new(TypeKind::Struct(id)))
    }

    fn mangle_generic(&self, base: &str, args: &[Type], symbols: &SymbolTable) -> String {
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
            other => primitive_mangle(other).to_string(),
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
    fn push_signature(&mut self, signature: FunctionSignature) -> FunctionId {
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
        if let Some(qualifier) = qualifier {
            if let Some(id) = self.resolve_qualified_call(qualifier, name, symbols) {
                return Some(id);
            }
        }

        self.resolve_function(symbols, |m| m.item(name))
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
        match self.receiver_type().map(|t| t.kind()) {
            Some(TypeKind::Ref { mutable, .. }) => mutable,
            _ => false,
        }
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
        .map(|id| Type::new(TypeKind::Struct(id)))
        .or_else(|| enum_map.get(&symbol).copied().map(|id| Type::new(TypeKind::Enum(id))))
}

#[inline]
fn is_generic_impl(imp: &statement::Impl<'_>) -> bool {
    !imp.generics.is_empty() || matches!(imp.receiver.value_ref(), statement::Type::Generic(..))
}

fn build_substitution(
    generics: &[statement::GenericBound<'_>],
    args: &[Type],
) -> HashMap<String, Type> {
    generics.iter().zip(args).map(|(g, &arg)| (g.name.to_string(), arg)).collect()
}

fn primitive_mangle<'s>(kind: TypeKind) -> &'s str {
    match kind {
        TypeKind::I8 => "i8",
        TypeKind::U8 => "u8",
        TypeKind::I16 => "i16",
        TypeKind::U16 => "u16",
        TypeKind::I32 => "i32",
        TypeKind::U32 => "u32",
        TypeKind::I64 => "i64",
        TypeKind::U64 => "u64",
        TypeKind::F32 => "f32",
        TypeKind::F64 => "f64",
        TypeKind::Bool => "bool",
        TypeKind::Uptr => "uptr",
        TypeKind::Iptr => "iptr",
        TypeKind::Char => "char",
        TypeKind::Str => "str",
        TypeKind::String => "string",
        TypeKind::SelfType => "self",
        TypeKind::Never => "never",
        TypeKind::Unit => "unit",
        _ => "type",
    }
}
