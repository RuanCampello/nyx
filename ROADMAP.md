# Nyx Roadmap

This document outlines the implementation status and roadmap for Nyx. It includes completed features, active goals, and planned capabilities based on the current state of the codebase.

## Targets

- [ ] x86
  - [x] 64-bit (`x86_64`)
  - [ ] 32-bit (`x86`)
- [x] ARM64 (`aarch64`)
- [ ] RISC-V (`riscv64`)

## Compiler Optimisations

- [x] Optimisations flags (**requires** definition of each optimisation level scope)
- [ ] If-conversion
- [ ] Constant Folding & Propagation
- [ ] Dead Code Elimination (DCE)
- [x] Function Inlining
  - [ ] Heuristics to or not-to inline a function
- [ ] Common Subexpression Elimination (CSE)
- [ ] Loop Unrolling
- [ ] Scalar evolution
- [ ] Peephole Optimisations

## Language Features

### Data Types

- [x] Integer Types
  - [x] Signed (`i8`, `i16`, `i32`, `i64`)
  - [x] Unsigned (`u8`, `u16`, `u32`, `u64`)
  - [x] Target-dependent pointer-sized (`iptr`, `uptr`)
- [x] Floating-Point Types (`f32`, `f64`)
- [x] Boolean (`bool`)
- [ ] Strings
  - [x] `char`
  - [x] `&str`
  - [ ] `String` (**requires** memory allocator implementation)
- [ ] Fixed-size arrays (`[T; N]`)
  - [ ] Compiler-time bound checking
- [ ] Pointers (**requires** _unsafe_ blocks implementation)
  - [ ] Raw pointer
  - [ ] Raw pointer dereference
- [x] References
  - [x] Reference (`&`)
  - [x] Mutable References (`&mut`)
- [ ] Type definition
  - [x] Generics with monomorphisation (`<T>`)
    - [x] Interface bounds / constraints (`where` clause)
  - [ ] Polymorphism (`Interface`)
    - [x] Static dispatch
    - [ ] Dynamic dispatch
    - [x] Interface composition
  - [ ] Composite data declaration (`struct`)
    - [x] Field access and instantiation
    - [ ] Compatibility with `C` memory layout representation (extern, packed, align)
    - [x] Methods
      - [x] Reference (`&self`)
      - [x] Mutable reference (`&mut self`)
  - [ ] Enumerables / Tag Union
    - [x] Sum enumerable (`enum`)
    - [x] Payload-carrying variants (tagged union)
    - [ ] C-like `union` representation

### Expressions & Operators

