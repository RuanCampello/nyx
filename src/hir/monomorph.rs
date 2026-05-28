use crate::{
    hir::error::HirError,
    lexer::{Spanned, token::Span},
    parser::{
        expression,
        statement::{self, Block, Else, Enum, Function, Impl, Parameter, Statement, Struct, Type},
        subst::{self as s, Env},
    },
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Instantiation<'src> {
    name: &'src str,
    args: Vec<Type<'src>>,
}

/// Traverses the AST to collect all generic instantiation (turbofish/struct declarations)
/// that need to be concrete during monomorphisation
struct Collector<'src, 't> {
    templates: &'t HashSet<&'src str>,
    worklist: Vec<Instantiation<'src>>,
    seen: HashSet<Instantiation<'src>>,
}

/// Run AST-level monomorphisation on `statements`
///
/// Removes generic templates, emits concrete instances, and rewrites all
/// remaining type references so HIR never sees a `Type::Generic` or turbofish
#[derive(Default)]
pub(in crate::hir) struct Templates<'src> {
    pub(crate) structs: HashMap<&'src str, Struct<'src>>,
    pub(crate) enums: HashMap<&'src str, Enum<'src>>,
    pub(crate) fns: HashMap<&'src str, Function<'src>>,
    pub(crate) impls: Vec<Impl<'src>>,
}

pub(in crate::hir) fn monomorphise<'src>(
    statements: &mut Vec<Statement<'src>>,
    arena: &'src bumpalo::Bump,
) -> Result<(), HirError<'src>> {
    let mut templates = Templates::default();

    extract_templates(statements, &mut templates);
    monomorphise_with_templates(statements, &templates, arena)
}

pub(in crate::hir) fn monomorphise_with_templates<'src>(
    statements: &mut Vec<Statement<'src>>,
    templates: &Templates<'src>,
    arena: &'src bumpalo::Bump,
) -> Result<(), HirError<'src>> {
    if templates.structs.is_empty() && templates.fns.is_empty() && templates.enums.is_empty() {
        // no generics in this module, nothing to do :D
        return Ok(());
    }

    let template_names = templates
        .structs
        .keys()
        .chain(templates.fns.keys())
        .chain(templates.enums.keys())
        .copied()
        .collect();
    let mut collector = Collector::new(&template_names);

    for stmt in statements.iter() {
        collector.collect_stmt(stmt);
    }

    let mut new_statements: Vec<Statement<'src>> = Vec::new();

    while let Some(inst) = collector.worklist.pop() {
        // env: generic_param_name to concrete type
        if let Some(tmpl) = templates.structs.get(inst.name) {
            let env = build_env(tmpl.generics.iter().map(|s| *s), &inst.args, arena);
            let mangled_name = arena.alloc_str(&s::mangle(inst.name, &spanned_args(&inst.args)));

            let concrete_struct = subst_struct(tmpl, mangled_name, &env, arena);
            new_statements.push(Statement::Struct(concrete_struct));

            // emit associated impl templates whose receiver matches this struct
            for impl_tmpl in
                templates.impls.iter().filter(|i| impl_receiver_name(i) == Some(inst.name))
            {
                let concrete_impl =
                    subst_impl(impl_tmpl, mangled_name, &env, &template_names, arena);
                collector.collect_impl(&concrete_impl);
                new_statements.push(Statement::Impl(concrete_impl));
            }

            continue;
        }

        if let Some(tmpl) = templates.enums.get(inst.name) {
            let env = build_env(tmpl.generics.iter().map(|s| *s), &inst.args, arena);
            let mangled_name = arena.alloc_str(&s::mangle(inst.name, &spanned_args(&inst.args)));

            let concrete_enum = s::subst_enum(tmpl, mangled_name, &env, arena);
            new_statements.push(Statement::Enum(concrete_enum));

            // emit associated impl templates whose receiver matches this enum
            for impl_tmpl in
                templates.impls.iter().filter(|i| impl_receiver_name(i) == Some(inst.name))
            {
                let concrete_impl =
                    subst_impl(impl_tmpl, mangled_name, &env, &template_names, arena);
                collector.collect_impl(&concrete_impl);
                new_statements.push(Statement::Impl(concrete_impl));
            }

            continue;
        }

        if let Some(tmpl) = templates.fns.get(inst.name) {
            let env = build_env(tmpl.generics.iter().map(|gb| gb.name), &inst.args, arena);
            let mangled_name = arena.alloc_str(&s::mangle(inst.name, &spanned_args(&inst.args)));

            let concrete_fn = s::subst_fn(tmpl, mangled_name, &env, &template_names, arena);
            collector.collect_block(&concrete_fn.body);
            new_statements.push(Statement::Fn(concrete_fn));

            continue;
        }
    }

    let empty = HashMap::new();
    for stmt in statements.iter_mut() {
        *stmt = rewrite_stmt(stmt, &empty, &template_names, arena);
    }

    // append concrete instances
    statements.extend(new_statements);

    Ok(())
}

