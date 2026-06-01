//! Low-level IR (LIR).
//!
//! The LIR sits between MIR and assembly emission. It is a low-level
//! representation of the program that is closer to the target ISA. Every
//! instruction matches the shape of the target ISA so the emitter is purely
//! mechanical. The register allocator works on VRegs and assigns them to
//! physical registers or stack slots.

use crate::{
    hir::{Type, TypeKind},
    lir::target::{Emittable, Lowerable, RegClass, Target},
    mir::{self, Layout},
};
use std::collections::BTreeMap;
use std::fmt::Write;

mod regalloc;
pub mod target;

/// A function in LIR form, parameterised over the target.
#[derive(Debug)]
pub struct Function<T: Target> {
    name: String,
    blocks: Vec<Block<T::Instruction>>,

    /// maps a [virtual register](self::VReg) index to a [machine type](self::MachineType)
    vreg_types: Vec<MachineType>,
    next_vreg: u32,

    /// VRegs that must be pinned to specific physical registers.
    pub(in crate::lir) precolours: Vec<(VReg, T::Reg)>,

    /// float constants needed for `.rodata` labels
    ///
    /// - *key* = bit pattern
    /// - *value* = key
    floats: BTreeMap<u64, String>,
    float_counter: u32,
}

/// A linear sequence of instructions ending in exactly one
/// [terminator](self::Term).
#[derive(Debug, Clone, PartialEq)]
pub struct Block<I> {
    id: BlockId,
    instructions: Vec<I>,
    term: Term,
}

/// All control-flow terminators
#[derive(Debug, PartialEq, Clone)]
pub enum Term {
    Jump(BlockId),
    Branch { cond: VReg, then_block: BlockId, else_block: BlockId },
    Switch { cond: VReg, targets: Vec<(i64, BlockId)>, default: BlockId },
    Return(Option<VReg>),
}

/// A virtual register, which is a dense index identifying a single SSA value.
///
/// VRegs exist only within the LIR. The register allocator maps each one to
/// either a physical register or a stack slot.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub struct VReg(u32);

/// A stable index into function's `blocks` vector
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash)]
pub struct BlockId(u32);

/// Machine-level type
///
/// We don't need much info here, only byte widths matter at this layer.
#[derive(Debug, Clone, Copy)]
pub enum MachineType {
    Int { bytes: u8, signed: bool },
    Float { bytes: u8 },
    Struct { size: u32, align: u32 },
}

thread_local! {
    static PANIC_HANDLERS: std::cell::Cell<u8> = Default::default();
}

const DEFAULT_SIZE: usize = 1 << 10;

#[macro_export]
macro_rules! emit {
    ($dst:expr, $($arg:tt)*) => {
        writeln!($dst, "    {}", format_args!($($arg)*)).unwrap()
    }
}

#[macro_export]
macro_rules! label {
    ($dst:expr, $($arg:tt)*) => {
        writeln!($dst, "{}", format_args!($($arg)*)).unwrap()
    }
}

pub fn emit<T: Lowerable>(mir: &mir::Mir) -> String
where
    Function<T>: Emittable<T>,
{
    let mut out = String::with_capacity(DEFAULT_SIZE);
    label!(out, ".text");

    for function in &mir.functions {
        if function.intrinsic.is_some() {
            continue;
        }

        let lir = T::lower(
            function,
            &mir.symbols,
            &mir.functions,
            &mir.struct_layouts,
            &mir.enum_layouts,
        );
        let alloc = lir.allocate();
        lir.emit(alloc, &mut out);
    }

    Function::<T>::emit_panic_handlers(&mut out);

    // emit a `_start` trampoline if the program defines `fn main`
    //
    // this allows the binary to be linked with `ld` directly
    // `_start` calls `nyx_main`, passes its return value to the exit syscall
    let main = mir
        .symbols
        .iter()
        .find(|name| name.as_str() == "main" || name.ends_with("::main"))
        .map(|name| assembly_label(name));
    if let Some(main) = main {
        Function::<T>::start(&mut out, &main);
    }

    if !mir.strings.is_empty() {
        label!(out, ".section .rodata");
        for (idx, string) in mir.strings.iter().enumerate() {
            label!(out, ".align 1");
            label!(out, ".L_str_{}:", idx);
            label!(out, "    .asciz {:?}", string);
        }
    }

    out
}

