# Changelog

All notable changes to the Nyx compiler will be documented in this file

## [0.5.0] - 2026-07-01

Added fixed-size arrays (`[T; N]`) and borrowed slices (`&[T]` / `&mut [T]`) with bounds-checked indexing, element assignment, and array→slice coercion threaded from the parser through `x86_64`/`aarch64` codegen, alongside function-scoped `const` declarations evaluated at compile time.

### Language Server (`nyx-lsp` 0.2.1)

- Hover and inlay hints now render array and slice types ([!10])
- Corrected inlay-hint placement for array bindings ([!10])

### Added

- **Arrays & Slices** ([!10]):
  - Array and repeat literals (`[a, b, c]`, `[0; N]`), `[T; N]` and `&[T]` / `&mut [T]` type syntax, and indexing / index-assignment expressions
  - HIR representation, type resolution, layout computation and array interning, array→slice coercion, and `impl [T]` method blocks
  - MIR `ElementLoad`, `ElementStore`, and `ElementAddr` instructions for indexing, storing, and yielding a pointer instead of loading
  - LIR lowering and emission of the element instructions for both `x86_64` and `aarch64`
- **Bounds Checking** ([!10]):
  - `BoundsCheck` instruction that panics on out-of-range access, wired for both targets, with panic constants folded into a `Panic` enum
