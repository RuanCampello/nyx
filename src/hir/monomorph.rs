//! AST-level monomorphisation pass.
//!
//! Owned by HIR because monomorphisation is a *semantic* step, not a syntactic one.
//! It walks parser AST nodes only because the
//! current pipeline still represents generic bodies in their AST form

use crate::{
    hir::error::HirError,
    lexer::{Spanned, token::Span},
    parser::{
        expression::{Expression, StructField},
        statement::{
            self, Block, Else, Enum, EnumVariant, Function, If, Impl, Let, Parameter, Return,
            Statement, Struct, Type, While,
        },
    },
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Instantiation<'src> {
    name: &'src str,
    args: Vec<Type<'src>>,
}

/// Traverses the AST to collect all generic instantiation (turbofish/struct declarations)
/// that need to be concrete during monomorphisation.
struct Collector<'src, 't> {
    templates: &'t HashSet<&'src str>,
    worklist: Vec<Instantiation<'src>>,
    seen: HashSet<Instantiation<'src>>,
}

/// Generic templates extracted from a module's statement list, keyed by source name
///
/// Drained by the worklist as concrete instances are emitted. Templates the worklist
/// never reaches are silently dropped (lazy specialisation)
#[derive(Default)]
pub(in crate::hir) struct Templates<'src> {
    pub(crate) structs: HashMap<&'src str, Struct<'src>>,
    pub(crate) enums: HashMap<&'src str, Enum<'src>>,
    pub(crate) fns: HashMap<&'src str, Function<'src>>,
    pub(crate) impls: Vec<Impl<'src>>,
}

type Env<'src> = HashMap<&'src str, Type<'src>>;

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
        if let Some(tmpl) = templates.structs.get(inst.name) {
            let env = build_env(tmpl.generics.iter().map(|gb| gb.name), &inst.args, arena);
            let mangled_name = arena.alloc_str(&mangle(inst.name, &spanned_args(&inst.args)));

            let concrete_struct = subst_struct(tmpl, mangled_name, &env, arena);
            new_statements.push(Statement::Struct(concrete_struct));

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
            let env = build_env(tmpl.generics.iter().map(|gb| gb.name), &inst.args, arena);
            let mangled_name = arena.alloc_str(&mangle(inst.name, &spanned_args(&inst.args)));

            let concrete_enum = subst_enum(tmpl, mangled_name, &env, arena);
            new_statements.push(Statement::Enum(concrete_enum));

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
            let mangled_name = arena.alloc_str(&mangle(inst.name, &spanned_args(&inst.args)));

            let concrete_fn = subst_fn(tmpl, mangled_name, &env, &template_names, arena);
            collector.collect_block(&concrete_fn.body);
            new_statements.push(Statement::Fn(concrete_fn));

            continue;
        }
    }

    let empty = Env::new();
    for stmt in statements.iter_mut() {
        *stmt = rewrite_stmt(stmt, &empty, &template_names, arena);
    }

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

    fn collect_if(&mut self, i: &If<'src>) {
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

    fn collect_expr(&mut self, expr: &Expression<'src>) {
        match expr {
            Expression::Call { callee, args, type_args, .. } => {
                if !type_args.is_empty() {
                    if let Expression::Identifier(name, _) = callee.as_ref() {
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
            Expression::Struct { name, fields, type_args, .. } => {
                if !type_args.is_empty() && self.templates.contains(name) {
                    self.push_instantiation(name, get_args(type_args));
                }
                for f in fields {
                    self.collect_expr(&f.value);
                }
            },
            Expression::QualifiedCall { qualifier, name, args, type_args, .. } => {
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
            Expression::Binary { left, right, .. } => {
                self.collect_expr(left);
                self.collect_expr(right);
            },
            Expression::Unary { expr, .. } | Expression::Field { expr, .. } => {
                self.collect_expr(expr)
            },
            Expression::Assignment { target, value, .. } => {
                self.collect_expr(target);
                self.collect_expr(value);
            },
            _ => {},
        }
    }
}

fn subst_spanned_type<'src>(
    t: &Spanned<Type<'src>>,
    env: &Env<'src>,
    arena: &'src bumpalo::Bump,
) -> Spanned<Type<'src>> {
    Spanned::new(subst_type(t.value_ref(), env, arena), t.span())
}

/// substitute type variables from `env`, flattening `Generic(name, concrete_args)` to
/// `Named(mangled)`.
fn subst_type<'src>(t: &Type<'src>, env: &Env<'src>, arena: &'src bumpalo::Bump) -> Type<'src> {
    match t {
        Type::Named(name) => env.get(*name).cloned().unwrap_or(Type::Named(*name)),
        Type::Ref(typ) => Type::Ref(Box::new(subst_type(typ, env, arena))),
        Type::Generic(name, args) => {
            let new_args: Vec<_> = args.iter().map(|a| subst_spanned_type(a, env, arena)).collect();
            let mangled = mangle(*name, &new_args);
            Type::Named(arena.alloc_str(&mangled))
        },
        _ => t.clone(),
    }
}

