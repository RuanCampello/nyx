# Nyx 🌑

A modern, strict, and compiled programming language.

## Overview

Nyx is an imperative, statically-typed, compiled language designed for performance and clarity. The compiler implements a multi-pass architecture with distinct intermediate representations before emitting native assembly for `x86_64` and `AArch64`.

> [!NOTE]
> This is primarily a learning project, built while working through books, papers, and online resources on compilers and language design. Things may not always be done in the most optimal way, but the intent is always to improve.

## The Nyx Look

Tagged unions and pattern matching make data-oriented code concise without hiding its control flow.

```rust
struct Point {
    x: i32,
    y: i32,
}

enum Shape {
    Empty,
    At(Point),
}

fn score(shape: Shape): i32 {
    match shape {
        Shape::Empty -> 0,
        Shape::At(Point { x, y: 0 }) -> x,
        Shape::At(Point { x, y }) -> x * x + y * y,
    }
}

fn main(): i32 {
    score(Shape::At(Point { x: 3, y: 4 })) // 25
}
```

<details>
<summary>Interfaces and generic static dispatch</summary>

```rust
interface Sink {
    fn push(&mut self, value: i32);
    fn total(&self): i32;
}

struct Counter {
    value: i32,
}

impl Counter with Sink {
    fn push(&mut self, value: i32) {
        self.value = self.value + value;
    }

    fn total(&self): i32 {
        self.value
    }
}

fn collect<S: Sink>(sink: &mut S): i32 {
    loop value in 1..=4 {
        sink.push(value);
    }
    sink.total()
}

fn main(): i32 {
    let mut counter = Counter { value: 0 };
    collect(&mut counter) // returns 10
}
```

</details>

<details>
<summary>Fixed arrays, slices, and range loops</summary>

```rust
fn sum(values: &[i32]): i32 {
    let mut total = 0;

    loop value in values {
        total = total + value;
    }

    total
}

fn main(): i32 {
    let values: [i32; 5] = [1, 2, 3, 4, 5];
    sum(&values) // arrays coerce to slices
}
```

</details>

<details>
<summary>Compile-time functions</summary>

```rust
const fn fibonacci(n: i32): i32 {
    if n < 2 return n;
    else return fibonacci(n - 1) + fibonacci(n - 2);
}

fn main(): i32 {
    fibonacci(10) // evaluated at compile time
}
```

</details>

<details>
<summary>Raw pointers and explicit unsafe boundaries</summary>

```rust
@unsafe
fn increment(value: *mut i32) {
    *value = *value + 1;
}

fn main(): i32 {
    let mut value: i32 = 41;

    @unsafe {
        increment(&mut value);
    }

    value // returns 42
}
```

</details>

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
- **No Windows support.** Nyx targets Linux (and eventually other Unix-like systems). Windows is not on the roadmap.
- **Not a replacement for everything.** Nyx is not trying to be `C++`, `Rust`, or `Zig`. If you need their feature sets, use them.

## Current Status

Nyx is currently in early development. However, the core compiler pipeline, from the lexer and parser to semantic analysis, register allocation, and native code generation for `x86_64` and `aarch64`, is functional.

For a detailed breakdown of completed features and active development goals, please see the [ROADMAP](ROADMAP.md).

## Language Server

Nyx ships an LSP server (`lsp`) that brings diagnostics, hover types, inlay hints, semantic-token highlighting and document symbols to your editor.

See [`lsp/README.md`](lsp/README.md) for build and editor setup, currently documented for **Neovim** and **Vim**.

> [!NOTE]
> For a simplier highlighting, check the [vim](nyx.vim) syntax rules.

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

- [GCC Optimisation Options](https://gcc.gnu.org/onlinedocs/gcc/Optimize-Options.html) - Documentation on optimisation levels and flags used in `gcc` compilers
- [The Hare Programming Language](https://harelang.org/) - A clean, simple, and statically-typed systems language that served design inspiration
- [rustc IR guide](https://rustc-dev-guide.rust-lang.org/part-3-intro.html) - Documentation on different levels to represent a rust source code before code generation
- [rustc architecture overview](https://rustc-dev-guide.rust-lang.org/overview.html) - Overhaul processing of rust compiler pipeline
- [rustc niche optimisations](https://www.0xatticus.com/posts/understanding_rust_niche/) - Blog post on specific optimisations of some rust's types
- [rust ranges](https://kaylynn.gay/blog/post/rust_ranges_and_suffering) - Blog post critique over rust `Range` type take in consideration when implementing Nyx's ranges

### Algorithms

Foundational papers for algorithms implemented or used as research in Nyx

- [Register Allocation and Spilling via Graph Coloring](https://dl.acm.org/doi/epdf/10.1145/872726.806984) - Gregory J. Chaitin's seminal 1982 paper detailing the graph-colouring approach to register allocation and spilling

## License

Nyx is released under the **GNU AGPL v3**. See `LICENSE.md` for details.
