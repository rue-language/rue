+++
title = "Block Expressions"
weight = 5
template = "spec/page.html"
+++

# Block Expressions

{{ rule(id="4.5:1", cat="normative") }}

A block expression is a sequence of statements followed by an optional expression, enclosed in braces.

{{ rule(id="4.5:2", cat="syntax") }}

```ebnf
block_expr = "{" { statement } [ expression ] "}" ;
```

{{ rule(id="4.5:3", cat="normative") }}

A block expression with a final expression evaluates to that expression's value, and its type is that expression's type. (A block elaborates to a `let`/sequence chain, and a sequence evaluates to its tail — core calculus `docs/formal/01-core-calculus.md` §4.3, §6.7.)

{{ rule(id="4.5:7", cat="normative") }}

A block expression with no final expression — one that ends with a statement, or is empty — evaluates to `()` and has type `()`.

{{ rule(id="4.5:4", cat="normative") }}

Variables declared in a block are local to that block and shadow any outer variables with the same name.

{{ rule(id="4.5:5") }}

```rue
fn main() -> i32 {
    let x = 1;
    let y = {
        let x = 10;  // shadows outer x
        x + 5
    };
    x + y  // 1 + 15 = 16
}
```

{{ rule(id="4.5:6", cat="dynamic-semantics") }}

When a block exits, the live bindings declared in it are dropped, newest-first (3.9:4, 3.9:18; core calculus `docs/formal/01-core-calculus.md` §6.7, rule `(D-EndScope)`).
