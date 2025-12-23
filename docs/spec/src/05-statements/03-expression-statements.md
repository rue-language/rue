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
