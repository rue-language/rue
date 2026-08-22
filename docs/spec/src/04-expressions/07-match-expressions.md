+++
title = "Match Expressions"
weight = 7
template = "spec/page.html"
+++

# Match Expressions

{{ rule(id="4.7:1", cat="normative") }}

A match expression provides multi-way branching based on pattern matching.

{{ rule(id="4.7:2", cat="normative") }}

```ebnf
match_expr = "match" expression "{" { match_arm "," } [ match_arm ] "}" ;
match_arm = pattern "=>" expression ;
pattern = "_" | [ "-" ] INTEGER | BOOL | enum_variant_pattern ;
enum_variant_pattern = IDENT "." IDENT ;
```

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

A tuple-variant pattern binds the variant's payload into fresh names:
`EnumName.Variant(a, b)` matches a value of that variant and binds `a`, `b`
to its payload fields in order. A binding position may instead be the wildcard
`_`, which matches and discards that field without binding it; unlike a name it
introduces nothing and so may repeat (`Rect(_, _)`). The number of binding
positions **MUST** equal the variant's payload arity (see spec 6.3), with one
carve-out: a *bare* variant pattern that supplies no binding list at all —
`EnumName.Variant` on a variant of arity one or more — **is** the all-wildcard
form `EnumName.Variant(_, ..., _)` and is therefore exempt from the arity rule
(4.7:34 says the same value-context consumption applies to it).

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
