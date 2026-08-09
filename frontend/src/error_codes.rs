//! The central registry of error codes
//!
//! Every diagnostic the compiler can emit carries exactly one [ErrorCode].
//! The `#[diagnostic(code = "...")]` attribute on error enums resolves to a
//! variant of this enum at compile time, so a code that does not exist here
//! does not build. Each code carries a title and a long-form explanation with
//! an erroneous example.

macro_rules! error_codes {
    ($($variant:ident => $title:literal, $explanation:literal;)*) => {
        /// A stable identifier for one kind of compiler error
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum ErrorCode {
            $($variant,)*
        }

        impl ErrorCode {
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$variant,)*];

            #[inline]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant),)*
                }
            }

            #[inline]
            pub const fn title(self) -> &'static str {
                match self {
                    $(Self::$variant => $title,)*
                }
            }

            #[inline]
            pub const fn explanation(self) -> &'static str {
                match self {
                    $(Self::$variant => $explanation,)*
                }
            }

            pub fn parse(code: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|c| c.as_str() == code)
            }
        }

        impl std::fmt::Display for ErrorCode {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

error_codes! {
E001 => "unexpected character", r#"
A character that is not part of Nyx's grammar appeared in the source.

```nyx
fn main() {
    let x = `;  // error: unexpected character `
}
```

Remove the character, or check for a typo in an operator.
"#;

E002 => "unterminated string literal", r#"
A string literal was opened with `"` but never closed on the same line.

```nyx
fn main() {
    let s = "hello;  // error: unterminated string literal
}
```

Add the closing `"` before the end of the line.
"#;

E003 => "unterminated character literal", r#"
A character literal was opened with `'` but the line or file ended before the
closing quote.

```nyx
fn main() {
    let c = 'a  // error: unterminated character literal
}
```
"#;

E004 => "empty character literal", r#"
A character literal must contain exactly one character.

```nyx
fn main() {
    let c = '';  // error: empty character literal
}
```
"#;

E005 => "character literal too long", r#"
A character literal holds exactly one character, several were found before the
closing quote.

```nyx
fn main() {
    let c = 'ab';  // error: character literal contains more than one character
}
```

Use double quotes for string literals: `"ab"`.
"#;

E006 => "invalid escape sequence", r#"
The escape after `\` is not recognised.

```nyx
fn main() {
    let s = "a\qb";  // error: invalid escape sequence \q
}
```

Valid escapes are `\\`, `\"`, `\n`, `\t`, `\r`, `\0`, `\xXX` and `\u{XXXXXX}`.
"#;

E007 => "invalid float literal", r#"
A float literal could not be parsed as an `f32`/`f64` value.
"#;

E008 => "invalid integer literal", r#"
An integer literal could not be parsed, most commonly because it does not fit
in the largest integer type (64 bits).

```nyx
fn main() {
    let x = 99999999999999999999999999999999999999;  // error: invalid integer literal
}
```
"#;

E020 => "expected a specific token", r#"
The parser required one particular token and found something else. The most
common case is a missing semicolon:

```nyx
fn main() {
    let x = 1  // error: expected ;, found }
}
```
"#;

E021 => "expected an identifier", r#"
An identifier (a name) was required at this position.

```nyx
fn main() {
    let 42 = 1;  // error: expected an identifier, found 42
}
```
"#;

E022 => "invalid assignment target", r#"
Only identifiers and field paths can appear on the left of `=`.

```nyx
fn main() {
    (a + b) = 1;  // error: invalid assignment target
}
```
"#;

E023 => "not a binary operator", r#"
A token that cannot join two operands appeared in operator position.
"#;

E024 => "not a prefix operator", r#"
A token that cannot prefix an operand appeared in prefix position. Note that
Nyx has no unary `+`.
"#;

E025 => "expected an expression", r#"
An expression was required at this position.

```nyx
fn main() {
    let x = ;  // error: expected an expression, found ;
}
```
"#;

E026 => "expected a literal pattern", r#"
Range pattern endpoints must be literals, not arbitrary expressions.

```nyx
fn main(): i32 {
    match 1 {
        y..=2 -> 0,  // error: range endpoints must be literals
        _ -> 1,
    }
}
```
"#;

E027 => "expected a type name", r#"
A type was required at this position.
"#;

E028 => "unexpected end of file", r#"
The source ended in the middle of a construct.

```nyx
fn main() {  // error: unexpected end of file — the { is never closed
```

Check for unclosed braces, parentheses or brackets.
"#;

E029 => "unknown marker", r#"
A `@name` marker ahead of a declaration is not one the compiler knows.

```nyx
@fast fn work() {}  // error: unknown marker fast
```

The markers Nyx defines are `@unsafe` and `@intrinsic`.
"#;

E030 => "marker does not open a block", r#"
A marker that only annotates declarations was written ahead of a block.

```nyx
fn main() {
    @intrinsic { }  // error: marker intrinsic does not open a block
}
```

`@unsafe { … }` is the only marker that opens a block. `@intrinsic` sits above
a declaration whose body the compiler supplies.
"#;

E031 => "expression-bodied function needs a return type", r#"
An expression-bodied function must declare the type produced by its expression.

```nyx
fn double(value: i32) = value * 2;  // error: return type required
```

Add the return type between the parameter list and `=`:

```nyx
fn double(value: i32): i32 = value * 2;
```

Nyx resolves function signatures before checking their bodies, so return types
on expression-bodied functions remain explicit.
"#;

E040 => "module file not found", r#"
A `use` declaration points at a module whose `.nyx` file does not exist.

```nyx
use project::missing;  // error: cannot find the imported module
fn main() { }
```

Create the file next to the entry module, or fix the path.
"#;

E041 => "circular import", r#"
Two (or more) modules import each other, directly or transitively.

```nyx
// a.nyx
use project::b;
// b.nyx
use project::a;  // error: circular import
```

Break the cycle, for example by moving the shared definitions into a third
module both can import.
"#;

E042 => "empty import path", r#"
A `use` path needs at least a root and a module segment, e.g.
`use project::module;`.
"#;

E043 => "unknown module root", r#"
The first segment of a `use` path must be the project name or `std`.

```nyx
use nope::thing;  // error: unknown module root nope
fn main() { }
```
"#;

E044 => "symbol not exported", r#"
A named import refers to a symbol the module does not export.

```nyx
// util.nyx
fn helper() { }          // not pub!
// main.nyx
use project::util::{helper};  // error: symbol helper is not exported
```

Add `pub` to the declaration to export it.
"#;

E045 => "statement at top level", r#"
Only declarations (`fn`, `struct`, `enum`, `interface`, `impl`, `const`,
`use`) may appear at a module's top level.

```nyx
let x: i32 = 1;  // error: statements are not allowed at the top level
```
"#;

E046 => "project contains no modules", r#"
The directory passed to the compiler does not contain any `.nyx` source files.

Add at least one source module directly inside the directory, or pass a `.nyx`
file explicitly.
"#;

E100 => "statement at top level", r#"
Only declarations may appear at a module's top level, see E045.
"#;

    E101 => "duplicate function", r#"
A function name was declared more than once in the same scope.

```nyx
fn foo(): i32 { 1 }
fn foo(): i32 { 2 }  // error: function foo cannot be declared multiple times
```
"#;

E102 => "duplicate method", r#"
The same method was defined twice for one type, possibly across separate
`impl` blocks.

```nyx
struct Counter { value: i32 }
impl Counter { fn get(&self): i32 { self.value } }
impl Counter { fn get(&self): i32 { self.value } }  // error: duplicate method
```
"#;

E103 => "undeclared identifier", r#"
A name was used before any binding introduced it.

```nyx
fn main() {
    x + 1;  // error: cannot find x in this scope
}
```
"#;

E104 => "unknown function", r#"
A call names a function that is not declared anywhere in scope.

```nyx
fn main() {
    foo();  // error: cannot find function foo
}
```
"#;

E105 => "unknown method", r#"
The receiver's type has no method with this name.

```nyx
struct Point { x: i32 }
fn main() {
    let p = Point { x: 1 };
    p.frobnicate();  // error: type Point has no method named frobnicate
}
```
"#;

E106 => "unknown type", r#"
A type annotation names a type that is not declared.

```nyx
fn main() {
    let x: Phantom = 1;  // error: cannot find type Phantom
}
```
"#;

E107 => "orphan impl", r#"
Methods can only be implemented on types declared in the same module.
"#;

E108 => "duplicate struct", r#"
A struct name was declared more than once.

```nyx
struct Foo { x: i32 }
struct Foo { y: i32 }  // error: struct Foo cannot be declared multiple times
```
"#;

E109 => "duplicate enum", r#"
An enum name was declared more than once, see E108.
"#;

E110 => "duplicate field", r#"
A struct declares the same field name twice.

```nyx
struct Bad { x: i32, x: i64 }  // error: field x is declared twice
```
"#;

E111 => "duplicate enum variant", r#"
An enum declares the same variant name twice.

```nyx
enum Bad { A = 1, A = 2 } as u8  // error: variant A is declared twice
```
"#;

E112 => "invalid field access", r#"
Field access is only supported on local variables and their fields, not on
arbitrary expressions such as call results.

```nyx
struct Point { x: i32 }
fn make(): Point { Point { x: 1 } }
fn main(): i32 { make().x }  // error: bind the value first
```
"#;

E113 => "invalid assignment target", r#"
Only `name = value` and `name.field = value` are assignable, see E022.
"#;

E114 => "unknown field", r#"
The struct has no field with this name.

```nyx
struct Point { x: i32 }
fn main() {
    let p = Point { x: 1 };
    let q = p.z;  // error: type Point has no field named z
}
```
"#;

E115 => "missing field in literal", r#"
A struct literal must initialise every field of the struct.

```nyx
struct Point { x: i32, y: i32 }
fn main() {
    let p = Point { x: 1 };  // error: field y is missing
}
```
"#;

E116 => "circular struct", r#"
A struct stored by value cannot contain itself, directly or through other
structs — the cycle would have infinite size.

```nyx
struct A { b: B }
struct B { a: A }  // error: struct A contains itself by value
```
"#;

E117 => "wrong number of arguments", r#"
A call passes more or fewer arguments than the function declares.

```nyx
fn add(a: i32, b: i32): i32 { a + b }
fn main() {
    add(1, 2, 3);  // error: called with 3 arguments, but add expects 2
}
```
"#;

E118 => "duplicate binding", r#"
The same name was bound twice in one scope. Nyx does not allow same-scope
shadowing.

```nyx
fn main() {
    let x: i32 = 1;
    let x: i32 = 2;  // error: the name x is already bound in this scope
}
```

Shadow it in a nested block instead, or pick another name.
"#;

E119 => "missing initialiser", r#"
A binding with neither a type annotation nor a value gives inference nothing
to work with.

```nyx
fn main() {
    let x;  // error: binding x has no type and no value
}
```
"#;

E120 => "receiver outside impl", r#"
`&self`/`&mut self` receivers are only valid on methods inside an `impl`
block.

```nyx
fn foo(&self): i32 { 0 }  // error: receiver outside an impl block
```
"#;

E121 => "type mismatch", r#"
An expression's type does not match the type required by its context.

```nyx
fn main() {
    if 42 { }  // error: type mismatch: expected bool, found i32
}
```
"#;

E122 => "missing return", r#"
A function declares a return type, but some path through the body completes
without producing a value.

```nyx
fn foo(): i32 {
    let x = 1;
}  // error: foo must return i32, but can complete without returning
```

End the body with an expression of the declared type, or `return` from every
path.
"#;

E123 => "immutable binding mutated", r#"
Bindings are immutable by default, assigning to one (or calling a `&mut self`
method on it) requires `mut`.

```nyx
fn main() {
    let x: i32 = 1;
    x = 2;  // error: cannot mutate immutable binding x
}
```

Declare it mutable: `let mut x = 1;`.
"#;

E124 => "non-const call in const fn", r#"
A `const fn` may only call other `const` functions.

```nyx
fn helper(): i32 { 42 }
const fn bad(): i32 { helper() }  // error: helper is not a const function
```
"#;

E125 => "invalid cast", r#"
`as` casts are only supported between primitive integer, bool, and char
types.

```nyx
struct P { x: i32 }
fn main() {
    let p = P { x: 1 };
    let y = p as i32;  // error: cannot cast P to i32
}
```
"#;

E126 => "type cannot be indexed", r#"
Indexing with `[...]` is only supported on arrays `[T; N]` and slices `&[T]`.

```nyx
fn main() {
    let x = 1;
    let y = x[0];  // error: type {integer} cannot be indexed
}
```
"#;

E127 => "index out of bounds", r#"
A constant index is outside the bounds of a fixed-size array.

```nyx
fn main() {
    let a = [1, 2, 3];
    let x = a[5];  // error: index 5 is out of bounds for an array of length 3
}
```
"#;

E128 => "invalid range endpoint", r#"
Range endpoints must be integers.

```nyx
fn main() {
    loop 1.0..2.0 { }  // error: f64 cannot be used as a range endpoint
}
```
"#;

E129 => "empty range", r#"
The range's lower bound is greater than its upper bound, so it matches no
values.

```nyx
fn main(): i32 {
    match 1 {
        5..=1 -> 0,  // error: this range matches no values
        _ -> 1,
    }
}
```
"#;

E130 => "loop item is not Copy", r#"
Looping over an array copies each element into the loop binding, which
requires the element type to implement `Copy`.
"#;

E131 => "type is not iterable", r#"
Loops iterate over fixed arrays, slices, and integer ranges.

```nyx
fn main() {
    let x = 42;
    loop v in x { }  // error: type {integer} is not iterable
}
```
"#;

E132 => "loop control outside a loop", r#"
`break` and `continue` are only valid inside a loop body.

```nyx
fn main() {
    break;  // error: break outside a loop
}
```
"#;

E133 => "cannot infer empty array type", r#"
An empty array literal gives inference no element type.

```nyx
fn main() {
    let a = [];  // error: cannot infer the element type of an empty array
}
```

Annotate the binding: `let a: [i32; 0] = [];`.
"#;

E134 => "assignment through shared reference", r#"
A shared `&` reference is read-only, writing through it needs `&mut`.

```nyx
struct P { x: i32 }
fn set(p: &P) {
    p.x = 2;  // error: cannot assign through a shared & reference
}
```
"#;

E135 => "duplicate interface", r#"
An interface name was declared more than once, see E108.
"#;

E136 => "unknown interface", r#"
An `impl … with` or superinterface clause names an interface that is not
declared.

```nyx
struct Foo { x: i32 }
impl Foo with Ghost { }  // error: cannot find interface Ghost
```
"#;

E137 => "missing interface method", r#"
An `impl Type with Interface` block does not implement every method the
interface requires.

```nyx
interface Greet {
    fn hello(&self): i32;
    fn bye(&self): i32;
}
struct Foo { x: i32 }
impl Foo with Greet {
    fn hello(&self): i32 { 1 }
}  // error: Foo is missing bye required by Greet
```
"#;

E138 => "missing superinterface implementation", r#"
Implementing an interface that extends another requires implementing the
superinterface too.

```nyx
interface Base { fn base(&self): i32; }
interface Derived: Base { fn derived(&self): i32; }
struct Foo { x: i32 }
impl Foo with Derived {
    fn derived(&self): i32 { 1 }
}  // error: Derived requires Base, which Foo does not implement
```
"#;

E139 => "interface signature mismatch", r#"
A method in an `impl ... with` block does not match the signature the interface
declares — return type, parameters, or receiver mutability differ.

```nyx
interface Shape { fn area(&self): i64; }
struct Rect { w: i32, h: i32 }
impl Rect with Shape {
    fn area(&self): i32 { self.w }  // error: Shape declares fn area(&self): i64
}
```
"#;

E140 => "circular constant", r#"
A constant's initialiser refers to the constant itself.

```nyx
const A: i32 = A;  // error: constant A depends on itself
```
"#;

E141 => "duplicate constant", r#"
A constant name was declared more than once in the same scope, see E108.
"#;

E142 => "unsatisfied bound", r#"
A generic function was called with a type that does not implement the bound's
interface.

```nyx
interface Greet { fn hello(&self): i32; }
struct P { x: i32 }
fn greet<T: Greet>(t: T): i32 { t.hello() }
fn main() {
    greet(P { x: 1 });  // error: type P does not satisfy the bound Greet
}
```
"#;

E143 => "operator requires interface", r#"
Comparison operators on user types require the matching interface
(`PartialEq` for `==`/`!=`, `PartialOrd` for orderings).

```nyx
struct P { x: i32 }
fn main() {
    let a = P { x: 1 };
    let b = P { x: 2 };
    if a == b { }  // error: operator == requires PartialEq
}
```
"#;

E144 => "item declared inside a function", r#"
Only `const` declarations may appear inside a function body.

```nyx
fn main() {
    struct Inner { x: i32 }  // error: struct declarations are not allowed here
}
```
"#;

E145 => "non-constant value in constant", r#"
A constant is evaluated independently of the function it appears in and
cannot refer to runtime locals.

```nyx
fn main() {
    let x = 1;
    const C: i32 = x;  // error: constants cannot refer to runtime values
}
```
"#;

E151 => "missing interface constant", r#"
An `impl Type with Interface` block does not define every associated constant
required by the interface.

```nyx
interface Buffer { const SIZE: uptr; }
struct Page {}
impl Page with Buffer {}  // error: Page is missing SIZE
```
"#;

E152 => "interface constant type mismatch", r#"
An associated constant in an `impl ... with` block has a different type from
the interface declaration.

```nyx
interface Buffer { const SIZE: uptr; }
struct Page {}
impl Page with Buffer {
    const SIZE: i32 = 4096;  // error: Buffer declares SIZE as uptr
}
```
"#;

E154 => "mutable static touched outside an unsafe context", r#"
A `static mut` is shared, unsynchronised storage, so every read and write of one
has to be marked.

```nyx
static mut CURSOR: uptr = 0;

fn bump() {
    CURSOR = CURSOR + 1;  // error: requires an unsafe context
}

@unsafe
fn bump_ok() {
    CURSOR = CURSOR + 1;  // fine
}
```
"#;

E157 => "dereference of a non-pointer", r#"
Only references and raw pointers can be dereferenced.

```nyx
fn main(): i32 {
    let x: i32 = 1;
    *x  // error: i32 is not a reference or a raw pointer
}
```
"#;

E156 => "implementation leaves an associated type unbound", r#"
An interface declares `type X,` and the implementation never says what `X` is.

```nyx
impl Buffer with Index<uptr> {
    type Output = i32;  // required
}
```
"#;

E155 => "associated type is not bound by this implementation", r#"
An implementation names `Self::X`, but no `type X = ...;` binds it. An interface
declares the association, every implementation supplies the type.

```nyx
pub interface Index<Idx> {
    type Output;

    fn index(&self, index: Idx): &Self::Output;
}

impl Buffer with Index<uptr> {
    type Output = i32;                                  // fine

    fn index(&self, index: uptr): &Self::Output { ... }
}
```
"#;

E153 => "static initialiser is not a compile-time value", r#"
A `static` is storage the compiler lays out in the executable, so its initial
value has to be written there at build time.

```nyx
fn seed(): uptr { 7 }

static mut CURSOR: uptr = seed();  // error: seed() runs at run time
static mut LIMIT: uptr = 4096;     // fine
```
"#;

E146 => "type does not match its annotation", r#"
An initialiser or returned value does not match the type declared for it. The
report points back at the annotation that fixed the expected type.

```nyx
fn foo(): i32 {
    return true;  // error: type bool does not match the declared type i32
}
```

A valueless `return;` produces `unit`, so it is only valid in a function that
does not declare a value return.
"#;

E147 => "unsafe function called from a safe one", r#"
A function marked `@unsafe` carries obligations its caller must uphold, so it
can only be called from another `@unsafe` function.

```nyx
@unsafe fn read(p: *i32): i32 { *p }

fn main(): i32 {
    return read(p);  // error: read is unsafe
}
```

Mark the caller `@unsafe` to pass the obligation on, or wrap the call in a safe
function that guarantees the invariants itself.
"#;

E149 => "nested indirection", r#"
A pointer or reference cannot point at another one: Nyx packs a type into a
single word, which leaves room for exactly one level of indirection.

```nyx
fn read(pp: **i32) {}  // error: cannot point at *i32
```

Wrap the inner pointer in a struct when a second level is genuinely needed.
"#;

E150 => "unknown intrinsic", r#"
A declaration marked `@intrinsic` claims the compiler supplies its body, but no
implementation is registered under that name.

```nyx
impl str {
    @intrinsic
    pub const fn reversed(&self): str {}  // error: no compiler implementation
}
```

`@intrinsic` is reserved for the handful of operations the compiler emits
directly, such as `len` and the `wrapping_*` arithmetic. Anything else needs a
body written in Nyx.
"#;

E148 => "raw pointer used in a safe function", r#"
Dereferencing a raw pointer cannot be checked by the compiler, so it is only
allowed inside a function marked `@unsafe`.

```nyx
fn read(p: *i32): i32 {
    return *p;  // error: dereferencing a raw pointer is unsafe
}
```
"#;
}

