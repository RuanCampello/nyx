use crate::{
    hir::error::HirError,
    lexer::token::Span,
    parser::statement::{
        self, Const, Enum, Function, Impl, Interface, ItemKind, Statement, Static, Struct, UseDecl,
    },
};

#[derive(Debug)]
pub(in crate::hir) struct Declarations<'d, 'src> {
    pub uses: Vec<&'d UseDecl<'src>>,
    pub structs: Vec<&'d Struct<'src>>,
    pub enums: Vec<&'d Enum<'src>>,
    pub constants: Vec<&'d Const<'src>>,
    pub statics: Vec<&'d Static<'src>>,
    pub functions: Vec<&'d Function<'src>>,
    pub interfaces: Vec<&'d Interface<'src>>,
    pub impls: Vec<&'d Impl<'src>>,
    pub docs: Vec<(Span, &'d [&'src str])>,
}

impl<'d, 'src> Declarations<'d, 'src> {
    pub fn partition<'b>(
        statements: &'d mut [Statement<'src>],
        lookup_interface: impl Fn(&str) -> Option<&'b Interface<'src>>,
    ) -> Result<Self, HirError<'src>>
    where
        'src: 'b,
    {
        statement::inject_default_methods(statements, lookup_interface);
        Self::collect(statements)
    }

    /// categorise already-injected top-level items by kind, gathering doc comments
    pub fn collect(statements: &'d [Statement<'src>]) -> Result<Self, HirError<'src>> {
        let (declarations, errors) = Self::collect_recovering(statements);

        match errors.into_iter().next() {
            Some(error) => Err(error),
            None => Ok(declarations),
        }
    }

    /// As [Declarations::collect], but skipping and reporting every statement
    /// that is not an item instead of stopping at the first one
    pub fn collect_recovering(statements: &'d [Statement<'src>]) -> (Self, Vec<HirError<'src>>) {
        let mut errors = Vec::new();
        let mut declarations = Self {
            uses: Vec::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            constants: Vec::new(),
            statics: Vec::new(),
            functions: Vec::new(),
            interfaces: Vec::new(),
            impls: Vec::new(),
            docs: Vec::new(),
        };

        for statement in statements.iter() {
            let Statement::Item(item) = statement else {
                errors.push(HirError {
                    kind: super::error::HirErrorKind::TopLevelNonFunction,
                    span: statement.span(),
                });
                continue;
            };

            declarations.docs.push((item.kind.span(), &item.docs));
            match &item.kind {
                ItemKind::Fn(f) => declarations.functions.push(f),
                ItemKind::Interface(i) => {
                    for (span, lines) in &i.member_docs {
                        declarations.docs.push((*span, lines));
                    }
                    declarations.interfaces.push(i);
                },
                ItemKind::Use(u) => declarations.uses.push(u),
                ItemKind::Struct(s) => {
                    for (span, lines) in &s.member_docs {
                        declarations.docs.push((*span, lines));
                    }
                    declarations.structs.push(s);
                },
                ItemKind::Enum(e) => {
                    for (span, lines) in &e.member_docs {
                        declarations.docs.push((*span, lines));
                    }
                    declarations.enums.push(e);
                },
                ItemKind::Impl(i) => {
                    for (span, lines) in &i.member_docs {
                        declarations.docs.push((*span, lines));
                    }
                    declarations.impls.push(i);
                },
                ItemKind::Const(c) => declarations.constants.push(c),
                ItemKind::Static(s) => declarations.statics.push(s),
            }
        }

        (declarations, errors)
    }

    pub fn functions(&self) -> impl Iterator<Item = &'d Function<'src>> + '_ {
        self.functions
            .iter()
            .copied()
            .chain(self.impls.iter().flat_map(|i| i.methods.iter()))
    }
}
