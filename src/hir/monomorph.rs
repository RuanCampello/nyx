use crate::{
    hir::error::HirError,
    lexer::{Spanned, token::Span},
    parser::{
        expression,
        statement::{
            self, Block, Function, GenericBound, Impl, Parameter, Statement, Struct, Type,
        },
        subst::{self as s, Env},
    },
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Instantiation<'src> {
    name: &'src str,
    args: Vec<Type<'src>>,
}

/// Run AST-level monomorphisation on `statements`
///
/// Removes generic templates, emits concrete instances, and rewrites all
/// remaining type references so HIR never sees a `Type::Generic` or turbofish
pub(crate) fn monomorphise<'src>(
    statements: &mut Vec<Statement<'src>>,
    arena: &'src bumpalo::Bump,
) -> Result<(), HirError<'src>> {
    let mut struct_templates: HashMap<&'src str, Struct<'src>> = HashMap::new();
    let mut fn_templates: HashMap<&'src str, Function<'src>> = HashMap::new();
    let mut impl_templates: Vec<Impl<'src>> = Vec::new();

    statements.retain(|stmt| match stmt {
        Statement::Struct(s) if !s.generics.is_empty() => {
            struct_templates.insert(s.name, s.clone());
            false
        },
        Statement::Fn(f) if !f.generics.is_empty() => {
            fn_templates.insert(f.name, f.clone());
            false
        },
        Statement::Impl(i) if !i.generics.is_empty() => {
            impl_templates.push(i.clone());
            false
        },
        _ => true,
    });

    if struct_templates.is_empty() && fn_templates.is_empty() {
        // no generics in this module, nothing to do :D
        return Ok(());
    }

    let template_names = struct_templates.keys().chain(fn_templates.keys()).copied().collect();

    let mut worklist: Vec<Instantiation<'src>> = Vec::new();
    let mut seen: HashSet<Instantiation<'src>> = HashSet::new();

    for stmt in statements.iter() {
        collect_stmt(stmt, &template_names, &mut worklist, &mut seen);
    }

    let mut new_statements: Vec<Statement<'src>> = Vec::new();

    while let Some(inst) = worklist.pop() {
        // env: generic_param_name to concrete type
        if let Some(tmpl) = struct_templates.get(inst.name) {
            let env = build_env(tmpl.generics.iter().map(|s| *s), &inst.args, arena);
            let mangled_name = arena.alloc_str(&s::mangle(inst.name, &spanned_args(&inst.args)));
            let concrete_struct = subst_struct(tmpl, mangled_name, &env, arena);
            new_statements.push(Statement::Struct(concrete_struct));

            // emit associated impl templates whose receiver matches this struct
            for impl_tmpl in &impl_templates {
                if impl_receiver_name(impl_tmpl) == Some(inst.name) {
                    let concrete_impl = subst_impl(impl_tmpl, mangled_name, &env, arena);

                    collect_impl_instantiations(
                        &concrete_impl,
                        &template_names,
                        &mut worklist,
                        &mut seen,
                    );
                    new_statements.push(Statement::Impl(concrete_impl));
                }
            }
        } else if let Some(tmpl) = fn_templates.get(inst.name) {
            let env = build_env(
                tmpl.generics.iter().map(|gb: &GenericBound<'src>| gb.name),
                &inst.args,
                arena,
            );
            let mangled_name = arena.alloc_str(&s::mangle(inst.name, &spanned_args(&inst.args)));
            let concrete_fn = s::subst_fn(tmpl, mangled_name, &env, arena);

            collect_block_instantiations(
                &concrete_fn.body,
                &template_names,
                &mut worklist,
                &mut seen,
            );
            new_statements.push(Statement::Fn(concrete_fn));
        }
    }

    let empty = HashMap::new();
    for stmt in statements.iter_mut() {
        *stmt = rewrite_stmt(stmt, &empty, arena);
    }

    // append concrete instances
    statements.extend(new_statements);

    Ok(())
}

fn collect_stmt<'src>(
    stmt: &Statement<'src>,
    templates: &HashSet<&str>,
    out: &mut Vec<Instantiation<'src>>,
    seen: &mut HashSet<Instantiation<'src>>,
) {
    match stmt {
        Statement::Fn(f) => collect_block_instantiations(&f.body, templates, out, seen),
        Statement::Impl(i) => {
            for m in &i.methods {
                collect_block_instantiations(&m.body, templates, out, seen);
            }
        },
        Statement::Let(l) => {
            if let Some(v) = &l.value {
                collect_expr(v, templates, out, seen);
            }
        },
        Statement::Return(r) => {
            if let Some(v) = &r.value {
                collect_expr(v, templates, out, seen);
            }
        },
        Statement::If(i) => collect_if(i, templates, out, seen),
        Statement::While(w) => {
            collect_expr(&w.condition, templates, out, seen);
            collect_block_instantiations(&w.body, templates, out, seen);
        },
        Statement::Expr(e, _) => collect_expr(e, templates, out, seen),
        Statement::Block(b) => collect_block_instantiations(b, templates, out, seen),
        _ => {},
    }
}

