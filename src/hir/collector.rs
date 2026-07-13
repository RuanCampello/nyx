//! Item collection: the per-module phase that extends the [Scope] namespace
//! with declarations before any body is lowered

use crate::{
    hir::{
        self, Enum, EnumId, EnumRepr, EnumVariant, Function, FunctionId, FunctionKind, Intrinsic,
        Layout, Method, StructId, Type, TypeKind, constants,
        declarations::Declarations,
        error::{HirError, HirErrorKind, hir_error},
        index_vec::IndexVec,
        interfaces, lower,
        scope::{
            FunctionSignature, GenericEnv, InterfaceMethodSignature, InterfaceSignature, Scope,
            Structs, generic_param_env, intrinsic_method, is_generic_impl, resolve_primitive_type,
        },
        structs,
        symbols::qualified,
    },
    parser::statement,
};
use std::{collections::HashSet, str::FromStr};

impl<'hir> Scope<'hir> {
    /// Analyse `declarations` and extend this scope with their declarations
    ///
    /// Ids are assigned relative to what is already in the scope so this can be called
    /// once per module in dependency order
    pub fn extend<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        arena: &'hir bumpalo::Bump,
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        self.collect_docs(declarations);
        self.extend_enums(declarations)?;
        self.extend_structs(declarations)?;
        self.extend_interfaces(declarations)?;
        self.extend_signatures(declarations)?;
        constants::extend(self, declarations, arena)?;
        interfaces::validate(self, declarations)?;

