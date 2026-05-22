use crate::{
    hir::error::HirError,
    parser::statement::{Const, Function, Impl, Interface, Statement, Struct, UseDecl},
};

#[derive(Debug)]
pub(in crate::hir) struct Declarations<'d, 'src> {
    pub uses: Vec<&'d UseDecl<'src>>,
    pub structs: Vec<&'d Struct<'src>>,
    pub constants: Vec<&'d Const<'src>>,
    pub functions: Vec<&'d Function<'src>>,
    pub interfaces: Vec<&'d Interface<'src>>,
    pub impls: Vec<&'d Impl<'src>>,
}

impl<'d, 'src> Declarations<'d, 'src> {
    pub fn partition(statements: &'d [Statement<'src>]) -> Result<Self, HirError<'src>> {
        let mut declarations = Self {
            uses: Vec::new(),
            structs: Vec::new(),
            constants: Vec::new(),
            functions: Vec::new(),
            interfaces: Vec::new(),
            impls: Vec::new(),
        };

        for statement in statements {
            match statement {
                Statement::Fn(f) => declarations.functions.push(f),
                Statement::Interface(i) => declarations.interfaces.push(i),
                Statement::Use(u) => declarations.uses.push(u),
                Statement::Struct(s) => declarations.structs.push(s),
                Statement::Impl(i) => declarations.impls.push(i),
                Statement::Const(c) => declarations.constants.push(c),

                other => {
                    return Err(HirError {
                        kind: super::error::HirErrorKind::TopLevelNonFunction,
                        span: other.span(),
                    });
                }
            }
        }

        Ok(declarations)
    }

    pub fn functions(&self) -> impl Iterator<Item = &'d Function<'src>> + '_ {
        self.functions
            .iter()
            .copied()
            .chain(self.impls.iter().flat_map(|i| i.methods.iter()))
    }
}
