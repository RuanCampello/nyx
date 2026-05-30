//! AST-level monomorphisation pass.
//!
//! Owned by HIR because monomorphisation is a *semantic* step, not a syntactic one.
//! It walks parser AST nodes only because the
//! current pipeline still represents generic bodies in their AST form

use crate::{
    hir::error::HirError,
    lexer::{Spanned, token::Span},
    parser::{
        expression::{Expression, StructField, UnaryOperator},
        statement::{
            self, Block, Else, Enum, EnumVariant, Function, If, Impl, Interface, InterfaceMethod,
            Let, Match, MatchArm, Parameter, Return, Statement, Struct, Type, While,
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

/// Type-argument inference pre-pass
///
/// Walks every function/impl-method body tracking a per-body local variable type
/// environment, and fills in empty `type_args` on generic calls, struct literals and
/// enum-variant constructors that were written without an explicit turbofish
struct Inferrer<'src, 't> {
    templates: &'t Templates<'src>,
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

    // infer and fill in empty `type_args` on calls/struct literals/variant
    // constructors that lack turbofish, by unifying declared template types against the
    // inferred types of the supplied argument/field expressions
    let inferrer = Inferrer { templates };
    for stmt in statements.iter_mut() {
        inferrer.infer_stmt(stmt);
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

    /// Scan a type annotation
    ///
    /// a `Generic(name, args)` whose `name` is a template produces an instantiation
    fn collect_type(&mut self, ty: &Type<'src>) {
        match ty {
            Type::Ref(inner) => self.collect_type(inner),
            Type::Generic(name, args) => {
                for a in args {
                    self.collect_type(a.value_ref());
                }
                if self.templates.contains(name) {
                    self.push_instantiation(name, get_args(args));
                }
            },
            _ => {},
        }
    }

    fn collect_fn_signature(&mut self, f: &Function<'src>) {
        for param in &f.params {
            self.collect_type(param.typ.value_ref());
        }

        if let Some(rt) = &f.return_type {
            self.collect_type(rt.value_ref());
        }
    }

    fn collect_stmt(&mut self, stmt: &Statement<'src>) {
        match stmt {
            Statement::Fn(f) => {
                self.collect_fn_signature(f);
                self.collect_block(&f.body);
            },
            Statement::Impl(i) => {
                for m in &i.methods {
                    self.collect_fn_signature(m);
                    self.collect_block(&m.body);
                }
            },
            Statement::Interface(iface) => self.collect_interface(iface),
            Statement::Struct(s) => {
                for field in &s.fields {
                    self.collect_type(field.typ.value_ref());
                }
            },
            Statement::Enum(e) => {
                for v in &e.variants {
                    if let Some(p) = &v.payload {
                        self.collect_type(p.value_ref());
                    }
                }
            },
            Statement::Const(c) => {
                self.collect_type(c.typ.value_ref());
                self.collect_expr(&c.value);
            },
            Statement::Let(l) => {
                if let Some(t) = &l.typ {
                    self.collect_type(t.value_ref());
                }
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
            Statement::Match(m) => {
                self.collect_expr(&m.scrutinee);
                for arm in &m.arms {
                    self.collect_expr(&arm.body);
                }
            },
            Statement::Expr(e, _) => self.collect_expr(e),
            Statement::Block(b) => self.collect_block(b),
            _ => {},
        }
    }

    /// Scan an interface's method signatures for instantiations
    ///
    /// The interface's own generic params (e.g. `Rhs`) are NOT template names, so a `Named("Rhs")` is left
    /// alone only `Generic(name, ..)` with a template `name` is collected
    fn collect_interface(&mut self, iface: &Interface<'src>) {
        for method in &iface.methods {
            for param in &method.params {
                self.collect_type(param.typ.value_ref());
            }

            if let Some(typ) = &method.return_type {
                self.collect_type(typ.value_ref());
            }

            if let Some(body) = &method.body {
                self.collect_block(body);
            }
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

impl<'src, 't> Inferrer<'src, 't> {
    fn infer_stmt(&self, stmt: &mut Statement<'src>) {
        match stmt {
            Statement::Fn(f) => self.infer_fn(f),
            Statement::Impl(i) => {
                for m in &mut i.methods {
                    self.infer_fn(m);
                }
            },
            _ => {},
        }
    }

    fn infer_fn(&self, f: &mut Function<'src>) {
        let mut env: Env<'src> = Env::new();
        for p in &f.params {
            env.insert(p.name, p.typ.value());
        }
        self.infer_block(&mut f.body, &mut env);
    }

    fn infer_block(&self, block: &mut Block<'src>, env: &mut Env<'src>) {
        for stmt in &mut block.statements {
            self.infer_body_stmt(stmt, env);
        }
    }

    fn infer_body_stmt(&self, stmt: &mut Statement<'src>, env: &mut Env<'src>) {
        match stmt {
            Statement::Let(l) => {
                if let Some(v) = &mut l.value {
                    self.infer_expr(v, env);
                }
                // record the binding's type: explicit annotation wins, else infer the init
                let ty = l
                    .typ
                    .as_ref()
                    .map(|t| t.value())
                    .or_else(|| l.value.as_ref().and_then(|v| self.infer_expr_type(v, env)));
                if let Some(ty) = ty {
                    env.insert(l.name, ty);
                }
            },
            Statement::Return(r) => {
                if let Some(v) = &mut r.value {
                    self.infer_expr(v, env);
                }
            },
            Statement::If(i) => self.infer_if(i, env),
            Statement::While(w) => {
                self.infer_expr(&mut w.condition, env);
                self.infer_block(&mut w.body, env);
            },
            Statement::Expr(e, _) => self.infer_expr(e, env),
            Statement::Block(b) => {
                let mut inner = env.clone();
                self.infer_block(b, &mut inner);
            },
            Statement::Match(m) => {
                self.infer_expr(&mut m.scrutinee, env);
                for arm in &mut m.arms {
                    let mut inner = env.clone();
                    self.infer_expr(&mut arm.body, &mut inner);
                }
            },
            _ => {},
        }
    }

    fn infer_if(&self, i: &mut If<'src>, env: &mut Env<'src>) {
        self.infer_expr(&mut i.condition, env);
        let mut then_env = env.clone();
        self.infer_block(&mut i.then_branch, &mut then_env);
        if let Some(else_branch) = &mut i.else_branch {
            match else_branch.as_mut() {
                Else::If(nested) => self.infer_if(nested, env),
                Else::Block(b) => {
                    let mut else_env = env.clone();
                    self.infer_block(b, &mut else_env);
                },
                Else::Expr(e) => self.infer_expr(e, env),
            }
        }
    }

    fn infer_expr(&self, expr: &mut Expression<'src>, env: &Env<'src>) {
        match expr {
            Expression::Call { callee, args, type_args, .. } => {
                for a in args.iter_mut() {
                    self.infer_expr(a, env);
                }

                if let Some(inferred) = type_args
                    .is_empty()
                    .then(|| callee.as_ref())
                    .and_then(|expr| match expr {
                        Expression::Identifier(name, _) => Some(name),
                        _ => None,
                    })
                    .and_then(|name| self.templates.fns.get(*name))
                    .and_then(|tmpl| self.solve_fn(tmpl, args, env))
                {
                    *type_args = inferred;
                }
            },
            Expression::Struct { name, fields, type_args, .. } => {
                for f in fields.iter_mut() {
                    self.infer_expr(&mut f.value, env);
                }
                if type_args.is_empty() {
                    if let Some(tmpl) = self.templates.structs.get(*name) {
                        if let Some(inferred) = self.solve_struct(tmpl, fields, env) {
                            *type_args = inferred;
                        }
                    }
                }
            },
            Expression::QualifiedCall { qualifier, name, args, type_args, .. } => {
                for a in args.iter_mut() {
                    self.infer_expr(a, env);
                }
                if type_args.is_empty() {
                    if let Some(tmpl) = self.templates.enums.get(*qualifier) {
                        if let Some(inferred) = self.solve_enum_variant(tmpl, name, args, env) {
                            *type_args = inferred;
                        }
                    }
                }
            },
            Expression::Binary { left, right, .. } => {
                self.infer_expr(left, env);
                self.infer_expr(right, env);
            },
            Expression::Unary { expr, .. } | Expression::Field { expr, .. } => {
                self.infer_expr(expr, env)
            },
            Expression::Assignment { target, value, .. } => {
                self.infer_expr(target, env);
                self.infer_expr(value, env);
            },
            Expression::Cast { expr, .. } => self.infer_expr(expr, env),
            _ => {},
        }
    }

    fn solve_fn(
        &self,
        tmpl: &Function<'src>,
        args: &[Expression<'src>],
        env: &Env<'src>,
    ) -> Option<Vec<Spanned<Type<'src>>>> {
        let generics = tmpl.generics.iter().map(|g| g.name).collect();
        let mut bindings = Env::new();
        for (param, arg) in tmpl.params.iter().zip(args.iter()) {
            if let Some(actual) = self.infer_expr_type(arg, env) {
                unify(param.typ.value_ref(), &actual, &generics, &mut bindings);
            }
        }

        self.collect_solved(&tmpl.generics, &bindings)
    }

    fn solve_struct(
        &self,
        tmpl: &Struct<'src>,
        fields: &[StructField<'src>],
        env: &Env<'src>,
    ) -> Option<Vec<Spanned<Type<'src>>>> {
        let generics = tmpl.generics.iter().map(|g| g.name).collect();
        let mut bindings = Env::new();
        for field in fields {
            if let Some(decl) = tmpl.fields.iter().find(|f| f.name == field.name) {
                if let Some(actual) = self.infer_expr_type(&field.value, env) {
                    unify(decl.typ.value_ref(), &actual, &generics, &mut bindings);
                }
            }
        }

        self.collect_solved(&tmpl.generics, &bindings)
    }

    fn solve_enum_variant(
        &self,
        tmpl: &Enum<'src>,
        variant: &str,
        args: &[Expression<'src>],
        env: &Env<'src>,
    ) -> Option<Vec<Spanned<Type<'src>>>> {
        let generics = tmpl.generics.iter().map(|g| g.name).collect();
        let mut bindings = Env::new();
        let v = tmpl.variants.iter().find(|v| v.name == variant)?;
        // unify the variant's payload type against the first (single) argument
        if let (Some(payload), Some(arg)) = (&v.payload, args.first()) {
            if let Some(actual) = self.infer_expr_type(arg, env) {
                unify(payload.value_ref(), &actual, &generics, &mut bindings);
            }
        }

        self.collect_solved(&tmpl.generics, &bindings)
    }

    /// Assemble the solved bindings into generic-param order
    fn collect_solved(
        &self,
        generics: &[statement::GenericBound<'src>],
        bindings: &Env<'src>,
    ) -> Option<Vec<Spanned<Type<'src>>>> {
        if generics.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(generics.len());
        for g in generics {
            let ty = bindings.get(g.name)?;
            out.push(Spanned::new(ty.clone(), Span::default()));
        }
        Some(out)
    }

    /// Best-effort static type of an expression, used only to solve generic params
    fn infer_expr_type(&self, expr: &Expression<'src>, env: &Env<'src>) -> Option<Type<'src>> {
        match expr {
            Expression::Identifier(name, _) => env.get(*name).cloned(),
            Expression::Struct { name, type_args, .. } => Some(match type_args.is_empty() {
                true => Type::Named(name),
                false => Type::Generic(name, type_args.clone()),
            }),
            Expression::Unary { operator: UnaryOperator::Ref, expr, .. } => {
                Some(Type::Ref(Box::new(self.infer_expr_type(expr, env)?)))
            },
            Expression::Unary { operator: UnaryOperator::Deref, expr, .. } => {
                match self.infer_expr_type(expr, env)? {
                    Type::Ref(inner) => Some(*inner),
                    other => Some(other),
                }
            },
            Expression::Cast { target_type, .. } => Some(target_type.value()),
            Expression::Integer(..) => Some(Type::I32),
            Expression::Bool(..) => Some(Type::Bool),
            Expression::Char(..) => Some(Type::Char),
            Expression::Float(..) => Some(Type::F64),
            Expression::String(..) => Some(Type::Str),
            Expression::Call { callee, args, type_args, .. } => {
                let Expression::Identifier(fn_name, _) = callee.as_ref() else {
                    return None;
                };
                let tmpl = self.templates.fns.get(*fn_name)?;
                // resolve the call's type args (turbofish or freshly inferred)
                let solved = match type_args.is_empty() {
                    false => get_args(type_args),
                    true => get_args(&self.solve_fn(tmpl, args, env)?),
                };
                let ret = tmpl.return_type.as_ref()?;
                // map generic param name -> concrete arg, then substitute into the return type
                let map =
                    tmpl.generics.iter().map(|g| g.name).zip(solved.iter().cloned()).collect();
                Some(subst_type_simple(ret.value_ref(), &map))
            },
            _ => None,
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

/// Substitute generic param names with concrete types without allocating (no flattening
/// of `Generic`). Used by inference to compute a template fn's concrete return type.
fn subst_type_simple<'src>(t: &Type<'src>, map: &Env<'src>) -> Type<'src> {
    match t {
        Type::Named(name) => map.get(*name).cloned().unwrap_or(Type::Named(*name)),
        Type::Ref(inner) => Type::Ref(Box::new(subst_type_simple(inner, map))),
        Type::Generic(name, args) => Type::Generic(
            *name,
            args.iter()
                .map(|a| Spanned::new(subst_type_simple(a.value_ref(), map), a.span()))
                .collect(),
        ),
        _ => t.clone(),
    }
}

/// Unify a declared (template) type against an actual (inferred) type, recording any
/// generic-param solutions into `bindings`. Best-effort: unsolvable shapes are ignored.
fn unify<'src>(
    decl: &Type<'src>,
    actual: &Type<'src>,
    generics: &HashSet<&str>,
    bindings: &mut Env<'src>,
) {
    // strip matching `&` layers on both sides
    if let (Type::Ref(d), Type::Ref(a)) = (decl, actual) {
        unify(d, a, generics, bindings);
        return;
    }

    match decl {
        // a bare generic param binds directly to the actual type
        Type::Named(name) if generics.contains(*name) => {
            bindings.entry(*name).or_insert_with(|| actual.clone());
        },
        // `Generic(n, decl_args)` vs `Generic(n, actual_args)` -> recurse pairwise
        Type::Generic(dn, dargs) => {
            if let Type::Generic(an, aargs) = actual {
                if dn == an {
                    for (d, a) in dargs.iter().zip(aargs.iter()) {
                        unify(d.value_ref(), a.value_ref(), generics, bindings);
                    }
                }
            }
        },
        // a declared `&T` against a non-ref actual: peel the declared ref and retry
        Type::Ref(d) => unify(d, actual, generics, bindings),
        _ => {},
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
        Statement::Match(m) => Statement::Match(Match {
            scrutinee: subst_expr(&m.scrutinee, env, templates, arena),
            arms: m
                .arms
                .iter()
                .map(|arm| MatchArm {
                    // pattern carries no generic type args; copy as-is
                    pattern: arm.pattern.clone(),
                    guard: arm.guard.clone(),
                    body: subst_expr(&arm.body, env, templates, arena),
                    span: arm.span,
                })
                .collect(),
            span: m.span,
        }),
        Statement::Interface(iface) => {
            Statement::Interface(subst_interface(iface, env, templates, arena))
        },
        _ => s.clone(),
    }
}

/// Rewrite an interface's method signatures, flattening `Generic(name, args)` template
/// references (e.g. `Optional<Ordering>` -> `Named("Optional$Ordering")`). The
/// interface's own generic params surface as `Named` and are left untouched by `subst_type`.
fn subst_interface<'src>(
    iface: &Interface<'src>,
    env: &Env<'src>,
    templates: &HashSet<&'src str>,
    arena: &'src bumpalo::Bump,
) -> Interface<'src> {
    let methods = iface
        .methods
        .iter()
        .map(|m| InterfaceMethod {
            name: m.name,
            generics: m.generics.clone(),
            receiver: m.receiver,
            params: m
                .params
                .iter()
                .map(|p| Parameter {
                    name: p.name,
                    mutable: p.mutable,
                    typ: subst_spanned_type(&p.typ, env, arena),
                    span: p.span,
                })
                .collect(),
            return_type: m.return_type.as_ref().map(|t| subst_spanned_type(t, env, arena)),
            body: m.body.as_ref().map(|b| subst_block(b, env, templates, arena)),
            span: m.span,
        })
        .collect();

    Interface {
        name: iface.name,
        generics: iface.generics.clone(),
        superinterfaces: iface.superinterfaces.clone(),
        methods,
        is_pub: iface.is_pub,
        span: iface.span,
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
        Type::Never => "!".into(),
    }
}

/// Produce a mangled name: `base$arg1$arg2`.
fn mangle(base: &str, args: &[Spanned<Type<'_>>]) -> String {
    let args_str: Vec<String> = args.iter().map(|a| mangle_type_str(&a.value())).collect();
    format!("{base}${}", args_str.join("$"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn mono_debug(src: &str) -> String {
        let mut statements = Parser::new(src).parse().unwrap();
        let bump = bumpalo::Bump::new();
        monomorphise(&mut statements, &bump).unwrap();
        format!("{statements:#?}")
    }

    #[test]
    fn infers_fn_type_args_without_turbofish() {
        // `pick(7, 9)` must produce a concrete `pick$i32` and a call to it
        let out = mono_debug(
            r#"
            fn pick<T>(a: T, b: T): T { a }
            fn main(): i32 { pick(7, 9) }
            "#,
        );
        assert!(out.contains("pick$i32"), "expected concrete fn pick$i32 in:\n{out}");
    }

    #[test]
    fn infers_struct_type_args_without_turbofish() {
        // `Pair { a: 2, b: 5 }` must produce a concrete struct `Pair$i32`
        let out = mono_debug(
            r#"
            struct Pair<T> { a: T, b: T }
            fn main(): i32 {
                let p = Pair { a: 2, b: 5 };
                0
            }
            "#,
        );
        assert!(out.contains("Pair$i32"), "expected concrete struct Pair$i32 in:\n{out}");
    }

    #[test]
    fn turbofish_path_still_works() {
        let out = mono_debug(
            r#"
            fn id<T>(x: T): T { x }
            fn main(): i32 { id::<i32>(3) }
            "#,
        );
        assert!(out.contains("id$i32"), "expected concrete fn id$i32 in:\n{out}");
    }

    #[test]
    fn collects_instantiation_from_type_annotation() {
        let out = mono_debug(
            r#"
            struct Box<T> { val: T }
            fn make(): Box<i32> { Box::<i32> { val: 1 } }
            fn main(): i32 {
                let b: Box<i32> = make();
                0
            }
            "#,
        );
        assert!(out.contains("Box$i32"), "expected concrete struct Box$i32 in:\n{out}");
    }

    #[test]
    fn rewrites_generic_in_interface_method_signature() {
        let out = mono_debug(
            r#"
            enum Optional<T> { Some(T), None }
            enum Ordering { Less, Equal, Greater }
            interface PartialOrd<Rhs> {
                fn partial_compare(&self, other: &Rhs): Optional<Ordering>;
            }
            fn main(): i32 { 0 }
            "#,
        );
        assert!(
            out.contains("Optional$Ordering"),
            "expected Optional$Ordering generated and rewritten in:\n{out}"
        );
        assert!(out.contains("\"Rhs\""), "interface generic param Rhs must be preserved:\n{out}");
    }
}
