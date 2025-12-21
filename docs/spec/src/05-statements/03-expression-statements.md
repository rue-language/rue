# Expression Statements

r[5.3.1#normative]
An expression followed by a semicolon becomes an expression statement.

r[5.3.2#normative]
```ebnf
expr_stmt = expression ";" ;
```

r[5.3.3#normative]
The value of the expression is discarded. The type of an expression statement is `()`.

r[5.3.4]
```rue
fn side_effect() { }

fn main() -> i32 {
    side_effect();  // expression statement
    42
}
```

r[5.3.5]
Expression statements are useful for calling functions for their side effects while discarding their return values.
