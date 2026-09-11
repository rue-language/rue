+++
title = "Match Expressions"
weight = 7
template = "spec/page.html"
+++

# Match Expressions

{{ rule(id="4.7:1", cat="normative") }}

A match expression provides multi-way branching based on pattern matching.

{{ rule(id="4.7:2", cat="normative") }}

<!-- grammar-sync(id="4.7:2", production="pattern", role="source") -->
<!-- grammar-sync(id="4.7:2", production="path_pattern", role="source") -->
<!-- grammar-sync(id="4.7:2", production="pattern_elements", role="source") -->
<!-- grammar-sync(id="4.7:2", production="pattern_element", role="source") -->

```ebnf
match_expr     = "match" expression "{" [ match_arms ] "}" ;
match_arms     = match_arm { "," match_arm } [ "," ] ;
match_arm = pattern "=>" expression ;
pattern        = "_"
               | [ "-" ] INTEGER
               | BOOL
               | path_pattern
               | struct_pattern ;
path_pattern = pattern_head "." IDENT [ "(" pattern_elements ")" ] ;
pattern_head   = qualified_ident [ "(" [ call_args ] ")" ] ;
pattern_elements = pattern_element { "," pattern_element } [ "," ] ;
pattern_element = IDENT | "_" | path_pattern | struct_pattern ;
```

An enum variant is a `path_pattern`: its `pattern_head` may be a qualified
identifier (such as `module.Enum`) or an inline type-constructor call (such as
`Result(i32, i32)`), followed by `.` and the variant name. A head segment names
its enum however that enum is reachable there — as a declaration, as a
comptime-bound type, or as a `const` type alias, including one named through a
module binding (`module.Alias.Variant`, 10.4:21). A bare variant
pattern omits the optional payload list and is the all-wildcard form. An
explicit payload list contains one or more positions and may have a trailing
comma; an empty list such as `Enum.Variant()` is not a pattern. Each position is
a binding name, the `_` wildcard, a nested variant pattern (4.7:37), or a
struct pattern (4.7:42). A `struct_pattern` is the production of 5.1:2; a
struct pattern's head is a type, so a name that continues into `{` — directly,
through a module path, or through a type-constructor call — begins a struct
pattern and any other name begins a `path_pattern`.

## Patterns

{{ rule(id="4.7:3", cat="normative") }}

A pattern is *irrefutable* if it matches any value of its type.
A pattern is *refutable* if there exist values of its type that it does not match.

{{ rule(id="4.7:4", cat="normative") }}

The wildcard pattern `_` is irrefutable. It matches any value.

{{ rule(id="4.7:5", cat="normative") }}

An integer literal pattern is refutable. It matches only the specific integer value it denotes.

{{ rule(id="4.7:6", cat="normative") }}

A boolean literal pattern (`true` or `false`) is refutable. It matches only the specific boolean value it denotes.

{{ rule(id="4.7:7", cat="normative") }}

An enum variant pattern is refutable. It matches only values of that specific variant.

## Exhaustiveness

{{ rule(id="4.7:8", cat="normative") }}

A set of patterns is *exhaustive* for a type if every possible value of that type
is matched by at least one pattern in the set.

{{ rule(id="4.7:9", cat="normative") }}

A match expression **MUST** have an exhaustive set of patterns for its scrutinee type.
A match expression with a non-exhaustive pattern set is rejected with a compile-time error.

{{ rule(id="4.7:10", cat="normative") }}

The following rules determine whether a pattern set is exhaustive:

1. Any pattern set containing an irrefutable pattern is exhaustive.
2. For type `bool`: a pattern set containing both `true` and `false` is exhaustive.
3. For an enum type: a pattern set containing a pattern for every variant of that enum is exhaustive.
4. For integer types: only rule (1) applies; explicit enumeration of integer values is not sufficient to establish exhaustiveness.

{{ rule(id="4.7:11") }}

