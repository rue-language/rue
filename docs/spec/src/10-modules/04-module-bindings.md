+++
title = "Module Bindings and Re-exports"
weight = 4
template = "spec/page.html"
+++

# Module Bindings and Re-exports

This section specifies how the result of `@import` is bound to a name, the
re-export idiom (`pub const m = @import(...)`), and the (non-)status of
modules as runtime values. The imported module may be bound to any name
(rule [4.13:87](@/04-expressions/13-intrinsics.md)).

## Binding Forms

{{ rule(id="10.4:1", cat="normative") }}

The result of `@import` **MAY** be bound at the top level of a file with a
`const` item, or locally inside a function body with a `let` statement.
Member access through either binding form has the same semantics.

{{ rule(id="10.4:2", cat="example") }}

```rue
const math = @import("math.rue");  // top-level const binding

fn main() -> i32 {
    let m = @import("math.rue");   // local let binding
    math.add(1, m.add(2, 3))       // equivalent access through either
}
```

## Per-File Scoping

{{ rule(id="10.4:8", cat="normative") }}

A top-level `const` module binding is scoped to the file that declares it,
like every other top-level name (10.5:2). Two files **MAY** bind the same
name — whether to the same module or to different modules — without
conflict; references in each file resolve to that file's own binding.
Declaring two module bindings with the same name in one file remains a
compile-time error (E0418).

{{ rule(id="10.4:9", cat="example") }}

```rue
// a.rue
const utils = @import("utils");        // fine
pub fn from_a() -> i32 { utils.one() }

// b.rue
const utils = @import("utils");        // fine: per-file, no collision
pub fn from_b() -> i32 { utils.one() }
```

## Re-exports

{{ rule(id="10.4:3", cat="normative") }}

A top-level `const` whose initializer is an `@import` expression makes the
bound name a member of the enclosing module — a *re-export*. Accessing that
member through an import of the enclosing module yields the inner imported
module, and such access chains through multiple levels of re-export.

{{ rule(id="10.4:4", cat="normative") }}

A `const` re-export follows the same visibility rules as any other item
(10.3): a `pub const` re-export is accessible from any directory, while a
non-`pub` re-export is private to the directory of its defining file.

{{ rule(id="10.4:5", cat="example") }}

```rue
// outer/_outer.rue
pub const inner = @import("inner.rue"); // pub re-export
const hidden = @import("inner.rue");    // private outside outer/

// outer/inner.rue
pub fn value() -> i32 { 42 }

// main.rue
fn main() -> i32 {
    let outer = @import("outer");
    outer.inner.value()       // 42, chained through the re-export
    // outer.hidden.value()   // error: `hidden` is private (E0706)
}
```

## Module Aliases

{{ rule(id="10.4:10", cat="normative") }}

A top-level `const` whose initializer *evaluates to* a module — an alias of
another module binding in the same file (`const m2 = m;`) or a member-access
chain ending at a module (`const math = std.math;`) — is itself a module
binding, with the same per-file scoping (10.4:8) and re-export semantics
(10.4:3) as a direct `@import` binding.

{{ rule(id="10.4:11", cat="example") }}

```rue
const std = @import("std");
const math = std.math;          // alias of a nested re-export
const m2 = math;                // alias of an alias

fn main() -> i32 {
    math.abs(-7) - m2.abs(7)    // 0: all three name the same module
}
```

## Value Constants as Module Members

{{ rule(id="10.4:12", cat="normative") }}

A value constant declared at the top level of a module's file is a member of
that module. Accessing it through the module yields the constant's value, with
the constant's declared type — a value constant always carries an explicit type
annotation (6.5:4). The access is itself compile-time evaluable, so it may
appear in another constant's initializer.

{{ rule(id="10.4:13", cat="normative") }}

A value constant member follows the same visibility rules as any other item
(10.3): a `pub const` is accessible from any directory, while a non-`pub`
constant is private to the directory of its defining file (E0706).

{{ rule(id="10.4:14", cat="example") }}

```rue
// math.rue
pub const ANSWER: i32 = 42;
const SECRET: i32 = 99;          // private outside math.rue's directory

// main.rue (same directory)
const math = @import("math.rue");
const COPY: i32 = math.ANSWER;   // const-evaluable member access

fn main() -> i32 { math.ANSWER + COPY }   // 84
```

## Types, Enum Variants, and Associated Functions Through a Module

The preceding sections cover functions and value constants as module members.
This section specifies how a module's **types**, **enum variants**, and
**associated functions** (6.4:11) are referenced through a module binding, and
how privacy applies to each form.

{{ rule(id="10.4:15", cat="normative") }}

A type, enum variant, or associated function of a module **MAY** be named
through a module binding in an *expression* or *pattern* position by writing
the binding, a `.`, and the item's path: a struct type in a struct-literal
expression (`m.Point { .. }`), an enum variant in a variant expression or a
match pattern (`m.Color.Red`), and an associated function in a call
(`m.Point.origin()`). Each of these forms is accepted wherever the
corresponding *bare* path — `Point { .. }`, `Color.Red`, `Point.origin()` —
is accepted, and has the same meaning.

{{ rule(id="10.4:16", cat="normative") }}

