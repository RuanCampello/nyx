use crate::analysis::{
    HoverTarget, Snapshot,
    hover::{nominal_name_span, split_path, through_reference},
};
use frontend::hir::{
    self, Block, Constant, ExpressionKind, FunctionId, LocalId, Res, Statement, SymbolId, TypeKind,
    ids::IndexVec, visit::Visitor,
};
use frontend::{lexer::token::Span, source_map::SourceMap};
use std::collections::HashMap;

pub(super) struct Walker<'a, 'h> {
    pub(super) typeck: &'a hir::TypeckResults<'h>,
    pub(super) locals: &'a IndexVec<LocalId, hir::Local<'h>>,
    pub(super) index: &'a Snapshot<'h>,
    /// resolves a path span back to its text, the only place segment boundaries
    /// survive: the hir keeps one span for the whole path
    pub(super) map: &'a SourceMap,
    /// position of the function being walked within the snapshot's function list
    pub(super) function: u32,
    /// frozen hir function table, [FunctionId] is its dense index
    pub(super) functions: &'a IndexVec<FunctionId, hir::Function<'h>>,
    /// resolves a spliced constant use back to its declaration
    pub(super) constants: &'a HashMap<SymbolId, (u32, &'a Constant<'h>)>,
    pub(super) hover: &'a mut Vec<(Span, HoverTarget<'h>)>,
    pub(super) defs: &'a mut HashMap<Span, Span>,
    pub(super) hints: &'a mut Vec<(Span, hir::Type<'h>, u32)>,
    /// how each name in this body was declared, so a use hovers as its
    /// declaration rather than as a bare type
    pub(super) forms: HashMap<LocalId, Binding>,
}

/// how a name entered scope, which decides how its hover reads back
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// a `let`, shown with its keyword and mutability
    Let,
    /// destructured by a pattern, which has no keyword of its own
    Pattern,
}

impl<'a, 'h> Walker<'a, 'h> {
    fn type_of(&self, id: hir::ExprId) -> hir::Type<'h> {
        self.typeck.type_of(id)
    }

    fn qualified(
        &mut self,
        path: Span,
        callee: HoverTarget<'h>,
        name_span: Span,
        owner: hir::Type<'h>,
    ) {
        let Some((qualifier, name)) = split_path(self.map, path) else {
            return;
        };

        self.hover.push((name, callee));
        if name_span != Span::default() {
            self.defs.insert(name, name_span);
        }

        self.hover
            .push((qualifier, HoverTarget::Nominal { typ: owner, function: None }));
        match nominal_name_span(owner, self.index) {
            Some(target) if target != Span::default() => {
                self.defs.insert(qualifier, target);
            },
            _ => {},
        }
    }

    /// [`hir::Type`] — avoids fabricating a fresh interned type just to name it
    fn qualified_adt(
        &mut self,
        path: Span,
        callee: HoverTarget<'h>,
        name_span: Span,
        adt: hir::AdtId,
    ) {
        let Some((qualifier, name)) = split_path(self.map, path) else {
            return;
        };

        self.hover.push((name, callee));
        if name_span != Span::default() {
            self.defs.insert(name, name_span);
        }

        self.hover.push((qualifier, HoverTarget::Enum(adt)));
        if let Some(def) = self.index.adts.get(adt)
            && def.name_span != Span::default()
        {
            self.defs.insert(qualifier, def.name_span);
        }
    }

    fn binding(&mut self, id: LocalId, form: Binding) {
        let span = self.locals[id].decl_span;

        self.hints.push((span, self.locals[id].typ, self.function));
        self.hover
            .push((span, HoverTarget::Local { function: self.function, local: id, form }));
        self.forms.insert(id, form);
    }
}