pub(in crate::hir) fn extract_templates<'src>(
    statements: &mut Vec<Statement<'src>>,
    templates: &mut Templates<'src>,
) {
    statements.retain(|stmt| match stmt {
        Statement::Struct(s) if !s.generics.is_empty() => {
            templates.structs.insert(s.name, s.clone());
            false
        },
        Statement::Enum(e) if !e.generics.is_empty() => {
            templates.enums.insert(e.name, e.clone());
            false
        },
        Statement::Fn(f) if !f.generics.is_empty() => {
            templates.fns.insert(f.name, f.clone());
            false
        },
        Statement::Impl(i) if !i.generics.is_empty() => {
            templates.impls.push(i.clone());
            false
        },
        _ => true,
    });
}

impl<'src, 'a> Collector<'src, 'a> {
    fn new(templates: &'a HashSet<&'src str>) -> Self {
        Self {
            templates,
            worklist: Vec::with_capacity(1 << 5),
            seen: HashSet::with_capacity(1 << 5),
        }
    }

    fn push_instantiation(&mut self, name: &'src str, args: Vec<Type<'src>>) {
        let inst = Instantiation { name, args };
        if self.seen.insert(inst.clone()) {
            self.worklist.push(inst);
        }
    }

    fn collect_stmt(&mut self, stmt: &Statement<'src>) {
        match stmt {
            Statement::Fn(f) => self.collect_block(&f.body),
            Statement::Impl(i) => {
                for m in &i.methods {
                    self.collect_block(&m.body);
                }
            },
            Statement::Let(l) => {
                if let Some(v) = &l.value {
                    self.collect_expr(v);
                }
            },
            Statement::Return(r) => {
                if let Some(v) = &r.value {
                    self.collect_expr(v);
                }
            },
            Statement::If(i) => self.collect_if(i),
            Statement::While(w) => {
                self.collect_expr(&w.condition);
                self.collect_block(&w.body);
            },
            Statement::Expr(e, _) => self.collect_expr(e),
            Statement::Block(b) => self.collect_block(b),
            _ => {},
        }
    }

    fn collect_if(&mut self, i: &statement::If<'src>) {
        self.collect_expr(&i.condition);
        self.collect_block(&i.then_branch);

        if let Some(else_branch) = &i.else_branch {
            match else_branch.as_ref() {
                Else::If(nested) => self.collect_if(nested),
                Else::Block(b) => self.collect_block(b),
                Else::Expr(e) => self.collect_expr(e),
            }
        }
    }

    fn collect_block(&mut self, block: &Block<'src>) {
        for stmt in &block.statements {
            self.collect_stmt(stmt);
        }
    }

    fn collect_impl(&mut self, imp: &Impl<'src>) {
        for m in &imp.methods {
            self.collect_block(&m.body);
        }
    }

    fn collect_expr(&mut self, expr: &expression::Expression<'src>) {
        use expression::Expression as E;

        match expr {
            E::Call { callee, args, type_args, .. } => {
                if !type_args.is_empty() {
                    if let E::Identifier(name, _) = callee.as_ref() {
                        if self.templates.contains(name) {
                            self.push_instantiation(name, get_args(type_args));
                        }
                    }
                }
                self.collect_expr(callee);
                for a in args {
                    self.collect_expr(a);
                }
            },
            E::Struct { name, fields, type_args, .. } => {
                if !type_args.is_empty() && self.templates.contains(name) {
                    self.push_instantiation(name, get_args(type_args));
                }
                for f in fields {
                    self.collect_expr(&f.value);
                }
            },
            E::QualifiedCall { qualifier, name, args, type_args, .. } => {
                if !type_args.is_empty() {
                    if self.templates.contains(qualifier) {
                        self.push_instantiation(qualifier, get_args(type_args));
                    } else if self.templates.contains(name) {
                        self.push_instantiation(name, get_args(type_args));
                    }
                }
                for a in args {
                    self.collect_expr(a);
                }
            },
            E::Binary { left, right, .. } => {
                self.collect_expr(left);
                self.collect_expr(right);
            },
            E::Unary { expr, .. } | E::Field { expr, .. } => self.collect_expr(expr),
            E::Assignment { target, value, .. } => {
                self.collect_expr(target);
                self.collect_expr(value);
            },
            _ => {},
        }
    }
}

