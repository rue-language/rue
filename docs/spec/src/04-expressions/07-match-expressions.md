# Match Expressions

r[4.7.1#normative]
A match expression provides multi-way branching based on pattern matching.

r[4.7.2#normative]
```ebnf
match_expr = "match" expression "{" { match_arm "," } [ match_arm ] "}" ;
match_arm = pattern "=>" expression ;
pattern = "_" | INTEGER | BOOL | enum_variant_pattern ;
enum_variant_pattern = IDENT "::" IDENT ;
```

## Patterns

r[4.7.3#normative]
A pattern is *irrefutable* if it matches any value of its type.
A pattern is *refutable* if there exist values of its type that it does not match.

r[4.7.4#normative]
The wildcard pattern `_` is irrefutable. It matches any value.

r[4.7.5#normative]
An integer literal pattern is refutable. It matches only the specific integer value it denotes.

r[4.7.6#normative]
A boolean literal pattern (`true` or `false`) is refutable. It matches only the specific boolean value it denotes.

r[4.7.7#normative]
An enum variant pattern is refutable. It matches only values of that specific variant.

## Exhaustiveness

r[4.7.8#normative]
A set of patterns is *exhaustive* for a type if every possible value of that type
is matched by at least one pattern in the set.

r[4.7.9#normative]
A match expression shall have an exhaustive set of patterns for its scrutinee type.
A match expression with a non-exhaustive pattern set is rejected with a compile-time error.

r[4.7.10#normative]
The following rules determine whether a pattern set is exhaustive:

1. Any pattern set containing an irrefutable pattern is exhaustive.
2. For type `bool`: a pattern set containing both `true` and `false` is exhaustive.
3. For an enum type: a pattern set containing a pattern for every variant of that enum is exhaustive.
4. For integer types: only rule (1) applies; explicit enumeration of integer values is not sufficient to establish exhaustiveness.

r[4.7.11]
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

r[4.7.12#normative]
All match arms shall have the same type. The type of the match expression is the common type of its arms.

r[4.7.13#normative]
The type of each pattern shall be compatible with the type of the scrutinee.
A pattern with an incompatible type is rejected with a compile-time error.

## Arm Bodies

r[4.7.14#normative]
Match arm bodies may be simple expressions or block expressions.

r[4.7.15]
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

r[4.7.16#normative]
Arms are evaluated in order. The first arm whose pattern matches the scrutinee value
is selected, and its body expression is evaluated. The result of that evaluation
becomes the value of the match expression.
