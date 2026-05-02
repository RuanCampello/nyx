# Nyx Design Decision

This document is to discuss the key design decision taken when implementing the `nyx` compiler and defining the language syntax, design and semantics.

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

item import: directly binding into the scope
use std::io::{println};
// usage: println(...)

project-relative import
use my_project::math::{add, subtract};
```
