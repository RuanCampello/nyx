use crate::diagnostic;
use crate::hir::collect::ItemTable;
use crate::hir::diagnostics::ErrorGuaranteed;
use crate::hir::ids::{AdtId, ArrayId};
use crate::parser::statement;
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{Hash, Hasher},
    rc::Rc,
};

mod subst;

/// A canonical, arena-owned type handle
///
/// Equality and hashing are pointer operations
/// [TyInterner] is therefore the only place that may allocate a non-primitive type
#[derive(Clone, Copy)]
pub struct Type<'hir>(&'hir TypeKind<'hir>);

#[derive(Debug)]
pub struct TyInterner<'hir> {
    arena: &'hir bumpalo::Bump,
    cache: Rc<RefCell<HashMap<TypeKind<'hir>, Type<'hir>>>>,
    pub common: CommonTypes<'hir>,
}

#[derive(Debug, Clone, Copy)]
pub struct CommonTypes<'hir> {
    pub unit: Type<'hir>,
    pub i8: Type<'hir>,
    pub u8: Type<'hir>,
    pub i16: Type<'hir>,
    pub u16: Type<'hir>,
    pub i32: Type<'hir>,
    pub u32: Type<'hir>,
    pub i64: Type<'hir>,
    pub u64: Type<'hir>,
    pub f32: Type<'hir>,
    pub f64: Type<'hir>,
    pub bool: Type<'hir>,
    pub uptr: Type<'hir>,
    pub iptr: Type<'hir>,
    pub char: Type<'hir>,
    pub str: Type<'hir>,
    pub string: Type<'hir>,
    pub self_type: Type<'hir>,
    pub never: Type<'hir>,
    pub error: Type<'hir>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[rustfmt::skip]
