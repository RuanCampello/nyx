#![allow(unused)]

use crate::{
    hir::{
        Function, FunctionId, Intrinsic, Method, Struct, StructId, SymbolId, SymbolTable, Type,
        declarations::Declarations,
        error::{HirError, HirErrorKind},
        lower::{self},
    },
    lexer::Spanned,
    parser::statement,
};
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};

/// The single accumulated namespace for a compilation
///
/// Grows incrementally as modules are loaded: structs and function
/// signatures are assigned monotonically increasing IDs across all modules
pub struct Scope {
    pub signatures: Vec<FunctionSignature>,
    pub functions: Functions,
    pub methods: Methods,
    pub structs: Vec<Struct>,
    pub struct_map: Structs,
    pub interfaces: Interfaces,
    pub interface_impls: InterfaceImpls,
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
pub(in crate::hir) type Methods = HashMap<(StructId, SymbolId), FunctionId>;
pub(in crate::hir) type Interfaces = HashMap<SymbolId, InterfaceSignature>;
pub(in crate::hir) type InterfaceImpls = HashSet<(StructId, SymbolId)>;

impl Scope {
    pub fn new() -> Self {
        Self {
            signatures: Vec::new(),
            functions: HashMap::new(),
            methods: HashMap::new(),
            structs: Vec::new(),
            struct_map: HashMap::new(),
            interfaces: HashMap::new(),
            interface_impls: HashSet::new(),
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
    ) -> Result<(), HirError<'s>> {
        self.extend_structs(declarations, symbols)?;
        self.extend_interfaces(declarations, symbols)?;
        self.extend_signatures(declarations, symbols)?;
        self.validate_interfaces(declarations, symbols)?;
        self.validate_interface_hierarchy(declarations, symbols)?;

        Ok(())
    }

    pub fn lower_functions<'d, 's>(
        &self,
        declarations: &Declarations<'d, 's>,
        symbols: &mut SymbolTable,
    ) -> Result<Vec<Function>, HirError<'s>> {
        declarations
            .functions()
            .map(|function| {
                let id = self.function_id(function, symbols, None, |name| {
                    HirErrorKind::UnknownFunction { name }
                })?;
                lower::FunctionBuilder::new(self, symbols, id, function).lower()
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
        let mut states = vec![lower::Visit::Unvisited; local_declarations.len()];

        for id in 0..local_declarations.len() {
            lower::lower_struct(
                id,
                &local_declarations,
                &local_map,
                symbols,
                &mut lowered,
                &mut states,
            )?;
        }

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
    ) -> Result<(), HirError<'h>> {
        for function in &declarations.functions {
            if function.receiver.is_some() {
                return Err(HirError {
                    kind: HirErrorKind::ReceiverOutsideImpl,
                    span: function.span,
                });
            }

            let symbol = symbols.insert(function.name);
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
            let intrinsic = Intrinsic::from_str(symbols.get(symbol)).ok();

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

        for implementation in &declarations.impls {
            let struct_symbol = symbols.insert(implementation.name);
            let struct_id = *self.struct_map.get(&struct_symbol).ok_or_else(|| HirError {
                kind: HirErrorKind::UnknownType {
                    name: implementation.name.to_string(),
                },
                span: implementation.span,
            })?;

            if let Some(interface_name) = implementation.interface {
                let interface = symbols.insert(interface_name);
                self.interface_impls.insert((struct_id, interface));
            }

            for method in &implementation.methods {
                let method_symbol = symbols.insert(method.name);
                let mangled = symbols.insert(&format!("{}__{}", implementation.name, method.name));

                match method.receiver {
                    Some(receiver) => {
                        if self.methods.contains_key(&(struct_id, method_symbol)) {
                            return Err(HirError {
                                kind: HirErrorKind::DuplicateMethod {
                                    struct_name: implementation.name.to_string(),
                                    name: method.name.to_string(),
                                },
                                span: method.span,
                            });
                        }

                        let id = FunctionId(self.signatures.len() as u32);
                        self.methods.insert((struct_id, method_symbol), id);

                        let mut params = Vec::with_capacity(method.params.len() + 1);
                        params.push(Type::Ref {
                            mutable: receiver.mutable,
                            to: struct_id,
                        });
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
                                receiver: struct_id,
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
                let struct_symbol = symbols.insert(impl_type);
                let struct_id = *self.struct_map.get(&struct_symbol).ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownType {
                        name: impl_type.to_string(),
                    },
                    span: function.span,
                })?;

                let method_symbol = symbols.insert(function.name);
                self.methods.get(&(struct_id, method_symbol)).copied().ok_or_else(|| HirError {
                    kind: error_kind(function.name.to_string()),
                    span: function.span,
                })
            }

            (Some(_), None) => Err(HirError {
                kind: error_kind(function.name.to_string()),
                span: function.span,
            }),

            (None, Some(impl_type)) => {
                let mangled = symbols.insert(&format!("{impl_type}__{}", function.name));
                self.functions.get(&mangled).copied().ok_or_else(|| HirError {
                    kind: error_kind(format!("{impl_type}::{}", function.name)),
                    span: function.span,
                })
            }
            (None, None) => {
                let sym = symbols.insert(function.name);
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
}