/// rewrite a statement with an empty env only flattens generic types and mangles turbofish calls
fn rewrite_stmt<'src>(
    s: &Statement<'src>,
    env: &Env<'src>,
    templates: &HashSet<&'src str>,
    arena: &'src bumpalo::Bump,
) -> Statement<'src> {
    match s {
        Statement::Fn(f) => Statement::Fn(Function {
            name: f.name,
            generics: f.generics.clone(),
            impl_type: f.impl_type,
            receiver: f.receiver,
            params: f
                .params
                .iter()
                .map(|p| Parameter {
                    name: p.name,
                    mutable: p.mutable,
                    typ: s::subst_spanned_type(&p.typ, env, arena),
                    span: p.span,
                })
                .collect(),
            return_type: f.return_type.as_ref().map(|t| s::subst_spanned_type(t, env, arena)),
            body: s::subst_block(&f.body, env, templates, arena),
            is_const: f.is_const,
            is_pub: f.is_pub,
            inline: f.inline,
            span: f.span,
        }),
        _ => s::subst_stmt(s, env, templates, arena),
    }
}

fn build_env<'src, 'a>(
    params: impl Iterator<Item = &'a str>,
    args: &[Type<'src>],
    arena: &'src bumpalo::Bump,
) -> Env<'src>
where
    'src: 'a,
{
    params
        .zip(args.iter())
        .map(|(param, arg)| (arena.alloc_str(param) as &str, arg.clone()))
        .collect()
}

fn spanned_args<'src>(args: &[Type<'src>]) -> Vec<Spanned<Type<'src>>> {
    args.iter().map(|t| Spanned::new(t.clone(), Span::default())).collect()
}

#[inline]
fn impl_receiver_name<'src>(imp: &Impl<'src>) -> Option<&'src str> {
    match imp.receiver.value() {
        Type::Generic(name, _) => Some(name),
        Type::Named(name) => Some(name),
        _ => None,
    }
}

fn subst_struct<'src>(
    tmpl: &Struct<'src>,
    mangled_name: &'src str,
    env: &Env<'src>,
    bump: &'src bumpalo::Bump,
) -> Struct<'src> {
    Struct {
        name: mangled_name,
        generics: vec![],
        fields: tmpl
            .fields
            .iter()
            .map(|f| statement::StructField {
                name: f.name,
                typ: s::subst_spanned_type(&f.typ, env, bump),
                span: f.span,
            })
            .collect(),
        repr: tmpl.repr,
        is_pub: tmpl.is_pub,
        span: tmpl.span,
    }
}

fn subst_impl<'src>(
    tmpl: &Impl<'src>,
    mangled_receiver: &'src str,
    env: &Env<'src>,
    templates: &HashSet<&'src str>,
    arena: &'src bumpalo::Bump,
) -> Impl<'src> {
    let methods = tmpl
        .methods
        .iter()
        .map(|m| {
            let mut substed = s::subst_fn(m, m.name, env, templates, arena);
            substed.impl_type = Some(mangled_receiver);
            substed
        })
        .collect();
    Impl {
        name: mangled_receiver,
        receiver: Spanned::new(Type::Named(mangled_receiver), tmpl.receiver.span()),
        interface_type: None,
        interface: None,
        generics: vec![],
        methods,
        constants: tmpl.constants.clone(),
        span: tmpl.span,
    }
}

#[inline(always)]
fn get_args<'src>(spanned: &[Spanned<Type<'src>>]) -> Vec<Type<'src>> {
    spanned.iter().map(|a| a.value().clone()).collect()
}