pub enum TypeKind<'hir> {
    #[default]
    Unit,
    I8, U8,
    I16, U16,
    I32, U32,
    I64, U64,
    F32, F64,
    Bool,
    Uptr, Iptr,
    Char,
    Str, String,
    Adt(AdtId, &'hir [Type<'hir>]),
    Array(ArrayId),
    Slice { mutable: bool, element: Type<'hir> },
    SelfType,
    Ref { mutable: bool, to: Type<'hir> },
    Raw { mutable: bool, to: Type<'hir> },
    GenericParam(u8),
    Never,
    Infer(u32),
    Error,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[rustfmt::skip]
pub enum EnumRepr {
    I8, U8, I16, U16,
    #[default]
    I32,
    U32, I64, U64, Iptr, Uptr,
}

impl<'hir> TyInterner<'hir> {
    pub fn new(arena: &'hir bumpalo::Bump) -> Self {
        Self {
            arena,
            cache: Rc::new(RefCell::new(HashMap::new())),
            common: CommonTypes::new(),
        }
    }

    fn intern(&self, kind: TypeKind<'hir>) -> Type<'hir> {
        if let Some(typ) = self.primitive(kind) {
            return typ;
        }
        if let Some(typ) = self.cache.borrow().get(&kind).copied() {
            return typ;
        }

        let typ = Type(self.arena.alloc(kind));
        self.cache.borrow_mut().insert(kind, typ);

        typ
    }

    #[inline]
    pub fn adt(&self, id: AdtId, args: &[Type<'hir>]) -> Type<'hir> {
        let args = self.arena.alloc_slice_copy(args);
        self.intern(TypeKind::Adt(id, args))
    }

    #[inline]
    pub fn array(&self, id: ArrayId) -> Type<'hir> {
        self.intern(TypeKind::Array(id))
    }

    #[inline]
    pub fn slice(&self, element: Type<'hir>, mutable: bool) -> Type<'hir> {
        self.intern(TypeKind::Slice { mutable, element })
    }

    #[inline]
    pub fn generic_param(&self, index: u8) -> Type<'hir> {
        self.intern(TypeKind::GenericParam(index))
    }

    #[inline]
    pub(crate) fn infer(&self, variable: u32) -> Type<'hir> {
        self.intern(TypeKind::Infer(variable))
    }

    #[inline]
    pub fn refer(&self, to: Type<'hir>, mutable: bool) -> Type<'hir> {
        self.intern(TypeKind::Ref { mutable, to })
    }

    #[inline]
    pub fn raw(&self, to: Type<'hir>, mutable: bool) -> Type<'hir> {
        self.intern(TypeKind::Raw { mutable, to })
    }

    pub fn from_primitive_ast(&self, typ: &statement::Type<'_>) -> Option<Type<'hir>> {
        use statement::Type as AstType;
        Some(match typ {
            AstType::I8 => self.common.i8,
            AstType::U8 => self.common.u8,
            AstType::I16 => self.common.i16,
            AstType::U16 => self.common.u16,
            AstType::I32 => self.common.i32,
            AstType::U32 => self.common.u32,
            AstType::I64 => self.common.i64,
            AstType::U64 => self.common.u64,
            AstType::F32 => self.common.f32,
            AstType::F64 => self.common.f64,
            AstType::Bool => self.common.bool,
            AstType::Uptr => self.common.uptr,
            AstType::Iptr => self.common.iptr,
            AstType::Char => self.common.char,
            AstType::Str => self.common.str,
            AstType::String => self.common.string,
            AstType::SelfType => self.common.self_type,
            AstType::RefSelf => self.refer(self.common.self_type, false),
            AstType::Never => self.common.never,
            AstType::Unit => self.common.unit,
            AstType::Named(_)
            | AstType::Ref(_, _)
            | AstType::Raw(_, _)
            | AstType::Array(_, _)
            | AstType::Slice(_, _)
            | AstType::Generic(_, _)
            | AstType::Associated(_, _) => return None,
        })
    }
}

impl Clone for TyInterner<'_> {
    fn clone(&self) -> Self {
        Self {
            arena: self.arena,
            cache: Rc::clone(&self.cache),
            common: self.common,
        }
    }
}

impl PartialEq for TyInterner<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.arena, other.arena)
    }
}

impl<'hir> Type<'hir> {
    #[inline]
    pub const fn kind(self) -> TypeKind<'hir> {
        *self.0
    }

    #[inline]
    pub(crate) const fn error(_guaranteed: ErrorGuaranteed) -> Self {
        Type(&ERROR)
    }

    #[inline]
    pub(crate) const fn is_error(self) -> bool {
        matches!(self.kind(), TypeKind::Error)
    }

    #[inline]
    pub(crate) const fn is_infer(self) -> bool {
        matches!(self.kind(), TypeKind::Infer(_))
    }

    #[inline]
    pub(crate) const fn infer_var(self) -> Option<u32> {
        match self.kind() {
            TypeKind::Infer(variable) => Some(variable),
            _ => None,
        }
    }

    #[inline]
    pub(crate) const fn strip_reference(self) -> Self {
        match self.kind() {
            TypeKind::Ref { to, .. } | TypeKind::Raw { to, .. } => to,
            _ => self,
        }
    }

    #[inline]
    pub const fn is_number(self) -> bool {
        self.is_integer() || self.is_float()
    }

    #[inline]
    pub const fn is_integer(self) -> bool {
        matches!(
            self.kind(),
            TypeKind::I8
                | TypeKind::U8
                | TypeKind::I16
                | TypeKind::U16
                | TypeKind::I32
                | TypeKind::U32
                | TypeKind::I64
                | TypeKind::U64
                | TypeKind::Uptr
                | TypeKind::Iptr
        )
    }

    #[inline]
    pub const fn is_float(self) -> bool {
        matches!(self.kind(), TypeKind::F32 | TypeKind::F64)
    }

    #[inline]
    pub const fn is_32_bit(self) -> bool {
        matches!(self.kind(), TypeKind::F32 | TypeKind::I32 | TypeKind::U32)
    }

    #[inline]
    pub(crate) const fn is_primitive_castable(self) -> bool {
        self.is_integer() || matches!(self.kind(), TypeKind::Bool | TypeKind::Char)
    }

    #[inline]
    pub const fn is_aggregate(self) -> bool {
        matches!(
            self.kind(),
            TypeKind::Str | TypeKind::String | TypeKind::Array(_) | TypeKind::Slice { .. }
        )
    }

    #[inline]
    pub const fn is_slice(self) -> bool {
        matches!(self.kind(), TypeKind::Slice { .. })
    }

    #[inline]
    pub const fn diverges(self) -> bool {
        matches!(self.kind(), TypeKind::Never)
    }

    #[inline]
    pub const fn is_ref(self) -> bool {
        matches!(self.kind(), TypeKind::Ref { .. })
    }

    #[inline]
    pub const fn is_raw(self) -> bool {
        matches!(self.kind(), TypeKind::Raw { .. })
    }

    #[inline]
    pub const fn is_pointer(self) -> bool {
        matches!(self.kind(), TypeKind::Ref { .. } | TypeKind::Raw { .. })
    }

    pub(in crate::hir) fn is_copy(self, scope: &ItemTable<'hir>) -> bool {
        self.is_number()
            || matches!(self.kind(), TypeKind::Bool | TypeKind::Char)
            || self.is_pointer()
            || scope
                .symbols
                .get_id("Copy")
                .is_some_and(|copy| scope.implements_interface(self, copy))
    }
}

impl Default for Type<'_> {
    fn default() -> Self {
        Type(&UNIT)
    }
}

impl<'hir> From<TypeKind<'hir>> for Type<'hir> {
    fn from(kind: TypeKind<'hir>) -> Self {
        Self::new(kind)
    }
}

