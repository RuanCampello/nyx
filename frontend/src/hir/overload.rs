//! Resolution and canonicalisation of source-level operator overloads

use crate::{
    hir::{
        Expression, ExpressionKind, FunctionId, Res, Type, TypeKind,
        error::{HirError, hir_error},
        lang,
        lower::{FunctionBuilder, Lowered},
    },
    lexer::token::Span,
    parser::{
        expression::{self, BinaryOperator, UnaryOperator},
        statement,
    },
};

#[derive(Clone, Copy)]
struct IndexMethod<'hir> {
    function: FunctionId,
    index: Type<'hir>,
    output: Type<'hir>,
}

impl<'s, 'f, 'hir, 'src> FunctionBuilder<'s, 'f, 'hir, 'src>
where
    'src: 'hir,
{
    pub(super) fn lower_overloaded_comparison(
        &mut self,
        operator: BinaryOperator,
        left: Lowered<'hir>,
        right: Lowered<'hir>,
        span: Span,
    ) -> Result<Option<Lowered<'hir>>, HirError<'hir>> {
        let Some(comparison) = lang::comparison(operator) else {
            return Ok(None);
        };

        let receiver = left.typ.strip_reference();
        if let TypeKind::GenericParam(param) = receiver.kind() {
            let interface_name = comparison.interface.to_string();
            let interface = self.scope.symbols.get_id(&interface_name);
            let supported = self.generics.get(param as usize).is_some_and(|generic| {
                generic.bounds.iter().any(|bound| match bound.value_ref() {
                    statement::Type::Named(name) | statement::Type::Generic(name, _) => {
                        *name == interface_name
                    },
                    _ => false,
                })
            });

            if supported {
                let method = self.scope.symbols.insert(comparison.method);
                let lowered = self.alloc(
                    ExpressionKind::Binary { operator, left: left.expr, right: right.expr },
                    self.scope.types.common.bool,
                    span,
                );
                self.typeck.type_dependent_defs.insert(
                    lowered.expr.id,
                    Res::ParamMethod {
                        param,
                        interface: interface.expect("declared comparison interface is collected"),
                        name: method,
                    },
                );
                return Ok(Some(lowered));
            }
        }

        if !matches!(receiver.kind(), TypeKind::Adt(_, _)) {
            return Ok(None);
        }

        let method = self.scope.symbols.insert(comparison.method);
        let Some(function) = self.scope.method(receiver, method) else {
            let type_name = match receiver.kind() {
                TypeKind::Adt(id, _) => self.scope.symbols.get(self.scope[id].name).to_string(),
                _ => unreachable!("comparison receiver must be nominal"),
            };
            let type_name = self.arena.alloc_str(&type_name);
            return Err(hir_error!(
                span,
                OperatorRequiresInterface {
                    op: comparison.symbol,
                    type_name,
                    interface_name: comparison.interface,
                }
            ));
        };

        self.check_call_safety(function, span);
        let lowered = self.alloc(
            ExpressionKind::Binary { operator, left: left.expr, right: right.expr },
            self.scope.types.common.bool,
            span,
        );
        self.typeck.type_dependent_defs.insert(lowered.expr.id, Res::Function(function));

        Ok(Some(lowered))
    }

    pub(super) fn lower_index_overload(
        &mut self,
        base: Lowered<'hir>,
        index: &expression::Expression<'src>,
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let method =
            self.resolve_index_method(base.typ.strip_reference(), self.mutable_place, span)?;
        let index = self.lower_expr(index, Some(method.index))?;
        self.assert_type(method.index, index.typ, index.span)?;

        let return_type = self.scope.functions.defs[method.function].return_type;
        let name = self.scope.symbols.insert(match self.mutable_place {
            true => "index_mut",
            false => "index",
        });

        let args = self.arena.alloc_slice_copy(&[index.expr]);
        let call = self.alloc(
            ExpressionKind::MethodCall { name, receiver: base.expr, args },
            return_type,
            span,
        );
        self.typeck
            .type_dependent_defs
            .insert(call.expr.id, Res::Function(method.function));

        Ok(self.alloc(
            ExpressionKind::Unary { operator: UnaryOperator::Deref, expr: call.expr },
            method.output,
            span,
        ))
    }

    pub(super) fn make_place_mutable(
        &mut self,
        expr: &'hir Expression<'hir>,
    ) -> Result<(), HirError<'hir>> {
        match expr.kind {
            ExpressionKind::Field { base, .. } => self.make_place_mutable(base),
            ExpressionKind::Index { base, .. } => {
                if self.typeck.type_dependent_def(expr.id).and_then(Res::function).is_some() {
                    let base_type = self.typeck.type_of(base.id);
                    if matches!(
                        base_type.kind(),
                        TypeKind::Ref { mutable: false, .. } | TypeKind::Raw { mutable: false, .. }
                    ) {
                        return Err(hir_error!(expr.span, AssignBehindSharedRef));
                    }

                    let method =
                        self.resolve_index_method(base_type.strip_reference(), true, expr.span)?;
                    assert_eq!(
                        method.output,
                        self.typeck.type_of(expr.id),
                        "Index and IndexMutable must use the same Output type"
                    );

                    self.typeck.type_dependent_defs.insert(expr.id, Res::Function(method.function));
                }

                self.make_place_mutable(base)
            },
            ExpressionKind::Unary { operator: UnaryOperator::Deref, .. } => Ok(()),
            _ => Ok(()),
        }
    }

    fn resolve_index_method(
        &mut self,
        receiver: Type<'hir>,
        mutable: bool,
        span: Span,
    ) -> Result<IndexMethod<'hir>, HirError<'hir>> {
        let overload = lang::index_overload(mutable);
        let interface = self.scope.symbols.get_id(overload.interface);
        let name = self.scope.symbols.insert(overload.method);
        let implemented =
            interface.is_some_and(|interface| self.scope.implements_interface(receiver, interface));
        let function = implemented.then(|| self.scope.method(receiver, name)).flatten();

        let function = match (mutable, function) {
            (_, Some(function)) => function,
            (true, None) => {
                return Err(hir_error!(span, NotMutablyIndexable { typ: receiver }));
            },
            _ => return Err(hir_error!(span, NotIndexable { typ: receiver })),
        };

        let signature = &self.scope.functions.defs[function];
        let [index] = signature.explicit_params() else {
            panic!("index method must have exactly one explicit parameter")
        };
        let index = *index;
        let output = match signature.return_type.kind() {
            TypeKind::Ref { mutable: found, to } => {
                assert_eq!(found, mutable, "index method returned a reference of wrong mutability");
                to.into()
            },
            _ => panic!("index method must return a reference"),
        };

        self.check_call_safety(function, span);
        Ok(IndexMethod { function, index, output })
    }
}
