#![allow(unused)]

use crate::{
    hir::{
        Constant, Enum, EnumId, EnumRepr, EnumVariant, Function, FunctionId, Intrinsic, Method,
        RefTarget, RefTargetKind, Struct, StructId, SymbolId, SymbolTable, Type, TypeKind,
        declarations::Declarations,
        error::{HirError, HirErrorKind, hir_error},
        lower::{self},
        symbols::Mangler,
    },
    lexer::Spanned,
    parser::{expression, statement, visitor},
};
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};

/// The single accumulated namespace for a compilation
///
/// Grows incrementally as modules are loaded: structs and function
/// signatures are assigned monotonically increasing IDs across all modules
pub struct Scope<'s> {
    pub(in crate::hir) mangler: Mangler<'s>,
    pub signatures: Vec<FunctionSignature>,
    pub functions: Functions,
    pub methods: Methods,
    pub structs: Vec<Struct>,
    pub struct_map: Structs,
    pub enums: Vec<Enum>,
    pub enum_map: Enums,
    pub enum_variants: EnumVariants,
    pub interfaces: Interfaces,
    pub interface_impls: InterfaceImpls,
    pub constants: HashMap<SymbolId, Constant>,
}

#[derive(Debug)]
pub(in crate::hir) struct FunctionSignature {
    pub name: SymbolId,
    pub params: Vec<Type>,
    pub return_type: Type,
    pub intrinsic: Option<Intrinsic>,
    pub method: Option<Method>,
    pub is_const: bool,
    pub inline: bool,
}

#[derive(Debug)]
pub(in crate::hir) struct InterfaceSignature {
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
    has_receiver: bool,
    receiver_mut: bool,
}

pub(in crate::hir) type Functions = HashMap<SymbolId, FunctionId>;
pub(in crate::hir) type Structs = HashMap<SymbolId, StructId>;
pub(in crate::hir) type Enums = HashMap<SymbolId, EnumId>;
pub(in crate::hir) type EnumVariants = HashMap<(SymbolId, SymbolId), (EnumId, i64)>;
pub(in crate::hir) type Methods = HashMap<(Type, SymbolId), FunctionId>;
pub(in crate::hir) type Interfaces = HashMap<SymbolId, InterfaceSignature>;
pub(in crate::hir) type InterfaceImpls = HashSet<(Type, SymbolId)>;

impl<'sc> Scope<'sc> {
    pub fn new() -> Self {
        Self {
            mangler: Mangler::default(),
            signatures: Vec::new(),
            functions: HashMap::new(),
            methods: HashMap::new(),
            structs: Vec::new(),
            struct_map: HashMap::new(),
            enums: Vec::new(),
            enum_map: HashMap::new(),
            enum_variants: HashMap::new(),
            interfaces: HashMap::new(),
            interface_impls: HashSet::new(),
            constants: HashMap::new(),
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
    ) -> Result<(), HirError<'s>> {
        self.extend_enums(declarations, symbols)?;
        self.extend_structs(declarations, symbols)?;
        self.extend_interfaces(declarations, symbols)?;
        self.extend_signatures(declarations, symbols, in_std)?;
        self.extend_constants(declarations, symbols, in_std)?;

        self.validate_interfaces(declarations, symbols)?;
        self.validate_interface_hierarchy(declarations, symbols)?;

        Ok(())
    }