```rue
fn main() -> i32 {
    match 2 {
        1 => 10,
        2 => 20,
        _ => 0,  // wildcard required for integer scrutinees
    }
}
```

## Type Checking

{{ rule(id="4.7:12", cat="normative") }}

All match arms **MUST** have the same type. The type of the match expression is the common type of its arms. Exception: in a `match` whose scrutinee is compile-time known, only the selected arm's body is analyzed (rule 4.14:19) — the unselected arms' bodies are not type-checked, so they are exempt from this rule; the match expression's type is the selected arm's type.

{{ rule(id="4.7:13", cat="normative") }}

The type of each pattern **MUST** be identical to the type of the scrutinee, up
to the one admitted never-type coercion (3.4:3). A pattern with any other type
difference is rejected with a compile-time error.

## Arm Bodies

{{ rule(id="4.7:14", cat="normative") }}

Match arm bodies **MAY** be simple expressions or block expressions.

{{ rule(id="4.7:15") }}

```rue
fn main() -> i32 {
    match 2 {
        1 => 10,
        2 => {
            let x = 20;
            x + 5
        },
        _ => 0,
    }
}
```

## Execution

{{ rule(id="4.7:16", cat="dynamic-semantics") }}

Arms are evaluated in order. The first arm whose pattern matches the scrutinee value
is selected, and its body expression is evaluated. The result of that evaluation
becomes the value of the match expression (core calculus
`docs/formal/01-core-calculus.md` §6.6, rule `(D-Match)`).

## Unreachable Patterns

{{ rule(id="4.7:17", cat="normative") }}

A pattern is *unreachable* if all values it could match are already matched by
a preceding pattern in the same match expression.

{{ rule(id="4.7:18", cat="normative") }}

A pattern following an irrefutable pattern (such as `_`) is always unreachable,
since the irrefutable pattern matches all possible values.

{{ rule(id="4.7:19", cat="normative") }}

A pattern that is identical to a preceding pattern in the same match expression
is unreachable, since the earlier pattern will match first.

{{ rule(id="4.7:20", cat="normative") }}

An unreachable pattern produces a compile-time warning. The program remains
well-formed and the unreachable arm body is not evaluated at runtime.

{{ rule(id="4.7:21") }}

```rue
fn main() -> i32 {
    match 5 {
        _ => 10,
        1 => 20,  // warning: unreachable pattern '1'
    }
}
```

{{ rule(id="4.7:22") }}

```rue
fn main() -> i32 {
    match 1 {
        1 => 10,
        1 => 20,  // warning: unreachable pattern '1'
        _ => 0,
    }
}
```

## Pattern Range Requirements

{{ rule(id="4.7:23", cat="legality-rule") }}

An integer literal pattern **MUST** denote a value representable in the
scrutinee's type. A pattern whose value is out of range for the scrutinee type
is rejected with a compile-time error, exactly as an out-of-range integer
literal in any other position (3.1:17). A negated literal that denotes the
minimum value of a signed scrutinee type remains valid (3.1:18).

{{ rule(id="4.7:24", cat="legality-rule") }}

A negative integer literal pattern **MUST NOT** be used with a scrutinee of
unsigned type. Such a pattern is rejected with a compile-time error; unsigned
values are never negative, so the arm could never match.

{{ rule(id="4.7:25", cat="example") }}

```rue
fn main() -> i32 {
    let x: u32 = 0;
    match x {
        4294967296 => 1,  // error: out of range for u32
        -1 => 2,          // error: negative pattern on unsigned scrutinee
        _ => 0,
    }
}
```

## Empty Match Expressions

{{ rule(id="4.7:26", cat="normative") }}

A match expression with zero arms is legal if and only if the scrutinee's type
is an enum with zero variants. Such a type has no values, so the empty pattern
set vacuously satisfies exhaustiveness (4.7:8). A match expression with zero
arms on any other type is rejected with a compile-time error.

{{ rule(id="4.7:27", cat="normative") }}

The type of a match expression with zero arms is `!` (the never type): the
expression can never be reached with a scrutinee value, so it never produces
a value.

