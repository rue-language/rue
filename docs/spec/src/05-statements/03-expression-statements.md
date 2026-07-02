+++
title = "Expression Statements"
weight = 3
template = "spec/page.html"
+++

# Expression Statements

{{ rule(id="5.3:1", cat="normative") }}

An expression followed by a semicolon becomes an expression statement.

{{ rule(id="5.3:2", cat="normative") }}

```ebnf
expr_stmt = expression ";" ;
```

{{ rule(id="5.3:3", cat="normative") }}

The value of the expression is discarded. The type of an expression statement is `()`.

{{ rule(id="5.3:4") }}

```rue
fn side_effect() { }

fn main() -> i32 {
    side_effect();  // expression statement
    42
}
```

{{ rule(id="5.3:5") }}

Expression statements are useful for calling functions for their side effects while discarding their return values.

## Block-like expression statements

{{ rule(id="5.3:6", cat="normative") }}

A *block-like expression* is an `if`, `match`, `while`, `loop`, or block (`{ ... }`) expression: one whose syntax is terminated by a closing brace. When a block-like expression appears in statement position (as an item of a block, rather than as an operand within a larger expression), it forms a complete statement on its own and does not require a trailing semicolon.

{{ rule(id="5.3:7", cat="normative") }}

When a block-like expression in statement position is immediately followed by a `-`, the `-` begins a *new* statement — a unary negation — rather than continuing the block-like expression as the left operand of a subtraction. For example, in a block containing `if c { a } else { b }` immediately followed by `-x`, the `if` expression is one statement and `-x` is a separate statement whose `-` is unary negation, not the subtraction `(if c { a } else { b }) - x`. The `-` is singled out because it is the only operator that is both a unary (prefix) and a binary (infix) operator, so it is the only one whose meaning at this boundary is otherwise ambiguous. This rule applies only in statement position: a block-like expression used as an operand (for example, on the right-hand side of a `let` binding, as in `let n = if c { a } else { b } - x;`) participates in the surrounding subtraction as usual.

{{ rule(id="5.3:8") }}

```rue
fn main() -> i32 {
    if true { 999 } else { 888 }  // a complete statement; value discarded
    -1                            // a separate statement: unary negation
}
```
