# Nyx error index

<!-- GENERATED FILE — do not edit. -->
<!-- Regenerated from `src/error_codes.rs` by `cargo test`. -->

Every diagnostic the compiler emits carries one of the codes below.

## Lexer

### E001: unexpected character

A character that is not part of Nyx's grammar appeared in the source.

```rust
fn main() {
    let x = `;  // error: unexpected character `
}
```

Remove the character, or check for a typo in an operator.

### E002: unterminated string literal

A string literal was opened with `"` but never closed on the same line.

```rust
fn main() {
    let s = "hello;  // error: unterminated string literal
}
```

Add the closing `"` before the end of the line.

### E003: unterminated character literal

A character literal was opened with `'` but the line or file ended before the
closing quote.

```rust
fn main() {
    let c = 'a  // error: unterminated character literal
}
```

### E004: empty character literal

A character literal must contain exactly one character.

```rust
fn main() {
    let c = '';  // error: empty character literal
}
```

### E005: character literal too long

A character literal holds exactly one character, several were found before the
closing quote.

```rust
fn main() {
    let c = 'ab';  // error: character literal contains more than one character
}
```

Use double quotes for string literals: `"ab"`.

### E006: invalid escape sequence

The escape after `\` is not recognised.

```rust
fn main() {
    let s = "a\qb";  // error: invalid escape sequence \q
}
```

Valid escapes are `\\`, `\"`, `\n`, `\t`, `\r`, `\0`, `\xXX` and `\u{XXXXXX}`.

### E007: invalid float literal

A float literal could not be parsed as an `f32`/`f64` value.

### E008: invalid integer literal

An integer literal could not be parsed, most commonly because it does not fit
in the largest integer type (64 bits).

```rust
fn main() {
    let x = 99999999999999999999999999999999999999;  // error: invalid integer literal
}
```

## Parser

### E020: expected a specific token

The parser required one particular token and found something else. The most
common case is a missing semicolon:

```rust
fn main() {
    let x = 1  // error: expected ;, found }
}
```

### E021: expected an identifier

An identifier (a name) was required at this position.

```rust
fn main() {
    let 42 = 1;  // error: expected an identifier, found 42
}
```

### E022: invalid assignment target

Only identifiers and field paths can appear on the left of `=`.

```rust
fn main() {
    (a + b) = 1;  // error: invalid assignment target
}
```

### E023: not a binary operator

A token that cannot join two operands appeared in operator position.

### E024: not a prefix operator

A token that cannot prefix an operand appeared in prefix position. Note that
Nyx has no unary `+`.

### E025: expected an expression

An expression was required at this position.

```rust
fn main() {
    let x = ;  // error: expected an expression, found ;
}
```

### E026: expected a literal pattern

Range pattern endpoints must be literals, not arbitrary expressions.

```rust
fn main(): i32 {
    match 1 {
        y..=2 -> 0,  // error: range endpoints must be literals
        _ -> 1,
    }
}
```

### E027: expected a type name

A type was required at this position.

### E028: unexpected end of file

The source ended in the middle of a construct.

```rust
fn main() {  // error: unexpected end of file — the { is never closed
```

Check for unclosed braces, parentheses or brackets.

### E029: unknown marker

A `@name` marker ahead of a declaration is not one the compiler knows.

```rust
@fast fn work() {}  // error: unknown marker fast
```

The markers Nyx defines are `@unsafe` and `@intrinsic`.

### E030: marker does not open a block

A marker that only annotates declarations was written ahead of a block.

```rust
fn main() {
    @intrinsic { }  // error: marker intrinsic does not open a block
}
```

`@unsafe { … }` is the only marker that opens a block. `@intrinsic` sits above
a declaration whose body the compiler supplies.

## Modules

### E040: module file not found

A `use` declaration points at a module whose `.nyx` file does not exist.

```rust
use project::missing;  // error: cannot find the imported module
fn main() { }
```

Create the file next to the entry module, or fix the path.

### E041: circular import

Two (or more) modules import each other, directly or transitively.

