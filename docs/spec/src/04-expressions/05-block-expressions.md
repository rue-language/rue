+++
title = "Block Expressions"
weight = 5
template = "spec/page.html"
+++

# Block Expressions

{{ rule(id="4.5:1", cat="normative") }}

A block expression is a sequence of statements followed by an optional expression, enclosed in braces.

{{ rule(id="4.5:2", cat="normative") }}

```ebnf
block_expr = "{" { statement } [ expression ] "}" ;
```

{{ rule(id="4.5:3", cat="normative") }}

The type of a block expression is the type of its final expression. If the block ends with a statement, the type is `()`.

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

{{ rule(id="4.5:6", cat="normative") }}

When a block exits, all local variables declared in that block are destroyed.
