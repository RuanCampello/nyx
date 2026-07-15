# Nyx Design Decision

This document is to discuss the **key** design decisions taken when implementing the `nyx` compiler and defining the language syntax, design and semantics.

## Architecture

### Compilation Pipeline

Nyx follows a traditional multi-pass compiler architecture with well-defined intermediate representations:

```
Source Code (.nyx)
    ↓
[Lexer] → Tokens
    ↓
[Parser] → AST (Abstract Syntax Tree)
    ↓
[Semantic Analysis] → HIR (High-level IR)
    ↓
[MIR Lowering] → MIR (Mid-level IR / CFG)
    ↓
[LIR Lowering] → LIR (Low-level IR / Target-specific)
    ↓
[Register Allocation]
    ↓
[Code Generation] → Assembly (.s)
    ↓
[Assembler + Linker] → Executable
```

### Design Rationale

**Why multiple IRs?**
Each IR serves a specific phase and makes certain operations natural:

- `HIR`: type checking, scope resolution, semantic validation
- `MIR`: optimization passes, control-flow analysis
- `LIR`: instruction selection, register allocation, target-specific lowering
  This follows the design of production compilers (LLVM, GCC) and allows clean separation of concerns.

**Reference:** _Multiple IR Systems_ (Zhang et al., 2024) — Survey of 29 real-world IR systems shows this is the standard approach for serious compilers.

## Structs

### Semantic Model

Structs are value types with a fully known layout at compile time. HIR resolves field names, field types, byte offsets, total size and alignment. Later stages must treat field access as offset-based aggregate access, not as source-level name lookup.

Structs may be nested by value, but circular by-value definitions are rejected because their size cannot be computed.

Field reordering is an internal layout optimization only. Any C-visible struct must preserve declaration order and use the target ABI's padding and alignment rules.

### ABI Model

Struct layout and call lowering must be target-ABI driven. The language semantics say a struct is passed or returned by value; the target decides whether that value is carried in registers, copied on the stack, or passed through an address according to its ABI.

This keeps code generation close to C and leaves a direct path for future C interop. ABI decisions belong in target LIR lowering, not in MIR or in the final assembly emitter.

Current aggregate passing uses address-based copies as a conservative implementation step. The long-term rule is an ABI classifier per target, matching the platform C ABI for layout, argument passing and return values.

## Module System

### Goals

- **Minimal syntax**: simple `use` keyword
- **No circular dependencies**: enforced at compile time
- **File = module**: no need to declare modules as they're file-based

### Semantic Model

Every `.nyx` file is a module identified by its **canonical file-system path**. The `use` keyword binds symbols from other modules into the local scope.

```rust
// namespace import: access via qualified name
use std::io;
// usage: io::println(...)

// item import: directly binding into the scope
use std::io::{println};
// usage: println(...)

// project-relative import
// use my_project::math::{add, subtract};
```

## Interfaces

### Semantic Model

Interfaces define a contract of method signatures. The compiler validates interface compliance at the High-level IR (HIR) boundary.

An implementation block binds methods to a struct under a given interface:

```rust
impl Rectangle with Shape {
    fn area(&self): i64 { ... }
}
```

The semantic analyzer enforces the following rules:

- **Completeness**: Every method declared in the interface must be defined in the `impl` block.
- **Signature Matching**: Method names, parameter types, receiver mutability (`&self` vs `&mut self`), and return types must match exactly.
- **Superinterface Constraints**: Interfaces support single inheritance (`interface B: A`) and multiple composition (`interface C: A + B`). The implementing struct must explicitly implement all superinterfaces in the hierarchy.

### Dispatch Model

Nyx implements **static dispatch** for interface methods. Method resolution occurs during HIR semantic analysis, mapping call sites directly to the mangled implementation function (e.g. `Rectangle__area`).

Dynamic dispatch (vtable-based trait objects) is not supported.

## Generics

### Semantic Model

Generic type parameters parameterise functions, `struct`s, and `enum`s. Interface bounds restrict these parameters to enforce semantic constraints at compile time:

```rust
pub fn assert_eq<A, B>(left: A, right: B) where A: PartialEq<B> { ... }
```

### Compilation Model

Nyx uses static monomorphisation. Generic definitions are stored as template representations in the High-level IR (HIR) and lowered on-demand. When a generic definition is referenced with concrete type arguments, the compiler specialises the function or type, name-mangling the resulting instance (e.g. `Optional$i32`) to produce a distinct monomorphic definition in the Mid-level IR (MIR).

## Integers

An integer literal is lexed as an unsigned 64-bit magnitude; a leading `-` is a separate unary operator, not part of the literal. This mirrors Rust's literal model and keeps the full `u64` range (and `i64::MIN`) representable.

Integer `+`, `-` and `*` panic on overflow in `debug` builds and wrap at the `sane`/`max` optimisation levels. The `wrapping_*` methods are intrinsics exempt from the check, so wrap-around algorithms (hashing, PRNGs) behave identically at every level.
