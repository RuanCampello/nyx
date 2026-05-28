use crate::parser::expression::StructField;
use crate::{
    lexer::Spanned,
    parser::{expression::*, statement::*},
};

pub type Env<'src> = std::collections::HashMap<&'src str, Type<'src>>;

pub fn subst_spanned_type<'src>(
    t: &Spanned<Type<'src>>,
    env: &Env<'src>,
    arena: &'src bumpalo::Bump,
) -> Spanned<Type<'src>> {
    Spanned::new(subst_type(t.value_ref(), env, arena), t.span())
}

/// substitute type variables from `env`, flattening `generic(name, concrete_args)` to `named(mangled)`
pub fn subst_type<'src>(t: &Type<'src>, env: &Env<'src>, arena: &'src bumpalo::Bump) -> Type<'src> {
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

pub fn subst_expr<'src>(
    e: &Expression<'src>,
    env: &Env<'src>,
    arena: &'src bumpalo::Bump,
) -> Expression<'src> {
    match e {
        Expression::Binary { left, operator, right, span } => Expression::Binary {
            left: Box::new(subst_expr(left, env, arena)),
            operator: *operator,
            right: Box::new(subst_expr(right, env, arena)),
            span: *span,
        },
        Expression::Unary { operator, expr, span } => Expression::Unary {
            operator: *operator,
            expr: Box::new(subst_expr(expr, env, arena)),
            span: *span,
        },
        Expression::Assignment { target, value, span } => Expression::Assignment {
            target: Box::new(subst_expr(target, env, arena)),
            value: Box::new(subst_expr(value, env, arena)),
            span: *span,
        },
        Expression::Field { expr, field, span } => Expression::Field {
            expr: Box::new(subst_expr(expr, env, arena)),
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
                    value: subst_expr(&f.value, env, arena),
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
            // if the callee is an identifier and we have type_args, mangle the name
            let new_type_args: Vec<_> =
                type_args.iter().map(|a| subst_spanned_type(a, env, arena)).collect();

            let new_callee = match callee.as_ref() {
                Expression::Identifier(name, id_span) if !new_type_args.is_empty() => {
                    let mangled = arena.alloc_str(&mangle(name, &new_type_args));
                    Box::new(Expression::Identifier(mangled, *id_span))
                },
                _ => Box::new(subst_expr(callee, env, arena)),
            };
            let new_args = args.iter().map(|a| subst_expr(a, env, arena)).collect();
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
            let new_name = match !new_type_args.is_empty() {
                true => arena.alloc_str(&mangle(*name, &new_type_args)),
                _ => *name,
            };
            let new_args = args.iter().map(|a| subst_expr(a, env, arena)).collect();

            Expression::QualifiedCall {
                qualifier: *qualifier,
                name: new_name,
                args: new_args,
                type_args: vec![],
                span: *span,
            }
        },
        Expression::Cast { expr, target_type, span } => Expression::Cast {
            expr: Box::new(subst_expr(expr, env, arena)),
            target_type: subst_spanned_type(target_type, env, arena),
            span: *span,
        },
        _ => e.clone(),
    }
}

pub fn subst_block<'src>(
    b: &Block<'src>,
    env: &Env<'src>,
    bump: &'src bumpalo::Bump,
) -> Block<'src> {
    Block {
        statements: b.statements.iter().map(|s| subst_stmt(s, env, bump)).collect(),
        span: b.span,
    }
}

pub fn subst_stmt<'src>(
    s: &Statement<'src>,
    env: &Env<'src>,
    bump: &'src bumpalo::Bump,
) -> Statement<'src> {
    match s {
        Statement::Let(l) => Statement::Let(Let {
            mutable: l.mutable,
            name: l.name,
            typ: l.typ.as_ref().map(|t| subst_spanned_type(t, env, bump)),
            value: l.value.as_ref().map(|v| subst_expr(v, env, bump)),
            span: l.span,
        }),
        Statement::Return(r) => Statement::Return(Return {
            value: r.value.as_ref().map(|v| subst_expr(v, env, bump)),
            span: r.span,
        }),
        Statement::If(i) => Statement::If(subst_if(i, env, bump)),
        Statement::While(w) => Statement::While(While {
            condition: subst_expr(&w.condition, env, bump),
            body: subst_block(&w.body, env, bump),
            span: w.span,
        }),
        Statement::Expr(e, span) => Statement::Expr(subst_expr(e, env, bump), *span),
        Statement::Block(b) => Statement::Block(subst_block(b, env, bump)),
        _ => s.clone(),
    }
}

fn subst_if<'src>(i: &If<'src>, env: &Env<'src>, bump: &'src bumpalo::Bump) -> If<'src> {
    If {
        condition: subst_expr(&i.condition, env, bump),
        then_branch: subst_block(&i.then_branch, env, bump),
        else_branch: i.else_branch.as_ref().map(|e| {
            Box::new(match e.as_ref() {
                Else::If(nested) => Else::If(subst_if(nested, env, bump)),
                Else::Block(b) => Else::Block(subst_block(b, env, bump)),
                Else::Expr(e) => Else::Expr(subst_expr(e, env, bump)),
            })
        }),
        span: i.span,
    }
}

pub fn subst_fn<'src>(
    f: &Function<'src>,
    name: &'src str,
    env: &Env<'src>,
    bump: &'src bumpalo::Bump,
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
                typ: subst_spanned_type(&p.typ, env, bump),
                span: p.span,
            })
            .collect(),
        return_type: f.return_type.as_ref().map(|t| subst_spanned_type(t, env, bump)),
        body: subst_block(&f.body, env, bump),
        is_const: f.is_const,
        is_pub: f.is_pub,
        inline: f.inline,
        span: f.span,
    }
}

pub fn mangle_type_str(t: &Type<'_>) -> String {
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

/// produce a mangled name: `base$arg1$arg2`
pub fn mangle(base: &str, args: &[Spanned<Type<'_>>]) -> String {
    let args_str: Vec<String> = args.iter().map(|a| mangle_type_str(&a.value())).collect();
    format!("{base}${}", args_str.join("$"))
}