A module-qualified path **MAY** be used in a *type* position — a type
annotation, a field type, or a parameter or return type. In those positions,
`m.Type` names the type member reached through the module binding `m`, and
`m.Type(T)` applies a module-qualified type constructor. This is the preferred
form for referring to standard-library types such as `std.option.Option(i64)`.

{{ rule(id="10.4:17", cat="normative") }}

The module qualifier is load-bearing for every item kind. In the forms of
10.4:15 the type, variant, or associated function resolves as a member of
the named module's file — exactly as functions and value constants do
(10.4:3, 10.4:12). If the named module's file does not define the item,
the reference is an unknown-member error even when another loaded file
defines an item of that name; same-named types in distinct modules are
distinct nominal types (10.5:2).

{{ rule(id="10.4:18", cat="legality-rule") }}

Privacy of module-qualified access is uniform across item kinds (10.3:7)
and reports one member-access diagnostic: accessing a private member
through a module binding from a source file in another directory is a
compile-time error E0706 — for a struct named in a qualified struct
literal or type annotation, for an enum's variant, for an associated
function's enclosing type, and for functions and constants alike. The one
exception is the comptime type-constructor application form in a type
position (10.4:16), which reports E0460 (10.3:7).

{{ rule(id="10.4:19", cat="legality-rule") }}

An associated function's visibility follows the visibility of its enclosing
type (10.3:7). A module-qualified call on a private type from another
directory (`lib.Secret.make()`) is a compile-time error E0706 (10.4:18).
An unqualified call (`Secret.make()`) never reaches the visibility check
from another file: the bare type name does not resolve outside its
defining module (10.3:8), so the reference is a name-resolution error.
Binding the type value first does not launder privacy: the binding
(`let P = lib.Secret;`) is itself a module-member access and reports
E0706 at the binding site.

{{ rule(id="10.4:20", cat="example") }}

```rue
// sub/lib.rue
pub struct Point {
    x: i32,
    y: i32,
    fn origin() -> Point { Point { x: 30, y: 12 } }  // associated fn
}
pub enum Color { Red, Green, Blue, }
enum Hidden { A, B, }               // private outside sub/

// main.rue — a different directory
const lib = @import("sub/lib.rue");
fn main() -> i32 {
    let p = lib.Point { x: 40, y: 2 };     // qualified struct literal
    let q = lib.Point.origin();           // qualified associated fn
    let c = lib.Color.Green;              // qualified variant expression
    let typed: lib.Point = p;           // qualified type annotation
    // let h = lib.Hidden.A;            // error E0706: `Hidden` is private outside sub/
    let base = typed.x + typed.y + q.x + q.y; // 40 + 2 + 30 + 12 = 84
    match c {
        lib.Color.Red => 0,             // qualified variant pattern
        lib.Color.Green => base,        // 84
        lib.Color.Blue => 0,
    }
}
```

{{ rule(id="10.4:21", cat="normative") }}

A top-level `const` whose initializer evaluates to a type — a **type alias**,
such as `pub const R = Result(u64, E);` — is a type member of its module. It is
named through a module binding in every form of 10.4:15 and 10.4:16, wherever
the aliased type itself may be named: as a qualified struct literal, an
associated-function receiver, a variant expression, a match-pattern head, and a
type annotation. The alias denotes the type its initializer evaluated to and
introduces no new nominal type (10.5:2), so `m.Alias.Variant` and
`m.Declared.Variant` name the same variant of the same enum.

{{ rule(id="10.4:22", cat="legality-rule") }}

A module-qualified reference to a type alias names the constant, not the
declaration behind it, so the constant's own visibility governs the access
(10.4:13): a `pub` alias of a private type is accessible from another directory
and re-exports that type, while a private alias named from another directory is
a compile-time error E0706 reporting the constant. This does not weaken 10.4:19:
naming the private declaration itself (`lib.Secret`, including through a
client-side binding `let P = lib.Secret;`) remains E0706, because that reference
does name the declaration.

{{ rule(id="10.4:23", cat="example") }}

```rue
// sub/lib.rue
enum Hidden { A, B, }
pub const H = Hidden;                   // pub alias re-exports a private enum
pub struct Point { x: i32, y: i32, }
pub const P = Point;

// main.rue - a different directory
const lib = @import("sub/lib.rue");
fn main() -> i32 {
    let p = lib.P { x: 40, y: 2 };      // alias as a struct-literal head
    let h = lib.H.A;                    // alias as a variant expression
    match h {
        lib.H.A => p.x + p.y,           // alias as a match-pattern head: 42
        lib.H.B => 0,
    }
}
```

## Modules Are Not Runtime Values

{{ rule(id="10.4:6", cat="legality-rule") }}

A module is not a runtime value. It is a compile-time error to use a module
binding where a value of some type is expected — as a function argument, as a
struct field value, as an operand of an operator such as `==`, or as an
expression whose type must match an expected type. This includes the expected
type `()`: a module expression is never treated as the unit value. The
diagnostic is a type mismatch (`E0206`) reporting `<module>` as the found type.

{{ rule(id="10.4:7", cat="legality-rule") }}

A module expression in statement position — a bare expression statement such as
`m;`, or a block statement whose tail is a module expression — is accepted. No
type is expected there, so the module is discarded without being materialized as
a value. This is not an exception to 10.4:6: no module value is produced.