pub trait CheckedOperation {
    const ADD: u8 = 1 << 0;
    const SUB: u8 = 1 << 1;
    const MUL: u8 = 1 << 2;

    fn flag(&self) -> Option<u8>;

    #[inline]
    #[rustfmt::skip]
    fn symbol_for_flag<'s>(flag: u8) -> Option<&'s str> {
        if flag == Self::ADD { Some("__nyx_panic_add_overflow") }
        else if flag == Self::SUB { Some("__nyx_panic_sub_overflow") }
        else if flag == Self::MUL { Some("__nyx_panic_mul_overflow") }
        else { None }
    }

    #[inline]
    fn mark<'s>(&self) -> Option<&'s str> {
        let flag = self.flag()?;
        PANIC_HANDLERS.with(|h| h.set(h.get() | flag));

        Self::symbol_for_flag(flag)
    }

    #[inline]
    fn take() -> u8 {
        PANIC_HANDLERS.with(|h| h.take())
    }
}

impl<T: Target> Function<T> {
    pub fn new(name: String) -> Self {
        Self {
            name,
            blocks: Vec::new(),
            vreg_types: Vec::new(),
            precolours: Vec::new(),
            floats: BTreeMap::new(),
            next_vreg: 0,
            float_counter: 0,
        }
    }

    #[inline(always)]
    pub fn new_vreg(&mut self, typ: MachineType) -> VReg {
        let id = self.next_vreg;

        self.next_vreg += 1;
        self.vreg_types.push(typ);

        VReg(id)
    }

    /// Override a VReg's machine type.
    /// Used after Movzx widens a 1-byte setcc result into 4 bytes.
    #[inline(always)]
    pub fn set_vreg_type(&mut self, vreg: VReg, typ: MachineType) {
        self.vreg_types[vreg.0 as usize] = typ;
    }

    /// Pin a VReg to a specific physical register.
    #[inline(always)]
    pub fn add_precolour(&mut self, vreg: VReg, reg: T::Reg) {
        self.precolours.push((vreg, reg));
    }

    #[inline(always)]
    pub fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);

        self.blocks
            .push(Block { id, instructions: Vec::new(), term: Term::Return(None) });

        id
    }

    #[inline(always)]
    pub fn push_instr(&mut self, block: &BlockId, instruction: T::Instruction) {
        self.blocks[block.0 as usize].instructions.push(instruction);
    }

    #[inline(always)]
    pub fn set_term(&mut self, block: &BlockId, term: Term) {
        self.blocks[block.0 as usize].term = term;
    }

    pub fn new_float(&mut self, bits: u64, is_32: bool) -> String {
        if let Some(label) = self.floats.get(&bits) {
            return label.clone();
        }

        let idx = self.float_counter;
        self.float_counter += 1;

        let prefix = if is_32 {
            "f32"
        } else {
            "f64"
        };
        let label = format!(".LC_{}_{prefix}_{idx}_{bits}", self.name);

        self.floats.insert(bits, label.clone());
        label
    }
}

impl MachineType {
    #[inline(always)]
    pub const fn bytes(self) -> u8 {
        match self {
            Self::Int { bytes, .. } | Self::Float { bytes } => bytes,
            Self::Struct { .. } => 8,
        }
    }

    #[inline(always)]
    pub const fn stack_size(self) -> i32 {
        match self {
            Self::Int { bytes, .. } | Self::Float { bytes } => bytes as i32,
            Self::Struct { size, .. } => size as i32,
        }
    }

    #[inline(always)]
    pub const fn stack_align(self) -> i32 {
        match self {
            Self::Int { bytes, .. } | Self::Float { bytes } => bytes as i32,
            Self::Struct { align, .. } => align as i32,
        }
    }

    #[inline(always)]
    pub const fn class(self) -> RegClass {
        match self {
            Self::Int { .. } | Self::Struct { .. } => RegClass::Int,
            Self::Float { .. } => RegClass::Float,
        }
    }