impl<'a, 'h> Visitor<'h> for Walker<'a, 'h> {
    fn visit_block(&mut self, block: &'h Block<'h>) {
        for stmt in block.statements {
            self.visit_statement(stmt);
        }
    }

    fn visit_pattern(&mut self, pattern: &'h hir::Pattern<'h>) {
        use hir::PatternKind as Patt;

        match &pattern.kind {
            Patt::Binding(id) => self.binding(*id, Binding::Pattern),
            Patt::Bind { local, sub } => {
                self.binding(*local, Binding::Pattern);
                self.visit_pattern(sub);
            },
            Patt::Variant { sub: Some(sub), .. } => self.visit_pattern(sub),
            Patt::Struct { fields, .. } => {
                for (_, sub) in *fields {
                    self.visit_pattern(sub);
                }
            },
            Patt::Or(alternatives) => {
                for alternative in *alternatives {
                    self.visit_pattern(alternative);
                }
            },
            Patt::Wildcard
            | Patt::Variant { sub: None, .. }
            | Patt::Literal(_)
            | Patt::Range { .. } => {},
        }
    }

    fn visit_statement(&mut self, stmt: &'h Statement<'h>) {
        use Statement::*;
        match stmt {
            LetInit { id, init } => {
                self.binding(*id, Binding::Let);
                self.visit_expression(init);
            },
            LetUninit { id } => self.binding(*id, Binding::Let),
            Expr(e) | Return(Some(e)) => self.visit_expression(e),
            If { condition, then_block, else_block } => {
                self.visit_expression(condition);
                self.visit_block(then_block);
                if let Some(eb) = else_block {
                    self.visit_block(eb);
                }
            },
            Loop { kind, body } => {
                match kind {
                    hir::LoopKind::Infinite => {},
                    hir::LoopKind::Range { start, end, .. } => {
                        self.visit_expression(start);
                        self.visit_expression(end);
                    },
                    hir::LoopKind::Iterable { iterable, .. } => self.visit_expression(iterable),
                }
                self.visit_block(body);
            },
            Block(b) => self.visit_block(b),
            Return(None) | Break | Continue => {},
        }
    }

    fn visit_expression(&mut self, expr: &'h hir::Expression<'h>) {
        use ExpressionKind::*;

        if let Some(symbol) = self.typeck.const_use(expr.id)
            && let Some(&(position, constant)) = self.constants.get(&symbol)
        {
            self.hover.push((expr.span, HoverTarget::Constant(position)));
            if constant.name_span != Span::default() {
                self.defs.insert(expr.span, constant.name_span);
            }
            return;
        }

        // a variant constructor lowers to a call, so both share the resolution
        // recorded against the call expression :D
        let variant = match &expr.kind {
            Call { .. } => match self.typeck.type_dependent_def(expr.id) {
                Some(Res::Variant { id, index }) => self.index.adts[id]
                    .variants()
                    .get(index)
                    .map(|variant| (id, index as u32, variant.name_span)),
                _ => None,
            },
            _ => None,
        };

        if let Some((enumeration, index, name_span)) = variant {
            if name_span != Span::default() {
                self.defs.insert(expr.span, name_span);
            }
            if let Call { callee, .. } = &expr.kind {
                let target = HoverTarget::Variant { enumeration, variant: index };
                self.qualified_adt(callee.span, target, name_span, enumeration);
            }
        }

        let resolved = match &expr.kind {
            Call { .. } | MethodCall { .. } => self
                .typeck
                .type_dependent_def(expr.id)
                .and_then(Res::function)
                .and_then(|id| self.functions.get(id).map(|function| (id.0, function))),
            _ => None,
        };

        // a field access names the field's declaration, not just its type
        let field = match &expr.kind {
            Field { base, field } => match through_reference(self.type_of(base.id)).kind() {
                TypeKind::Adt(id, _) => self
                    .index
                    .adts
                    .get(id)
                    .and_then(|def| def.fields().iter().position(|f| f.name == *field))
                    .map(|at| (id, at as u32)),
                _ => None,
            },
            _ => None,
        };

        if let Some((structure, at)) = field {
            let name_span = self.index.adts[structure].fields()[at as usize].name_span;
            if name_span != Span::default() {
                self.defs.insert(expr.span, name_span);
            }
        }

        let function = self.function;
        let hover = match (variant, resolved, &expr.kind) {
            (Some((enumeration, variant, _)), ..) => HoverTarget::Variant { enumeration, variant },
            _ if field.is_some() => {
                let (structure, field) = field.expect("just checked");
                HoverTarget::Field { structure, field }
            },
            (_, Some((position, _)), _) => HoverTarget::Function(position),
            (_, _, Struct { .. } | Path(_) | Literal(_)) => {
                HoverTarget::Nominal { typ: self.type_of(expr.id), function: Some(function) }
            },
            (_, _, Local(id)) => match self.forms.get(id) {
                Some(&form) => HoverTarget::Local { function, local: *id, form },
                _ => HoverTarget::Type { typ: self.type_of(expr.id), function },
            },
            _ => HoverTarget::Type { typ: self.type_of(expr.id), function },
        };

        self.hover.push((expr.span, hover));

        match &expr.kind {
            Local(id) => {
                self.defs.insert(expr.span, self.locals[*id].decl_span);
            },
            // a leaf here: the value tree belongs to the definition site and
            // lives in the constant's own ExprId space
            Const(constant) => {
                self.defs.insert(expr.span, constant.name_span);
            },
            ParamConst { .. } => {},
            // likewise a leaf: a static's initialiser is folded at its declaration
            Static(id) => {
                if let Some(item) = self.index.statics.get(*id) {
                    self.defs.insert(expr.span, item.name_span);
                }
            },
            Call { callee, args } => {
                if let Some((position, target)) = resolved {
                    self.defs.insert(callee.span, target.name_span);
                    if let hir::Owner::Inherent(typ) | hir::Owner::Interface { on: typ, .. } =
                        target.owner
                    {
                        let hover = HoverTarget::Function(position);
                        self.qualified(callee.span, hover, target.name_span, typ.into());
                    }
                }
                for arg in *args {
                    self.visit_expression(arg);
                }
            },
            MethodCall { receiver, args, .. } => {
                if let Some((_, target)) = resolved {
                    self.defs.insert(expr.span, target.name_span);
                }
                self.visit_expression(receiver);
                for arg in *args {
                    self.visit_expression(arg);
                }
            },
            Unary { expr: sub, .. } => self.visit_expression(sub),
            Binary { left, right, .. } => {
                self.visit_expression(left);
                self.visit_expression(right);
            },
            Field { base, .. } => self.visit_expression(base),
            Assign { target, value } => {
                self.visit_expression(target);
                self.visit_expression(value);
            },
            Struct { fields, .. } => {
                for (_, fexpr) in *fields {
                    self.visit_expression(fexpr);
                }
            },
            Cast { from, .. } => self.visit_expression(from),
            Array { elements } => {
                for element in *elements {
                    self.visit_expression(element);
                }
            },
            ArrayRepeat { value, .. } => self.visit_expression(value),
            Index { base, index } => {
                self.visit_expression(base);
                self.visit_expression(index);
            },
            Match { scrutinee, arms } => {
                self.visit_expression(scrutinee);
                for arm in *arms {
                    self.visit_pattern(arm.pattern);
                    self.visit_expression(arm.body);
                    if let Some(guard) = arm.guard {
                        self.visit_expression(guard);
                    }
                }
            },
            Literal(_) | Path(_) | TypeIntrinsic { .. } => {},
        }
    }
}
