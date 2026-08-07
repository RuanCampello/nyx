//! Static-declaration analysis
//!
//! A [constant] is a value spliced into each of its uses, so it never needs an
//! address. A static is the opposite: one piece of storage the compiler lays out
//! in the executable, which is what lets `static mut` carry state between calls.
//!
//! That makes the initialiser a build-time question. It is lowered exactly like a
//! constant initialiser and then folded to the single [Literal] the storage is
//! born holding, so the backend only ever has to write one scalar.
//!
//! [constant]: crate::hir::Constant

use crate::{
    hir::{
        self, Literal, Static, StaticId,
        declarations::Declarations,
        error::{HirError, hir_error},
        lower,
        scope::Scope,
        type_resolver,
    },
    parser::expression::UnaryOperator,
};

/// Lower every top-level static, folding its initialiser to a literal
///
/// Runs after [constants], so an initialiser may name a `const`
///
/// [constants]: crate::hir::constants::extend
pub(in crate::hir) fn extend<'hir, 'd, 's>(
    scope: &mut Scope<'hir>,
    declarations: &Declarations<'d, 's>,
    arena: &'hir bumpalo::Bump,
) -> Result<(), HirError<'hir>>
where
    's: 'hir,
{
    for declaration in declarations.statics.iter().copied() {
        let mangled = scope.mangler.item(declaration.name);
        let symbol = scope.symbols.insert(&mangled);

        let resolved = {
            let (structs, enums, arrays) = (&scope.struct_map, &scope.enum_map, &scope.arrays);
            let ctx = type_resolver::ResolveCtx::root(&scope.symbols, structs, enums, arrays);
            type_resolver::resolve_annotation(
                &ctx,
                &declaration.typ.value(),
                declaration.typ.span(),
            )
        };
        let typ = resolved.or_else(|error| scope.poison(error))?;

        let lowered = lower::lower_const(scope, &declaration.value, typ, arena);
        let (value, _) = match lowered {
            Ok(lowered) => lowered,
            Err(error) => {
                scope.soft(error)?;
                continue;
            },
        };

        let Some(init) = fold_literal(value) else {
            let name = arena.alloc_str(declaration.name);
            scope.soft(hir_error!(declaration.value.span(), NonConstStaticInit { name }))?;
            continue;
        };

        let id = StaticId(scope.statics.len() as u32);
        scope.statics.insert(
            symbol,
            Static {
                id,
                name: symbol,
                typ,
                is_mut: declaration.is_mut,
                is_pub: declaration.is_pub,
                init,
                decl_span: declaration.span,
                name_span: declaration.name_span,
            },
        );
    }

    Ok(())
}

/// The single scalar an initialiser settles on, if it settles on one at all
#[inline]
fn fold_literal(expr: &hir::Expression<'_>) -> Option<Literal> {
    use hir::ExpressionKind as Kind;

    match &expr.kind {
        Kind::Literal(literal) => Some(*literal),
        Kind::Const(constant) => fold_literal(constant.value),
        Kind::Unary { operator: UnaryOperator::Neg, expr } => match fold_literal(expr)? {
            Literal::Int(value) => Some(Literal::Int(-value)),
            Literal::Float(value) => Some(Literal::Float(-value)),
            _ => None,
        },
        _ => None,
    }
}
