use crate::{
    hir::{
        self, FnDef, FunctionKind, InterfaceConstSignature, InterfaceMethodSignature,
        InterfaceSignature, Intrinsic, Method, Owner, SymbolId, TyInterner, Type,
        collect::{
            GenericEnv, ItemTable, associated_key, extend_generic_env, generic_param_env,
            is_generic_impl, receiver_param_type, resolve_primitive_type, source_span,
        },
        declarations::Declarations,
        error::{HirError, hir_error},
        lang::intrinsic_method,
        symbols::qualified,
    },
    lexer::token::Span,
    parser::statement,
};
use std::{collections::HashMap, str::FromStr};

struct ImplCtx<'hir, 'h> {
    receiver_type: Type<'hir>,
    param_env: Option<GenericEnv<'hir>>,
    open: bool,
    owner: Owner<'hir>,
    name: &'h str,
}

impl<'hir> ItemTable<'hir> {
    pub(super) fn extend_interfaces<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
    ) -> Result<(), HirError<'hir>>
    where
        's: 'hir,
    {
        for interface in &declarations.interfaces {
            let name = self.symbols.insert(interface.name);
            let already_exists = self.interfaces.defs.contains_key(&name);
            if self.declare_or_error(already_exists, |this| {
                let previous = source_span(this.interfaces.defs[&name].decl_span);
                hir_error!(interface.span, DuplicateInterface { name: interface.name, previous })
            }) {
                continue;
            }

            let superinterfaces: Vec<_> =
                interface.superinterfaces.iter().map(|name| self.symbols.insert(name)).collect();

            let generic_params =
                interface.generics.iter().map(|g| self.symbols.insert(g.name)).collect();

            let mut param_env: GenericEnv<'hir> = interface
                .generics
                .iter()
                .enumerate()
                .map(|(i, g)| (g.name.to_owned(), self.types.generic_param(i as u8)))
                .collect();

            let mut associated_types: Vec<_> =
                interface.types.iter().map(|t| self.symbols.insert(t.name)).collect();
            let inherited: Vec<_> = superinterfaces
                .iter()
                .filter_map(|parent| self.interfaces.defs.get(parent))
                .flat_map(|parent| parent.associated_types.iter().copied())
                .collect();

            for name in inherited {
                if !associated_types.contains(&name) {
                    associated_types.push(name);
                }
            }

            let declared = interface.generics.len();
            let slots: Vec<_> = associated_types
                .iter()
                .enumerate()
                .map(|(i, &name)| {
                    (
                        associated_key(self.symbols.get(name)),
                        self.types.generic_param((declared + i) as u8),
                    )
                })
                .collect();
            param_env.extend(slots);

            let base_env = (!param_env.is_empty()).then_some(&param_env);
            let mut methods = Vec::with_capacity(interface.methods.len());

            for method in &interface.methods {
                let name = self.symbols.insert(method.name);
                let has_receiver = method.receiver.is_some();
                let receiver_mut = method.receiver.map(|r| r.mutable).unwrap_or(false);

                let mut method_env = None;
                let env = match method.generics.is_empty() {
                    true => base_env,
                    false => {
                        let mut extended = param_env.clone();
                        extend_generic_env(&mut extended, &self.types, &method.generics);
                        Some(&*method_env.insert(extended))
                    },
                };

                let (params, return_type) =
                    self.resolve_signature(&method.params, method.return_type.as_ref(), None, env)?;

                methods.push(InterfaceMethodSignature {
                    name,
                    params,
                    return_type,
                    has_receiver,
                    receiver_mut,
                    is_const: method.is_const,
                    decl_span: method.span,
                    name_span: method.name_span,
                });
            }

            let mut constants = Vec::with_capacity(interface.constants.len());
            for constant in &interface.constants {
                constants.push(InterfaceConstSignature {
                    name: self.symbols.insert(constant.name),
                    typ: self.resolve_type(
                        constant.typ.value_ref(),
                        constant.typ.span(),
                        None,
                        base_env,
                    )?,
                    decl_span: constant.span,
                    name_span: constant.name_span,
                });
            }

            let signature = InterfaceSignature {
                name,
                superinterfaces,
                methods,
                constants,
                generic_params,
                associated_types,
                decl_span: interface.span,
                name_span: interface.name_span,
            };
            self.interfaces.defs.insert(name, signature);
        }

        Ok(())
    }

    pub(in crate::hir) fn declared_intrinsic<'h>(
        &mut self,
        function: &statement::Function<'h>,
        receiver: Option<&str>,
    ) -> Result<Option<Intrinsic>, HirError<'hir>>
    where
        'h: 'hir,
    {
        if !function.is_intrinsic() {
            return Ok(None);
        }

        let found = match receiver {
            Some(receiver) => intrinsic_method(receiver, function.name),
            None => Intrinsic::from_str(function.name).ok(),
        };

        if found.is_none() {
            let name = function.name;
            self.soft(hir_error!(function.name_span, UnknownIntrinsic { name }));
        }

        Ok(found)
    }

    pub(super) fn extend_signatures<'d, 'h>(
        &mut self,
        declarations: &Declarations<'d, 'h>,
    ) -> Result<(), HirError<'hir>>
    where
        'h: 'hir,
    {
        for function in &declarations.functions {
            if function.receiver.is_some() {
                self.soft(hir_error!(function.span, ReceiverOutsideImpl));
                continue;
            }

            let symbol = self.symbols.insert(&self.mangler.item(function.name));
            let already_exists = self.functions.by_name.contains_key(&symbol);
            if self.declare_or_error(already_exists, |this| {
                let previous =
                    source_span(this.functions.defs[this.functions.by_name[&symbol]].decl_span);
                hir_error!(function.span, DuplicateFunction { name: function.name, previous })
            }) {
                continue;
            }

            if !function.generics.is_empty() {
                let env = generic_param_env(&self.types, &function.generics);
                let (params, return_type) = self.resolve_signature(
                    &function.params,
                    function.return_type.as_ref(),
                    None,
                    Some(&env),
                )?;

                let sig = FnDef {
                    name: symbol,
                    params,
                    return_type,
                    kind: FunctionKind::Free,
                    owner: Owner::Free,
                    is_const: function.is_const,
                    is_unsafe: function.is_unsafe(),
                    has_receiver: false,
                    decl_span: function.span,
                    body: Some((*function).clone()),
                    generic_env: HashMap::new(),
                };
                let id = self.push_signature(sig);
                self.functions.by_name.insert(symbol, id);

                continue;
            }

            let (params, return_type) = self.resolve_signature(
                &function.params,
                function.return_type.as_ref(),
                None,
                None,
            )?;
            let intrinsic = self.declared_intrinsic(function, None)?;
            let kind = match intrinsic {
                Some(i) => FunctionKind::Intrinsic(i),
                None => FunctionKind::Free,
            };

            let sig = FnDef {
                name: symbol,
                params,
                return_type,
                kind,
                owner: Owner::Free,
                is_const: function.is_const,
                is_unsafe: function.is_unsafe(),
                has_receiver: false,
                decl_span: function.span,
                body: None,
                generic_env: HashMap::new(),
            };
            let id = self.push_signature(sig);
            self.functions.by_name.insert(symbol, id);
        }

        // when compiling std, inject a built-in 'syscall' signature so std modules
        // don't need to declare it
        if self.in_std.get() {
            let syscall_sym = self.symbols.insert(&self.mangler.item("syscall"));
            if !self.functions.by_name.contains_key(&syscall_sym) {
                let signature = FnDef {
                    name: syscall_sym,
                    params: vec![],
                    return_type: self.types.common.iptr,
                    kind: FunctionKind::Intrinsic(Intrinsic::Syscall),
                    owner: Owner::Free,
                    is_const: false,
                    is_unsafe: false,
                    has_receiver: false,
                    decl_span: Span::default(),
                    body: None,
                    generic_env: HashMap::new(),
                };
                let id = self.functions.defs.push(signature);
                self.functions.by_name.insert(syscall_sym, id);
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
            let open_env = open_impl_env(&self.types, implementation);

            let receiver_type = match self.impl_receiver_type(
                implementation,
                declarations,
                (!open_env.is_empty()).then_some(&open_env),
            ) {
                Ok(receiver_type) => receiver_type,
                Err(error) => {
                    self.soft(error);
                    continue;
                },
            };

            let param_env = self.build_impl_param_env(
                implementation,
                receiver_type,
                (!open_env.is_empty()).then_some(open_env),
            )?;
            let ctx = ImplCtx {
                receiver_type,
                param_env,
                open: is_generic_impl(implementation),
                owner: match implementation.interface {
                    Some(interface) => Owner::Interface {
                        on: receiver_type,
                        interface: self.symbols.insert(interface),
                    },
                    None => Owner::Inherent(receiver_type),
                },
                name: match implementation.name {
                    hir::SLICE_IMPL_NAME => "slice",
                    name => name,
                },
            };

            for method in &implementation.methods {
                let mut method_env = None;
                let impl_env_ref = match method.generics.is_empty() {
                    true => ctx.param_env.as_ref(),
                    false => {
                        let mut env = ctx.param_env.clone().unwrap_or_default();
                        extend_generic_env(&mut env, &self.types, &method.generics);
                        Some(&*method_env.insert(env))
                    },
                };

                let mangled = match implementation.interface {
                    Some(interface) => self.symbols.insert(&self.mangler.interface_item(
                        ctx.name,
                        interface,
                        method.name,
                    )),
                    _ => self.symbols.insert(&self.mangler.scoped_item(ctx.name, method.name)),
                };

                match method.receiver {
                    Some(receiver) => self.push_method_signature(
                        &ctx,
                        implementation,
                        method,
                        receiver,
                        mangled,
                        impl_env_ref,
                    )?,
                    None => self.push_free_impl_function(
                        &ctx,
                        implementation,
                        method,
                        mangled,
                        impl_env_ref,
                    )?,
                }
            }
        }
        Ok(())
    }

    fn push_method_signature<'h>(
        &mut self,
        ctx: &ImplCtx<'hir, 'h>,
        implementation: &statement::Impl<'h>,
        method: &statement::Function<'h>,
        receiver: statement::Receiver,
        mangled: SymbolId,
        env: Option<&GenericEnv<'hir>>,
    ) -> Result<(), HirError<'hir>>
    where
        'h: 'hir,
    {
        let method_symbol = self.symbols.insert(method.name);
        let already_exists =
            self.functions.methods.contains_key(&(ctx.receiver_type, method_symbol));
        if self.declare_or_error(already_exists, |this| {
            let existing = this.functions.methods[&(ctx.receiver_type, method_symbol)];
            let previous = source_span(this.functions.defs[existing].decl_span);
            hir_error!(
                method.span,
                DuplicateMethod {
                    struct_name: implementation.name,
                    name: method.name,
                    previous
                }
            )
        }) {
            return Ok(());
        }

        let (resolved, return_type) = self.resolve_signature(
            &method.params,
            method.return_type.as_ref(),
            Some(ctx.receiver_type),
            env,
        )?;
        let mut params = Vec::with_capacity(resolved.len() + 1);
        params.push(receiver_param_type(&self.types, ctx.receiver_type, receiver.mutable));
        params.extend(resolved);

        let intrinsic = self.declared_intrinsic(method, Some(implementation.name))?;

        let kind = match intrinsic {
            Some(i) => FunctionKind::Intrinsic(i),
            None => FunctionKind::Method(Method {
                receiver: ctx.receiver_type,
                name: method_symbol,
                mutable: receiver.mutable,
            }),
        };
        let id = self.push_signature(FnDef {
            name: mangled,
            params,
            return_type,
            kind,
            owner: ctx.owner,
            is_const: method.is_const,
            is_unsafe: method.is_unsafe(),
            has_receiver: true,
            decl_span: method.span,
            body: ((ctx.open || !method.generics.is_empty()) && intrinsic.is_none())
                .then(|| method.clone()),
            generic_env: env.cloned().unwrap_or_default(),
        });
        self.functions.methods.insert((ctx.receiver_type, method_symbol), id);
        Ok(())
    }

    fn push_free_impl_function<'h>(
        &mut self,
        ctx: &ImplCtx<'hir, 'h>,
        implementation: &statement::Impl<'h>,
        method: &statement::Function<'h>,
        mangled: SymbolId,
        env: Option<&GenericEnv<'hir>>,
    ) -> Result<(), HirError<'hir>>
    where
        'h: 'hir,
    {
        let already_exists = self.functions.by_name.contains_key(&mangled);
        if self.declare_or_error(already_exists, |this| {
            let name = qualified(this.arena, implementation.name, method.name);
            let existing = this.functions.by_name[&mangled];
            let previous = source_span(this.functions.defs[existing].decl_span);
            hir_error!(method.span, DuplicateFunction { name, previous })
        }) {
            return Ok(());
        }

        let (params, return_type) = self.resolve_signature(
            &method.params,
            method.return_type.as_ref(),
            Some(ctx.receiver_type),
            env,
        )?;

        let id = self.push_signature(FnDef {
            name: mangled,
            params,
            return_type,
            is_const: method.is_const,
            is_unsafe: method.is_unsafe(),
            kind: FunctionKind::Free,
            owner: ctx.owner,
            has_receiver: false,
            decl_span: method.span,
            body: (ctx.open || !method.generics.is_empty()).then(|| method.clone()),
            generic_env: env.cloned().unwrap_or_default(),
        });
        self.functions.by_name.insert(mangled, id);
        Ok(())
    }

    fn impl_receiver_type<'h>(
        &mut self,
        implementation: &statement::Impl<'h>,
        declarations: &Declarations<'_, 'h>,
        env: Option<&GenericEnv<'hir>>,
    ) -> Result<Type<'hir>, HirError<'hir>>
    where
        'h: 'hir,
    {
        if matches!(implementation.receiver.value_ref(), statement::Type::Slice(..)) {
            if !self.in_std.get() {
                return Err(hir_error!(
                    implementation.span,
                    OrphanImpl { name: implementation.name }
                ));
            }

            return self.resolve_type(
                implementation.receiver.value_ref(),
                implementation.receiver.span(),
                None,
                env,
            );
        }

        if let Some(primitive) = resolve_primitive_type(&self.types, implementation.name) {
            return match self.in_std.get() {
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
            (Some(_), true) if env.is_some() => self.resolve_type(
                implementation.receiver.value_ref(),
                implementation.receiver.span(),
                None,
                env,
            ),
            (Some(typ), true) => Ok(typ),
            (Some(_), false) => {
                Err(hir_error!(implementation.span, OrphanImpl { name: implementation.name }))
            },
            (None, _) => {
                Err(hir_error!(implementation.span, UnknownType { name: implementation.name }))
            },
        }
    }

    fn build_impl_param_env<'h>(
        &mut self,
        implementation: &statement::Impl<'h>,
        receiver_type: Type<'hir>,
        base: Option<GenericEnv<'hir>>,
    ) -> Result<Option<GenericEnv<'hir>>, HirError<'hir>>
    where
        'h: 'hir,
    {
        let mut base = base.unwrap_or_default();
        let associated = self.bind_associated_types(
            implementation,
            receiver_type,
            (!base.is_empty()).then_some(&base),
        )?;
        base.extend(associated.clone().into_iter().flatten());
        let Some(interface_name) = implementation.interface else {
            return Ok((!base.is_empty()).then_some(base));
        };

        let interface_sym = self.symbols.insert(interface_name);
        self.interfaces.impls.insert((receiver_type, interface_sym));

        let Some(interface) = self
            .interfaces
            .defs
            .get(&interface_sym)
            .filter(|interface| !interface.generic_params.is_empty())
        else {
            return Ok((!base.is_empty()).then_some(base));
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
                            Some(receiver_type),
                            (!base.is_empty()).then_some(&base),
                        )
                        .unwrap_or_else(|error| self.poison(error));
                    resolved.push(typ);
                }
                resolved
            },
            _ => Vec::new(),
        };

        let interface_env: GenericEnv<'hir> = generic_params
            .into_iter()
            .enumerate()
            .map(|(i, sym)| {
                let name = self.symbols.get(sym).to_owned();
                let typ = explicit_args.get(i).copied().unwrap_or(receiver_type);
                (name, typ)
            })
            .collect();
        base.extend(interface_env);

        Ok(Some(base))
    }

    /// resolve every `type X = T;` an implementation declares
    pub(in crate::hir) fn bind_associated_types<'h>(
        &mut self,
        implementation: &statement::Impl<'h>,
        receiver_type: Type<'hir>,
        env: Option<&GenericEnv<'hir>>,
    ) -> Result<Option<GenericEnv<'hir>>, HirError<'hir>>
    where
        'h: 'hir,
    {
        if implementation.types.is_empty() {
            return Ok(None);
        }

        let mut bindings = GenericEnv::with_capacity(implementation.types.len());
        for associated in &implementation.types {
            let typ = self
                .resolve_type(
                    associated.typ.value_ref(),
                    associated.typ.span(),
                    Some(receiver_type),
                    env,
                )
                .unwrap_or_else(|error| self.poison(error));

            let symbol = self.symbols.insert(associated.name);
            self.interfaces.associated_types.insert((receiver_type, symbol), typ);
            bindings.insert(associated_key(associated.name), typ);
        }

        Ok(Some(bindings))
    }
}

fn open_impl_env<'hir>(
    types: &TyInterner<'hir>,
    implementation: &statement::Impl<'_>,
) -> GenericEnv<'hir> {
    use statement::Type::*;

    let mut names: Vec<_> = implementation.generics.iter().map(|g| g.name).collect();

    let receiver_types = match implementation.receiver.value_ref() {
        Generic(_, args) => args.iter().map(|arg| arg.value_ref()).collect::<Vec<_>>(),
        Slice(element, _) => vec![element.as_ref()],
        _ => Vec::new(),
    };

    for ty in receiver_types {
        if let Named(name) = ty {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }

    names
        .into_iter()
        .enumerate()
        .map(|(i, name)| (name.to_owned(), types.generic_param(i as u8)))
        .collect()
}
