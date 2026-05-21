# Nyx Roadmap

This document outlines the implementation status and roadmap for Nyx. It includes completed features, active goals, and planned capabilities based on the current state of the codebase.

## Targets

- [ ] x86
  - [x] 64-bit (`x86_64`)
  - [ ] 32-bit (`x86`)
- [x] ARM64 (`aarch64`)
- [ ] RISC-V (`riscv64`)

## Compiler Optimisations

- [ ] Optimisations flags (**requires** definition of each optimisation level scope)
- [ ] If-conversion
- [ ] Constant Folding & Propagation
- [ ] Dead Code Elimination (DCE)
- [ ] Function Inlining
- [ ] Common Subexpression Elimination (CSE)
- [ ] Loop Unrolling
- [ ] Scalar evolution
- [ ] Peephole Optimizations

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
- [ ] References (**requires** safety model design and implementation)
  - [ ] Reference (`&`)
  - [ ] Mutable References (`&mut`)
- [ ] Type definition
  - [ ] Polymorphism (`Interface`)
    - [x] Static dispatch
    - [ ] Dynamic dispatch
    - [x] Interface composition
  - [ ] Composite data declaration (`struct`)
    - [x] Field access and instantiation
    - [ ] Compatibility with `C` memory layout representation (**requires** syntax definition)
    - [x] Methods
      - [x] Reference (`&self`)
      - [x] Mutable reference (`&mut self`)
  - [ ] Enumerables / Tag Union (`enum`)

### Expressions & Operators

- [x] Arithmetic Operators (`+`, `-`, `*`, `/`)
  - [ ] Compound Assignment (`+=`, `-=`, `*=`, `/=`)
  - [ ] Compiler-time panic on overflow ([reference](https://doc.rust-lang.org/core/panicking/panic_const/index.html))
- [x] Comparison Operators (`==`, `!=`, `<`, `>`, `<=`, `>=`)
- [ ] Logical Operators
  - [x] Logical Connectives (`&&`, `||`, `!`)
  - [ ] XOR, NOR, NAND (**requires** syntax definition)
- [ ] Bitwise Operators
  - [ ] Logic (`&`, `|`, `^`, `!`)
  - [ ] Shifts (`>>`, `<<`)
- [x] Variable Assignment (`=`)
- [x] Function Calls
- [ ] Reference Operators
  - [ ] Reference (`&`)
  - [ ] Dereference (`*`) (**requires** safety assurance)

### Control Flow

- [x] `if` / `else` statements
  - [x] Inline return (`if this return that;`)
- [x] `while` loops
- [ ] `for` loops
- [ ] Pattern matching (**requires** syntax definition)

### Variables & Functions

- [ ] Variable declaration
  - [x] Immutable (`let`)
  - [x] Mutable (`let mut`)
  - [ ] Constants (`const`)
- [ ] Function definition
  - [x] Definition (`fn`)
  - [ ] Mutable parameters
  - [ ] Default parameter values (**requires** definition of _default_ interface)
  - [ ] Modifiers
    - [ ] Constant constraint (`const`)
    - [ ] Inlining (`inline`)
- [x] Return statements (`return`)

### Others

- [x] Module system
  - [x] Imports resolver
  - [x] Project (_dir_) compilation
- [ ] Standard Library
  - [ ] I/O
    - [x] Printing to standard out (`println`, `printf`)
  - [ ] Collections (**requires** syntax definition and memory allocator)
    - [ ] Array
    - [ ] Hash table
    - [ ] Set
  - [ ] Networking
    - [ ] TCP
    - [ ] UDP
- [ ] Memory allocator (**requires** definition of memory layout)
- [ ] Error handling (**requires** definition of error handling model)