fn subst_expr<'src>(
    e: &Expression<'src>,
    env: &Env<'src>,
    templates: &HashSet<&'src str>,
    arena: &'src bumpalo::Bump,
) -> Expression<'src> {
    match e {
        Expression::Binary { left, operator, right, span } => Expression::Binary {
            left: Box::new(subst_expr(left, env, templates, arena)),
            operator: *operator,
            right: Box::new(subst_expr(right, env, templates, arena)),
            span: *span,
        },
        Expression::Unary { operator, expr, span } => Expression::Unary {
            operator: *operator,
            expr: Box::new(subst_expr(expr, env, templates, arena)),
            span: *span,
        },
        Expression::Assignment { target, value, span } => Expression::Assignment {
            target: Box::new(subst_expr(target, env, templates, arena)),
            value: Box::new(subst_expr(value, env, templates, arena)),
            span: *span,
        },
        Expression::Field { expr, field, span } => Expression::Field {
            expr: Box::new(subst_expr(expr, env, templates, arena)),
            field: *field,
            span: *span,
        },
        Expression::Struct { name, fields, type_args, span } => {
            let new_type_args: Vec<_> =
                type_args.iter().map(|a| subst_spanned_type(a, env, arena)).collect();
            let new_name = match !new_type_args.is_empty() {
                true => arena.alloc_str(&mangle(*name, &new_type_args)),
                _ => *name,
            };
            let new_fields = fields
                .iter()
                .map(|f| StructField {
                    name: f.name,
                    value: subst_expr(&f.value, env, templates, arena),
                    span: f.span,
                })
                .collect();
            Expression::Struct {
                name: new_name,
                fields: new_fields,
                type_args: vec![],
                span: *span,
            }
        },
        Expression::Call { callee, args, type_args, span } => {
            let new_type_args: Vec<_> =
                type_args.iter().map(|a| subst_spanned_type(a, env, arena)).collect();

            let new_callee = match callee.as_ref() {
                Expression::Identifier(name, id_span) if !new_type_args.is_empty() => {
                    let mangled = arena.alloc_str(&mangle(name, &new_type_args));
                    Box::new(Expression::Identifier(mangled, *id_span))
                },
                _ => Box::new(subst_expr(callee, env, templates, arena)),
            };
            let new_args = args.iter().map(|a| subst_expr(a, env, templates, arena)).collect();
            Expression::Call {
                callee: new_callee,
                args: new_args,
                type_args: vec![],
                span: *span,
            }
        },
        Expression::QualifiedCall { qualifier, name, args, type_args, span } => {
            let new_type_args: Vec<_> =
                type_args.iter().map(|a| subst_spanned_type(a, env, arena)).collect();
            let (new_qualifier, new_name) = match !new_type_args.is_empty() {
                true if templates.contains(qualifier) => {
                    (arena.alloc_str(&mangle(*qualifier, &new_type_args)) as &str, *name)
                },
                true => (*qualifier, arena.alloc_str(&mangle(*name, &new_type_args)) as &str),
                _ => (*qualifier, *name),
            };
            let new_args = args.iter().map(|a| subst_expr(a, env, templates, arena)).collect();

            Expression::QualifiedCall {
                qualifier: new_qualifier,
                name: new_name,
                args: new_args,
                type_args: vec![],
                span: *span,
            }
        },
        Expression::Cast { expr, target_type, span } => Expression::Cast {
            expr: Box::new(subst_expr(expr, env, templates, arena)),
            target_type: subst_spanned_type(target_type, env, arena),
            span: *span,
        },
        _ => e.clone(),
    }
}

fn subst_block<'src>(
    b: &Block<'src>,
    env: &Env<'src>,
    templates: &HashSet<&'src str>,
    arena: &'src bumpalo::Bump,
) -> Block<'src> {
    Block {
        statements: b.statements.iter().map(|s| subst_stmt(s, env, templates, arena)).collect(),
        span: b.span,
    }
}

fn subst_stmt<'src>(
    s: &Statement<'src>,
    env: &Env<'src>,
    templates: &HashSet<&'src str>,
    arena: &'src bumpalo::Bump,
) -> Statement<'src> {
    match s {
        Statement::Let(l) => Statement::Let(Let {
            mutable: l.mutable,
            name: l.name,
            typ: l.typ.as_ref().map(|t| subst_spanned_type(t, env, arena)),
            value: l.value.as_ref().map(|v| subst_expr(v, env, templates, arena)),
            span: l.span,
        }),
        Statement::Return(r) => Statement::Return(Return {
            value: r.value.as_ref().map(|v| subst_expr(v, env, templates, arena)),
            span: r.span,
        }),
        Statement::If(i) => Statement::If(subst_if(i, env, templates, arena)),
        Statement::While(w) => Statement::While(While {
            condition: subst_expr(&w.condition, env, templates, arena),
            body: subst_block(&w.body, env, templates, arena),
            span: w.span,
        }),
        Statement::Expr(e, span) => Statement::Expr(subst_expr(e, env, templates, arena), *span),
        Statement::Block(b) => Statement::Block(subst_block(b, env, templates, arena)),
        Statement::Enum(e) => Statement::Enum(subst_enum(e, e.name, env, arena)),
        _ => s.clone(),
    }
}

