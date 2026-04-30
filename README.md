# Nyx 🌑

A modern, strict, compiled programming language.

## Overview

Nyx is designed for performance without sacrificing safety and clarity. It is a strictly typed language that focuses on catching errors early through a robust, uncompromising compiler.

> [!NOTE]
> This is primarily a learning project, built while working through books, papers, and online resources on compilers and language design. Things may not always be done in the most optimal way, but the intent is always to improve.

## The Nyx Look

```rust
fn add(a: i32, b: i32): i32 {
  a + b
}

fn main() {
  let x = 10;
  let mut y = 20;

  let z = add(x, y);
}
```

## Design Goals and Non-Goals

The goal is to keep Nyx focused. It should feel like a language you can understand end-to-end, not a sprawling ecosystem that grew beyond its own ambitions.

**What we're building toward:**

- A complete standard library with collections, I/O, and higher-level utilities like `HTTP`.
- An ownership and borrowing memory model, adapted to stay as simple as possible in practice.
- Structs with method implementations and some form of interface-based polymorphism.
- `x86_64` as the primary target, with `ARM` on the horizon.
- C ABI compatibility, so interoperating with C code stays practical.

**What we're not building:**

- **No object-oriented programming.** No inheritance, no class hierarchies. Structs and their method implementations are the model.
- **No garbage collector.** Memory is managed through ownership. If you want a GC, use a language designed around one.
- **No generics or templates** — not planned for the foreseeable future.
- **No Windows support.** Nyx targets Linux (and eventually other Unix-like systems). Windows is not on the roadmap.
- **Not a replacement for everything.** Nyx is not trying to be `C++`, `Rust`, or `Zig`. If you need their feature sets, use them.

## Current Status

Nyx is currently in early development. The core compiler pipeline, from the lexer and parser to semantic analysis, register allocation, and `x86_64` code generation, is functional.

For a detailed breakdown of completed features and active development goals, please see the [ROADMAP](ROADMAP.md).

## Philosophy

- **Strict by Design**: Minimal implicit behaviour, maximizing type safety.
- **Modern Syntax**: Clean, readable and _subjectively_ aesthetic syntax.
- **Compiled Performance**: Direct compilation for high-performance execution.
- **Simplicity**: Build without unnecessary complexity focused on learning.

## References

### x64

- <https://www.felixcloutier.com/x86/>
- <https://exercism.org/tracks/x86-64-assembly/concepts/floating-point-numbers>
- <https://refspecs.linuxbase.org/elf/x86_64-abi-0.99.pdf>
- <https://filippo.io/linux-syscall-table/>

### other compilers & backends

- <https://gcc.gnu.org/onlinedocs/gcc/Optimize-Options.html>
- <https://harelang.org/>

### algorithms

- <https://dl.acm.org/doi/epdf/10.1145/872726.806984>

## License

Nyx is released under the **GNU AGPL v3**. See `LICENSE.md` for details.
