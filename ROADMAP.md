# Nyx Roadmap

This document outlines the implementation status and roadmap for Nyx. It includes completed features, active goals, and planned capabilities based on the current state of the codebase.

## Targets

- [ ] x86
  - [x] 64-bit (`x86_64`)
  - [ ] 32-bit (`x86`)
- [ ] ARM64 (`aarch64`)
- [ ] RISC-V (`riscv64`)

## Compiler Optimisations

- [ ] Optimisations flags (**requires** definition of each optimisation level scope)
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
  - [ ] Target-dependent pointer-sized (`iptr`, `uptr`)
- [x] Floating-Point Types (`f32`, `f64`)
- [x] Boolean (`bool`)
- [ ] Strings
  - [ ] `char`
  - [ ] `&str`
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
    - [ ] Static dispatch
  - [ ] Composite data declaration (`struct`)
    - [ ] Field access and instantiation
    - [ ] Compatibility with `C` memory layout representation
    - [ ] Methods
  - [ ] Enumerables / Tag Union (`enum`)

### Expressions & Operators

- [x] Arithmetic Operators (`+`, `-`, `*`, `/`)
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
- [x] `while` loops
- [ ] `for` loops
- [ ] Pattern matching

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

- [ ] Module system
  - [ ] Imports resolver
- [ ] Standard Library
  - [ ] I/O
    - [ ] Printing to standard out (`println`, `printf`)
  - [ ] Collections (**requires** syntax definition and memory allocator)
    - [ ] Array
    - [ ] Hash table
    - [ ] Set
  - [ ] Networking
    - [ ] TCP
    - [ ] UDP
- [ ] Memory allocator (**requires** definition of memory layout)
- [ ] Error handling (**requires** definition of error handling model)