{{ rule(id="4.7:28", cat="normative") }}

A struct literal **MUST NOT** appear as the outermost expression of a match
scrutinee; a program that requires one parenthesizes the scrutinee.
Consequently, in `match v {}` the braces denote the match expression's empty
arm list, not a struct literal `v {}`.

{{ rule(id="4.7:29", cat="example") }}

```rue
enum Never {}

fn absurd(n: Never) -> i32 {
    match n {}  // legal: zero arms cover the zero values of `Never`
}
```

## Patterns with Payload Bindings

{{ rule(id="4.7:30", cat="normative") }}

A tuple-variant path pattern binds the variant's payload into fresh names:
`EnumName.Variant(a, b)` matches a value of that variant and binds `a`, `b`
to its payload fields in order. The enum head may be qualified or an inline
type-constructor head (4.7:2), and the binding list may end in a trailing
comma, as in `EnumName.Variant(a, b,)`. A binding position may instead be the
wildcard `_`, which matches and discards that field without binding it; unlike
a name it introduces nothing and so may repeat (`Rect(_, _)`). The number of
binding positions **MUST** equal the variant's payload arity (see spec 6.3),
with one carve-out: a *bare* variant pattern that supplies no binding list at
all — `EnumName.Variant` on a variant of arity one or more — **is** the
all-wildcard form `EnumName.Variant(_, ..., _)` and is therefore exempt from
the arity rule (4.7:34 says the same value-context consumption applies to it).

A discarded field — one covered by `_`, or by the bare form — is bound to a
fresh *unnameable* binding: it is moved out of the scrutinee like any other
payload binding, but no expression can name it. Consequently it is **dropped at
the end of its arm**, interleaved with the arm's named bindings in the usual
reverse-declaration order (3.9:4), not before the arm's body and not as part of
the scrutinee. A discarded field whose type carries a linear value is a
compile-time error: nothing can name that binding, so its must-consume
obligation (3.8:50) could never be discharged. Bind such a field by name and
consume it — `@drop` (3.9:37) is the explicit-discard escape hatch.

{{ rule(id="4.7:31", cat="normative") }}

Payload bindings inherit the scrutinee's access mode (ADR-0037/ADR-0038). A
bare `match e` uses the scrutinee in value context: the matched arm's bindings
**move** the payload out of the enum (or copy it, if the payload type is
`Copy`). Each binding is in scope for its arm's body and shadows any outer
binding of the same name. A named payload binding is an ordinary local
binding: one whose type carries a linear value (3.8:57) is subject to the
must-consume obligation (3.8:32, 3.8:50) at the end of its arm, exactly as a
`let` binding is at the end of its block.

{{ rule(id="4.7:32") }}

```rue
enum Shape { Circle(i32), Rect(i32, i32), Empty }

fn main() -> i32 {
    match Shape.Rect(3, 4) {
        Shape.Circle(r) => r,
        Shape.Rect(w, h) => w + h,
        Shape.Empty => 0,
    }
}
```

## Scrutinee Access Mode

{{ rule(id="4.7:33", cat="normative") }}

The scrutinee of a match is used in **value context** (3.8:76): evaluating a match
*uses* its scrutinee. If the scrutinee's type is `Copy`, the match copies it and the
scrutinee remains valid after the match. If the scrutinee's type is a move type, the
match consumes — moves — the scrutinee, which is invalid after the match. Rue has no
`borrow` or `inout` scrutinee form (those access modes apply only to function
parameters and arguments, 6.1); `match e` always uses `e` by value. The payload
bindings introduced by a matched arm (4.7:31) then project sub-places of the value
the match has already used.

{{ rule(id="4.7:34", cat="legality-rule") }}

A move-type scrutinee is consumed by the match independently of whether the matched
arm binds a payload *by name*. A match whose selected arm introduces no name — a
bare variant pattern (which is the all-wildcard form, 4.7:30), a wildcard `_`, or a
literal pattern — still moves a move-type scrutinee, because the scrutinee occurs in
value context regardless of the pattern. Using the scrutinee after such a match is
therefore a use-after-move error (3.8:5).