fn collect_if<'src>(
    i: &statement::If<'src>,
    templates: &HashSet<&str>,
    out: &mut Vec<Instantiation<'src>>,
    seen: &mut HashSet<Instantiation<'src>>,
) {
    use statement::Else;
    collect_expr(&i.condition, templates, out, seen);
    collect_block_instantiations(&i.then_branch, templates, out, seen);

    if let Some(else_branch) = &i.else_branch {
        match else_branch.as_ref() {
            Else::If(nested) => collect_if(nested, templates, out, seen),
            Else::Block(b) => collect_block_instantiations(b, templates, out, seen),
            Else::Expr(e) => collect_expr(e, templates, out, seen),
        }
    }
}

fn collect_block_instantiations<'src>(
    block: &Block<'src>,
    templates: &HashSet<&str>,
    out: &mut Vec<Instantiation<'src>>,
    seen: &mut HashSet<Instantiation<'src>>,
) {
    for stmt in &block.statements {
        collect_stmt(stmt, templates, out, seen);
    }
}

fn collect_impl_instantiations<'src>(
    imp: &Impl<'src>,
    templates: &HashSet<&str>,
    out: &mut Vec<Instantiation<'src>>,
    seen: &mut HashSet<Instantiation<'src>>,
) {
    for m in &imp.methods {
        collect_block_instantiations(&m.body, templates, out, seen);
    }
}

fn collect_expr<'src>(
    expr: &expression::Expression<'src>,
    templates: &HashSet<&str>,
    out: &mut Vec<Instantiation<'src>>,
    seen: &mut HashSet<Instantiation<'src>>,
) {
    use expression::Expression as E;
    match expr {
        // `compare_boxes::<i32>(&b1, &b2)`: call with type_args on identifier callee
        E::Call { callee, args, type_args, .. } => {
            if !type_args.is_empty() {
                if let E::Identifier(name, _) = callee.as_ref() {
                    if templates.contains(name) {
                        push_instantiation(name, get_args(type_args), out, seen);
                    }
                }
            }
            collect_expr(callee, templates, out, seen);
            for a in args {
                collect_expr(a, templates, out, seen);
            }
        },
        // `box::<i32> { val: 42 }`: struct literal with type_args
        E::Struct { name, fields, type_args, .. } => {
            if !type_args.is_empty() && templates.contains(name) {
                push_instantiation(name, get_args(type_args), out, seen);
            }
            for f in fields {
                collect_expr(&f.value, templates, out, seen);
            }
        },
        E::QualifiedCall { name, args, type_args, .. } => {
            if !type_args.is_empty() && templates.contains(name) {
                push_instantiation(name, get_args(type_args), out, seen);
            }
            for a in args {
                collect_expr(a, templates, out, seen);
            }
        },
        E::Binary { left, right, .. } => {
            collect_expr(left, templates, out, seen);
            collect_expr(right, templates, out, seen);
        },
        E::Unary { expr, .. } => collect_expr(expr, templates, out, seen),
        E::Assignment { target, value, .. } => {
            collect_expr(target, templates, out, seen);
            collect_expr(value, templates, out, seen);
        },
        E::Field { expr, .. } => collect_expr(expr, templates, out, seen),
        _ => {},
    }
}

fn push_instantiation<'src>(
    name: &'src str,
    args: Vec<Type<'src>>,
    out: &mut Vec<Instantiation<'src>>,
    seen: &mut HashSet<Instantiation<'src>>,
) {
    let inst = Instantiation { name, args };
    if seen.insert(inst.clone()) {
        out.push(inst);
    }
}

/// rewrite a statement with an empty env only flattens generic types and mangles turbofish calls
fn rewrite_stmt<'src>(
    s: &Statement<'src>,
    env: &Env<'src>,
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
            body: s::subst_block(&f.body, env, arena),
            is_const: f.is_const,
            is_pub: f.is_pub,
            inline: f.inline,
            span: f.span,
        }),
        _ => s::subst_stmt(s, env, arena),
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
    arena: &'src bumpalo::Bump,
) -> Impl<'src> {
    let methods = tmpl
        .methods
        .iter()
        .map(|m| {
            let mut substed = s::subst_fn(m, m.name, env, arena);
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
