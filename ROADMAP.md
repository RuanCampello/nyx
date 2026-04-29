# Nyx Roadmap

This document outlines the implementation status and roadmap for Nyx. It includes completed features, active goals, and planned capabilities based on the current state of the codebase.

## Targets

- [ ] x86
  - [x] 64-bit (`x86_64`)
  - [ ] 32-bit (`x86`)
- [ ] ARM64 (`aarch64`)
- [ ] RISC-V (`riscv64`)

## Compiler Optimizations

- [ ] Constant Folding & Propagation
- [ ] Dead Code Elimination (DCE)
- [ ] Function Inlining
- [ ] Common Subexpression Elimination (CSE)
- [ ] Loop Unrolling
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
- [ ] Pointers (**requires** _unsafe_ blocks implementation)
  - [ ] Raw pointer
  - [ ] Raw pointer dereference
- [ ] References (**requires** safety model design and implementation)
  - [ ] Reference (`&`)
  - [ ] Mutable References (`&mut`)
- [ ] Type definition
  - [ ] Polymorphism (`Interface`)
  - [ ] Composite data declaration (`struct`)
    - [ ] Compatibility with `C` memory layout representation
    - [ ] Methods
  - [ ] Enumerables / Tag Union (`enum`)

### Expressions & Operators

- [x] Arithmetic Operators (`+`, `-`, `*`, `/`)
- [x] Comparison Operators (`==`, `!=`, `<`, `>`, `<=`, `>=`)
- [ ] Logical Operators
  - [x] Logical Connectives (`&&`, `||`, `!`)
  - [ ] XOR, NOR, NAND (**requires** syntax definition)
- [ ] Binary Operators
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
  - [ ] Modifiers
    - [ ] Constant constraint (`const`)
    - [ ] Inlining (`inline`)
- [x] Return statements (`return`)

### Others

- [ ] Standard Library
  - [ ] I/O
    - [ ] Printing to standard out (`println`, `printf`)
  - [ ] Collections (**requires** syntax definition and memory allocator)
    - [ ] Array
    - [ ] Hash table
    - [ ] Set

