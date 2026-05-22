#![allow(unused)]

use crate::{
    hir::{
        Constant, Function, FunctionId, Intrinsic, Method, RefTarget, Struct, StructId, SymbolId,
        SymbolTable, Type,
        declarations::Declarations,
        error::{HirError, HirErrorKind},
        lower::{self},
        mangle::Mangler,
    },
    lexer::Spanned,
    parser::{expression, statement},
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
pub(in crate::hir) type Methods = HashMap<(Type, SymbolId), FunctionId>;
pub(in crate::hir) type Interfaces = HashMap<SymbolId, InterfaceSignature>;
pub(in crate::hir) type InterfaceImpls = HashSet<(StructId, SymbolId)>;

impl<'sc> Scope<'sc> {
    pub fn new() -> Self {
        Self {
            mangler: Mangler::default(),
            signatures: Vec::new(),
            functions: HashMap::new(),
            methods: HashMap::new(),
            structs: Vec::new(),
            struct_map: HashMap::new(),
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
        declarations
            .functions()
            .map(|function| {
                let id = self.function_id(function, symbols, None, |name| {
                    HirErrorKind::UnknownFunction { name }
                })?;

                #[cfg(test)]
                if in_std {
                    crate::hir::STD_FUNCTIONS_COUNT.with(|c| c.set(c.get() + 1));
                }

                lower::FunctionBuilder::new(self, symbols, id, function, in_std).lower()
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
            if self.struct_map.contains_key(&symbol) || local_map.contains_key(&symbol) {
                return Err(HirError {
                    kind: HirErrorKind::DuplicateStruct {
                        name: struct_decl.name.to_string(),
                    },
                    span: struct_decl.span,
                });
            }
            let local_id = StructId(local_declarations.len() as u32);
            local_map.insert(symbol, local_id);
            local_declarations.push((symbol, *struct_decl));
        }

        if local_declarations.is_empty() {
            return Ok(());
        }

        let mut lowered = vec![None; local_declarations.len()];

        lower::lower_structs(&local_declarations, &local_map, symbols, &mut lowered)?;

        for mut s in lowered.into_iter().map(|s| s.expect("every struct must be lowered")) {
            s.id = StructId(s.id.0 + offset);
            for field in &mut s.fields {
                if let Type::Struct(id) = &mut field.typ {
                    id.0 += offset;
                }
            }
            self.struct_map.insert(s.name, s.id);
            self.structs.push(s);
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
                return Err(HirError {
                    kind: HirErrorKind::DuplicateInterface {
                        name: interface.name.to_string(),
                    },
                    span: interface.span,
                });
            }

            let superinterfaces =
                interface.superinterfaces.iter().map(|name| symbols.insert(name)).collect();

            let methods = interface
                .methods
                .iter()
                .map(|method| -> Result<InterfaceMethodSignature, HirError> {
                    let name = symbols.insert(method.name);
                    let has_receiver = method.receiver.is_some();
                    let receiver_mut = method.receiver.map(|r| r.mutable).unwrap_or(false);

                    let params = self.resolve_params(&method.params, symbols)?;
                    let return_type =
                        self.resolve_return_type(method.return_type.as_ref(), symbols)?;

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
                InterfaceSignature {
                    name,
                    superinterfaces,
                    methods,
                },
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
                    return Err(HirError {
                        kind: HirErrorKind::UnknownInterface {
                            name: superinterface.to_string(),
                        },
                        span: interface.span,
                    });
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
                return Err(HirError {
                    kind: HirErrorKind::ReceiverOutsideImpl,
                    span: function.span,
                });
            }

            let symbol = symbols.insert(&self.mangler.item(function.name));
            if self.functions.contains_key(&symbol) {
                return Err(HirError {
                    kind: HirErrorKind::DuplicateFunction {
                        name: function.name.to_string(),
                    },
                    span: function.span,
                });
            }

            let id = FunctionId(self.signatures.len() as u32);
            self.functions.insert(symbol, id);

            let params = self.resolve_params(&function.params, symbols)?;
            let return_type = self.resolve_return_type(function.return_type.as_ref(), symbols)?;
            let intrinsic = match in_std {
                true => Intrinsic::from_str(function.name).ok(),
                false => None,
            };

            self.signatures.push(FunctionSignature {
                name: symbol,
                params,
                return_type,
                intrinsic,
                method: None,
                is_const: function.is_const,
                inline: function.inline,
            });
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
                    return_type: Type::Iptr,
                    intrinsic: Some(Intrinsic::Syscall),
                    method: None,
                    is_const: false,
                    inline: false,
                });
            }
        }

        for implementation in &declarations.impls {
            let receiver_type = match resolve_primitive_type(implementation.name) {
                Some(primitive) if in_std => primitive,
                Some(primitive) => {
                    return Err(HirError {
                        kind: HirErrorKind::OrphanImpl {
                            name: implementation.name.to_string(),
                        },
                        span: implementation.span,
                    });
                }

                _ => {
                    let is_local =
                        declarations.structs.iter().any(|s| s.name == implementation.name);
                    if !is_local {
                        let struct_symbol = symbols.insert(implementation.name);
                        return match self.struct_map.contains_key(&struct_symbol) {
                            true => Err(HirError {
                                kind: HirErrorKind::OrphanImpl {
                                    name: implementation.name.to_string(),
                                },
                                span: implementation.span,
                            }),

                            _ => Err(HirError {
                                kind: HirErrorKind::UnknownType {
                                    name: implementation.name.to_string(),
                                },
                                span: implementation.span,
                            }),
                        };
                    }

                    let struct_symbol = symbols.insert(implementation.name);
                    let struct_id =
                        *self.struct_map.get(&struct_symbol).ok_or_else(|| HirError {
                            kind: HirErrorKind::UnknownType {
                                name: implementation.name.to_string(),
                            },
                            span: implementation.span,
                        })?;
                    Type::Struct(struct_id)
                }
            };

            if let Some(interface_name) = implementation.interface {
                let Type::Struct(struct_id) = receiver_type else {
                    return Err(HirError {
                        kind: HirErrorKind::TypeMismatch {
                            expected: Type::Struct(StructId::default()),
                            found: receiver_type,
                        },
                        span: implementation.span,
                    });
                };
                let interface = symbols.insert(interface_name);
                self.interface_impls.insert((struct_id, interface));
            }

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
                    }
                };

                match method.receiver {
                    Some(receiver) => {
                        if self.methods.contains_key(&(receiver_type, method_symbol)) {
                            return Err(HirError {
                                kind: HirErrorKind::DuplicateMethod {
                                    struct_name: implementation.name.to_string(),
                                    name: method.name.to_string(),
                                },
                                span: method.span,
                            });
                        }

                        let id = FunctionId(self.signatures.len() as u32);
                        self.methods.insert((receiver_type, method_symbol), id);

                        let mut params = Vec::with_capacity(method.params.len() + 1);
                        let first_param = Type::Ref {
                            mutable: receiver.mutable,
                            to: RefTarget::try_from(receiver_type)
                                .expect("receiver must be a reference target"),
                        };
                        params.push(first_param);
                        params.extend(self.resolve_params(&method.params, symbols)?);
                        let return_type =
                            self.resolve_return_type(method.return_type.as_ref(), symbols)?;

                        self.signatures.push(FunctionSignature {
                            name: mangled,
                            params,
                            return_type,
                            intrinsic: None,
                            is_const: method.is_const,
                            inline: method.inline,
                            method: Some(Method {
                                receiver: receiver_type,
                                name: method_symbol,
                                mutable: receiver.mutable,
                            }),
                        });
                    }

                    None => {
                        if self.functions.contains_key(&mangled) {
                            return Err(HirError {
                                kind: HirErrorKind::DuplicateFunction {
                                    name: format!("{}::{}", implementation.name, method.name),
                                },
                                span: method.span,
                            });
                        }

                        let id = FunctionId(self.signatures.len() as u32);
                        self.functions.insert(mangled, id);

                        let params = self.resolve_params(&method.params, symbols)?;
                        let return_type =
                            self.resolve_return_type(method.return_type.as_ref(), symbols)?;

                        self.signatures.push(FunctionSignature {
                            name: mangled,
                            params,
                            return_type,
                            is_const: method.is_const,
                            inline: method.inline,
                            intrinsic: None,
                            method: None,
                        });
                    }
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
        for implementation in &declarations.impls {
            let Some(interface_name) = implementation.interface else {
                continue;
            };

            let interface_sym = symbols.insert(interface_name);
            let interface = self.interfaces.get(&interface_sym).ok_or_else(|| HirError {
                kind: HirErrorKind::UnknownInterface {
                    name: interface_name.to_string(),
                },
                span: implementation.span,
            })?;

            let struct_sym = symbols.insert(implementation.name);
            let struct_id = *self
                .struct_map
                .get(&struct_sym)
                .expect("impl struct must exist in scope after extend_structs");

            let impl_methods: HashMap<_, _> =
                implementation.methods.iter().map(|method| (method.name, method)).collect();

            for required in &interface.superinterfaces {
                if !self.interfaces.contains_key(required) {
                    return Err(HirError {
                        kind: HirErrorKind::UnknownInterface {
                            name: symbols.get(*required).to_string(),
                        },
                        span: implementation.span,
                    });
                }

                if !self.interface_impls.contains(&(struct_id, *required)) {
                    return Err(HirError {
                        kind: HirErrorKind::MissingSuperinterfaceImpl {
                            struct_name: implementation.name.to_string(),
                            interface_name: interface_name.to_string(),
                            superinterface_name: symbols.get(*required).to_string(),
                        },
                        span: implementation.span,
                    });
                }
            }

            for required in &interface.methods {
                let method_name = symbols.get(required.name).to_string();
                let Some(impl_method) = impl_methods.get(method_name.as_str()) else {
                    return Err(HirError {
                        kind: HirErrorKind::MissingInterfaceMethod {
                            struct_name: implementation.name.to_string(),
                            interface_name: interface_name.to_string(),
                            method_name: method_name,
                        },
                        span: implementation.span,
                    });
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
                        let Type::Ref { mutable, .. } = signature.params[0] else {
                            unreachable!("method signature must start with a receiver reference");
                        };
                        (mutable, &signature.params[1..])
                    }
                    _ => (false, signature.params.as_slice()),
                };

                let signature_ok = impl_has_receiver == required.has_receiver
                    && (!required.has_receiver || required.receiver_mut == impl_receiver_mut)
                    && required.params == impl_explicit_params
                    && required.return_type == signature.return_type;

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

                    return Err(HirError {
                        kind: HirErrorKind::InterfaceSignatureMismatch {
                            struct_name: implementation.name.to_string(),
                            interface_name: interface_name.to_string(),
                            method_name: method_name.to_string(),
                            expected,
                            found,
                            impl_span: implementation.span,
                        },
                        span: impl_method.span,
                    });
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
                    _ => {
                        let struct_symbol = symbols.insert(impl_type);
                        let struct_id =
                            *self.struct_map.get(&struct_symbol).ok_or_else(|| HirError {
                                kind: HirErrorKind::UnknownType {
                                    name: impl_type.to_string(),
                                },
                                span: function.span,
                            })?;
                        Type::Struct(struct_id)
                    }
                };

                let method_symbol = symbols.insert(function.name);
                self.methods
                    .get(&(receiver_type, method_symbol))
                    .copied()
                    .ok_or_else(|| HirError {
                        kind: error_kind(function.name.to_string()),
                        span: function.span,
                    })
            }

            (Some(_), None) => Err(HirError {
                kind: error_kind(function.name.to_string()),
                span: function.span,
            }),

            (None, Some(impl_type)) => {
                let mangled = symbols.insert(&self.mangler.scoped_item(impl_type, function.name));
                self.functions.get(&mangled).copied().ok_or_else(|| HirError {
                    kind: error_kind(format!("{impl_type}::{}", function.name)),
                    span: function.span,
                })
            }
            (None, None) => {
                let sym = symbols.insert(&self.mangler.item(function.name));
                self.functions.get(&sym).copied().ok_or_else(|| HirError {
                    kind: error_kind(function.name.to_string()),
                    span: function.span,
                })
            }
        }
    }

    #[inline]
    fn resolve_return_type<'h>(
        &self,
        return_type: Option<&Spanned<statement::Type<'h>>>,
        symbols: &mut SymbolTable,
    ) -> Result<Type, HirError<'h>> {
        return_type
            .map(|s| lower::resolve_annotation(symbols, &self.struct_map, &s.value(), s.span()))
            .transpose()
            .map(|opt| opt.unwrap_or_default())
    }

    #[inline]
    fn resolve_params<'h>(
        &self,
        params: &[statement::Parameter<'h>],
        symbols: &mut SymbolTable,
    ) -> Result<Vec<Type>, HirError<'h>> {
        params
            .iter()
            .map(|p| {
                lower::resolve_annotation(symbols, &self.struct_map, &p.typ.value(), p.typ.span())
            })
            .collect()
    }

    fn extend_constants<'d, 's>(
        &mut self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
        in_std: bool,
    ) -> Result<(), HirError<'s>> {
        struct ConstDecl<'d, 's> {
            mangled_name: SymbolId,
            impl_type: Option<&'d str>,
            ast: &'d statement::Const<'s>,
        }

        let mut decls: HashMap<SymbolId, ConstDecl<'d, 's>> = HashMap::new();

        // collect top-level constants
        for c in &declarations.constants {
            let symbol_id = symbols.insert(&self.mangler.item(c.name));
            if decls.contains_key(&symbol_id) {
                return Err(HirError {
                    kind: HirErrorKind::DuplicateConstant {
                        name: c.name.to_string(),
                    },
                    span: c.span,
                });
            }

            decls.insert(
                symbol_id,
                ConstDecl {
                    mangled_name: symbol_id,
                    impl_type: None,
                    ast: c,
                },
            );
        }

        // collect impl constants
        for imp in &declarations.impls {
            for c in &imp.constants {
                let mangled = self.mangler.scoped_item(imp.name, c.name);
                let symbol_id = symbols.insert(&mangled);
                if decls.contains_key(&symbol_id) {
                    return Err(HirError {
                        kind: HirErrorKind::DuplicateConstant {
                            name: format!("{}::{}", imp.name, c.name),
                        },
                        span: c.span,
                    });
                }

                decls.insert(
                    symbol_id,
                    ConstDecl {
                        mangled_name: symbol_id,
                        impl_type: Some(imp.name),
                        ast: c,
                    },
                );
            }
        }

        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        let mut sorted = Vec::new();

        fn find_dependencies<'d, 'i>(
            expr: &expression::Expression<'i>,
            current_impl: Option<&str>,
            mangler: &Mangler,
            symbols: &SymbolTable,
            decls: &HashMap<SymbolId, ConstDecl<'d, 'i>>,
            deps: &mut Vec<SymbolId>,
        ) {
            use expression::Expression as Expr;

            match expr {
                Expr::Identifier(name, _) => {
                    if let Some(impl_type) = current_impl {
                        let mangled = mangler.scoped_item(impl_type, name);
                        if let Some(symbol_id) = symbols.get_id(&mangled) {
                            if decls.contains_key(&symbol_id) {
                                deps.push(symbol_id);
                                return;
                            }
                        }
                    }
                    if let Some(symbol_id) = symbols.get_id(&mangler.item(name)) {
                        if decls.contains_key(&symbol_id) {
                            deps.push(symbol_id);
                        }
                    }
                }
                Expr::QualifiedName {
                    qualifier, name, ..
                } => {
                    let mangled = mangler.scoped_item(qualifier, name);
                    if let Some(symbol_id) = symbols.get_id(&mangled) {
                        if decls.contains_key(&symbol_id) {
                            deps.push(symbol_id);
                        }
                    }
                }
                Expr::Unary { expr, .. } | Expr::Cast { expr, .. } => {
                    find_dependencies(expr, current_impl, mangler, symbols, decls, deps)
                }
                Expr::Binary { left, right, .. } => {
                    find_dependencies(left, current_impl, mangler, symbols, decls, deps);
                    find_dependencies(right, current_impl, mangler, symbols, decls, deps);
                }
                Expr::Assignment { target, value, .. } => {
                    find_dependencies(target, current_impl, mangler, symbols, decls, deps);
                    find_dependencies(value, current_impl, mangler, symbols, decls, deps);
                }
                Expr::Field { expr, .. } => {
                    find_dependencies(expr, current_impl, mangler, symbols, decls, deps)
                }
                Expr::Struct { fields, .. } => {
                    for f in fields {
                        find_dependencies(&f.value, current_impl, mangler, symbols, decls, deps);
                    }
                }
                Expr::Call { callee, args, .. } => {
                    find_dependencies(callee, current_impl, mangler, symbols, decls, deps);
                    for arg in args {
                        find_dependencies(arg, current_impl, mangler, symbols, decls, deps);
                    }
                }
                Expr::QualifiedCall { args, .. } => {
                    for arg in args {
                        find_dependencies(arg, current_impl, mangler, symbols, decls, deps);
                    }
                }
                Expr::TypeIntrinsic { .. } => {}
                Expr::Integer(_, _)
                | Expr::Float(_, _)
                | Expr::String(_, _)
                | Expr::Char(_, _)
                | Expr::Bool(_, _) => {}
            }
        }

        fn dfs<'d, 's>(
            symbol_id: SymbolId,
            mangler: &Mangler,
            symbols: &SymbolTable,
            decls: &HashMap<SymbolId, ConstDecl<'d, 's>>,
            visiting: &mut HashSet<SymbolId>,
            visited: &mut HashSet<SymbolId>,
            sorted: &mut Vec<SymbolId>,
        ) -> Result<(), HirError<'s>> {
            if visiting.contains(&symbol_id) {
                let decl = decls.get(&symbol_id).unwrap();
                let name = match decl.impl_type {
                    Some(impl_type) => format!("{}::{}", impl_type, decl.ast.name),
                    _ => decl.ast.name.to_string(),
                };
                return Err(HirError {
                    kind: HirErrorKind::CircularConstant { name },
                    span: decl.ast.span,
                });
            }
            if !visited.contains(&symbol_id) {
                visiting.insert(symbol_id);
                if let Some(decl) = decls.get(&symbol_id) {
                    let mut deps = Vec::new();
                    find_dependencies(
                        &decl.ast.value,
                        decl.impl_type,
                        mangler,
                        symbols,
                        decls,
                        &mut deps,
                    );
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
                &decl.ast.typ.value(),
                decl.ast.typ.span(),
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
}

#[inline]
fn resolve_primitive_type(name: &str) -> Option<Type> {
    match name {
        "i8" => Some(Type::I8),
        "u8" => Some(Type::U8),
        "i16" => Some(Type::I16),
        "u16" => Some(Type::U16),
        "i32" => Some(Type::I32),
        "u32" => Some(Type::U32),
        "i64" => Some(Type::I64),
        "u64" => Some(Type::U64),
        "f32" => Some(Type::F32),
        "f64" => Some(Type::F64),
        "bool" => Some(Type::Bool),
        "uptr" => Some(Type::Uptr),
        "iptr" => Some(Type::Iptr),
        "char" => Some(Type::Char),
        "str" => Some(Type::Str),
        "string" => Some(Type::String),
        "unit" => Some(Type::Unit),
        _ => None,
    }
}