impl PartialEq for Type<'_> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for Type<'_> {}

impl Hash for Type<'_> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.0, state);
    }
}

impl EnumRepr {
    #[inline]
    pub const fn typ(self) -> Type<'static> {
        match self {
            Self::I8 => Type(&I8),
            Self::U8 => Type(&U8),
            Self::I16 => Type(&I16),
            Self::U16 => Type(&U16),
            Self::I32 => Type(&I32),
            Self::U32 => Type(&U32),
            Self::I64 => Type(&I64),
            Self::U64 => Type(&U64),
            Self::Iptr => Type(&IPTR),
            Self::Uptr => Type(&UPTR),
        }
    }

    #[inline]
    pub const fn layout(self) -> (u32, u32) {
        match self {
            Self::I8 | Self::U8 => (1, 1),
            Self::I16 | Self::U16 => (2, 2),
            Self::I32 | Self::U32 => (4, 4),
            Self::I64 | Self::U64 | Self::Iptr | Self::Uptr => (8, 8),
        }
    }

    pub(crate) fn minimal_for(variants: &[statement::EnumVariant<'_>]) -> Self {
        if variants.iter().any(|variant| variant.payload.is_some()) {
            return Self::default();
        }

        let mut next = 0i64;
        let (mut min, mut max) = (0i64, 0i64);
        for variant in variants {
            let value = variant.value.unwrap_or(next);
            next = value.wrapping_add(1);
            min = min.min(value);
            max = max.max(value);
        }
        Self::fitting(min, max)
    }

    #[inline]
    const fn fitting(min: i64, max: i64) -> Self {
        match min >= 0 {
            true if max <= u8::MAX as i64 => Self::U8,
            true if max <= u16::MAX as i64 => Self::U16,
            true if max <= u32::MAX as i64 => Self::U32,
            true => Self::U64,
            false if min >= i8::MIN as i64 && max <= i8::MAX as i64 => Self::I8,
            false if min >= i16::MIN as i64 && max <= i16::MAX as i64 => Self::I16,
            false if min >= i32::MIN as i64 && max <= i32::MAX as i64 => Self::I32,
            false => Self::I64,
        }
    }
}

impl TypeKind<'_> {
    const fn scalar_name(self) -> Option<&'static str> {
        Some(match self {
            Self::I8 => "i8",
            Self::U8 => "u8",
            Self::I16 => "i16",
            Self::U16 => "u16",
            Self::I32 => "i32",
            Self::U32 => "u32",
            Self::I64 => "i64",
            Self::U64 => "u64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::Char => "char",
            Self::Uptr => "uptr",
            Self::Iptr => "iptr",
            Self::Unit => "unit",
            _ => return None,
        })
    }

    pub(in crate::hir) fn mangled(self) -> &'static str {
        self.scalar_name().unwrap_or(match self {
            Self::Str => "str",
            Self::String => "string",
            Self::SelfType => "self",
            Self::Never => "never",
            _ => "type",
        })
    }
}