- [x] Arithmetic Operators (`+`, `-`, `*`, `/`)
  - [ ] Compound Assignment (`+=`, `-=`, `*=`, `/=`)
  - [x] Compiler-time panic on overflow ([reference](https://doc.rust-lang.org/core/panicking/panic_const/index.html))
- [x] Comparison Operators (`==`, `!=`, `<`, `>`, `<=`, `>=`)
- [ ] Logical Operators
  - [x] Logical Connectives (`&&`, `||`, `!`)
- [x] Bitwise Operators
  - [x] Logic (`&`, `|`, `^`, `!`)
  - [x] Shifts (`>>`, `<<`)
- [x] Variable Assignment (`=`)
- [x] Function Calls
- [x] Reference Operators
  - [x] Reference (`&`)
  - [x] Dereference (`*`)

### Control Flow

- [x] `if` / `else` statements
  - [x] Inline return (`if this return that;`)
- [x] `while` loops
- [ ] `for` loops
- [x] Pattern matching

### Variables & Functions

- [ ] Variable declaration
  - [x] Immutable (`let`)
  - [x] Mutable (`let mut`)
  - [x] Constants (`const`)
    - [x] File scoped constants
    - [ ] Associated constants
      - [x] `struct`/primitive associated
      - [ ] `interface` associated
- [ ] Function definition
  - [x] Definition (`fn`)
  - [x] Mutable parameters
  - [ ] Default parameter values (**requires** definition of _default_ interface)
  - [ ] Modifiers
    - [x] Constant constraint (`const`)
    - [x] Inlining (`inline`)
- [x] Return statements (`return`)

### Language Server (LSP)

- [ ] Hover type information
  - [x] Module path & truncated declaration
  - [x] Size & alignment for `struct`/`enum`
  - [x] Method signatures with implementor
  - [x] Constants with their evaluated value
  - [x] Inside generic template bodies (identity instances)
  - [ ] Comment documentation
- [ ] Go-to-definition
  - [x] Variables
  - [x] Functions
  - [ ] Interfaces
    - [ ] Methods
  - [ ] `struct` and `enum`
- [x] Document symbols outline
- [x] Inlay hints (type inference visualisation)
- [x] Semantic tokens (syntactic highlighting)
- [ ] Auto-completion
- [ ] Rename symbol
- [ ] Code actions / quickfixes (the 💡 fix suggestions, e.g. "use `const` instead of top-level `let`")
- [x] Resilient features on invalid code (partial HIR + `ContentModified` versioning)
- [ ] Compiler Diagnostics
  - [x] Procedural-macro derived diagnostics (`#[derive(Diagnostic)]`)
  - [x] Raw error message extraction
  - [x] Structured diagnostics across the crate boundary (`RichDiagnostic`, plain + ANSI)
  - [x] Per-error diagnostic `code` (kebab-case error kind, shown as `nyx (kind)`)
  - [ ] Uniform error code numeration (stable `Exxxx` numbers + `code_description` docs URLs)
  - [ ] Multi-error accumulation (report every error in a pass)
    - [x] LSP analysis (body, signature, and item level)
    - [x] Whole-project analysis (uncalled functions included)
    - [ ] CLI `compile`/`compile_project`
  - [ ] Frontend error recovery
    - [x] `TypeKind::Error` poison + `ErrorGuaranteed` + diagnostics sink
    - [x] Opt-in recovery across lowering and collection
    - [ ] Parser sentinel nodes (`Expression::Error`/`Statement::Error`)
    - [ ] Per-function taint + MIR skip for poisoned functions
- [x] Integration test harness (in-process server, end-to-end JSON-RPC suite)

### Others

- [x] Module system
  - [x] Imports resolver
  - [x] Project (_dir_) compilation
- [ ] Standard Library
  - [ ] I/O
    - [ ] Console formatting & printing (`print`/`println`)
      - [ ] Better formatting (padding, alignment)
      - [ ] Interpolation of non-immediate values
    - [ ] Keyboard input reading
    - [ ] File system
  - [ ] Core interfaces
    - [x] Equality comparison (`cmp`)
    - [x] Default value initialisation (`default`)
    - [x] Cloning (`clone`)
  - [ ] Primitive helpers
    - [ ] Integers
      - [x] Integer constants & basic properties
      - [ ] Checked/wrapping integer arithmetic
    - [ ] Floating-point
      - [x] Floating-point constants
      - [ ] Floating-point mathematics (abs, floor, ceil, power, trigonometry)
    - [x] Character classification & ASCII conversions
    - [x] String length querying
    - [ ] String slicing, manipulation & search
  - [ ] Memory queries
    - [x] Size & alignment (`std/mem.nyx`)
  - [ ] System utilities
    - [x] Process exit execution
    - [x] Better assertions (with values that `impl PartialEq`)
  - [ ] Collections (**requires** syntax definition and memory allocator)
    - [ ] Array
    - [ ] Hash table
    - [ ] Set
  - [ ] Networking
    - [ ] TCP
    - [ ] UDP
- [ ] Memory allocator (**requires** definition of memory layout)
- [ ] Error handling (**requires** definition of error handling model)
- [ ] Panic handling
  - [x] Panicking primitives
  - [ ] Panicking unwinder
