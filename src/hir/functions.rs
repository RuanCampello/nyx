use std::collections::HashMap;

use crate::{
    hir::{
        FunctionId, SymbolId, Type,
        error::{HirError, HirErrorKind},
        symbols::SymbolTable,
    },
    parser::statement,
};

#[derive(Debug)]
pub(in crate::hir) struct FunctionSignature {
    name: SymbolId,
    params: Vec<Type>,
    return_type: Type,
}

pub(in crate::hir) type Functions = HashMap<SymbolId, FunctionId>;

pub fn collect_function_signatures<'h>(
    statements: &[statement::Statement<'h>],
    symbols: &mut SymbolTable,
) -> Result<(Vec<FunctionSignature>, Functions), HirError<'h>> {
    let mut signatures = Vec::new();
    let mut functions = Functions::new();

    for statement in statements {
        let function = match statement {
            statement::Statement::Fn(func) => func,
            other => {
                return Err(HirError {
                    kind: HirErrorKind::TopLevelNonFunction,
                });
            }
        };

        let symbol = symbols.insert(function.name);
        if functions.contains_key(&symbol) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateFunction {
                    name: function.name.to_string(),
                },
            });
        }

        let function_id = FunctionId(signatures.len() as u32);
        functions.insert(symbol, function_id);

        let params = function
            .params
            .iter()
            .map(|param| Type::from(param.typ))
            .collect::<Vec<_>>();
        let return_type = function.return_type.map(From::from).unwrap_or(Type::Unit);

        signatures.push(FunctionSignature {
            return_type,
            params,
            name: symbol,
        })
    }
    todo!()
}
