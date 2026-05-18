use crate::{
    hir::{
        Function, FunctionId, Intrinsic, Method, Struct, StructId, SymbolId, SymbolTable, Type,
        declarations::Declarations,
        error::HirError,
        error::HirErrorKind,
        lower::{self},
    },
    parser::statement,
};
use std::collections::HashMap;

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
}

#[derive(Debug)]
pub(in crate::hir) struct FunctionSignature {
    pub name: SymbolId,
    pub params: Vec<Type>,
    pub return_type: Type,
    pub intrinsic: Option<Intrinsic>,
    pub method: Option<Method>,
}

#[derive(Debug)]
pub(in crate::hir) struct InterfaceSignature {
    pub name: SymbolId,
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

impl Scope {
    pub fn new() -> Self {
        Self {
            signatures: Vec::new(),
            functions: HashMap::new(),
            methods: HashMap::new(),
            structs: Vec::new(),
            struct_map: HashMap::new(),
            interfaces: HashMap::new(),
        }
    }

    /// Analyse `declarations` and extend this scope with their declarations
    ///
    /// Ids are assigned relative to what is already in the scope so this can be called
    /// once per module in dependency order
    pub fn extend<'s>(
        &mut self,
        declarations: &Declarations<'s>,
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'s>> {
        self.extend_structs(declarations, symbols)?;
        self.extend_interfaces(declarations, symbols)?;

        Ok(())
    }

    pub fn lower_functions<'s>(
        &self,
        declarations: &Declarations<'s>,
        symbols: &mut SymbolTable,
    ) -> Result<Vec<Function>, HirError<'s>> {
        declarations
            .functions()
            .map(|function| {
                let id = self.function_id(function, symbols)?;
                lower::FunctionBuilder::new(self, symbols, id, function).lower()
            })
            .collect()
    }

    fn extend_structs<'s>(
        &mut self,
        declarations: &Declarations<'s>,
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

    fn extend_interfaces<'s>(
        &mut self,
        declarations: &Declarations<'s>,
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

            let methods = interface
                .methods
                .iter()
                .map(|method| {
                    let name = symbols.insert(method.name);
                    let has_receiver = method.receiver.is_some();
                    let receiver_mut = method.receiver.map(|r| r.mutable).unwrap_or(false);
                    let mut params =
                        Vec::with_capacity(method.params.len() + usize::from(has_receiver));
                    if has_receiver {
                        params.push(Type::Unit);
                    }
                    for param in &method.params {
                        params.push(Type::from(&param.typ.value()));
                    }
                    let return_type = method
                        .return_type
                        .as_ref()
                        .map(|t| Type::from(&t.value()))
                        .unwrap_or(Type::Unit);
                    InterfaceMethodSignature {
                        name,
                        params,
                        return_type,
                        has_receiver,
                        receiver_mut,
                    }
                })
                .collect();

            self.interfaces.insert(name, InterfaceSignature { name, methods });
        }

        Ok(())
    }

    #[inline]
    fn function_id<'s>(
        &self,
        function: &statement::Function<'s>,
        symbols: &mut SymbolTable,
    ) -> Result<FunctionId, HirError<'s>> {
        match (function.receiver, function.impl_type) {
            (Some(_), _) => {
                let impl_type = function.impl_type.expect("method must know its impl type");

                let struct_symbol = symbols.insert(impl_type);
                let struct_id = *self.struct_map.get(&struct_symbol).ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownType {
                        name: impl_type.to_string(),
                    },
                    span: function.span,
                })?;

                let method_symbol = symbols.insert(function.name);
                self.methods.get(&(struct_id, method_symbol)).copied().ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownFunction {
                        name: function.name.to_string(),
                    },
                    span: function.span,
                })
            }

            (None, Some(impl_type)) => {
                let mangled = symbols.insert(&format!("{impl_type}__{}", function.name));
                self.functions.get(&mangled).copied().ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownFunction {
                        name: format!("{impl_type}::{}", function.name),
                    },
                    span: function.span,
                })
            }
            (None, None) => {
                let sym = symbols.insert(function.name);
                self.functions.get(&sym).copied().ok_or_else(|| HirError {
                    kind: HirErrorKind::UnknownFunction {
                        name: function.name.to_string(),
                    },
                    span: function.span,
                })
            }
        }
    }
}