        Ok(())
    }

    fn collect_docs(&mut self, declarations: &Declarations<'_, '_>) {
        for (span, lines) in &declarations.docs {
            if let Some(joined) = hir::join_docs(lines) {
                self.docs.insert(*span, joined);
            }
        }
    }

    pub(in crate::hir) fn lower_matching_functions<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
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
            if self.is_generic_function(function) {
                continue;
            }

            let id = match self
                .function_id(function, None, |name| HirErrorKind::UnknownFunction { name })
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

            match lower::FunctionBuilder::new(self, id, function, arena).lower() {
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
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        let offset = self.structs.len() as u32;
        let mut local_map = Structs::new();
        let mut local_declarations = Vec::new();

        for struct_decl in &declarations.structs {
            let symbol = self.symbols.insert(struct_decl.name);

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
            &mut self.symbols,
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
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        for enum_decl in &declarations.enums {
            let symbol = self.symbols.insert(enum_decl.name);
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
                let variant_symbol = self.symbols.insert(variant.name);
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
                        self.resolve_type(typ.value_ref(), typ.span(), None, None)
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
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        for interface in &declarations.interfaces {
            let name = self.symbols.insert(interface.name);
            if self.interfaces.contains_key(&name) {
                self.soft(hir_error!(interface.span, DuplicateInterface { name: interface.name }))?;
                continue;
            }

            let superinterfaces =
                interface.superinterfaces.iter().map(|name| self.symbols.insert(name)).collect();

            let generic_params =
                interface.generics.iter().map(|g| self.symbols.insert(g.name)).collect();

            let param_env: GenericEnv = interface
                .generics
                .iter()
                .enumerate()
                .map(|(i, g)| (g.name.to_owned(), Type::generic_param(i as u8)))
                .collect();

            let env = (!param_env.is_empty()).then_some(&param_env);
            let mut methods = Vec::with_capacity(interface.methods.len());
            for method in &interface.methods {
                let name = self.symbols.insert(method.name);
                let has_receiver = method.receiver.is_some();
                let receiver_mut = method.receiver.map(|r| r.mutable).unwrap_or(false);

                let params = self.resolve_params(&method.params, None, env)?;
                let return_type =
                    self.resolve_return_type(method.return_type.as_ref(), None, env)?;

                methods.push(InterfaceMethodSignature {
                    name,
                    params,
                    return_type,
                    has_receiver,
                    receiver_mut,
                });
            }

            let signature = InterfaceSignature { name, superinterfaces, methods, generic_params };
            self.interfaces.insert(name, signature);
        }

        Ok(())
    }

    fn extend_signatures<'d, 'h>(
        &mut self,
        declarations: &Declarations<'d, 'h>,
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

            let symbol = self.symbols.insert(&self.mangler.item(function.name));
            if self.functions.contains_key(&symbol) {
                self.soft(hir_error!(function.span, DuplicateFunction { name: function.name }))?;
                continue;
            }

            if !function.generics.is_empty() {
                let env = generic_param_env(&function.generics);
                let params = self.resolve_params(&function.params, None, Some(&env))?;
                let return_type =
                    self.resolve_return_type(function.return_type.as_ref(), None, Some(&env))?;

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

            let params = self.resolve_params(&function.params, None, None)?;
            let return_type =
                self.resolve_return_type(function.return_type.as_ref(), None, None)?;
            let intrinsic = self.in_std.then(|| Intrinsic::from_str(function.name).ok()).flatten();
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
        if self.in_std {
            let syscall_sym = self.symbols.insert(&self.mangler.item("syscall"));
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

        self.extend_impl_signatures(declarations)?;

        Ok(())
    }

    fn extend_impl_signatures<'d, 'h>(
        &mut self,
        declarations: &Declarations<'d, 'h>,
    ) -> Result<(), HirError<'hir>>
    where
        'h: 'hir,
    {
        for implementation in declarations.impls.iter().copied() {
            // generic impls were collected in `extend_signatures`, specialised on demand
            if is_generic_impl(implementation) {
                continue;
            }

            let receiver_type = match self.impl_receiver_type(implementation, declarations) {
                Ok(receiver_type) => receiver_type,
                Err(error) => {
                    self.soft(error)?;
                    continue;
                },
            };

            let impl_param_env = self.build_impl_param_env(implementation, receiver_type)?;
            let impl_env_ref = impl_param_env.as_ref();

            for method in &implementation.methods {
                let method_symbol = self.symbols.insert(method.name);
                let mangled = match implementation.interface {
                    Some(interface) => self.symbols.insert(&self.mangler.interface_item(
                        implementation.name,
                        interface,
                        method.name,
                    )),
                    _ => self
                        .symbols
                        .insert(&self.mangler.scoped_item(implementation.name, method.name)),
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
                            Some(receiver_type),
                            impl_env_ref,
                        )?);
                        let return_type = self.resolve_return_type(
                            method.return_type.as_ref(),
                            Some(receiver_type),
                            impl_env_ref,
                        )?;

                        let intrinsic =
                            intrinsic_method(self.in_std, implementation.name, method.name);

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

                        let params =
                            self.resolve_params(&method.params, Some(receiver_type), impl_env_ref)?;
                        let return_type = self.resolve_return_type(
                            method.return_type.as_ref(),
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
    ) -> Result<Type, HirError<'hir>>
    where
        'h: 'hir,
    {
        if let Some(primitive) = resolve_primitive_type(implementation.name) {
            return match self.in_std {
                true => Ok(primitive),
                false => {
                    Err(hir_error!(implementation.span, OrphanImpl { name: implementation.name }))
                },
            };
        }

        let is_local = declarations.structs.iter().any(|s| s.name == implementation.name)
            || declarations.enums.iter().any(|e| e.name == implementation.name);
        let symbol = self.symbols.insert(implementation.name);

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
    /// build the param-name -> concrete-type map used when resolving method
    /// annotations.
    fn build_impl_param_env<'h>(
        &mut self,
        implementation: &statement::Impl<'h>,
        receiver_type: Type,
    ) -> Result<Option<GenericEnv>, HirError<'hir>>
    where
        'h: 'hir,
    {
        let Some(interface_name) = implementation.interface else {
            return Ok(None);
        };

        let interface_sym = self.symbols.insert(interface_name);
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
                        .resolve_type(arg.value_ref(), arg.span(), Some(receiver_type), None)
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
                let name = self.symbols.get(sym).to_owned();
                let typ = explicit_args.get(i).copied().unwrap_or(receiver_type);
                (name, typ)
            })
            .collect();

        Ok(Some(env))
    }
}