pub fn index_markdown() -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(64 * 1024);
    out.push_str(
        "# Nyx error index\n\n\
         <!-- GENERATED FILE — do not edit. -->\n\
         <!-- Regenerated from `src/error_codes.rs` by `cargo test`. -->\n\n\
         Every diagnostic the compiler emits carries one of the codes below.\n",
    );

    let mut current_group = "";
    for &code in ErrorCode::ALL {
        let group = match code.as_str() {
            s if s < "E020" => "Lexer",
            s if s < "E040" => "Parser",
            s if s < "E100" => "Modules",
            _ => "Semantic analysis",
        };
        if group != current_group {
            write!(out, "\n## {group}\n").expect("writing to a String never fails");
            current_group = group;
        }

        // github and gitlab have no `nyx` grammar, rust's highlighting fits nyx closely,
        // so the fences are translated for rendering only, the registry keeps
        // the tag if a real grammar ever lands
        let explanation = code.explanation().trim().replace("```nyx", "```rust");
        write!(out, "\n### {code}: {}\n\n{explanation}\n", code.title()).unwrap();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for &code in ErrorCode::ALL {
            let s = code.as_str();
            assert!(s.len() == 4 && s.starts_with('E'), "malformed code {s}");
            assert!(seen.insert(s), "duplicate code {s}");
            assert!(!code.title().is_empty(), "{s} has no title");
            assert!(!code.explanation().trim().is_empty(), "{s} has no explanation");
            assert_eq!(ErrorCode::parse(s), Some(code));
        }
    }

    #[test]
    fn error_index_is_up_to_date() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/ERRORS.md");
        let generated = index_markdown();
        let current = std::fs::read_to_string(path).unwrap_or_default();

        if current != generated {
            std::fs::write(path, &generated).expect("regenerate ERRORS.md");
            println!("ERRORS.md was stale and has been regenerated — commit the update");
        }
    }
}
