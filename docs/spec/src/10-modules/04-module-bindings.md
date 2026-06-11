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
