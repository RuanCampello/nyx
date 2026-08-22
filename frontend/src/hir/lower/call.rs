use crate::{
    hir::{
        Expression, ExpressionKind, FunctionId, InterfaceMethodSignature, Intrinsic, Res, SymbolId,
        Syscall, TyInterner, Type, TypeKind, collect,
        error::{HirError, hir_error},
        lower::{FunctionBuilder, Lowered},
        type_resolver::{self, resolve_annotation},
    },
    lexer::{Spanned, token::Span},
    parser::{
        expression::{self, UnaryOperator},
        statement::{self, GenericBound},
    },
};
use std::str::FromStr;

#[derive(Clone, Copy)]
pub(in crate::hir::lower) enum GenericCall<'hir> {
    Free,
    Method { name: SymbolId, receiver: &'hir Expression<'hir> },
}

impl<'s, 'f, 'hir, 'src> FunctionBuilder<'s, 'f, 'hir, 'src>
where
    'src: 'hir,
{
    pub(super) fn resolve_bound_method(
        &self,
        param: u8,
        method: SymbolId,
    ) -> Option<(SymbolId, InterfaceMethodSignature<'hir>)> {
        let generic = self.generics.get(param as usize)?;
        self.find_bound_signature(generic, |candidate| candidate.name == method)
    }

    pub(super) fn resolve_bound_function(
        &self,
        qualifier: &str,
        name: SymbolId,
    ) -> Option<(u8, SymbolId, InterfaceMethodSignature<'hir>)> {
        let concrete = *self.generic_env.get(qualifier)?;
        let TypeKind::GenericParam(param) = concrete.kind() else {
            return None;
        };
        let generic = self.resolve_generic(qualifier)?;
        let (interface, signature) = self.find_bound_signature(generic, |candidate| {
            !candidate.has_receiver && candidate.name == name
        })?;
        Some((param, interface, signature))
    }

    pub(super) fn resolve_generic(&self, qualifier: &str) -> Option<&GenericBound<'src>> {
        self.generics.iter().find(|generic| generic.name == qualifier)
    }

    /// find the first of `generic`'s bounds whose interface has an item `project` selects
    pub(super) fn find_bound_item<T>(
        &self,
        generic: &GenericBound<'src>,
        project: impl Fn(&collect::InterfaceSignature<'hir>) -> Option<T>,
    ) -> Option<(SymbolId, T)> {
        generic.bounds.iter().find_map(|bound| {
            let interface = self.bound_interface(bound)?;
            let item = project(self.scope.interfaces.defs.get(&interface)?)?;
            Some((interface, item))
        })
    }

    fn find_bound_signature(
        &self,
        generic: &GenericBound<'src>,
        predicate: impl Fn(&InterfaceMethodSignature<'hir>) -> bool,
    ) -> Option<(SymbolId, InterfaceMethodSignature<'hir>)> {
        self.find_bound_item(generic, |interface| {
            interface.methods.iter().find(|candidate| predicate(candidate)).cloned()
        })
    }

    pub(super) fn resolve_concrete_bound_function(
        &self,
        qualifier: &str,
        name: SymbolId,
    ) -> Option<FunctionId> {
        let concrete = *self.generic_env.get(qualifier)?;
        if matches!(concrete.kind(), TypeKind::GenericParam(_)) {
            return None;
        }
        let generic = self.resolve_generic(qualifier)?;

        generic.bounds.iter().find_map(|bound| {
            let interface = self.bound_interface(bound)?;
            self.scope.free_impl_function(concrete, interface, name)
        })
    }

    pub(super) fn lower_param_method_call(
        &mut self,
        param: u8,
        interface: SymbolId,
        method: SymbolId,
        signature: InterfaceMethodSignature<'hir>,
        receiver_ast: &expression::Expression<'src>,
        receiver: Lowered<'hir>,
        args: &[expression::Expression<'src>],
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let receiver = match signature.receiver_mut
            && !matches!(
                receiver.typ.kind(),
                TypeKind::Ref { mutable: true, .. } | TypeKind::Slice { mutable: true, .. }
            ) {
            true => self.lower_mutable_place(receiver_ast, None)?,
            _ => receiver,
        };
        let name = self.arena.alloc_str(self.scope.symbols.get(method));

        self.check_arity(
            name,
            signature.params.len(),
            args.len(),
            collect::source_span(signature.decl_span),
            span,
        )?;

        let mut lowered_args = Vec::with_capacity(args.len());
        let mut actual_types = Vec::with_capacity(args.len());
        for (arg, &expected) in args.iter().zip(&signature.params) {
            let lowered = self.lower_expr(arg, Some(expected))?;
            actual_types.push(lowered.typ);
            lowered_args.push(lowered.expr);
        }
        let arity = generic_arity(&signature.params, signature.return_type);
        let substs = infer_type_args(&signature.params, &actual_types, arity);
        let return_type =
            signature.return_type.subst(&self.scope.types, &self.scope.arrays, &substs);

        let lowered = self.alloc(
            ExpressionKind::MethodCall {
                name: method,
                receiver: receiver.expr,
                args: self.arena.alloc_slice_copy(&lowered_args),
            },
            return_type,
            span,
        );

        self.typeck
            .type_dependent_defs
            .insert(lowered.expr.id, Res::ParamMethod { param, interface, name: method });
        if !substs.is_empty() {
            self.typeck.node_args.insert(lowered.expr.id, substs);
        }

        Ok(lowered)
    }

    pub(super) fn lower_param_function_call(
        &mut self,
        param: u8,
        interface: SymbolId,
        name: SymbolId,
        signature: InterfaceMethodSignature<'hir>,
        args: &[expression::Expression<'src>],
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let display = self.arena.alloc_str(self.scope.symbols.get(name));
        self.check_arity(
            display,
            signature.params.len(),
            args.len(),
            collect::source_span(signature.decl_span),
            span,
        )?;

        let self_type = self.scope.types.generic_param(param);
        let params: Vec<_> = signature
            .params
            .iter()
            .map(|&typ| substitute_self_type(&self.scope.types, typ, self_type))
            .collect();

        let signature_return =
            substitute_self_type(&self.scope.types, signature.return_type, self_type);

        let mut lowered_args = Vec::with_capacity(args.len());
        for (arg, &expected) in args.iter().zip(&params) {
            let lowered = self.lower_expr(arg, Some(expected))?;
            lowered_args.push(lowered.expr);
        }

        let callee =
            self.alloc(ExpressionKind::Path(name), self.scope.types.common.unit, span).expr;
        let lowered = self.alloc(
            ExpressionKind::Call { callee, args: self.arena.alloc_slice_copy(&lowered_args) },
            signature_return,
            span,
        );

        self.typeck
            .type_dependent_defs
            .insert(lowered.expr.id, Res::ParamFunction { param, interface, name });

        Ok(lowered)
    }

    pub(super) fn lower_wrapping_call(
        &mut self,
        intrinsic: Intrinsic,
        function_id: FunctionId,
        receiver: Lowered<'hir>,
        args: &[expression::Expression<'src>],
        method_name: &'hir str,
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        self.check_arity(method_name, 1, args.len(), None, span)?;
        let arg = &args[0];

        let signature = &self.scope.functions.defs[function_id];
        let (rhs_param, return_type) = (signature.params[1], signature.return_type);

        let receiver_expr = match receiver.typ.kind() {
            TypeKind::Ref { to, .. } => {
                self.alloc(
                    ExpressionKind::Unary { operator: UnaryOperator::Deref, expr: receiver.expr },
                    to.into(),
                    receiver.span,
                )
                .expr
            },
            _ => receiver.expr,
        };

        let arg = self.lower_expr(arg, Some(rhs_param))?;
        self.assert_type(rhs_param, arg.typ, arg.span)?;

        let args = self.arena.alloc_slice_copy(&[receiver_expr, arg.expr]);
        let callee = self
            .alloc(
                ExpressionKind::Path(self.scope.functions.defs[function_id].name),
                self.scope.types.common.unit,
                span,
            )
            .expr;

        let lowered = self.alloc(ExpressionKind::Call { callee, args }, return_type, span);
        self.typeck
            .type_dependent_defs
            .insert(lowered.expr.id, Res::Intrinsic(intrinsic));

        Ok(lowered)
    }

    pub(super) fn lower_direct_call(
        &mut self,
        function_id: FunctionId,
        args: &[expression::Expression<'src>],
        type_args: &[Spanned<statement::Type<'src>>],
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let signature = self.scope.functions.defs[function_id].clone();
        let intrinsic = signature.kind.intrinsic();

        if intrinsic == Some(Intrinsic::Syscall) {
            return self.lower_syscall(args, signature.name, signature.return_type, span);
        }

        if intrinsic.is_none() {
            let name = self.arena.alloc_str(self.scope.symbols.get(signature.name));
            self.check_arity(
                name,
                signature.params.len(),
                args.len(),
                collect::source_span(signature.decl_span),
                span,
            )?;
        }

        let mut lowered_args = Vec::with_capacity(args.len());
        match intrinsic {
            // an interpolated literal flattens into the argument list here, so
            // everything downstream sees an ordinary variadic print
            Some(Intrinsic::Print | Intrinsic::PrintLn) => {
                lowered_args = self.lower_print_args(args)?;
            },
            Some(_) => {
                for arg in args {
                    let arg = self.lower_expr(arg, None)?;
                    lowered_args.push(arg.expr);
                }
            },
            _ => {
                for (expr, &param_type) in args.iter().zip(signature.params.iter()) {
                    let expr = self.lower_expr(expr, Some(param_type))?;
                    self.assert_type(param_type, expr.typ, expr.span)?;
                    lowered_args.push(expr.expr);
                }
            },
        }

        let lowered_args = self.arena.alloc_slice_copy(&lowered_args);
        let (callee_name, return_type) = (signature.name, signature.return_type);

        let callee = self
            .alloc(ExpressionKind::Path(callee_name), self.scope.types.common.unit, span)
            .expr;
        let substs = self.resolve_turbofish(type_args)?;
        let kind = ExpressionKind::Call { callee, args: lowered_args };
        let res = intrinsic.map_or(Res::Function(function_id), Res::Intrinsic);

        Ok(self.finish_call(function_id, kind, return_type, res, substs, span))
    }

    pub(in crate::hir::lower) fn resolve_turbofish(
        &self,
        type_args: &[Spanned<statement::Type<'src>>],
    ) -> Result<Vec<Type<'hir>>, HirError<'hir>> {
        if type_args.is_empty() {
            return Ok(Vec::new());
        }

        let mut ctx = type_resolver::ResolveCtx::root(
            &self.scope.symbols,
            &self.scope.adts.struct_map,
            &self.scope.adts.enum_map,
            &self.scope.adts.defs,
            &self.scope.arrays,
            &self.scope.types,
        );
        if let Some(typ) = self.impl_ctx.self_type {
            ctx = ctx.with_self(typ);
        }

        type_args
            .iter()
            .map(|t| resolve_annotation(&ctx, &t.value(), t.span()))
            .collect()
    }

    pub(in crate::hir::lower) fn lower_generic_call(
        &mut self,
        function_id: FunctionId,
        syntax: GenericCall<'hir>,
        args: &[expression::Expression<'src>],
        type_args: &[Spanned<statement::Type<'src>>],
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        let signature = self.scope.functions.defs[function_id].clone();
        let fixed = &self.scope.functions.defs[function_id].generic_env;
        let bounds: Vec<_> = self.scope.functions.defs[function_id]
            .body
            .as_ref()
            .expect("generic call target retains its source body")
            .generics
            .iter()
            .enumerate()
            .filter_map(|(at, generic)| match fixed.get(generic.name).map(|typ| typ.kind()) {
                Some(TypeKind::GenericParam(slot)) => Some((slot as usize, generic.clone())),
                None => Some((at, generic.clone())),
                _ => None,
            })
            .collect();

        let open_params = match syntax {
            GenericCall::Free => signature.params.as_slice(),
            GenericCall::Method { .. } => signature.explicit_params(),
        };

        let generic_count = generic_arity(&signature.params, signature.return_type)
            .max(bounds.iter().map(|&(slot, _)| slot + 1).max().unwrap_or(0));

        let name = self.arena.alloc_str(self.scope.symbols.get(signature.name));
        self.check_arity(
            name,
            open_params.len(),
            args.len(),
            collect::source_span(signature.decl_span),
            span,
        )?;

        let mut lowered_args = Vec::with_capacity(args.len());
        let mut arg_types = Vec::with_capacity(args.len());
        for arg in args {
            let lowered = self.lower_expr(arg, None)?;
            arg_types.push(lowered.typ);
            lowered_args.push(lowered.expr);
        }
        let lowered_args = self.arena.alloc_slice_copy(&lowered_args);

        let mut substs = match syntax {
            GenericCall::Free => infer_type_args(open_params, &arg_types, generic_count),
            GenericCall::Method { receiver, .. } => {
                let mut actual = Vec::with_capacity(arg_types.len() + 1);
                actual.push(self.typeck.type_of(receiver.id));
                actual.extend(arg_types.iter().copied());
                infer_type_args(&signature.params, &actual, generic_count)
            },
        };

        for (&(slot, _), typ) in bounds.iter().zip(self.resolve_turbofish(type_args)?) {
            if let Some(existing) = substs.get_mut(slot) {
                *existing = typ;
            }
        }
        self.check_bounds(&bounds, &substs, span)?;

        let return_type =
            signature.return_type.subst(&self.scope.types, &self.scope.arrays, &substs);
        let kind = match syntax {
            GenericCall::Free => {
                let callee = self
                    .alloc(ExpressionKind::Path(signature.name), self.scope.types.common.unit, span)
                    .expr;
                ExpressionKind::Call { callee, args: lowered_args }
            },
            GenericCall::Method { name, receiver } => {
                ExpressionKind::MethodCall { name, receiver, args: lowered_args }
            },
        };

        Ok(self.finish_call(
            function_id,
            kind,
            return_type,
            Res::Function(function_id),
            substs,
            span,
        ))
    }

    pub(super) fn finish_call(
        &mut self,
        function_id: FunctionId,
        kind: ExpressionKind<'hir>,
        return_type: Type<'hir>,
        res: Res,
        substs: Vec<Type<'hir>>,
        span: Span,
    ) -> Lowered<'hir> {
        let lowered = self.alloc(kind, return_type, span);

        self.check_call_safety(function_id, span);
        self.typeck.type_dependent_defs.insert(lowered.expr.id, res);
        if !substs.is_empty() {
            self.typeck.node_args.insert(lowered.expr.id, substs);
        }

        lowered
    }

    fn lower_syscall(
        &mut self,
        args: &[expression::Expression<'src>],
        callee_name: SymbolId,
        return_type: Type<'hir>,
        span: Span,
    ) -> Result<Lowered<'hir>, HirError<'hir>> {
        if !self.scope.in_std.get() {
            return Err(hir_error!(span, UnknownFunction { name: "syscall" }));
        }

        let Some((code_arg, value_args)) = args.split_first() else {
            return Err(hir_error!(
                span,
                ArityMismatch { name: "syscall", expected: 1, found: 0, decl: None }
            ));
        };

        if value_args.len() > 6 {
            return Err(hir_error!(
                span,
                ArityMismatch { name: "syscall", expected: 7, found: args.len(), decl: None }
            ));
        }

        let expression::Expression::Identifier(name, code_span) = code_arg else {
            let name = self.arena.alloc_str(&format!("{code_arg:?}"));
            return Err(hir_error!(code_arg.span(), UndeclaredIdentifier { name }));
        };

        let code = Syscall::from_str(name)
            .map_err(|_| hir_error!(*code_span, UndeclaredIdentifier { name }))?;

        let args = value_args
            .iter()
            .map(|arg| self.lower_expr(arg, None).map(|lowered| lowered.expr))
            .collect::<Result<Vec<_>, _>>()?;

        let return_type = match code {
            Syscall::Exit => self.scope.types.common.never,
            _ => return_type,
        };

        let args = self.arena.alloc_slice_copy(&args);
        let callee = self
            .alloc(ExpressionKind::Path(callee_name), self.scope.types.common.unit, span)
            .expr;
        let lowered = self.alloc(ExpressionKind::Call { callee, args }, return_type, span);
        self.typeck.type_dependent_defs.insert(lowered.expr.id, Res::Syscall(code));
        Ok(lowered)
    }

    fn check_bounds(
        &self,
        generics: &[(usize, GenericBound<'src>)],
        args: &[Type<'hir>],
        span: Span,
    ) -> Result<(), HirError<'hir>> {
        let current_generics = self.function.map(|f| f.generics.as_slice()).unwrap_or(&[]);
        for &(slot, ref param) in generics {
            let Some(&concrete_type) = args.get(slot) else {
                continue;
            };
            for bound in &param.bounds {
                let interface_name = match bound.value_ref() {
                    statement::Type::Named(name) => name,
                    statement::Type::Generic(name, _) => name,
                    _ => continue,
                };

                let satisfied = match concrete_type.kind() {
                    TypeKind::GenericParam(idx) => current_generics
                        .get(idx as usize)
                        .into_iter()
                        .chain(self.generics.iter().filter(|generic| {
                            self.generic_env.get(generic.name).copied() == Some(concrete_type)
                        }))
                        .any(|param_bound| {
                            param_bound.bounds.iter().any(|bound| {
                                let name = match bound.value_ref() {
                                    statement::Type::Named(name)
                                    | statement::Type::Generic(name, _) => name,
                                    _ => "",
                                };
                                name == *interface_name
                            })
                        }),
                    _ => self.scope.symbols.get_id(interface_name).is_some_and(|interface_sym| {
                        self.scope.implements_interface(concrete_type, interface_sym)
                    }),
                };

                if !satisfied {
                    return Err(hir_error!(
                        span,
                        UnsatisfiedBound { type_name: concrete_type, bound_name: interface_name }
                    ));
                }
            }
        }
        Ok(())
    }
}

fn infer_type_args<'hir>(
    open_params: &[Type<'hir>],
    arg_types: &[Type<'hir>],
    count: usize,
) -> Vec<Type<'hir>> {
    let mut bindings = vec![None; count];
    for (&param, &actual) in open_params.iter().zip(arg_types) {
        unify_generic(param, actual, &mut bindings);
    }
    bindings.into_iter().map(Option::unwrap_or_default).collect()
}

fn unify_generic<'hir>(param: Type<'hir>, actual: Type<'hir>, bindings: &mut [Option<Type<'hir>>]) {
    use TypeKind::*;
    match (param.kind(), actual.kind()) {
        (GenericParam(index), _) => {
            if let Some(slot) = bindings.get_mut(index as usize) {
                slot.get_or_insert(actual);
            }
        },
        (Ref { to: param, .. }, Ref { to: actual, .. })
        | (Raw { to: param, .. }, Raw { to: actual, .. })
        | (Raw { to: param, .. }, Ref { to: actual, .. }) => {
            unify_generic(param.into(), actual.into(), bindings);
        },
        (Ref { to: param, .. }, _) => unify_generic(param.into(), actual, bindings),
        (Slice { element: param, .. }, Slice { element: actual, .. }) => {
            unify_generic(param.into(), actual.into(), bindings)
        },
        (Adt(param_id, param_args), Adt(actual_id, actual_args)) if param_id == actual_id => {
            for (&param, &actual) in param_args.iter().zip(actual_args) {
                unify_generic(param, actual, bindings);
            }
        },
        _ => {},
    }
}

fn max_generic_index(typ: Type<'_>) -> Option<u8> {
    match typ.kind() {
        TypeKind::GenericParam(index) => Some(index),
        TypeKind::Ref { to, .. } | TypeKind::Raw { to, .. } => max_generic_index(to.into()),
        TypeKind::Slice { element, .. } => max_generic_index(element.into()),
        TypeKind::Adt(_, args) => args.iter().copied().filter_map(max_generic_index).max(),
        _ => None,
    }
}

fn generic_arity<'hir>(params: &[Type<'hir>], return_type: Type<'hir>) -> usize {
    params
        .iter()
        .copied()
        .chain(std::iter::once(return_type))
        .filter_map(max_generic_index)
        .max()
        .map_or(0, |index| index as usize + 1)
}

fn substitute_self_type<'hir>(
    types: &TyInterner<'hir>,
    typ: Type<'hir>,
    self_type: Type<'hir>,
) -> Type<'hir> {
    use TypeKind::*;
    match typ.kind() {
        SelfType => self_type,
        Ref { mutable, to } => types.refer(substitute_self_type(types, to, self_type), mutable),
        Raw { mutable, to } => types.raw(substitute_self_type(types, to, self_type), mutable),
        Slice { mutable, element } => {
            types.slice(substitute_self_type(types, element, self_type), mutable)
        },
        Adt(id, args) => {
            let args = args
                .iter()
                .map(|&arg| substitute_self_type(types, arg, self_type))
                .collect::<Vec<_>>();
            types.adt(id, &args)
        },
        _ => typ,
    }
}