```rust
// a.nyx
use project::b;
// b.nyx
use project::a;  // error: circular import
```

Break the cycle, for example by moving the shared definitions into a third
module both can import.

### E042: empty import path

A `use` path needs at least a root and a module segment, e.g.
`use project::module;`.

### E043: unknown module root

The first segment of a `use` path must be the project name or `std`.

```rust
use nope::thing;  // error: unknown module root nope
fn main() { }
```

### E044: symbol not exported

A named import refers to a symbol the module does not export.

```rust
// util.nyx
fn helper() { }          // not pub!
// main.nyx
use project::util::{helper};  // error: symbol helper is not exported
```

Add `pub` to the declaration to export it.

### E045: statement at top level

Only declarations (`fn`, `struct`, `enum`, `interface`, `impl`, `const`,
`use`) may appear at a module's top level.

```rust
let x: i32 = 1;  // error: statements are not allowed at the top level
```

### E046: project contains no modules

The directory passed to the compiler does not contain any `.nyx` source files.

Add at least one source module directly inside the directory, or pass a `.nyx`
file explicitly.

## Semantic analysis

### E100: statement at top level

Only declarations may appear at a module's top level, see E045.

### E101: duplicate function

A function name was declared more than once in the same scope.

```rust
fn foo(): i32 { 1 }
fn foo(): i32 { 2 }  // error: function foo cannot be declared multiple times
```

### E102: duplicate method

The same method was defined twice for one type, possibly across separate
`impl` blocks.

```rust
struct Counter { value: i32 }
impl Counter { fn get(&self): i32 { self.value } }
impl Counter { fn get(&self): i32 { self.value } }  // error: duplicate method
```

### E103: undeclared identifier

A name was used before any binding introduced it.

```rust
fn main() {
    x + 1;  // error: cannot find x in this scope
}
```

### E104: unknown function

A call names a function that is not declared anywhere in scope.

```rust
fn main() {
    foo();  // error: cannot find function foo
}
```

### E105: unknown method

The receiver's type has no method with this name.

```rust
struct Point { x: i32 }
fn main() {
    let p = Point { x: 1 };
    p.frobnicate();  // error: type Point has no method named frobnicate
}
```

### E106: unknown type

A type annotation names a type that is not declared.

```rust
fn main() {
    let x: Phantom = 1;  // error: cannot find type Phantom
}
```

### E107: orphan impl

Methods can only be implemented on types declared in the same module.

### E108: duplicate struct

A struct name was declared more than once.

```rust
struct Foo { x: i32 }
struct Foo { y: i32 }  // error: struct Foo cannot be declared multiple times
```

### E109: duplicate enum

An enum name was declared more than once, see E108.

### E110: duplicate field

A struct declares the same field name twice.

```rust
struct Bad { x: i32, x: i64 }  // error: field x is declared twice
```

### E111: duplicate enum variant

An enum declares the same variant name twice.

```rust
enum Bad { A = 1, A = 2 } as u8  // error: variant A is declared twice
```

### E112: invalid field access

Field access is only supported on local variables and their fields, not on
arbitrary expressions such as call results.

```rust
struct Point { x: i32 }
fn make(): Point { Point { x: 1 } }
fn main(): i32 { make().x }  // error: bind the value first
```

### E113: invalid assignment target

Only `name = value` and `name.field = value` are assignable, see E022.

### E114: unknown field

The struct has no field with this name.

```rust
struct Point { x: i32 }
fn main() {
    let p = Point { x: 1 };
    let q = p.z;  // error: type Point has no field named z
}
```

### E115: missing field in literal

A struct literal must initialise every field of the struct.

```rust
struct Point { x: i32, y: i32 }
fn main() {
    let p = Point { x: 1 };  // error: field y is missing
}
```

### E116: circular struct

A struct stored by value cannot contain itself, directly or through other
structs — the cycle would have infinite size.

```rust
struct A { b: B }
struct B { a: A }  // error: struct A contains itself by value
```

### E117: wrong number of arguments

A call passes more or fewer arguments than the function declares.

