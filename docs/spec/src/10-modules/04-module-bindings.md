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
const math = @import("math");      // top-level const binding

fn main() -> i32 {
    let m = @import("math");       // local let binding
    math.add(1, m.add(2, 3))       // equivalent access through either
}
```

## Per-File Scoping

{{ rule(id="10.4:8", cat="normative") }}

A top-level `const` module binding is scoped to the file that declares it.
Two files **MAY** bind the same name — whether to the same module or to
different modules — without conflict; references in each file resolve to
that file's own binding. This is an exception to the shared flat namespace
of rule 10.5:1, because every file writes its own imports. Declaring two
module bindings with the same name in one file remains a compile-time error
(E0418).

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
pub const inner = @import("inner");   // pub re-export
const hidden = @import("inner");      // private outside outer/

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
that module. Accessing it through the module yields the constant's value,
with the constant's declared (or inferred, 6.5:4) type. The access is itself
compile-time evaluable, so it may appear in another constant's initializer.

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
const math = @import("math");
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
match pattern (`m.Color::Red`), and an associated function in a call
(`m.Point::origin()`). Each of these forms is accepted wherever the
corresponding *bare* path — `Point { .. }`, `Color::Red`, `Point::origin()` —
is accepted, and has the same meaning.

{{ rule(id="10.4:16", cat="normative") }}

A module-qualified path **MAY** be used in a *type* position — a type
annotation, a field type, or a parameter or return type. In those positions,
`m.Type` names the type member reached through the module binding `m`, and
`m.Type(T)` applies a module-qualified type constructor. This is the preferred
form for referring to standard-library types such as `std.option.Option(i64)`.

{{ rule(id="10.4:17") }}

Resolution of the tail path in the forms of 10.4:15 currently proceeds through
the transitional flat namespace (10.5:2): the type, variant, or associated
function is looked up by name across the whole compilation, and the leading
module qualifier does **not** restrict the lookup to members of the named
module. A program must not rely on the qualifier naming the module that defines
the item; unlike functions and value constants (10.4:3, 10.4:12), which resolve
as genuine module members, the qualifier is not yet load-bearing for these item
kinds. This is a transitional artifact that will change when top-level names
become module-scoped (10.5:2).

{{ rule(id="10.4:18", cat="legality-rule") }}

Privacy of module-qualified type and enum-variant access is uniform (10.3:7).
Naming a private struct in a module-qualified struct literal from a source file
in another directory is a compile-time error (E0460, the same error as the
unqualified form, because the type resolves through the flat namespace).
Naming a private enum's variant through a module from another directory is a
compile-time error (E0706, because a variant path routes through
module-member resolution).

{{ rule(id="10.4:19", cat="legality-rule") }}

An associated function's visibility follows the visibility of its enclosing type
(10.3:7): calling an associated function of a private struct from a source file
in another directory is a compile-time error (E0460), exactly as naming that
struct in a struct literal or type annotation would be. This holds whether the
call is written unqualified (`Point::origin()`) or module-qualified
(`lib.Point::origin()`) — both resolve the receiver type through the flat
namespace (10.5:2), so both report E0460 (RUE-330). A call whose receiver type
arrives through a comptime binding rather than by naming the struct
(`let P = lib.Point; P::origin()`) is exempt, matching every other reference
that reaches a type through a binding.

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
const lib = @import("sub/lib");
fn main() -> i32 {
    let p = lib.Point { x: 40, y: 2 };     // qualified struct literal
    let q = lib.Point::origin();           // qualified associated fn
    let c = lib.Color::Green;              // qualified variant expression
    let typed: lib.Point = p;           // qualified type annotation
    // let h = lib.Hidden::A;            // error E0706: `Hidden` is private outside sub/
    let base = typed.x + typed.y + q.x + q.y; // 40 + 2 + 30 + 12 = 84
    match c {
        lib.Color::Red => 0,             // qualified variant pattern
        lib.Color::Green => base,        // 84
        lib.Color::Blue => 0,
    }
}
```

## Modules Are Not Runtime Values

{{ rule(id="10.4:6", cat="legality-rule") }}

A module is not a runtime value. It is a compile-time error to use a module
binding as a function argument, as a struct field value, or as an operand of
an operator such as `==`.

{{ rule(id="10.4:7") }}

In a few value positions (for example, the tail expression of a block whose
type is `()`) a module expression is currently accepted and treated as the
unit value. Programs must not rely on this; it is an artifact of the current
implementation, not a guarantee.
