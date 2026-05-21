//! Low-level IR (LIR).
//!
//! The LIR sits between MIR and assembly emission. It is a low-level
//! representation of the program that is closer to the target ISA. Every
//! instruction matches the shape of the target ISA so the emitter is purely
//! mechanical. The register allocator works on VRegs and assigns them to
//! physical registers or stack slots.

use crate::{
    hir::Type,
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
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Term {
    Jump(BlockId),
    Branch {
        cond: VReg,
        then_block: BlockId,
        else_block: BlockId,
    },
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

        let lir = T::lower(function, &mir.symbols, &mir.functions, &mir.struct_layouts);
        let alloc = lir.allocate();
        lir.emit(alloc, &mut out);
    }

    // emit a `_start` trampoline if the program defines `fn main`
    //
    // this allows the binary to be linked with `ld` directly
    // `_start` calls `nyx_main`, passes its return value to the exit syscall
    let has_main = mir.symbols.iter().any(|name| name == "main");
    if has_main {
        Function::<T>::start(&mut out);
    }

    if !mir.strings.is_empty() {
        label!(out, ".section .rodata");
        for (idx, string) in mir.strings.iter().enumerate() {
            label!(out, ".align 1");
            label!(out, ".L_str_{}:", idx);
            label!(out, "    .ascii {:?}", string);
        }
    }

    out
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

        self.blocks.push(Block {
            id,
            instructions: Vec::new(),
            term: Term::Return(None),
        });

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

        let prefix = if is_32 { "f32" } else { "f64" };
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

impl Type {
    #[inline(always)]
    pub(in crate::lir) fn machine_type(&self, layouts: &[Layout]) -> MachineType {
        match self {
            Type::I8 => MachineType::Int { bytes: 1, signed: true },
            Type::U8 | Type::Bool => MachineType::Int { bytes: 1, signed: false },
            Type::I16 => MachineType::Int { bytes: 2, signed: true },
            Type::U16 => MachineType::Int { bytes: 2, signed: false },
            Type::I32 => MachineType::Int { bytes: 4, signed: true },
            Type::U32 | Type::Char => MachineType::Int { bytes: 4, signed: false },
            Type::I64
            | Type::Iptr => MachineType::Int { bytes: 8, signed: true },
            Type::U64
            | Type::Uptr
            | Type::Str
            | Type::String
            | Type::Ref { .. } => MachineType::Int { bytes: 8, signed: false },
            Type::F32 => MachineType::Float { bytes: 4 },
            Type::F64 => MachineType::Float { bytes: 8 },
            Type::Struct(id) => {
                let (size, align) = layouts[id.0 as usize].into();
                MachineType::Struct { size, align }
            }
            Type::Unit => unreachable!("unit doesn't have a machine type"),
        }
    }

    // FIXME: that's a very workaround so future me that's your problem
    #[inline(always)]
    pub(crate) const fn unwrap_unit(self) -> Self {
        match self {
            Self::Unit => Self::I32,
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

impl Term {
    pub fn uses_of(&self) -> &[VReg] {
        match self {
            Self::Return(Some(v)) => std::slice::from_ref(v),
            Self::Branch { cond, .. } => std::slice::from_ref(cond),
            Self::Return(None) | Self::Jump(_) => &[],
        }
    }
}

impl From<crate::mir::BlockId> for BlockId {
    fn from(value: crate::mir::BlockId) -> Self {
        Self { 0: value.0 }
    }
}