```rust
fn add(a: i32, b: i32): i32 { a + b }
fn main() {
    add(1, 2, 3);  // error: called with 3 arguments, but add expects 2
}
```

### E118: duplicate binding

The same name was bound twice in one scope. Nyx does not allow same-scope
shadowing.

```rust
fn main() {
    let x: i32 = 1;
    let x: i32 = 2;  // error: the name x is already bound in this scope
}
```

Shadow it in a nested block instead, or pick another name.

### E119: missing initialiser

A binding with neither a type annotation nor a value gives inference nothing
to work with.

```rust
fn main() {
    let x;  // error: binding x has no type and no value
}
```

### E120: receiver outside impl

`&self`/`&mut self` receivers are only valid on methods inside an `impl`
block.

```rust
fn foo(&self): i32 { 0 }  // error: receiver outside an impl block
```

### E121: type mismatch

An expression's type does not match the type required by its context.

```rust
fn main() {
    if 42 { }  // error: type mismatch: expected bool, found i32
}
```

### E122: missing return

A function declares a return type, but some path through the body completes
without producing a value.

```rust
fn foo(): i32 {
    let x = 1;
}  // error: foo must return i32, but can complete without returning
```

End the body with an expression of the declared type, or `return` from every
path.

### E123: immutable binding mutated

Bindings are immutable by default, assigning to one (or calling a `&mut self`
method on it) requires `mut`.

```rust
fn main() {
    let x: i32 = 1;
    x = 2;  // error: cannot mutate immutable binding x
}
```

Declare it mutable: `let mut x = 1;`.

### E124: non-const call in const fn

A `const fn` may only call other `const` functions.

```rust
fn helper(): i32 { 42 }
const fn bad(): i32 { helper() }  // error: helper is not a const function
```

### E125: invalid cast

`as` casts are only supported between primitive integer, bool, and char
types.

```rust
struct P { x: i32 }
fn main() {
    let p = P { x: 1 };
    let y = p as i32;  // error: cannot cast P to i32
}
```

### E126: type cannot be indexed

Indexing with `[...]` is only supported on arrays `[T; N]` and slices `&[T]`.

```rust
fn main() {
    let x = 1;
    let y = x[0];  // error: type {integer} cannot be indexed
}
```

### E127: index out of bounds

A constant index is outside the bounds of a fixed-size array.

```rust
fn main() {
    let a = [1, 2, 3];
    let x = a[5];  // error: index 5 is out of bounds for an array of length 3
}
```

### E128: invalid range endpoint

Range endpoints must be integers.

```rust
fn main() {
    loop 1.0..2.0 { }  // error: f64 cannot be used as a range endpoint
}
```

### E129: empty range

The range's lower bound is greater than its upper bound, so it matches no
values.

```rust
fn main(): i32 {
    match 1 {
        5..=1 -> 0,  // error: this range matches no values
        _ -> 1,
    }
}
```

### E130: loop item is not Copy

Looping over an array copies each element into the loop binding, which
requires the element type to implement `Copy`.

### E131: type is not iterable

Loops iterate over fixed arrays, slices, and integer ranges.

```rust
fn main() {
    let x = 42;
    loop v in x { }  // error: type {integer} is not iterable
}
```

### E132: loop control outside a loop

`break` and `continue` are only valid inside a loop body.

```rust
fn main() {
    break;  // error: break outside a loop
}
```

### E133: cannot infer empty array type

An empty array literal gives inference no element type.

```rust
fn main() {
    let a = [];  // error: cannot infer the element type of an empty array
}
```

Annotate the binding: `let a: [i32; 0] = [];`.

### E134: assignment through shared reference

A shared `&` reference is read-only, writing through it needs `&mut`.

```rust
struct P { x: i32 }
fn set(p: &P) {
    p.x = 2;  // error: cannot assign through a shared & reference
}
```

### E135: duplicate interface

An interface name was declared more than once, see E108.

### E136: unknown interface

An `impl … with` or superinterface clause names an interface that is not
declared.

```rust
struct Foo { x: i32 }
impl Foo with Ghost { }  // error: cannot find interface Ghost
```

