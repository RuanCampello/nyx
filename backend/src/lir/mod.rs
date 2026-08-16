//! Low-level IR (LIR).
//!
//! The LIR sits between MIR and assembly emission. It is a low-level
//! representation of the program that is closer to the target ISA. Every
//! instruction matches the shape of the target ISA so the emitter is purely
//! mechanical. The register allocator works on VRegs and assigns them to
//! physical registers or stack slots.

#![allow(clippy::too_many_arguments)]
use crate::{
    hir::{EnumRepr, Static, Type, TypeKind},
    lir::target::{CondCode, Emittable, Lowerable, RegClass, Target},
    mir::{self, Layout},
};
use frontend::hir::StaticId;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

mod opt;
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

#[derive(Clone, Copy)]
pub struct Layouts<'a, 'hir> {
    pub adts: &'a HashMap<Type<'hir>, Layout>,
    pub adt_reprs: &'a [Option<EnumRepr>],
    pub arrays: &'a [Layout],
}

/// All control-flow terminators
#[derive(Debug, PartialEq, Clone)]
pub enum Term {
    Jump(BlockId),
    Branch {
        cond: VReg,
        then_block: BlockId,
        else_block: BlockId,
    },
    /// branch on the condition flags left by an earlier comparison in the same
    /// block, rather than on a materialised boolean
    ///
    /// produced only by [crate::lir::opt], which is responsible for proving
    /// that nothing between the comparison and the terminator disturbs the flags
    BranchCc {
        cond: CondCode,
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

/// A runtime fault that aborts the program through a shared panic handler
///
/// Each variant owns the runtime symbol it jumps to. Code generation calls
/// [Panic::require] at a fault site to both record that the handler is needed
/// and obtain its symbol, [Panic::required] then drives handler emission
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panic {
    AddOverflow,
    SubOverflow,
    MulOverflow,
    IndexOutOfBounds,
}

pub trait TypeExt {
    fn is_aggregate_lir(self, layouts: Layouts) -> bool;
    fn machine_type(&self, layouts: Layouts) -> MachineType;
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

        let mut lir = T::lower(
            function,
            &mir.symbols,
            &mir.functions,
            &mir.adt_layouts,
            &mir.adt_reprs,
            &mir.array_layouts,
        );
        opt::combine(&mut lir);

        let alloc = lir.allocate();
        lir.emit(alloc, &mut out);
    }

    Function::<T>::emit_panic_handlers(&mut out);

    // emit a `_start` trampoline if the program defines `fn main`
    //
    // this allows the binary to be linked with `ld` directly
    // `_start` calls `nyx_main`, passes its return value to the exit syscall
    let main = mir
        .functions
        .iter()
        .find(|function| {
            let name = mir.symbols.get(function.name_symbol);
            name == "main" || name.ends_with("::main")
        })
        .map(|function| {
            (
                assembly_label(mir.symbols.get(function.name_symbol)),
                !matches!(function.return_type.kind(), TypeKind::Unit),
            )
        });
    if let Some((main, returns_value)) = main {
        Function::<T>::start(&mut out, &main, returns_value);
    }

    if !mir.strings.is_empty() {
        label!(out, ".section .rodata");
        for (idx, string) in mir.strings.iter().enumerate() {
            label!(out, ".align 1");
            label!(out, ".L_str_{}:", idx);
            label!(out, "    .asciz {:?}", string);
        }
    }

    let layouts = Layouts {
        adts: &mir.adt_layouts,
        adt_reprs: &mir.adt_reprs,
        arrays: &mir.array_layouts,
    };
    emit_statics(&mir.statics, layouts, &mut out);

    out
}

/// Lay out module-level globals
///
/// a zero initialiser costs nothing in the image, so those go to `.bss` and the
/// loader zeroes them, everything else has to carry its bytes in `.data`
fn emit_statics(statics: &[Static], layouts: Layouts, out: &mut String) {
    let entries = statics.iter().enumerate().map(|(id, item)| (StaticId(id as u32), item));
    let (zeroed, initialised): (Vec<_>, Vec<_>) =
        entries.partition(|(_, item)| item.init.is_zero());

    for (section, items) in [(".bss", zeroed), (".data", initialised)] {
        if items.is_empty() {
            continue;
        }

        label!(out, ".section {}", section);
        for (id, item) in items {
            let (size, align) = match item.typ.machine_type(layouts) {
                MachineType::Struct { size, align } => (size, align),
                scalar => (u32::from(scalar.bytes()), u32::from(scalar.bytes())),
            };
            label!(out, ".align {align}");
            label!(out, "{}:", static_label(id));

            match section {
                ".bss" => label!(out, "    .zero {size}"),
                _ => label!(out, "    {}", item.init.static_directive(size)),
            }
        }
    }
}

impl Panic {
    const ALL: [Self; 4] =
        [Self::AddOverflow, Self::SubOverflow, Self::MulOverflow, Self::IndexOutOfBounds];