    pub fn lower_functions<'d, 's>(
        &self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
        in_std: bool,
    ) -> Result<Vec<Function>, HirError<'s>> {
        self.lower_matching_functions(declarations, symbols, in_std, |_| true)
    }

    pub(in crate::hir) fn lower_matching_functions<'d, 's>(
        &self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
        in_std: bool,
        mut should_lower: impl FnMut(FunctionId) -> bool,
    ) -> Result<Vec<Function>, HirError<'s>> {
        declarations
            .functions()
            .filter_map(|function| {
                let id = match self.function_id(function, symbols, None, |name| {
                    HirErrorKind::UnknownFunction { name }
                }) {
                    Ok(id) => id,
                    Err(err) => return Some(Err(err)),
                };

                if !should_lower(id) {
                    return None;
                }

                Some(lower::FunctionBuilder::new(self, symbols, id, function, in_std).lower())
            })
            .collect()
    }

    fn extend_structs<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'s>> {
        let offset = self.structs.len() as u32;
        let mut local_map = Structs::new();
        let mut local_declarations = Vec::new();

        for struct_decl in &declarations.structs {
            let symbol = symbols.insert(struct_decl.name);
            if self.struct_map.contains_key(&symbol)
                || self.enum_map.contains_key(&symbol)
                || local_map.contains_key(&symbol)
            {
                return Err(hir_error!(
                    struct_decl.span,
                    DuplicateStruct { name: struct_decl.name.to_string() }
                ));
            }
            let local_id = StructId(local_declarations.len() as u32);
            local_map.insert(symbol, local_id);
            local_declarations.push((symbol, *struct_decl));
        }

        if local_declarations.is_empty() {
            return Ok(());
        }

        let mut lowered = vec![None; local_declarations.len()];

        lower::lower_structs(
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
    ) -> Result<(), HirError<'s>> {
        for enum_decl in &declarations.enums {
            let symbol = symbols.insert(enum_decl.name);
            if self.enum_map.contains_key(&symbol)
                || self.struct_map.contains_key(&symbol)
                || declarations.structs.iter().any(|s| s.name == enum_decl.name)
            {
                return Err(hir_error!(
                    enum_decl.span,
                    DuplicateEnum { name: enum_decl.name.to_string() }
                ));
            }

            let repr = enum_decl.repr.value().try_into().map_err(|_| {
                hir_error!(
                    enum_decl.repr.span(),
                    TypeMismatch {
                        expected: Type::new(TypeKind::I32),
                        found: Type::from(&enum_decl.repr.value())
                    }
                )
            })?;
            let id = EnumId(self.enums.len() as u32, repr);
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
                self.enum_variants.insert((symbol, variant_symbol), (id, value));
                variants.push(EnumVariant { name: variant_symbol, value });
            }

            self.enum_map.insert(symbol, id);
            self.enums.push(Enum { id, name: symbol, variants, repr });
        }

        Ok(())
    }

    fn extend_interfaces<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'s>> {
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
                interface.generics.iter().map(|p| symbols.insert(p)).collect();

            // Build a temporary env mapping each generic param name → GenericParam(i) placeholder.
            // This lets resolve_annotation succeed without a real type for the param.
            let param_env: HashMap<String, Type> = interface
                .generics
                .iter()
                .enumerate()
                .map(|(i, &name)| (name.to_owned(), Type::new(TypeKind::GenericParam(i as u8))))
                .collect();

            let env = if param_env.is_empty() {
                None
            } else {
                Some(&param_env)
            };

            let methods = interface
                .methods
                .iter()
                .map(|method| -> Result<InterfaceMethodSignature, HirError> {
                    let name = symbols.insert(method.name);
                    let has_receiver = method.receiver.is_some();
                    let receiver_mut = method.receiver.map(|r| r.mutable).unwrap_or(false);

                    let params = self.resolve_params(&method.params, symbols, None, env)?;
                    let return_type =
                        self.resolve_return_type(method.return_type.as_ref(), symbols, None, env)?;

                    Ok(InterfaceMethodSignature {
                        name,
                        params,
                        return_type,
                        has_receiver,
                        receiver_mut,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;

            self.interfaces.insert(
                name,
                InterfaceSignature { name, superinterfaces, methods, generic_params },
            );
        }

        Ok(())
    }

    fn validate_interface_hierarchy<'d, 's>(
        &self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'s>> {
        for interface in &declarations.interfaces {
            for superinterface in &interface.superinterfaces {
                let symbol = symbols.insert(superinterface);

                if !self.interfaces.contains_key(&symbol) {
                    return Err(hir_error!(
                        interface.span,
                        UnknownInterface { name: superinterface.to_string() }
                    ));
                }
            }
        }

        Ok(())
    }

    fn extend_signatures<'d, 'h>(
        &mut self,
        declarations: &Declarations<'d, 'h>,
        symbols: &mut SymbolTable,
        in_std: bool,
    ) -> Result<(), HirError<'h>> {
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

            let params = self.resolve_params(&function.params, symbols, None, None)?;
            let return_type =
                self.resolve_return_type(function.return_type.as_ref(), symbols, None, None)?;
            let intrinsic = in_std.then(|| Intrinsic::from_str(function.name).ok()).flatten();

            let sig = FunctionSignature {
                name: symbol,
                params,
                return_type,
                intrinsic,
                method: None,
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
                    intrinsic: Some(Intrinsic::Syscall),
                    method: None,
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
    ) -> Result<(), HirError<'h>> {
        for implementation in declarations.impls.iter().copied() {
            let receiver_type = match resolve_primitive_type(implementation.name) {
                Some(primitive) if in_std => primitive,
                Some(primitive) => {
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

            // Build a generic param env if this impl targets a generic interface.
            // Injected default methods (e.g., `ne` from `PartialEq<Rhs>`) have
            // params like `&Rhs` that must resolve against the concrete type args.
            let impl_param_env: Option<HashMap<String, Type>> =
                if let Some(interface_name) = implementation.interface {
                    let interface_sym = symbols.insert(interface_name);
                    self.interface_impls.insert((receiver_type, interface_sym));

                    if let Some(interface) = self.interfaces.get(&interface_sym) {
                        if !interface.generic_params.is_empty() {
                            // Extract explicit type args from `impl T with Iface<Arg1, Arg2>`.
                            // If none provided (e.g., `impl T with PartialEq`), default each
                            // generic param to the receiver type (i.e., `Self`).
                            let explicit_args: Vec<Type> =
                                match implementation.interface_type.as_ref().map(|s| s.value()) {
                                    Some(statement::Type::Generic(_, args)) => args
                                        .iter()
                                        .map(|arg| {
                                            lower::resolve_annotation(
                                                symbols,
                                                &self.struct_map,
                                                &self.enum_map,
                                                &arg.value(),
                                                arg.span(),
                                                Some(receiver_type),
                                                None,
                                            )
                                        })
                                        .collect::<Result<Vec<_>, _>>()?,
                                    _ => vec![],
                                };

                            let param_names: Vec<String> = interface
                                .generic_params
                                .iter()
                                .map(|&sym| symbols.get(sym).to_owned())
                                .collect();
                            let env: HashMap<String, Type> = param_names
                                .into_iter()
                                .enumerate()
                                .map(|(i, name)| {
                                    let t = explicit_args.get(i).copied().unwrap_or(receiver_type);
                                    (name, t)
                                })
                                .collect();
                            Some(env)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
            let impl_env_ref = impl_param_env.as_ref().map(|e| e as &HashMap<String, Type>);

            for method in &implementation.methods {
                let method_symbol = symbols.insert(method.name);
                let mangled = match implementation.interface {
                    Some(interface) => symbols.insert(&self.mangler.interface_item(
                        implementation.name,
                        interface,
                        method.name,
                    )),
                    None => {
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

                        self.signatures.push(FunctionSignature {
                            name: mangled,
                            params,
                            return_type,
                            intrinsic,
                            is_const: method.is_const,
                            inline: method.inline,
                            method: Some(Method {
                                receiver: receiver_type,
                                name: method_symbol,
                                mutable: receiver.mutable,
                            }),
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
                            intrinsic: None,
                            method: None,
                        });
                    },
                }
            }
        }
        Ok(())
    }

    fn validate_interfaces<'d, 'h>(
        &self,
        declarations: &Declarations<'d, 'h>,
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'h>> {
        #[inline]
        fn substitute_self(typ: Type, self_type: Type) -> Type {
            match typ.kind() {
                TypeKind::SelfType => self_type,
                TypeKind::Ref { mutable, to } if to.kind() == RefTargetKind::SelfType => {
                    match RefTarget::try_from(self_type) {
                        Ok(to) => Type::new(TypeKind::Ref { mutable, to }),
                        _ => typ,
                    }
                },
                _ => typ,
            }
        }

        // Substitute GenericParam(i) placeholders with the concrete type args[i].
        // Missing args fall back to SelfType so that the existing substitute_self pass handles them.
        #[inline]
        fn substitute_generic_params(typ: Type, concrete: &[Type]) -> Type {
            match typ.kind() {
                TypeKind::GenericParam(i) => {
                    concrete.get(i as usize).copied().unwrap_or(Type::new(TypeKind::SelfType))
                },
                TypeKind::Ref { mutable, to } => match to.kind() {
                    RefTargetKind::GenericParam(i) => {
                        let inner = concrete
                            .get(i as usize)
                            .copied()
                            .unwrap_or(Type::new(TypeKind::SelfType));
                        match RefTarget::try_from(inner) {
                            Ok(new_to) => Type::new(TypeKind::Ref { mutable, to: new_to }),
                            _ => typ,
                        }
                    },
                    _ => typ,
                },
                _ => typ,
            }
        }

        for implementation in &declarations.impls {
            let Some(interface_name) = implementation.interface else {
                continue;
            };

            let interface_sym = symbols.insert(interface_name);
            let interface = self.interfaces.get(&interface_sym).ok_or_else(|| {
                hir_error!(implementation.span, UnknownInterface { name: interface_name.into() })
            })?;

            let receiver_type = match resolve_primitive_type(implementation.name) {
                Some(primitive) => primitive,
                _ => {
                    let symbol = symbols.insert(implementation.name);
                    self.nominal_type(symbol)
                        .expect("impl type must exist in scope after declaration extension")
                },
            };

            // Build concrete type args for the interface's generic params from the impl's interface_type.
            // e.g. `impl i32 with PartialEq<i32>` → concrete_args = [i32]
            // e.g. `impl Status with PartialEq` → concrete_args = [] (falls back to SelfType)
            let concrete_args: Vec<Type> = if !interface.generic_params.is_empty() {
                match implementation.interface_type.as_ref().map(|s| s.value()) {
                    Some(statement::Type::Generic(_, args)) => args
                        .iter()
                        .map(|arg| {
                            lower::resolve_annotation(
                                symbols,
                                &self.struct_map,
                                &self.enum_map,
                                &arg.value(),
                                arg.span(),
                                Some(receiver_type),
                                None,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => vec![],
                }
            } else {
                vec![]
            };

            let impl_methods: HashMap<_, _> =
                implementation.methods.iter().map(|method| (method.name, method)).collect();

            for required in &interface.superinterfaces {
                if !self.interfaces.contains_key(required) {
                    return Err(hir_error!(
                        implementation.span,
                        UnknownInterface { name: symbols.get(*required).to_string() }
                    ));
                }

                if !self.interface_impls.contains(&(receiver_type, *required)) {
                    return Err(hir_error!(
                        implementation.span,
                        MissingSuperinterfaceImpl {
                            struct_name: implementation.name.to_string(),
                            interface_name: interface_name.to_string(),
                            superinterface_name: symbols.get(*required).to_string(),
                        }
                    ));
                }
            }

            for required in &interface.methods {
                let method_name = symbols.get(required.name).to_string();
                let Some(impl_method) = impl_methods.get(method_name.as_str()) else {
                    return Err(hir_error!(
                        implementation.span,
                        MissingInterfaceMethod {
                            struct_name: implementation.name.into(),
                            interface_name: interface_name.into(),
                            method_name: method_name,
                        }
                    ));
                };

                let impl_has_receiver = impl_method.receiver.is_some();
                let function_id =
                    self.function_id(impl_method, symbols, Some(implementation.name), |_| {
                        HirErrorKind::MissingInterfaceMethod {
                            struct_name: implementation.name.to_string(),
                            interface_name: interface_name.to_string(),
                            method_name: method_name.to_string(),
                        }
                    })?;

                let signature = &self.signatures[function_id.0 as usize];

                let (impl_receiver_mut, impl_explicit_params) = match impl_has_receiver {
                    true => {
                        let TypeKind::Ref { mutable, .. } = signature.params[0].kind() else {
                            unreachable!("method signature must start with a receiver reference");
                        };
                        (mutable, &signature.params[1..])
                    },
                    _ => (false, signature.params.as_slice()),
                };

                let required_params: Vec<_> = required
                    .params
                    .iter()
                    .map(|&t| {
                        substitute_self(substitute_generic_params(t, &concrete_args), receiver_type)
                    })
                    .collect();
                let required_return_type = substitute_self(
                    substitute_generic_params(required.return_type, &concrete_args),
                    receiver_type,
                );

                let signature_ok = impl_has_receiver == required.has_receiver
                    && (!required.has_receiver || required.receiver_mut == impl_receiver_mut)
                    && required_params == impl_explicit_params
                    && required_return_type == signature.return_type;

                if !signature_ok {
                    fn format(
                        name: &str,
                        has_receiver: bool,
                        receiver_mut: bool,
                        params: &[Type],
                        return_type: Type,
                    ) -> String {
                        let mut parameters = match has_receiver {
                            true => Vec::from([match receiver_mut {
                                true => "&mut self".to_string(),
                                _ => "&self".to_string(),
                            }]),
                            _ => Vec::new(),
                        };

                        parameters.extend(params.iter().map(|t| t.to_string()));
                        format!("fn {name}({}): {return_type}", parameters.join(", "))
                    }

                    #[rustfmt::skip]
                    let expected = format(
                        method_name.as_str(), required.has_receiver, required.receiver_mut,
                        &required.params, required.return_type,
                    );
                    #[rustfmt::skip]
                    let found = format(
                        method_name.as_str(), impl_has_receiver, impl_receiver_mut,
                        impl_explicit_params, signature.return_type,
                    );

                    return Err(hir_error!(
                        impl_method.span,
                        InterfaceSignatureMismatch {
                            struct_name: implementation.name.to_string(),
                            interface_name: interface_name.to_string(),
                            method_name: method_name.to_string(),
                            expected,
                            found,
                            impl_span: implementation.span,
                        }
                    ));
                }
            }
        }
        Ok(())
    }

    #[inline]
    fn function_id<'s>(
        &self,
        function: &statement::Function<'s>,
        symbols: &mut SymbolTable,
        hint: Option<&str>,
        error_kind: impl FnOnce(String) -> HirErrorKind<'s>,
    ) -> Result<FunctionId, HirError<'s>> {
        let impl_type = function.impl_type.or(hint);

        match (function.receiver, impl_type) {
            (Some(_), Some(impl_type)) => {
                let receiver_type = match resolve_primitive_type(impl_type) {
                    Some(primitive) => primitive,
                    _ => self.nominal_type(symbols.insert(impl_type)).ok_or_else(|| HirError {
                        kind: HirErrorKind::UnknownType { name: impl_type.to_string() },
                        span: function.span,
                    })?,
                };

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

            (None, Some(impl_type)) => {
                let mangled = symbols.insert(&self.mangler.scoped_item(impl_type, function.name));

                self.functions
                    .get(&mangled)
                    .copied()
                    .or_else(|| {
                        let receiver_type = match resolve_primitive_type(impl_type) {
                            Some(primitive) => primitive,
                            _ => self.nominal_type(symbols.insert(impl_type))?,
                        };
                        self.interface_impls.iter().filter(|&&(t, _)| t == receiver_type).find_map(
                            |&(_, interface_sym)| {
                                let interface_name = symbols.get(interface_sym);
                                let interface_mangled =
                                    symbols.insert(&self.mangler.interface_item(
                                        impl_type,
                                        interface_name,
                                        function.name,
                                    ));
                                self.functions.get(&interface_mangled).copied()
                            },
                        )
                    })
                    .ok_or_else(|| HirError {
                        kind: error_kind(format!("{impl_type}::{}", function.name)),
                        span: function.span,
                    })
            },
            (None, None) => {
                let sym = symbols.insert(&self.mangler.item(function.name));
                self.functions.get(&sym).copied().ok_or_else(|| HirError {
                    kind: error_kind(function.name.to_string()),
                    span: function.span,
                })
            },
        }
    }

    #[inline]
    fn resolve_return_type<'h>(
        &self,
        return_type: Option<&Spanned<statement::Type<'h>>>,
        symbols: &mut SymbolTable,
        self_type: Option<Type>,
        env: Option<&HashMap<String, Type>>,
    ) -> Result<Type, HirError<'h>> {
        return_type
            .map(|s| {
                lower::resolve_annotation(
                    symbols,
                    &self.struct_map,
                    &self.enum_map,
                    &s.value(),
                    s.span(),
                    self_type,
                    env,
                )
            })
            .transpose()
            .map(|opt| opt.unwrap_or_default())
    }

    #[inline]
    fn resolve_params<'h>(
        &self,
        params: &[statement::Parameter<'h>],
        symbols: &mut SymbolTable,
        self_type: Option<Type>,
        env: Option<&HashMap<String, Type>>,
    ) -> Result<Vec<Type>, HirError<'h>> {
        params
            .iter()
            .map(|p| {
                lower::resolve_annotation(
                    symbols,
                    &self.struct_map,
                    &self.enum_map,
                    &p.typ.value(),
                    p.typ.span(),
                    self_type,
                    env,
                )
            })
            .collect()
    }

    fn extend_constants<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
        in_std: bool,
    ) -> Result<(), HirError<'s>> {
        let mut decls: HashMap<SymbolId, ConstDecl<'d, 's>> = HashMap::new();

        // collect top-level constants
        for c in &declarations.constants {
            let symbol_id = symbols.insert(&self.mangler.item(c.name));
            if decls.contains_key(&symbol_id) {
                return Err(hir_error!(c.span, DuplicateConstant { name: c.name.to_string() }));
            }

            decls.insert(symbol_id, ConstDecl { mangled_name: symbol_id, impl_type: None, ast: c });
        }

        // collect impl constants
        for imp in &declarations.impls {
            for c in &imp.constants {
                let mangled = self.mangler.scoped_item(imp.name, c.name);
                let symbol_id = symbols.insert(&mangled);
                if decls.contains_key(&symbol_id) {
                    return Err(hir_error!(
                        c.span,
                        DuplicateConstant { name: format!("{}::{}", imp.name, c.name) }
                    ));
                }

                decls.insert(
                    symbol_id,
                    ConstDecl { mangled_name: symbol_id, impl_type: Some(imp.name), ast: c },
                );
            }
        }

        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        let mut sorted = Vec::new();

        fn dfs<'d, 's>(
            symbol_id: SymbolId,
            mangler: &Mangler,
            symbols: &SymbolTable,
            decls: &HashMap<SymbolId, ConstDecl<'d, 's>>,
            visiting: &mut HashSet<SymbolId>,
            visited: &mut HashSet<SymbolId>,
            sorted: &mut Vec<SymbolId>,
        ) -> Result<(), HirError<'s>> {
            use visitor::Visitor;

            if visiting.contains(&symbol_id) {
                let decl = decls.get(&symbol_id).unwrap();
                let name = match decl.impl_type {
                    Some(impl_type) => format!("{}::{}", impl_type, decl.ast.name),
                    _ => decl.ast.name.to_string(),
                };
                return Err(hir_error!(decl.ast.span, CircularConstant { name }));
            }
            if !visited.contains(&symbol_id) {
                visiting.insert(symbol_id);
                if let Some(decl) = decls.get(&symbol_id) {
                    let mut deps = Vec::new();
                    let mut visitor = ConstVisitor {
                        current_impl: decl.impl_type,
                        mangler,
                        symbols,
                        decls,
                        deps: &mut deps,
                    };
                    visitor.visit_expression(&decl.ast.value);
                    for dep in deps {
                        if decls.contains_key(&dep) {
                            dfs(dep, mangler, symbols, decls, visiting, visited, sorted)?;
                        }
                    }
                }
                visiting.remove(&symbol_id);
                visited.insert(symbol_id);
                sorted.push(symbol_id);
            }
            Ok(())
        }

        for &symbol_id in decls.keys() {
            if !visited.contains(&symbol_id) {
                dfs(
                    symbol_id,
                    &self.mangler,
                    symbols,
                    &decls,
                    &mut visiting,
                    &mut visited,
                    &mut sorted,
                )?;
            }
        }

        for symbol_id in sorted {
            let decl = decls.get(&symbol_id).unwrap();
            let expected_type = lower::resolve_annotation(
                symbols,
                &self.struct_map,
                &self.enum_map,
                &decl.ast.typ.value(),
                decl.ast.typ.span(),
                None,
                None,
            )?;

            let lowered =
                lower::lower_const(self, symbols, &decl.ast.value, expected_type, in_std)?;

            let constant = Constant {
                name: symbol_id,
                typ: expected_type,
                value: lowered,
                is_pub: decl.ast.is_pub,
            };
            self.constants.insert(symbol_id, constant);
        }

        Ok(())
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
}

struct ConstVisitor<'a, 'd, 'i, 'sc> {
    current_impl: Option<&'a str>,
    mangler: &'a Mangler<'sc>,
    symbols: &'a SymbolTable,
    decls: &'a HashMap<SymbolId, ConstDecl<'d, 'i>>,
    deps: &'a mut Vec<SymbolId>,
}

struct ConstDecl<'d, 's> {
    mangled_name: SymbolId,
    impl_type: Option<&'d str>,
    ast: &'d statement::Const<'s>,
}

impl<'i, 'sc> visitor::Visitor<'i> for ConstVisitor<'_, '_, 'i, 'sc> {
    fn visit_expression(&mut self, expr: &expression::Expression<'i>) {
        use expression::Expression as Expr;

        match expr {
            Expr::Identifier(name, _) => {
                if let Some(impl_type) = self.current_impl {
                    let mangled = self.mangler.scoped_item(impl_type, name);
                    if let Some(symbol_id) = self.symbols.get_id(&mangled) {
                        if self.decls.contains_key(&symbol_id) {
                            self.deps.push(symbol_id);
                            return;
                        }
                    }
                }
                if let Some(symbol_id) = self.symbols.get_id(&self.mangler.item(name)) {
                    if self.decls.contains_key(&symbol_id) {
                        self.deps.push(symbol_id);
                    }
                }
            },
            Expr::QualifiedName { qualifier, name, .. } => {
                let mangled = self.mangler.scoped_item(qualifier, name);
                if let Some(symbol_id) = self.symbols.get_id(&mangled) {
                    if self.decls.contains_key(&symbol_id) {
                        self.deps.push(symbol_id);
                    }
                }
            },
            _ => visitor::walk_expression(self, expr),
        }
    }
}

#[inline(always)]
pub(in crate::hir) fn resolve_primitive_type(name: &str) -> Option<Type> {
    statement::Type::from_str(name).map(|ast_ty| Type::from(&ast_ty))
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