### E137: missing interface method

An `impl Type with Interface` block does not implement every method the
interface requires.

```rust
interface Greet {
    fn hello(&self): i32;
    fn bye(&self): i32;
}
struct Foo { x: i32 }
impl Foo with Greet {
    fn hello(&self): i32 { 1 }
}  // error: Foo is missing bye required by Greet
```

### E138: missing superinterface implementation

Implementing an interface that extends another requires implementing the
superinterface too.

```rust
interface Base { fn base(&self): i32; }
interface Derived: Base { fn derived(&self): i32; }
struct Foo { x: i32 }
impl Foo with Derived {
    fn derived(&self): i32 { 1 }
}  // error: Derived requires Base, which Foo does not implement
```

### E139: interface signature mismatch

A method in an `impl ... with` block does not match the signature the interface
declares — return type, parameters, or receiver mutability differ.

```rust
interface Shape { fn area(&self): i64; }
struct Rect { w: i32, h: i32 }
impl Rect with Shape {
    fn area(&self): i32 { self.w }  // error: Shape declares fn area(&self): i64
}
```

### E140: circular constant

A constant's initialiser refers to the constant itself.

```rust
const A: i32 = A;  // error: constant A depends on itself
```

### E141: duplicate constant

A constant name was declared more than once in the same scope, see E108.

### E142: unsatisfied bound

A generic function was called with a type that does not implement the bound's
interface.

```rust
interface Greet { fn hello(&self): i32; }
struct P { x: i32 }
fn greet<T: Greet>(t: T): i32 { t.hello() }
fn main() {
    greet(P { x: 1 });  // error: type P does not satisfy the bound Greet
}
```

### E143: operator requires interface

Comparison operators on user types require the matching interface
(`PartialEq` for `==`/`!=`, `PartialOrd` for orderings).

```rust
struct P { x: i32 }
fn main() {
    let a = P { x: 1 };
    let b = P { x: 2 };
    if a == b { }  // error: operator == requires PartialEq
}
```

### E144: item declared inside a function

Only `const` declarations may appear inside a function body.

```rust
fn main() {
    struct Inner { x: i32 }  // error: struct declarations are not allowed here
}
```

### E145: non-constant value in constant

A constant is evaluated independently of the function it appears in and
cannot refer to runtime locals.

```rust
fn main() {
    let x = 1;
    const C: i32 = x;  // error: constants cannot refer to runtime values
}
```

### E146: type does not match its annotation

An initialiser or returned value does not match the type declared for it. The
report points back at the annotation that fixed the expected type.

```rust
fn foo(): i32 {
    return true;  // error: type bool does not match the declared type i32
}
```

### E147: unsafe function called from a safe one

A function marked `@unsafe` carries obligations its caller must uphold, so it
can only be called from another `@unsafe` function.

```rust
@unsafe fn read(p: *i32): i32 { *p }

fn main(): i32 {
    return read(p);  // error: read is unsafe
}
```

Mark the caller `@unsafe` to pass the obligation on, or wrap the call in a safe
function that guarantees the invariants itself.

### E149: nested indirection

A pointer or reference cannot point at another one: Nyx packs a type into a
single word, which leaves room for exactly one level of indirection.

```rust
fn read(pp: **i32) {}  // error: cannot point at *i32
```

Wrap the inner pointer in a struct when a second level is genuinely needed.

### E150: unknown intrinsic

A declaration marked `@intrinsic` claims the compiler supplies its body, but no
implementation is registered under that name.

```rust
impl str {
    @intrinsic
    pub const fn reversed(&self): str {}  // error: no compiler implementation
}
```

`@intrinsic` is reserved for the handful of operations the compiler emits
directly, such as `len` and the `wrapping_*` arithmetic. Anything else needs a
body written in Nyx.

### E148: raw pointer used in a safe function

Dereferencing a raw pointer cannot be checked by the compiler, so it is only
allowed inside a function marked `@unsafe`.

```rust
fn read(p: *i32): i32 {
    return *p;  // error: dereferencing a raw pointer is unsafe
}
```
