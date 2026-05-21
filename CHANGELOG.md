# Changelog

All notable changes to the Nyx compiler will be documented in this file

## [0.1.0] - 2026-05-21

Initial release of the Nyx compiler, introducing support for `struct`s, implementation blocks, interfaces, character primitive types, standard library additions, and target-dependent code generation

### Added

- **Interface System**:
  - Support for public and nested interface definitions (`interface Shape`)
  - Static dispatch interface implementation (`impl Struct with Interface`)
  - Interface composition/super-interfaces supporting single and multiple inheritance (`interface A: B + C`)
  - Type-checking and signature validation for method implementations (checking names, receivers, argument lists, and return types)
- **Structures (`struct`)**:
  - Support for structure definitions, block-based initialization, and instantiation
  - Compaction and field layout calculation with offset resolution in the HIR
  - Support for nested field access and assignment (e.g., `player.pos.x = 10`)
- **Implementation Blocks**:
  - Support for `impl` blocks for custom structures and primitive types
  - Semantic support for `&self` and `&mut self` receivers, verifying mutability rules on receiver calls
  - Orphan rules validation for struct and interface implementations across module boundaries
- **Language Primitives**:
  - Finalised target-dependent pointer-sized integer types (`uptr` and `iptr`)
  - Character type (`char`), supporting escape sequences like `\xNN` and `\u{NN}`
  - `inline` attribute support for functions and methods, implementing MIR-level inlining context
  - Support for inline return statements within `if` branches (`if condition return value;`)
- **Standard Library (`std`)**:
  - Implemented standard helper methods for characters (`std/char.nyx`): `is_digit`, `is_alphabetic`, `is_lowercase`, `is_uppercase`, `is_whitespace`, `is_alphanumeric`, and `is_ascii`
  - Added compiler-supported intrinsics `size_of` and `align_of` inside `std/mem.nyx`
  - Added raw `syscall` intrinsic support for target-dependent system calls
