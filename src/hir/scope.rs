use super::{
    Function, FunctionId, Intrinsic, Local, LocalId, Method, Struct, StructId, SymbolId,
    SymbolTable, Type, declarations::Declarations, error::HirError, error::HirErrorKind,
};
use crate::{hir::lower::FunctionBuilder, parser::statement};
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

    pub fn extend<'s>(
        &mut self,
        declarations: &Declarations<'s>,
        symbols: &mut SymbolTable,
    ) -> Result<(), HirError<'s>> {
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
                FunctionBuilder::new(self, symbols, id, function).lower()
            })
            .collect()
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
