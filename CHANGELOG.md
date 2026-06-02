# Changelog

All notable changes to the Nyx compiler will be documented in this file

## [0.3.0] - 2026-06-02

Added tagged-union `enum`s with pattern matching, generic types and functions with on-demand monomorphisation, the `Self` type and default interface methods, integer overflow checks, and a procedural-macro diagnostics system, alongside a large rearchitecture that moves layout into the MIR and types into side-tables.

### Added

- **Tagged-Union Enums & Pattern Matching** ([!8]):
  - Payload-carrying `enum`s, parsed and lowered through the HIR, plus the `never` type for diverging arms (`panic`, `assert`)
  - `match` statements with literal, variant, binding and wildcard patterns, `A | B` or-patterns, and single patterns with `if` guards
  - MIR and codegen for tag/payload lowering and match emission
- **Generics & Monomorphisation** ([!8]):
  - Generic `struct`s, `enum`s, free functions, and `impl` methods, resolved via turbofish or inference
  - On-demand monomorphisation deferred to the MIR through parked scope templates and per-call type side-tables, each specialisation is emitted once via name mangling (`Box$i32`)
- **`Self` Type & Default Interface Methods** ([!5]):
  - `Self` and `&Self` as valid annotations inside `impl` blocks and interface signatures
  - Default method bodies on interfaces, injected into `impl` blocks that don't override them
- **Comparison, Equality & Cloning** ([!5], [!8]):
  - `PartialEq` and `Default` interfaces with implementations for the integer primitives, `char`, `uptr`, and `iptr` ([!5])
  - `Clone` interface and a `Copy` marker, plus `assert_eq` generic over `PartialEq` ([!8])
  - MIR resolution of operator overloads (e.g. `PartialEq::eq` behind `==`) ([!8])
- **Integer Overflow Checks & Optimisation Levels** ([!7]):
  - Integer `+`, `-`, `*` panic on overflow in `debug` builds and wrap silently at higher levels
  - `optimisation` module with `Debug`, `Sane`, and `Max` levels, exposed via `--opt` on `build` and `run`
- **Standard Library**:
  - `std/optional.nyx`, `std/result.nyx`, `std/clone.nyx`, and `std/str.nyx` ([!8])
  - `std/panic.nyx` with `panic`/`assert` ([!7])
  - `std/cmp.nyx`, `std/default.nyx`, extended `std/char.nyx` and `iptr`/`uptr` impls ([!5])

### Changed & Refactored

- **HIR → MIR Rearchitecture** ([!8]):
  - Moved the layout engine (size/align/field offsets) out of the HIR into the MIR, replacing `FieldAccess`/`FieldAssign` with a unified `Place` abstraction
  - HIR expressions now live in a bump arena and `ExpressionKind` is `Copy`, expression types moved into `TypeckResults` side-tables keyed by `ExprId`, based on `rustc.`
  - Added an `IndexVec` for typed indices and split the HIR into focused modules (`types`, `constants`, `interfaces`, `structs`, `type_resolver`, `mono`)
- **Type System** ([!8]):
  - Removed `RefTargetKind`, folding it onto `TypeKind` via the shared layout, and collapsed every type-name printer (`Display`, mangling, parser keywords) onto a single source of truth
- **Diagnostics** ([!6], [!8]):
  - Added the `nyx_macros` crate with a `#[derive(Diagnostic)]` procedural macro, replacing manual diagnostic implementations across the lexer, parser, HIR, and module system ([!6])
  - `HirError` now borrows `&str` and derives `Copy`, dropping owned `String`s and the `into_other` shuffling ([!8])
- **Module System & Reachability** ([!5]):
  - Split `src/hir/module.rs` into focused submodules and added demand-driven loading so unreachable (and dead standard-library) functions are no longer lowered
  - Added an AST `Visitor` trait, replacing ad-hoc traversals in constant resolution and reachability analysis
- **Tooling** ([!5], [!6]):
  - `benches/parsing.rs` criterion suite over std compilation and the integration files ([!5])
  - Stabilised `rustfmt` configuration for deterministic formatting ([!6])

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
[!5]: https://gitlab.com/ruancampello/nyx/-/merge_requests/5
[!6]: https://gitlab.com/ruancampello/nyx/-/merge_requests/6
[!7]: https://gitlab.com/ruancampello/nyx/-/merge_requests/7
[!8]: https://gitlab.com/ruancampello/nyx/-/merge_requests/8
