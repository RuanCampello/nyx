# Nyx 🌑

A modern, strict, compiled programming language.

## Overview

Nyx is designed for performance without sacrificing safety and clarity. It is a strictly typed language that focuses on catching errors early through a robust, uncompromising compiler.

## Philosophy

- **Strict by Design**: Minimal implicit behaviour, maximizing type safety.
- **Modern Syntax**: Clean, readable and _subjectively_ aesthetic syntax.
- **Compiled Performance**: Direct compilation for high-performance execution.
- **Simplicity**: Build without unnecessary complexity focused on learning.

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

## Current Status

Nyx is currently in early development.

- [x] Lexical Analyzer
- [x] Parser
- [x] Semantic Analyzer
- [x] Intermediate Representation
- [x] Code Generation (x86_64)

## License

Nyx is released under the **GNU AGPL v3**. See `LICENSE.md` for details.
