# Changelog

All notable changes to the Nyx compiler will be documented in this file

## [0.2.0] - 2026-05-22

Added support for bitwise operations, explicit type casting (`as`), constants (`const`), and automated standard library prelude loading.

### Added

- **Bitwise Operations** ([!4]):
  - Support for bitwise And (`&`), Or (`|`), Xor (`^`), Not (`!`), Left Shift (`<<`), and Right Shift (`>>`) on integers and boolean types
  - Context-aware shifting: arithmetic shift right for signed integers and logical shift right for unsigned integers
- **Type Casting** ([!4]):
  - Explicit type casts between primitive types using the `as` operator
  - Semantic validation in the HIR for upcasts, downcasts (truncations), same-size integer conversions, and boolean byte coercion
- **Constants** ([!4]):
  - Parsing and HIR representation for `const` declarations at module level and inside `impl` blocks
- **Prelude & Standard Library** ([!4]):
  - Automatic loading of standard library prelude into scopes
  - Implemented base constants and helper methods (e.g., `abs`, `signum`, `is_power_of_two`) on primitive types in `std/int.nyx` and `std/float.nyx`

### Changed & Refactored

- **Target lowering & compilation** ([!4]):
  - Centralized function and method name mangling in `src/hir/mangle.rs`
  - Implemented static stack frame management and a unified parallel move resolver in LIR lowering
  - Enabled AArch64 qemu integration tests in the CI pipeline

## [0.1.0] - 2026-05-21

Initial release of the Nyx compiler, introducing support for `struct`s, implementation blocks, interfaces, character primitive types, standard library additions, and target-dependent code generation.

### Added

- **Interface System** ([!3]):
  - Support for public and nested interface definitions (`interface Shape`)
  - Static dispatch interface implementation (`impl Struct with Interface`)
  - Interface composition/super-interfaces supporting single and multiple inheritance (`interface A: B + C`)
  - Type-checking and signature validation for method implementations (checking names, receivers, argument lists, and return types)
- **Structures (`struct`)** ([!1]):
  - Support for structure definitions, block-based initialization, and instantiation
  - Compaction and field layout calculation with offset resolution in the HIR
  - Support for nested field access and assignment (e.g., `player.pos.x = 10`)
- **Implementation Blocks** ([!2]):
  - Support for `impl` blocks for custom structures and primitive types
  - Semantic support for `&self` and `&mut self` receivers, verifying mutability rules on receiver calls
  - Orphan rules validation for struct and interface implementations across module boundaries
- **Language Primitives**:
  - Finalised target-dependent pointer-sized integer types (`uptr` and `iptr`) ([!1])
  - Character type (`char`), supporting escape sequences like `\xNN` and `\u{NN}` ([!3])
  - `inline` attribute support for functions and methods, implementing MIR-level inlining context ([!3])
  - Support for inline return statements within `if` branches (`if condition return value;`) ([!3])
- **Standard Library (`std`)**:
  - Implemented standard helper methods for characters (`std/char.nyx`): `is_digit`, `is_alphabetic`, `is_lowercase`, `is_uppercase`, `is_whitespace`, `is_alphanumeric`, and `is_ascii` ([!3])
  - Added compiler-supported intrinsics `size_of` and `align_of` inside `std/mem.nyx` ([!3])
  - Added raw `syscall` intrinsic support for target-dependent system calls ([!3])

[!1]: https://gitlab.com/ruancampello/nyx/-/merge_requests/1
[!2]: https://gitlab.com/ruancampello/nyx/-/merge_requests/2
[!3]: https://gitlab.com/ruancampello/nyx/-/merge_requests/3
[!4]: https://gitlab.com/ruancampello/nyx/-/merge_requests/4