{{ rule(id="4.7:35", cat="example") }}

```rue
enum E { A(i32), B }

fn use_again(e: E) -> i32 { 2 }

fn main() -> i32 {
    let e = E.A(40);       // payload is Copy, so E is a Copy type
    let r = match e {
        E.A(x) => x,
        E.B => 0,
    };
    r + use_again(e)        // OK: Copy scrutinee still valid -> 42
}
```

{{ rule(id="4.7:36", cat="example") }}

```rue
struct Big { value: i32 }
enum E { A(Big), B }

fn use_again(e: E) -> i32 { 0 }

fn main() -> i32 {
    let e = E.A(Big { value: 7 });   // move type: Big is not Copy
    let r = match e {
        E.A => 1,                    // introduces no name, yet consumes `e`
        E.B => 2,
    };
    r + use_again(e)                  // ERROR: use of moved value 'e'
}
```

## Nested Variant Patterns

{{ rule(id="4.7:37", cat="normative") }}

A payload position of a tuple-variant pattern (4.7:30) may itself be a variant
pattern instead of a binding: `R.Err(E.A(b))` matches a value of `R.Err` whose
payload is a value of `E.A`, and binds `b` to that inner payload. Nesting is
recursive, so a nested pattern's own payload positions may nest again —
`R.Err(E.A(Inner.X(v)))`. A nested pattern accepts every head form a top-level
pattern accepts (4.7:2): unqualified, module-qualified, or an inline
type-constructor head. It is resolved against the **payload field type** of the
position it occupies, not against the match's scrutinee type, and the enum it
names **MUST** be that field's type; a generic enum reached through an
instantiation — the `E` of `Result(i64, E)` — resolves through the instantiated
payload type like any other field.

{{ rule(id="4.7:38", cat="normative") }}

A nested pattern binds and consumes exactly as a top-level one does (4.7:30,
4.7:31). The position it occupies is moved out of the enclosing payload, and the
nested pattern's own positions are then moved out of *that* value; every
position of every level is accounted for, so each is dropped once, at the end of
the arm, in reverse declaration order with its siblings. A `_` inside a nested
pattern is the same fresh unnameable binding a top-level `_` is, with the same
consequence: a discarded position whose type carries a linear value is a
compile-time error at any depth.

{{ rule(id="4.7:39", cat="normative") }}

Exhaustiveness (4.7:19) applies at every level. The arms that match one variant
and discriminate its payload together **MUST** cover every combination of values
those payload positions can hold; an arm whose position holds a binding or `_`
covers every remaining value there, as a wildcard arm does at the top level.
`R.Ok(_)`, `R.Err(E.A(b))` and `R.Err(E.B)` are together exhaustive over
`Result(i64, E)`; dropping the `R.Err(E.B)` arm leaves the match non-exhaustive,
and the diagnostic names the missing pattern as `R.Err(E.B)`. An arm the earlier
arms already cover — a repeated nested variant, or any arm after a binding at
the positions it discriminates — is unreachable (4.7:20).

{{ rule(id="4.7:40", cat="normative") }}

One pattern may nest variant patterns in any number of its payload positions,
and the arms that match the same variant may nest in different positions:
`Outer.Pair(Inner.A(v), Inner.B)` is a legal pattern, and
`Outer.Pair(Inner.A(v), b)` and `Outer.Pair(a, Inner.B)` may appear in the same
match. The arms sharing a variant are checked against the whole pattern matrix
those positions form — one column per position of the variant and of every
nested variant, one row per arm — so the exhaustiveness of 4.7:39 and the
unreachability of 4.7:20 are properties of the arms **together** rather than of
one position at a time. `Outer.Pair(Inner.A(v), Inner.B)` and
`Outer.Pair(_, _)` are together exhaustive; `Outer.Pair(Inner.A(v), Inner.B)`
and `Outer.Pair(Inner.B, _)` are not, and the diagnostic names the uncovered
combination `Outer.Pair(Inner.A(_), Inner.A(_))`.

