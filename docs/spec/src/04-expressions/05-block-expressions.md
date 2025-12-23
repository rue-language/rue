# Block Expressions

r[4.5:1#normative]
A block expression is a sequence of statements followed by an optional expression, enclosed in braces.

r[4.5:2#normative]
```ebnf
block_expr = "{" { statement } [ expression ] "}" ;
```

r[4.5:3#normative]
The type of a block expression is the type of its final expression. If the block ends with a statement, the type is `()`.

r[4.5:4#normative]
Variables declared in a block are local to that block and shadow any outer variables with the same name.

r[4.5:5]
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

r[4.5:6#normative]
When a block exits, all local variables declared in that block are destroyed.