impl std::fmt::Display for TypeKind<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(name) = self.scalar_name() {
            return f.write_str(name);
        }
        match *self {
            Self::Str => f.write_str("&str"),
            Self::String => f.write_str("String"),
            Self::SelfType => f.write_str("Self"),
            Self::Never => f.write_str("!"),
            Self::GenericParam(index) => write!(f, "T{index}"),
            Self::Adt(id, args) => {
                diagnostic::write_adt_name(f, id.0)?;
                if !args.is_empty() {
                    f.write_str("<")?;
                    for (index, arg) in args.iter().enumerate() {
                        if index != 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    f.write_str(">")?;
                }
                Ok(())
            },
            Self::Array(id) => diagnostic::write_array_name(f, id.0),
            Self::Slice { mutable, element } => {
                f.write_str(match mutable {
                    true => "&mut [",
                    _ => "&[",
                })?;
                element.kind().fmt(f)?;
                f.write_str("]")
            },
            Self::Ref { mutable, to } => {
                f.write_str(match mutable {
                    true => "&mut ",
                    _ => "&",
                })?;
                to.kind().fmt(f)
            },
            Self::Raw { mutable, to } => {
                f.write_str(match mutable {
                    true => "*mut ",
                    _ => "*",
                })?;
                to.kind().fmt(f)
            },
            Self::Infer(_) => f.write_str("{integer}"),
            Self::Error => f.write_str("{unknown}"),
            _ => unreachable!("every scalar kind is named by `scalar_name`"),
        }
    }
}

impl std::fmt::Display for Type<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.kind().fmt(f)
    }
}

impl std::fmt::Debug for Type<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.kind().fmt(f)
    }
}

impl TryFrom<statement::Type<'_>> for EnumRepr {
    type Error = ();

    fn try_from(value: statement::Type<'_>) -> Result<Self, Self::Error> {
        Ok(match value {
            statement::Type::I8 => EnumRepr::I8,
            statement::Type::U8 => EnumRepr::U8,
            statement::Type::I16 => EnumRepr::I16,
            statement::Type::U16 => EnumRepr::U16,
            statement::Type::I32 => EnumRepr::I32,
            statement::Type::U32 => EnumRepr::U32,
            statement::Type::I64 => EnumRepr::I64,
            statement::Type::U64 => EnumRepr::U64,
            statement::Type::Iptr => EnumRepr::Iptr,
            statement::Type::Uptr => EnumRepr::Uptr,
            _ => return Err(()),
        })
    }
}

macro_rules! primitive_kinds {
    ($($name:ident: $kind:ident => $field:ident),* $(,)?) => {
        $(static $name: TypeKind<'static> = TypeKind::$kind;)*

        impl CommonTypes<'_> {
            const fn new() -> Self {
                Self {
                    $($field: Type(&$name),)*
                }
            }
        }

        impl<'hir> TyInterner<'hir> {
            fn primitive(&self, kind: TypeKind<'hir>) -> Option<Type<'hir>> {
                Some(match kind {
                    $(TypeKind::$kind => self.common.$field,)*
                    _ => return None,
                })
            }
        }

        impl<'hir> Type<'hir> {
            const fn new(kind: TypeKind<'hir>) -> Self {
                match kind {
                    $(TypeKind::$kind => Type(&$name),)*
                    _ => panic!("structural types must be created by TyInterner"),
                }
            }
        }
    };
}

primitive_kinds! {
    UNIT: Unit => unit,
    I8: I8 => i8,
    U8: U8 => u8,
    I16: I16 => i16,
    U16: U16 => u16,
    I32: I32 => i32,
    U32: U32 => u32,
    I64: I64 => i64,
    U64: U64 => u64,
    F32: F32 => f32,
    F64: F64 => f64,
    BOOL: Bool => bool,
    UPTR: Uptr => uptr,
    IPTR: Iptr => iptr,
    CHAR: Char => char,
    STR: Str => str,
    STRING: String => string,
    SELF_TYPE: SelfType => self_type,
    NEVER: Never => never,
    ERROR: Error => error,
}