fn subst_if<'src>(
    i: &If<'src>,
    env: &Env<'src>,
    templates: &HashSet<&'src str>,
    arena: &'src bumpalo::Bump,
) -> If<'src> {
    If {
        condition: subst_expr(&i.condition, env, templates, arena),
        then_branch: subst_block(&i.then_branch, env, templates, arena),
        else_branch: i.else_branch.as_ref().map(|e| {
            Box::new(match e.as_ref() {
                Else::If(nested) => Else::If(subst_if(nested, env, templates, arena)),
                Else::Block(b) => Else::Block(subst_block(b, env, templates, arena)),
                Else::Expr(e) => Else::Expr(subst_expr(e, env, templates, arena)),
            })
        }),
        span: i.span,
    }
}

fn subst_fn<'src>(
    f: &Function<'src>,
    name: &'src str,
    env: &Env<'src>,
    templates: &HashSet<&'src str>,
    arena: &'src bumpalo::Bump,
) -> Function<'src> {
    Function {
        name,
        generics: vec![],
        impl_type: f.impl_type,
        receiver: f.receiver,
        params: f
            .params
            .iter()
            .map(|p| Parameter {
                name: p.name,
                mutable: p.mutable,
                typ: subst_spanned_type(&p.typ, env, arena),
                span: p.span,
            })
            .collect(),
        return_type: f.return_type.as_ref().map(|t| subst_spanned_type(t, env, arena)),
        body: subst_block(&f.body, env, templates, arena),
        is_const: f.is_const,
        is_pub: f.is_pub,
        inline: f.inline,
        span: f.span,
    }
}

fn subst_enum<'src>(
    tmpl: &Enum<'src>,
    mangled_name: &'src str,
    env: &Env<'src>,
    arena: &'src bumpalo::Bump,
) -> Enum<'src> {
    Enum {
        name: mangled_name,
        generics: vec![],
        variants: tmpl
            .variants
            .iter()
            .map(|v| EnumVariant {
                name: v.name,
                payload: v.payload.as_ref().map(|p| subst_spanned_type(p, env, arena)),
                value: v.value,
                span: v.span,
            })
            .collect(),
        repr: tmpl.repr.clone(),
        is_pub: tmpl.is_pub,
        span: tmpl.span,
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
                typ: subst_spanned_type(&f.typ, env, bump),
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
            let mut substed = subst_fn(m, m.name, env, templates, arena);
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
                    typ: subst_spanned_type(&p.typ, env, arena),
                    span: p.span,
                })
                .collect(),
            return_type: f.return_type.as_ref().map(|t| subst_spanned_type(t, env, arena)),
            body: subst_block(&f.body, env, templates, arena),
            is_const: f.is_const,
            is_pub: f.is_pub,
            inline: f.inline,
            span: f.span,
        }),
        _ => subst_stmt(s, env, templates, arena),
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

#[inline(always)]
fn get_args<'src>(spanned: &[Spanned<Type<'src>>]) -> Vec<Type<'src>> {
    spanned.iter().map(|a| a.value().clone()).collect()
}

fn mangle_type_str(t: &Type<'_>) -> String {
    match t {
        Type::I8 => "i8".into(),
        Type::U8 => "u8".into(),
        Type::I16 => "i16".into(),
        Type::U16 => "u16".into(),
        Type::I32 => "i32".into(),
        Type::U32 => "u32".into(),
        Type::I64 => "i64".into(),
        Type::U64 => "u64".into(),
        Type::F32 => "f32".into(),
        Type::F64 => "f64".into(),
        Type::Bool => "bool".into(),
        Type::Uptr => "uptr".into(),
        Type::Iptr => "iptr".into(),
        Type::Char => "char".into(),
        Type::Str => "str".into(),
        Type::String => "string".into(),
        Type::Named(name) => (*name).into(),
        Type::SelfType | Type::RefSelf => "self".into(),
        Type::Ref(inner) => format!("ref_{}", mangle_type_str(inner)),
        Type::Generic(name, args) => {
            let args_str: Vec<String> = args.iter().map(|a| mangle_type_str(&a.value())).collect();
            format!("{name}${}", args_str.join("$"))
        },
        Type::Unit => "unit".into(),
    }
}

/// Produce a mangled name: `base$arg1$arg2`.
fn mangle(base: &str, args: &[Spanned<Type<'_>>]) -> String {
    let args_str: Vec<String> = args.iter().map(|a| mangle_type_str(&a.value())).collect();
    format!("{base}${}", args_str.join("$"))
}
