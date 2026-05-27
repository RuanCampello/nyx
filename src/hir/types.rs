use super::Struct;
use crate::parser::statement;

/// A manually bit-packed 64-bit representation of a type.
///
///
/// Passing this type by value throughout the compiler ensures it fits in a single
/// CPU register on a 64-bit machine. This maximizes cache density and eliminates
/// alignment padding bloat in AST and HIR structures.
///
/// Pattern matching can be performed on-demand by calling [`.kind()`](Type::kind),
/// which unpacks this into a transient [`TypeKind`].
///
/// # Bit Layouts
///
/// The memory layout depends on the value of the least significant **Tag** byte.
///
/// ### primitives & `struct`s/`enum`s (Tag < 20)
///
/// | Bits | 63 .. 48 | 47 .. 40 | 39 .. 8 | 7 .. 0 |
/// | :--- | :---: | :---: | :---: | :---: |
/// | **field** | Unused | EnumRepr | ID Index | Tag |
/// | **size** | 16 bits | 8 bits | 32 bits | 8 bits |
///
/// ### References (Tag == 20)
///
/// | Bits | 63 .. 56 | 55 .. 48 | 47 .. 16 | 15 .. 9 | 8 | 7 .. 0 |
/// | :--- | :---: | :---: | :---: | :---: | :---: | :---: |
/// | **Field** | Unused | RefTarget EnumRepr | RefTarget ID Index | RefTarget Tag | Mut | Tag (20) |
/// | **Size** | 8 bits | 8 bits | 32 bits | 7 bits | 1 bit | 8 bits |
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Type(u64);

