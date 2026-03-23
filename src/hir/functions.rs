use std::collections::HashMap;

use crate::{
    hir::{
        Function, FunctionId, Local, LocalId, Parameter, SymbolId, Type,
        error::{HirError, HirErrorKind},
        symbols::SymbolTable,
    },
    lexer::token::Span,
    parser::statement,
};

#[derive(Debug)]
pub(in crate::hir) struct FunctionSignature {
    name: SymbolId,
    params: Vec<Type>,
    return_type: Type,
}

pub(in crate::hir) struct FunctionBuilder<'s, 'f> {
    signatures: &'s [FunctionSignature],
    functions: &'s Functions,
    locals: Vec<Local>,
    scopes: Vec<HashMap<SymbolId, LocalId>>,
    return_type: Type,
    function: Option<statement::Function<'f>>,
    next_local: u32,
    symbols: &'s mut SymbolTable,
}

pub(in crate::hir) type Functions = HashMap<SymbolId, FunctionId>;

impl<'s, 'f> FunctionBuilder<'s, 'f> {
    fn new(
        signatures: &'s [FunctionSignature],
        functions: &'s Functions,
        symbols: &'s mut SymbolTable,
        function: statement::Function<'f>,
    ) -> Self {
        let return_type = function
            .return_type
            .as_ref()
            .map(From::from)
            .unwrap_or(Type::Unit);

        Self {
            return_type,
            functions,
            symbols,
            signatures,
            function: Some(function),
            next_local: 0,
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
        }
    }

    #[inline(always)]
    pub fn lower(mut self) -> Result<Function, HirError<'f>> {
        let function = self.function.take().expect("function to be present");
        let symbol = self.symbols.insert(function.name);
        let id = *self
            .functions
            .get(&symbol)
            .expect("function id present for this name");
        let signatures = &self.signatures[id.0 as usize];

        let mut params = Vec::new();
        for (parameter, &typ) in function.params.iter().zip(signatures.params.iter()) {
            let symbol = self.symbols.insert(parameter.name);
            let id = self.declare_local(symbol, typ, true)?;

            params.push(Parameter {
                typ: typ,
                id,
                name: symbol,
            })
        }

        todo!()
    }

    fn declare_local(
        &mut self,
        name: SymbolId,
        typ: Type,
        mutable: bool,
    ) -> Result<LocalId, HirError<'f>> {
        let scope = self
            .scopes
            .last_mut()
            .expect("at least one scope is always present");

        if scope.contains_key(&name) {
            return Err(HirError {
                kind: HirErrorKind::DuplicateBind {
                    name: self.symbols.get(name).to_string(),
                },
            });
        }

        let id = LocalId(self.next_local);
        self.next_local += 1;

        scope.insert(name, id);
        self.locals.push(Local {
            id,
            name,
            typ,
            mutable,
        });

        Ok(id)
    }
}

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

    Ok((signatures, functions))
}