    #[inline]
    const fn bit(self) -> u8 {
        1 << (self as u8)
    }

    #[inline]
    pub const fn symbol<'s>(self) -> &'s str {
        match self {
            Self::AddOverflow => "__nyx_panic_add_overflow",
            Self::SubOverflow => "__nyx_panic_sub_overflow",
            Self::MulOverflow => "__nyx_panic_mul_overflow",
            Self::IndexOutOfBounds => "__nyx_panic_index_out_of_bounds",
        }
    }

    /// record that this handler must be emitted and return the symbol to jump to
    #[inline]
    pub fn require<'s>(self) -> &'s str {
        PANIC_HANDLERS.with(|handlers| handlers.set(handlers.get() | self.bit()));
        self.symbol()
    }

    /// drain the handlers required by the functions emitted so far
    #[inline]
    pub fn required() -> impl Iterator<Item = Self> {
        let bits = PANIC_HANDLERS.with(|handlers| handlers.take());
        Self::ALL.into_iter().filter(move |panic| bits & panic.bit() != 0)
    }
}

/// An instruction that may trap on overflow, mapping to the [Panic] it raises
pub trait Checked {
    fn overflow_panic(&self) -> Option<Panic>;
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

impl TypeExt for Type<'_> {
    #[inline(always)]
    fn is_aggregate_lir(self, layouts: Layouts) -> bool {
        if let TypeKind::Adt(id, _) = self.kind() {
            return match layouts.adt_reprs[id.0 as usize] {
                Some(repr) => {
                    let (enum_size, _) = layouts.adts[&self].into();
                    enum_size > repr.layout().0
                },
                None => true,
            };
        }
        self.is_aggregate()
    }

    #[inline(always)]
    fn machine_type(&self, layouts: Layouts) -> MachineType {
        match self.kind() {
            TypeKind::I8 => MachineType::Int { bytes: 1, signed: true },
            TypeKind::U8 | TypeKind::Bool => MachineType::Int { bytes: 1, signed: false },
            TypeKind::I16 => MachineType::Int { bytes: 2, signed: true },
            TypeKind::U16 => MachineType::Int { bytes: 2, signed: false },
            TypeKind::I32 => MachineType::Int { bytes: 4, signed: true },
            TypeKind::U32 | TypeKind::Char => MachineType::Int { bytes: 4, signed: false },
            TypeKind::I64 | TypeKind::Iptr => MachineType::Int { bytes: 8, signed: true },
            TypeKind::U64 | TypeKind::Uptr | TypeKind::Ref { .. } | TypeKind::Raw { .. } => {
                MachineType::Int { bytes: 8, signed: false }
            },
            TypeKind::Str | TypeKind::Slice { .. } => MachineType::Struct { size: 16, align: 8 },
            TypeKind::String => MachineType::Struct { size: 24, align: 8 },
            TypeKind::F32 => MachineType::Float { bytes: 4 },
            TypeKind::F64 => MachineType::Float { bytes: 8 },
            TypeKind::Adt(id, _) => {
                let layout = layouts.adts[self];
                match layouts.adt_reprs[id.0 as usize] {
                    None => {
                        let (size, align) = layout.into();
                        MachineType::Struct { size, align }
                    },
                    Some(repr) => {
                        let tag_size = repr.layout().0;
                        let (size, align) = layout.into();
                        match size > tag_size {
                            true => MachineType::Struct { size, align },
                            false => repr.typ().machine_type(layouts),
                        }
                    },
                }
            },
            TypeKind::Array(id) => {
                let (size, align) = layouts.arrays[id.0 as usize].into();
                MachineType::Struct { size, align }
            },
            TypeKind::Unit => unreachable!("unit does not have a machine type"),
            TypeKind::SelfType => unreachable!("Self type does not have a machine type"),
            TypeKind::GenericParam(_) => {
                unreachable!("GenericParam must be resolved before LIR lowering")
            },
            TypeKind::Never => MachineType::Int { bytes: 4, signed: true },
            TypeKind::Infer(_) => {
                unreachable!("integer inference variables must be resolved before LIR lowering")
            },
            TypeKind::Error => unreachable!("poisoned types must not reach LIR lowering"),
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

/// The assembly symbol a module-level global is laid out under
pub fn static_label(id: crate::hir::StaticId) -> String {
    format!(".L_static_{}", id.0)
}

impl Term {
    pub fn uses_of(&self) -> &[VReg] {
        match self {
            Self::Return(Some(v)) => std::slice::from_ref(v),
            Self::Branch { cond, .. } => std::slice::from_ref(cond),
            Self::Return(None) | Self::Jump(_) | Self::BranchCc { .. } => &[],
        }
    }
}

impl std::ops::Index<mir::ValueId> for Vec<VReg> {
    type Output = VReg;
    fn index(&self, index: mir::ValueId) -> &Self::Output {
        &self[index.0 as usize]
    }
}

impl From<mir::BlockId> for BlockId {
    fn from(value: mir::BlockId) -> Self {
        Self(value.0)
    }
}