- **Function-Scoped Constants** ([!10]):
  - `const` declarations inside function bodies, evaluated at compile time and spliced at use sites so they occupy no stack slot
  - references to an enclosing local report a `NonConstValue` error (matching `rustc`'s E0435), and body constants lexically shadow module and impl constants
- **Type Inference** ([!10]):
  - an `infer` type that lazily resolves the more ambiguous operations, such as array `len`

### Changed & Refactored

- **Parser** ([!10]): deduplicated parsing paths behind helpers, including unsigned-literal parsing
- **MIR** ([!10]): removed a duplicated lowering pattern and added an `eq` branch helper
- **Register Allocation** ([!10]): dropped the call-crossed over-approximation and the redundant `degree` tracking
- **Standard Library** ([!10]): slices are included in the prelude, removing the explicit import

## [0.4.1] - 2026-06-14

Added `///` documentation comments across the language, removing the old `/* ... */` block-comment syntax, and surfaced them through `nyx-lsp` hover. Alongside that, enums now pick the smallest tag representation that fits their variants, and a HIR/LIR rearchitecture drops the duplicate MIR layout engine and unifies `aarch64`/`x86_64` target lowering.

### Language Server (`nyx-lsp` 0.2.0)

- Doc comments rendered in hover for functions, `struct`s, `enum`s, interfaces and their members, with integration tests covering the new output ([`918a0b9`])

### Added

- **Doc Comments**:
  - `///` doc comment syntax lexed as dedicated tokens, replacing `/* ... */` block comments ([`fcd1581`])
  - Doc comments on declarations and `enum` variants stored in HIR side-tables ([`12130f4`], [`edc153a`])
  - `analysis` module support for querying doc comments ([`aec412e`])
- **Standard Library**: documentation for `str` methods, `exit`, and interfaces ([`113d93f`], [`e5b93d5`])

### Changed & Refactored

- **Layout**: nominal layouts (size, align, field offsets) are now cached directly on the HIR, removing the duplicate MIR `LayoutEngine` ([`f3621f4`])
- **HIR**:
  - Collapsed all item `Statement` variants into `Statement::Item`, so docs and other metadata are declared once ([`0374e1c`])
  - `ModuleLoader::load` now takes `self` by value instead of `&self`, removing module cloning ([`02441ff`], [`edc153a`])
  - Module declarations are partitioned once per compilation instead of on every use ([`3e8d47a`])
  - The lowering `Scope` is now created after module graph construction ([`3875461`])
  - Removed an old arena artifact ([`4ea597e`])
- **LIR**: unified `aarch64` and `x86_64` target lowering into a single generic `Lower<'f, T>` ([`6f23c1f`])

### Fixed

- **LIR**: scalar spill slots are now aligned to their byte width ([`fddabd6`])
- **Enums**: tag representation now uses the smallest integer type that fits the variant count ([`12130f4`])

## [0.4.0] - 2026-06-13

Shipped the first release of `nyx-lsp`, a dedicated language server, on top of a diagnostics rearchitecture that swaps per-file offsets for a global `SourceMap`, renders rich multi-file reports, and makes the frontend recoverable so a single pass surfaces several errors instead of bailing on the first.

### Language Server (`nyx-lsp` 0.1.0)

- First release of the Nyx language server as its own crate, with debounced analysis, a spinner while the project and `std` load and diagnostics surfaced live to the editor ([`d763e04`])
- **Hover** information for `struct`s, `enum`s and functions, including fields, variants and full signatures with highlighting ([`b58cee7`]), the size and alignment of local variables ([`e38f6c6`]), and evaluation of `const`s resolved through generics ([`f779f1e`])
- **Semantic tokens** for syntactic highlighting that holds up even on a buffer that does not yet parse, with consistent highlighting of `enum` variants and parameters ([`08509b8`], [`f779f1e`])
- **Inlay hints** for inferred types after `let` bindings, kept stable while editing, plus document symbols and basic go-to-definition ([`08509b8`])
- An in-process integration harness that boots a real server for end-to-end tests ([`ec57665`])

### Added

- **Diagnostics & Error Recovery**:
  - `RichDiagnostic` rendering of multi-file reports through the `SourceMap` ([`485ae09`]), plus a `rich()` plain renderer ([`5e96502`]) and raw messages for editor surfaces ([`fdd1a9b`])
  - Poison and sink foundation for error handling ([`6814bbd`]), making the frontend recoverable so a single pass can report multiple errors instead of stopping at the first ([`03de488`])
  - An `analysis` module at the crate root, extracting the editor query layer out of the HIR so the compiler and the language server share it ([`a05a044`])

### Changed & Refactored

- **Spans & `SourceMap`**:
  - Introduced a `SourceMap` and a compact 8-byte global `Span` ([`e7b3988`]), threading global byte offsets through the lexer and HIR and registering every file in the map ([`565e6f2`])
  - Reworked the `nyx_macros` crate with formatting helpers and an expanded `#[derive(Diagnostic)]` ([`5e96502`], [`8979c8e`])
- **Symbol Tables & Cleanup**:
  - Removed the dumped `symbols` vector across the HIR, MIR and LIR in favour of direct `SymbolTable` usage ([`64115c2`], [`6db2774`], [`763d4e8`]), and added a `Deref` impl for `IndexVec` ([`6cfb600`])
  - Richer parser error information with an optional `HasSpan` ([`ff2880d`])

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
[!10]: https://gitlab.com/ruancampello/nyx/-/merge_requests/10
[`d763e04`]: https://gitlab.com/ruancampello/nyx/-/commit/d763e04
[`b58cee7`]: https://gitlab.com/ruancampello/nyx/-/commit/b58cee7
[`e38f6c6`]: https://gitlab.com/ruancampello/nyx/-/commit/e38f6c6
[`f779f1e`]: https://gitlab.com/ruancampello/nyx/-/commit/f779f1e
[`08509b8`]: https://gitlab.com/ruancampello/nyx/-/commit/08509b8
[`ec57665`]: https://gitlab.com/ruancampello/nyx/-/commit/ec57665
[`485ae09`]: https://gitlab.com/ruancampello/nyx/-/commit/485ae09
[`5e96502`]: https://gitlab.com/ruancampello/nyx/-/commit/5e96502
[`fdd1a9b`]: https://gitlab.com/ruancampello/nyx/-/commit/fdd1a9b
[`6814bbd`]: https://gitlab.com/ruancampello/nyx/-/commit/6814bbd
[`03de488`]: https://gitlab.com/ruancampello/nyx/-/commit/03de488
[`a05a044`]: https://gitlab.com/ruancampello/nyx/-/commit/a05a044
[`e7b3988`]: https://gitlab.com/ruancampello/nyx/-/commit/e7b3988
[`565e6f2`]: https://gitlab.com/ruancampello/nyx/-/commit/565e6f2
[`8979c8e`]: https://gitlab.com/ruancampello/nyx/-/commit/8979c8e
[`64115c2`]: https://gitlab.com/ruancampello/nyx/-/commit/64115c2
[`6db2774`]: https://gitlab.com/ruancampello/nyx/-/commit/6db2774
[`763d4e8`]: https://gitlab.com/ruancampello/nyx/-/commit/763d4e8
[`6cfb600`]: https://gitlab.com/ruancampello/nyx/-/commit/6cfb600
[`ff2880d`]: https://gitlab.com/ruancampello/nyx/-/commit/ff2880d
[`918a0b9`]: https://gitlab.com/ruancampello/nyx/-/commit/918a0b9c670da62fac069c5b4d359440cc9d9ebd
[`fcd1581`]: https://gitlab.com/ruancampello/nyx/-/commit/fcd158104c535610cc3f24d87abf0bda753becda
[`12130f4`]: https://gitlab.com/ruancampello/nyx/-/commit/12130f4b244d26f16eea338712f092f995360e9e
[`edc153a`]: https://gitlab.com/ruancampello/nyx/-/commit/edc153a4f613d9c3aa43cb39dd0dd9c6ed93308c
[`aec412e`]: https://gitlab.com/ruancampello/nyx/-/commit/aec412e2558957fac99b2744200a4f87b8cbf970
[`04828b1`]: https://gitlab.com/ruancampello/nyx/-/commit/04828b1fe9685d53aab848f338452cb1e974bcb2
[`113d93f`]: https://gitlab.com/ruancampello/nyx/-/commit/113d93f23f0334f4d7b28057d720ae847c038f61
[`e5b93d5`]: https://gitlab.com/ruancampello/nyx/-/commit/e5b93d5b8e7d7944ba4d112af12162c5501c55e2
[`f3621f4`]: https://gitlab.com/ruancampello/nyx/-/commit/f3621f487d9b2ec9bfddefd940bf16f18a926395
[`0374e1c`]: https://gitlab.com/ruancampello/nyx/-/commit/0374e1c7e1555545f908cae9445561a5ab3834f3
[`02441ff`]: https://gitlab.com/ruancampello/nyx/-/commit/02441ff2f701d341c7e41a4f057d2b423f318f05
[`3e8d47a`]: https://gitlab.com/ruancampello/nyx/-/commit/3e8d47a3a8824fc7578e979586d8b61aa889ddae
[`3875461`]: https://gitlab.com/ruancampello/nyx/-/commit/3875461f9f4ce3c855640782037a4c82bcc19cff
[`4ea597e`]: https://gitlab.com/ruancampello/nyx/-/commit/4ea597eef1a829d6f880fc703a195a98e061652d
[`6f23c1f`]: https://gitlab.com/ruancampello/nyx/-/commit/6f23c1f31953d1934167bdc0c8b07b372cf3af47
[`fddabd6`]: https://gitlab.com/ruancampello/nyx/-/commit/fddabd61948404a5a06415f2570c81d634b3242e
