use crate::{
    hir::error::HirError,
    parser::statement::{Function, Impl, Interface, Statement, Struct, UseDecl},
};

#[derive(Debug)]
pub(in crate::hir) struct Declarations<'d> {
    pub uses: Vec<&'d UseDecl<'d>>,
    pub structs: Vec<&'d Struct<'d>>,
    pub functions: Vec<&'d Function<'d>>,
    pub interfaces: Vec<&'d Interface<'d>>,
    pub impls: Vec<&'d Impl<'d>>,
}

impl<'d> Declarations<'d> {
    pub fn partition(statements: &'d [Statement<'d>]) -> Result<Self, HirError<'d>> {
        let mut declarations = Self {
            uses: Vec::new(),
            structs: Vec::new(),
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

    pub fn functions(&self) -> impl Iterator<Item = &Function<'d>> + '_ {
        self.functions
            .iter()
            .copied()
            .chain(self.impls.iter().flat_map(|i| i.methods.iter()))
    }
}
