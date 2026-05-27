# Nyx 🌑

A modern, strict, and compiled programming language.

## Overview

Nyx is an imperative, statically-typed, compiled language designed for performance and clarity. The compiler implements a multi-pass architecture with distinct intermediate representations before emitting native assembly for `x86_64` and `AArch64`.

> [!NOTE]
> This is primarily a learning project, built while working through books, papers, and online resources on compilers and language design. Things may not always be done in the most optimal way, but the intent is always to improve.

## The Nyx Look

```rust
fn factorial(n: i32): i32 {
    let mut result = 1;
    let mut i = 2;

    while i <= n {
        result = result * i;
        i = i + 1;
    }

    result
}

fn main(): i32 {
    factorial(5)  // returns 120
}
```

## Design Goals and Non-Goals

The goal is to keep Nyx focused. It should feel like a language you can understand end-to-end, not a sprawling ecosystem that grew beyond its own ambitions.

**What we're building toward:**

- A complete standard library with collections, I/O, and higher-level utilities like `HTTP`.
- An ownership and borrowing memory model, adapted to stay as simple as possible in practice.
- Interface-based polymorphism via dynamic dispatch (static dispatch and interface composition are already implemented).
- C ABI compatibility, so interoperating with C code stays practical.

**What we're not building:**

- **No object-oriented programming.** No inheritance, no class hierarchies. Structs and their method implementations are the model.
- **No garbage collector.** Memory is managed through ownership. If you want a GC, use a language designed around one.
- **No generics or templates** — not planned for the foreseeable future.
- **No Windows support.** Nyx targets Linux (and eventually other Unix-like systems). Windows is not on the roadmap.
- **Not a replacement for everything.** Nyx is not trying to be `C++`, `Rust`, or `Zig`. If you need their feature sets, use them.

## Current Status

Nyx is currently in early development. However, the core compiler pipeline, from the lexer and parser to semantic analysis, register allocation, and native code generation for `x86_64` and `aarch64`, is functional.

For a detailed breakdown of completed features and active development goals, please see the [ROADMAP](ROADMAP.md).

## Philosophy

- **Strict by Design**: Minimal implicit behaviour, maximizing type safety.
- **Modern Syntax**: Clean, readable and _subjectively_ aesthetic syntax.
- **Compiled Performance**: Direct compilation for high-performance execution.
- **Simplicity**: Build without unnecessary complexity focused on learning.

## References

### x64 / ARM64 Codegen & ABI

Resources for native code generation, register usage, and platform calling conventions.

- [x86/x64 Instruction Reference](https://www.felixcloutier.com/x86/) - Complete, searchable reference for x86 and x64 instructions
- [Exercism: x86-64 Floating Point Numbers](https://exercism.org/tracks/x86-64-assembly/concepts/floating-point-numbers) - Tutorial on floating-point concepts, registers, and SSE instructions
- [System V Application Binary Interface (AMD64)](https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf) - Official ABI specification for x86_64 Linux, defining calling conventions, stack layout, and register classification
- [Linux Syscall Table (x86_64)](https://filippo.io/linux-syscall-table/) - Quick reference list of x86_64 system call numbers and register arguments

### Compiler Design & Language Reference

Inspiration and architectural references for the Nyx compiler and standard library

- [GCC Optimisation Options](https://gcc.gnu.org/onlinedocs/gcc/Optimize-Options.html) - Documentation on optimisation levels and flags used in modern compilers
- [The Hare Programming Language](https://harelang.org/) - A clean, simple, and statically-typed systems language that served design inspiration

### Algorithms

Foundational papers for algorithms implemented or used as research in Nyx

- [Register Allocation and Spilling via Graph Coloring](https://dl.acm.org/doi/epdf/10.1145/872726.806984) - Gregory J. Chaitin's seminal 1982 paper detailing the graph-colouring approach to register allocation and spilling

## License

Nyx is released under the **GNU AGPL v3**. See `LICENSE.md` for details.
