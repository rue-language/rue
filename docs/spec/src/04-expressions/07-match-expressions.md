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
pattern = "_" | INTEGER | BOOL | enum_variant_pattern ;
enum_variant_pattern = IDENT "::" IDENT ;
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

A match expression shall have an exhaustive set of patterns for its scrutinee type.
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

All match arms shall have the same type. The type of the match expression is the common type of its arms.

{{ rule(id="4.7:13", cat="normative") }}

The type of each pattern shall be compatible with the type of the scrutinee.
A pattern with an incompatible type is rejected with a compile-time error.

## Arm Bodies

{{ rule(id="4.7:14", cat="normative") }}

Match arm bodies may be simple expressions or block expressions.

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

{{ rule(id="4.7:16", cat="normative") }}

Arms are evaluated in order. The first arm whose pattern matches the scrutinee value
is selected, and its body expression is evaluated. The result of that evaluation
becomes the value of the match expression.

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
well-formed and the unreachable arm is not executed at runtime.

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
