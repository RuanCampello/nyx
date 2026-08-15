use crate::{
    hir::{EnumRepr, FunctionKind, Layout, Owner, SymbolId, Type},
    lexer::token::Span,
    parser::statement::{self, StructRepr},
};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct AdtDef<'hir> {
    pub name: SymbolId,
    pub decl_span: Span,
    pub name_span: Span,
    pub kind: AdtKind<'hir>,
    pub layout: Layout,
    pub generics: Vec<GenericParamDef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericParamDef {
    pub name: SymbolId,
    pub bounds: Vec<SymbolId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AdtKind<'hir> {
    Struct { fields: Vec<FieldDef<'hir>>, repr: StructRepr },
    Enum { variants: Vec<VariantDef<'hir>>, repr: EnumRepr, payload_offset: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldDef<'hir> {
    pub name: SymbolId,
    pub typ: Type<'hir>,
    pub offset: u32,
    pub name_span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VariantDef<'hir> {
    pub name: SymbolId,
    pub value: i64,
    pub payload: Option<Type<'hir>>,
    pub name_span: Span,
}

#[derive(Debug, Clone)]
pub(in crate::hir) struct FnDef<'hir> {
    pub name: SymbolId,
    pub params: Vec<Type<'hir>>,
    pub return_type: Type<'hir>,
    pub kind: FunctionKind<'hir>,
    pub owner: Owner<'hir>,
    pub is_const: bool,
    pub is_unsafe: bool,
    pub has_receiver: bool,
    pub decl_span: Span,
    /// source body retained until the open generic hir body is lowered once
    /// ordinary executable bodies are lowered directly and leave this empty
    pub body: Option<statement::Function<'hir>>,
    /// generic names already fixed by an enclosing impl instantiation
    pub generic_env: HashMap<String, Type<'hir>>,
}

impl<'hir> FnDef<'hir> {
    #[inline]
    pub(in crate::hir) fn receiver_type(&self) -> Option<Type<'hir>> {
        self.has_receiver.then(|| self.params[0])
    }

    #[inline]
    pub(in crate::hir) fn receiver_mutable(&self) -> bool {
        use crate::hir::TypeKind;
        self.receiver_type().is_some_and(|typ| {
            matches!(
                typ.kind(),
                TypeKind::Ref { mutable: true, .. } | TypeKind::Slice { mutable: true, .. }
            )
        })
    }

    #[inline]
    pub(in crate::hir) fn explicit_params(&self) -> &[Type<'hir>] {
        &self.params[self.has_receiver as usize..]
    }
}

impl<'hir> AdtDef<'hir> {
    #[inline]
    pub const fn is_struct(&self) -> bool {
        matches!(self.kind, AdtKind::Struct { .. })
    }

    #[inline]
    pub const fn is_enum(&self) -> bool {
        matches!(self.kind, AdtKind::Enum { .. })
    }

    #[inline]
    pub fn fields(&self) -> &[FieldDef<'hir>] {
        match &self.kind {
            AdtKind::Struct { fields, .. } => fields,
            AdtKind::Enum { .. } => panic!("fields requested from enum definition"),
        }
    }

    #[inline]
    pub fn variants(&self) -> &[VariantDef<'hir>] {
        match &self.kind {
            AdtKind::Enum { variants, .. } => variants,
            AdtKind::Struct { .. } => panic!("variants requested from struct definition"),
        }
    }

    #[inline]
    pub fn variants_mut(&mut self) -> &mut Vec<VariantDef<'hir>> {
        match &mut self.kind {
            AdtKind::Enum { variants, .. } => variants,
            AdtKind::Struct { .. } => panic!("variants requested from struct definition"),
        }
    }

    #[inline]
    pub const fn enum_repr(&self) -> EnumRepr {
        match self.kind {
            AdtKind::Enum { repr, .. } => repr,
            AdtKind::Struct { .. } => {
                panic!("enum representation requested from struct definition")
            },
        }
    }

    #[inline]
    pub const fn payload_offset(&self) -> u32 {
        match self.kind {
            AdtKind::Enum { payload_offset, .. } => payload_offset,
            AdtKind::Struct { .. } => panic!("payload offset requested from struct definition"),
        }
    }
}
