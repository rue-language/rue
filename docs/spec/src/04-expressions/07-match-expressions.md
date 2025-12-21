# Match Expressions

r[4.7.1#normative]
A match expression provides multi-way branching based on pattern matching.

r[4.7.2#normative]
```ebnf
match_expr = "match" expression "{" { match_arm "," } [ match_arm ] "}" ;
match_arm = pattern "=>" expression ;
pattern = "_" | INTEGER | BOOL ;
```

## Patterns

r[4.7.3#normative]
Integer literal patterns match a specific integer value.

r[4.7.4#normative]
Boolean literal patterns (`true`, `false`) match specific boolean values.

r[4.7.5#normative]
The wildcard pattern `_` matches any value.

## Exhaustiveness

r[4.7.6#normative]
Match expressions must be exhaustive: they must cover all possible values of the scrutinee type.

r[4.7.7#normative]
For integer scrutinees, a wildcard pattern is required to ensure exhaustiveness.

r[4.7.8#normative]
For boolean scrutinees, either both `true` and `false` must be covered, or a wildcard must be present.

r[4.7.9]
```rue
fn main() -> i32 {
    match 2 {
        1 => 10,
        2 => 20,
        _ => 0,  // required for integer matches
    }
}
```

## Type Checking

r[4.7.10#normative]
All match arms must have the same type. The type of the match expression is the type of its arms.

r[4.7.11#normative]
Pattern types must be compatible with the scrutinee type.

## Arm Bodies

r[4.7.12#normative]
Match arm bodies can be simple expressions or block expressions.

r[4.7.13]
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

r[4.7.14#normative]
Arms are evaluated in order. The first arm whose pattern matches the scrutinee is executed, and its body becomes the value of the match expression.