/// A memory-optimised, non-reference type representation, packed into a single 64-bit word.
///
/// Structurally identical to `Type`, but statically guaranteed by the type system and API
/// invariants to never contain a reference variant, allowing safe casts between `RefTarget` and `Type`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RefTarget(u64);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[rustfmt::skip]
pub enum TypeKind {
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
    Struct(StructId),
    Enum(EnumId),
    SelfType,
    Ref {
        mutable: bool,
        to: RefTarget,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[rustfmt::skip]
pub enum RefTargetKind {
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
    Struct(StructId),
    Enum(EnumId),
    SelfType,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[rustfmt::skip]
pub enum EnumRepr {
    I8, U8, I16, U16,
    #[default]
    I32,
    U32, I64, U64, Iptr, Uptr,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructId(pub u32);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnumId(pub u32, pub EnumRepr);

const UNIT: u8 = 0;
const I8: u8 = 1;
const U8: u8 = 2;
const I16: u8 = 3;
const U16: u8 = 4;
const I32: u8 = 5;
const U32: u8 = 6;
const I64: u8 = 7;
const U64: u8 = 8;
const F32: u8 = 9;
const F64: u8 = 10;
const BOOL: u8 = 11;
const UPTR: u8 = 12;
const IPTR: u8 = 13;
const CHAR: u8 = 14;
const STR: u8 = 15;
const STRING: u8 = 16;
const STRUCT: u8 = 17;
const ENUM: u8 = 18;
const SELF_TYPE: u8 = 19;
const REF: u8 = 20;

const MUT_BIT_SHIFT: u32 = 8;
const REF_TAG_SHIFT: u32 = 9;
const REF_PAYLOAD_SHIFT: u32 = 8;

const TAG_MASK: u64 = 0xFF;
const REF_TAG_MASK: u64 = 0x7F;

impl EnumRepr {
    #[inline]
    pub(crate) const fn typ(self) -> Type {
        match self {
            Self::I8 => Type::new(TypeKind::I8),
            Self::U8 => Type::new(TypeKind::U8),
            Self::I16 => Type::new(TypeKind::I16),
            Self::U16 => Type::new(TypeKind::U16),
            Self::I32 => Type::new(TypeKind::I32),
            Self::U32 => Type::new(TypeKind::U32),
            Self::I64 => Type::new(TypeKind::I64),
            Self::U64 => Type::new(TypeKind::U64),
            Self::Iptr => Type::new(TypeKind::Iptr),
            Self::Uptr => Type::new(TypeKind::Uptr),
        }
    }

    #[inline]
    pub(crate) const fn layout(self) -> (u32, u32) {
        match self {
            Self::I8 | Self::U8 => (1, 1),
            Self::I16 | Self::U16 => (2, 2),
            Self::I32 | Self::U32 => (4, 4),
            Self::I64 | Self::U64 | Self::Iptr | Self::Uptr => (8, 8),
        }
    }

    #[inline]
    const fn to_u8(self) -> u8 {
        match self {
            Self::I8 => 0,
            Self::U8 => 1,
            Self::I16 => 2,
            Self::U16 => 3,
            Self::I32 => 4,
            Self::U32 => 5,
            Self::I64 => 6,
            Self::U64 => 7,
            Self::Iptr => 8,
            Self::Uptr => 9,
        }
    }

    const fn from_u8(val: u8) -> Self {
        match val {
            0 => Self::I8,
            1 => Self::U8,
            2 => Self::I16,
            3 => Self::U16,
            4 => Self::I32,
            5 => Self::U32,
            6 => Self::I64,
            7 => Self::U64,
            8 => Self::Iptr,
            9 => Self::Uptr,
            _ => panic!("invalid EnumRepr tag"),
        }
    }
}

impl Default for Type {
    #[inline]
    fn default() -> Self {
        Self(UNIT as u64)
    }
}

impl Default for RefTarget {
    #[inline]
    fn default() -> Self {
        Self(UNIT as u64)
    }
}

impl Type {
    #[inline]
    pub const fn new(kind: TypeKind) -> Self {
        match kind {
            TypeKind::Unit => Self(UNIT as u64),
            TypeKind::I8 => Self(I8 as u64),
            TypeKind::U8 => Self(U8 as u64),
            TypeKind::I16 => Self(I16 as u64),
            TypeKind::U16 => Self(U16 as u64),
            TypeKind::I32 => Self(I32 as u64),
            TypeKind::U32 => Self(U32 as u64),
            TypeKind::I64 => Self(I64 as u64),
            TypeKind::U64 => Self(U64 as u64),
            TypeKind::F32 => Self(F32 as u64),
            TypeKind::F64 => Self(F64 as u64),
            TypeKind::Bool => Self(BOOL as u64),
            TypeKind::Uptr => Self(UPTR as u64),
            TypeKind::Iptr => Self(IPTR as u64),
            TypeKind::Char => Self(CHAR as u64),
            TypeKind::Str => Self(STR as u64),
            TypeKind::String => Self(STRING as u64),
            TypeKind::SelfType => Self(SELF_TYPE as u64),
            TypeKind::Struct(id) => Self((STRUCT as u64) | ((id.0 as u64) << 8)),
            TypeKind::Enum(id) => {
                Self((ENUM as u64) | ((id.0 as u64) << 8) | ((id.1.to_u8() as u64) << 40))
            },

            TypeKind::Ref { mutable, to } => {
                let mut bits = REF as u64;
                if mutable {
                    bits |= 1 << MUT_BIT_SHIFT;
                }
                let target_bits = to.0;
                let payload_bits = (target_bits & !TAG_MASK) << REF_PAYLOAD_SHIFT;
                let tag_bits = (target_bits & REF_TAG_MASK) << REF_TAG_SHIFT;
                Self(bits | tag_bits | payload_bits)
            },
        }
    }

    #[inline]
    pub const fn kind(self) -> TypeKind {
        let tag = tag(self.0);
        match tag {
            UNIT => TypeKind::Unit,
            I8 => TypeKind::I8,
            U8 => TypeKind::U8,
            I16 => TypeKind::I16,
            U16 => TypeKind::U16,
            I32 => TypeKind::I32,
            U32 => TypeKind::U32,
            I64 => TypeKind::I64,
            U64 => TypeKind::U64,
            F32 => TypeKind::F32,
            F64 => TypeKind::F64,
            BOOL => TypeKind::Bool,
            UPTR => TypeKind::Uptr,
            IPTR => TypeKind::Iptr,
            CHAR => TypeKind::Char,
            STR => TypeKind::Str,
            STRING => TypeKind::String,
            SELF_TYPE => TypeKind::SelfType,
            STRUCT => {
                let id = (self.0 >> 8) as u32;
                TypeKind::Struct(StructId(id))
            },
            ENUM => {
                let id = ((self.0 >> 8) & 0xFFFFFFFF) as u32;
                let repr_u8 = ((self.0 >> 40) & 0xFF) as u8;
                TypeKind::Enum(EnumId(id, EnumRepr::from_u8(repr_u8)))
            },
            REF => {
                let mutable = ((self.0 >> MUT_BIT_SHIFT) & 1) != 0;
                let to = RefTarget(
                    ((self.0 >> REF_PAYLOAD_SHIFT) & !TAG_MASK)
                        | ((self.0 >> REF_TAG_SHIFT) & REF_TAG_MASK),
                );
                TypeKind::Ref { mutable, to }
            },
            _ => panic!("invalid Type tag"),
        }
    }

    #[inline]
    pub(crate) fn strip_reference(self) -> Self {
        match self.kind() {
            TypeKind::Ref { to, .. } => Self::from(to),
            _ => self,
        }
    }

    #[inline(always)]
    pub const fn is_number(self) -> bool {
        self.is_integer() || self.is_float()
    }

    #[inline(always)]
    pub const fn is_integer(self) -> bool {
        let tag = tag(self.0);
        (tag >= I8 && tag <= U64) || tag == UPTR || tag == IPTR
    }

    #[inline(always)]
    pub const fn is_float(self) -> bool {
        let tag = tag(self.0);
        tag == F32 || tag == F64
    }

    #[inline(always)]
    pub const fn is_32_bit(self) -> bool {
        let tag = tag(self.0);
        tag == F32 || tag == I32 || tag == U32
    }

    #[inline(always)]
    pub(crate) const fn is_primitive_castable(self) -> bool {
        let tag = tag(self.0);
        (tag >= I8 && tag <= U64) || tag == UPTR || tag == IPTR || tag == BOOL || tag == CHAR
    }

    #[inline(always)]
    /// returns (size, alignment) of the type
    pub const fn layout(self, structs: &[Option<Struct>]) -> (u32, u32) {
        let tag = tag(self.0);
        match tag {
            I8 | U8 | BOOL => (1, 1),
            I16 | U16 => (2, 2),
            I32 | U32 | F32 | CHAR => (4, 4),
            I64 | U64 | IPTR | UPTR | REF | F64 => (8, 8),
            STR => (16, 8),
            STRING => (24, 8),
            UNIT => (0, 1),

            STRUCT => {
                let id = (self.0 >> 8) as u32;
                let definition = match structs[id as usize].as_ref() {
                    Some(s) => s,
                    None => panic!("dependent struct is already lowered"),
                };
                (definition.size, definition.align)
            },

            ENUM => {
                let repr_u8 = ((self.0 >> 40) & 0xFF) as u8;
                let repr = match repr_u8 {
                    0 => EnumRepr::I8,
                    1 => EnumRepr::U8,
                    2 => EnumRepr::I16,
                    3 => EnumRepr::U16,
                    4 => EnumRepr::I32,
                    5 => EnumRepr::U32,
                    6 => EnumRepr::I64,
                    7 => EnumRepr::U64,
                    8 => EnumRepr::Iptr,
                    9 => EnumRepr::Uptr,
                    _ => unreachable!(),
                };
                repr.layout()
            },
            SELF_TYPE => unreachable!(),
            _ => unreachable!(),
        }
    }
}

impl RefTarget {
    #[inline]
    pub const fn new(kind: RefTargetKind) -> Self {
        match kind {
            RefTargetKind::Unit => Self(UNIT as u64),
            RefTargetKind::I8 => Self(I8 as u64),
            RefTargetKind::U8 => Self(U8 as u64),
            RefTargetKind::I16 => Self(I16 as u64),
            RefTargetKind::U16 => Self(U16 as u64),
            RefTargetKind::I32 => Self(I32 as u64),
            RefTargetKind::U32 => Self(U32 as u64),
            RefTargetKind::I64 => Self(I64 as u64),
            RefTargetKind::U64 => Self(U64 as u64),
            RefTargetKind::F32 => Self(F32 as u64),
            RefTargetKind::F64 => Self(F64 as u64),
            RefTargetKind::Bool => Self(BOOL as u64),
            RefTargetKind::Uptr => Self(UPTR as u64),
            RefTargetKind::Iptr => Self(IPTR as u64),
            RefTargetKind::Char => Self(CHAR as u64),
            RefTargetKind::Str => Self(STR as u64),
            RefTargetKind::String => Self(STRING as u64),
            RefTargetKind::SelfType => Self(SELF_TYPE as u64),
            RefTargetKind::Struct(id) => Self((STRUCT as u64) | ((id.0 as u64) << 8)),
            RefTargetKind::Enum(id) => {
                Self((ENUM as u64) | ((id.0 as u64) << 8) | ((id.1.to_u8() as u64) << 40))
            },
        }
    }

    pub const fn kind(self) -> RefTargetKind {
        match tag(self.0) {
            UNIT => RefTargetKind::Unit,
            I8 => RefTargetKind::I8,
            U8 => RefTargetKind::U8,
            I16 => RefTargetKind::I16,
            U16 => RefTargetKind::U16,
            I32 => RefTargetKind::I32,
            U32 => RefTargetKind::U32,
            I64 => RefTargetKind::I64,
            U64 => RefTargetKind::U64,
            F32 => RefTargetKind::F32,
            F64 => RefTargetKind::F64,
            BOOL => RefTargetKind::Bool,
            UPTR => RefTargetKind::Uptr,
            IPTR => RefTargetKind::Iptr,
            CHAR => RefTargetKind::Char,
            STR => RefTargetKind::Str,
            STRING => RefTargetKind::String,
            SELF_TYPE => RefTargetKind::SelfType,
            STRUCT => {
                let id = (self.0 >> 8) as u32;
                RefTargetKind::Struct(StructId(id))
            },
            ENUM => {
                let id = ((self.0 >> 8) & 0xFFFFFFFF) as u32;
                let repr_u8 = ((self.0 >> 40) & 0xFF) as u8;
                RefTargetKind::Enum(EnumId(id, EnumRepr::from_u8(repr_u8)))
            },
            _ => panic!("invalid RefTarget tag"),
        }
    }
}

#[inline(always)]
pub const fn tag(packed: u64) -> u8 {
    (packed & TAG_MASK) as u8
}

impl From<RefTarget> for Type {
    #[inline]
    fn from(value: RefTarget) -> Self {
        // safe bitwise copy since ref target shares layout with non-ref typekind variants
        Self(value.0)
    }
}

impl TryFrom<Type> for RefTarget {
    type Error = ();

    #[inline]
    fn try_from(value: Type) -> Result<Self, Self::Error> {
        match tag(value.0) == REF {
            false => Ok(Self(value.0)),
            true => Err(()),
        }
    }
}

impl From<&statement::Type<'_>> for Type {
    fn from(value: &statement::Type<'_>) -> Self {
        use statement::Type as AstType;

        let kind = match value {
            AstType::I8 => TypeKind::I8,
            AstType::U8 => TypeKind::U8,
            AstType::I16 => TypeKind::I16,
            AstType::U16 => TypeKind::U16,
            AstType::I32 => TypeKind::I32,
            AstType::U32 => TypeKind::U32,
            AstType::I64 => TypeKind::I64,
            AstType::U64 => TypeKind::U64,
            AstType::F32 => TypeKind::F32,
            AstType::F64 => TypeKind::F64,
            AstType::Bool => TypeKind::Bool,
            AstType::Uptr => TypeKind::Uptr,
            AstType::Iptr => TypeKind::Iptr,
            AstType::Char => TypeKind::Char,
            AstType::Str => TypeKind::Str,
            AstType::String => TypeKind::String,
            AstType::Unit => TypeKind::Unit,
            AstType::SelfType => TypeKind::SelfType,
            AstType::RefSelf => {
                TypeKind::Ref { mutable: false, to: RefTarget::new(RefTargetKind::SelfType) }
            },
            AstType::Named(_) => unreachable!("already resolved by resolve_type"),
        };
        Type::new(kind)
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self.kind() {
            TypeKind::I8 => "i8",
            TypeKind::U8 => "u8",
            TypeKind::I16 => "i16",
            TypeKind::U16 => "u16",
            TypeKind::I32 => "i32",
            TypeKind::U32 => "u32",
            TypeKind::I64 => "i64",
            TypeKind::U64 => "u64",
            TypeKind::F32 => "f32",
            TypeKind::F64 => "f64",
            TypeKind::Bool => "bool",
            TypeKind::Char => "char",
            TypeKind::Uptr => "uptr",
            TypeKind::Iptr => "iptr",
            TypeKind::Str => "&str",
            TypeKind::String => "String",
            TypeKind::Unit => "unit",
            TypeKind::SelfType => "Self",
            TypeKind::Struct(id) => return write!(f, "struct#{}", id.0),
            TypeKind::Enum(id) => return write!(f, "enum#{}", id.0),
            TypeKind::Ref { mutable, to } => {
                let prefix = match mutable {
                    true => "&mut ",
                    _ => "&",
                };
                f.write_str(prefix)?;

                return match to.kind() {
                    RefTargetKind::Struct(id) => write!(f, "struct#{}", id.0),
                    RefTargetKind::Enum(id) => write!(f, "enum#{}", id.0),
                    RefTargetKind::SelfType => write!(f, "Self"),
                    _ => write!(f, "{}", Type::from(to)),
                };
            },
        };

        f.write_str(s)
    }
}

impl std::fmt::Debug for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.kind())
    }
}

impl std::fmt::Debug for RefTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.kind())
    }
}

impl TryFrom<statement::Type<'_>> for EnumRepr {
    type Error = ();
    #[inline]
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