{{ rule(id="4.7:41", cat="example") }}

```rue
const std = @import("std");

enum E { A(u8), B }
const R = std.result.Result(i64, E);

fn f(x: i64) -> R {
    if x > 0 { R.Ok(x) } else { R.Err(E.A(1)) }
}

fn main() -> i32 {
    match f(0) {
        R.Ok(_) => 0,
        R.Err(E.A(b)) => @intCast(b),   // -> 1
        R.Err(E.B) => 2,
    }
}
```

## Struct Patterns in Match Arms

{{ preview_feature(feature="struct_patterns", adr="ADR-0091", doc="0091-struct-patterns.md") }}

{{ rule(id="4.7:42", cat="normative") }}

A match arm's pattern **MAY** be a struct pattern (5.1:18), `T { f: b, ... }`,
and a payload position of a tuple-variant pattern (4.7:30) **MAY** hold one:
`match p { Point { x, y } => ... }` binds the fields of a struct scrutinee, and
`R.Ok(Point { x, y })` binds the fields of the payload value a variant carries.
The head is written with the type grammar exactly as a let statement's struct
pattern writes it — a struct name, a module-qualified name, or a
type-constructor call — and each field pattern takes the forms of 5.1:18: the
shorthand `f`, the rename `f: b`, the mutable binding `mut f` or `f: mut b`,
and the discard `f: _`. A field pattern is a binding, never a nested pattern.
Struct patterns are a preview feature: a match expression containing one
**MUST** be compiled with `--preview struct_patterns` (8.4:1).

{{ rule(id="4.7:43", cat="legality-rule") }}

The head of a struct pattern **MUST** name a struct type (E0213), and that type
**MUST** be the type of the value the pattern is matched against (E0206): the
scrutinee's type for an arm's own pattern, and the payload field's type for a
pattern in a payload position (as a nested variant pattern resolves against its
field, 4.7:37). The field list is checked as 5.1:20 checks it: every declared
field named exactly once, with a missing field E0400, an unknown name E0401,
and a repeated field E0402. A scrutinee of struct type is legal only in a match
some arm of which is a struct pattern; a match over a struct value whose arms
are all `_` is E0602 as before.

{{ rule(id="4.7:44", cat="normative") }}

A struct pattern is irrefutable (4.7:3): it matches every value of its type. An
arm whose pattern is a struct pattern therefore makes the match exhaustive by
itself, and every arm after it is unreachable (4.7:18, 4.7:20); in a payload
position it covers every value of that position, as a binding does (4.7:39).
The arm binds as the let statements of 5.1:21 bind: the matched value — the
scrutinee, or the payload position moved out of its enclosing payload
(4.7:38) — is bound to an unnameable temporary of the arm, and each field
pattern in source order is `let b = t.f;`, `let mut b = t.f;`, or
`let _ = t.f;`, run before the arm body in the body's own scope. A Copy field is
copied, a move field is moved out of the temporary, a field whose type carries
a linear value cannot be discarded (E0478), and a move field of a struct that
has a destructor cannot be moved out (E0456, 3.9). Whatever the temporary still
owns at the end of the arm is dropped then, with the arm's other bindings, in
reverse declaration order (4.7:31).

{{ rule(id="4.7:45", cat="example") }}

```rue
struct Point { x: i32, y: i32 }
enum Shape { Dot(Point), Empty }

fn origin_distance(s: Shape) -> i32 {
    match s {
        Shape.Dot(Point { x, y: py }) => x + py,   // binds x and py
        Shape.Empty => 0,
    }
}

fn main() -> i32 {
    let p = Point { x: 40, y: 2 };
    match p {
        Point { mut x, y: _ } => { x = x + 2; x }  // y is discarded -> 42
    }
}
```