    #[inline(always)]
    pub const fn is_signed(self) -> bool {
        match self {
            Self::Int { signed, .. } => signed,
            _ => false,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Layouts<'a> {
    pub structs: &'a [Layout],
    pub enums: &'a [Layout],
}

impl Type {
    #[inline(always)]
    pub(in crate::lir) fn is_aggregate_lir(self, layouts: Layouts) -> bool {
        if self.is_aggregate() {
            return true;
        }
        if let TypeKind::Enum(id) = self.kind() {
            let (enum_size, _) = layouts.enums[id.id() as usize].into();
            return enum_size > id.repr().layout().0;
        }
        false
    }

    #[inline(always)]
    pub(in crate::lir) fn machine_type(&self, layouts: Layouts) -> MachineType {
        match self.kind() {
            TypeKind::I8 => MachineType::Int { bytes: 1, signed: true },
            TypeKind::U8 | TypeKind::Bool => MachineType::Int { bytes: 1, signed: false },
            TypeKind::I16 => MachineType::Int { bytes: 2, signed: true },
            TypeKind::U16 => MachineType::Int { bytes: 2, signed: false },
            TypeKind::I32 => MachineType::Int { bytes: 4, signed: true },
            TypeKind::U32 | TypeKind::Char => MachineType::Int { bytes: 4, signed: false },
            TypeKind::I64 | TypeKind::Iptr => MachineType::Int { bytes: 8, signed: true },
            TypeKind::U64 | TypeKind::Uptr | TypeKind::Ref { .. } => {
                MachineType::Int { bytes: 8, signed: false, }
            }
            TypeKind::Str => MachineType::Struct { size: 16, align: 8 },
            TypeKind::String => MachineType::Struct { size: 24, align: 8 },
            TypeKind::F32 => MachineType::Float { bytes: 4 },
            TypeKind::F64 => MachineType::Float { bytes: 8 },
            TypeKind::Struct(id) => {
                let (size, align) = layouts.structs[id.0 as usize].into();
                MachineType::Struct { size, align }
            }
            TypeKind::Enum(id) => {
                let enum_layout = layouts.enums[id.id() as usize];
                let tag_size = id.repr().layout().0;
                let (enum_size, enum_align) = enum_layout.into();
                if enum_size > tag_size {
                    MachineType::Struct { size: enum_size, align: enum_align }
                } else {
                    id.repr().typ().machine_type(layouts)
                }
            }
            TypeKind::Unit => unreachable!("unit doesn't have a machine type"),
            TypeKind::SelfType => unreachable!("Self type doesn't have a machine type"),
            TypeKind::GenericParam(_) => {
                unreachable!("GenericParam must be resolved before LIR lowering")
            },
            TypeKind::Never => MachineType::Int { bytes: 4, signed: true },
        }
    }

    // FIXME: that's a very workaround so future me that's your problem
    #[inline(always)]
    pub(crate) const fn unwrap_unit(self) -> Self {
        match self.kind() {
            TypeKind::Unit | TypeKind::Never => Self::new(TypeKind::I32),
            _ => self,
        }
    }
}

pub(in crate::lir) fn aggregate_chunks(size: u32) -> impl Iterator<Item = (i32, u8)> {
    // PERFORMANCE: aggregate copies are lowered once in LIR using 8/4/2/1 byte chunks
    let mut offset = 0;
    std::iter::from_fn(move || {
        if offset >= size {
            return None;
        }

        let remaining = size - offset;
        let chunk = match remaining {
            8.. => 8,
            4..=7 => 4,
            2..=3 => 2,
            _ => 1,
        };
        let current = offset as i32;
        offset += chunk as u32;

        Some((current, chunk))
    })
}

/// Converts a fully-qualified [crate::hir] name into a valid *GAS* assembly label
#[inline(always)]
fn assembly_label(name: &str) -> String {
    name.replace("::", ".")
}

impl Term {
    pub fn uses_of(&self) -> &[VReg] {
        match self {
            Self::Return(Some(v)) => std::slice::from_ref(v),
            Self::Branch { cond, .. } | Self::Switch { cond, .. } => std::slice::from_ref(cond),
            Self::Return(None) | Self::Jump(_) => &[],
        }
    }
}

impl std::ops::Index<mir::ValueId> for Vec<VReg> {
    type Output = VReg;
    fn index(&self, index: mir::ValueId) -> &Self::Output {
        &self[index.0 as usize]
    }
}

impl From<crate::mir::BlockId> for BlockId {
    fn from(value: crate::mir::BlockId) -> Self {
        Self { 0: value.0 }
    }
}
