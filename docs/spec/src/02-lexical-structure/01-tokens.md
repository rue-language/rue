# Tokens

r[2.1:1#normative]
Tokens are the atomic units of syntax in a Rue program. The lexer processes source text and produces a sequence of tokens.

## Token Categories

r[2.1:2]
Rue tokens fall into the following categories:

| Category | Examples |
|----------|----------|
| Keywords | `fn`, `let`, `mut`, `if`, `else`, `while`, `match`, `return`, `break`, `continue`, `true`, `false` |
| Identifiers | `main`, `x`, `my_var`, `_unused` |
| Integer literals | `0`, `42`, `255`, `2147483647` |
| String literals | `"hello"`, `"world"`, `"with \"escapes\""` |
| Operators | `+`, `-`, `*`, `/`, `%`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `\|\|`, `!`, `&`, `\|`, `^`, `~`, `<<`, `>>` |
| Delimiters | `(`, `)`, `{`, `}`, `[`, `]`, `,`, `;`, `:`, `->`, `=>` |

## Integer Literals

r[2.1:3#normative]
An integer literal is a sequence of decimal digits.

```ebnf
integer_literal = digit { digit } ;
digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

r[2.1:4#normative]
Integer literals must be representable in their target type. An unadorned integer literal defaults to type `i32`.

r[2.1:5]
```rue
fn main() -> i32 {
    0        // zero
    42       // decimal integer
    255      // maximum u8 value
}
```

## String Literals

r[2.1:6#normative]
A string literal is a sequence of characters enclosed in double quotes (`"`).

```ebnf
string_literal = '"' { string_char } '"' ;
string_char = any_char_except_quote_or_backslash | escape_sequence ;
escape_sequence = "\\" | "\"" ;
```

r[2.1:7#normative]
String literals support escape sequences: `\\` for a backslash and `\"` for a double quote.

r[2.1:8#normative]
An invalid escape sequence in a string literal is a compile-time error.

r[2.1:9]
```rue
fn main() -> i32 {
    let a = "hello world";
    let b = "with \"quotes\"";
    let c = "with \\ backslash";
    0
}
```

## Identifiers

r[2.1:10#normative]
An identifier starts with a letter or underscore, followed by any number of letters, digits, or underscores.

```ebnf
identifier = (letter | "_") { letter | digit | "_" } ;
letter = "a" | ... | "z" | "A" | ... | "Z" ;
```

r[2.1:11#normative]
Identifiers cannot be keywords.

## Underscore Identifier

r[2.1:12#normative]
The identifier `_` (single underscore) is a *wildcard* that discards its value without creating a binding. When used in a let statement, the initializer expression is evaluated for its side effects, but no variable is created and no storage is allocated.

r[2.1:13#normative]
A reference to `_` as an expression is a compile-time error. The wildcard identifier cannot be used to retrieve a previously discarded value.

r[2.1:14#normative]
Multiple occurrences of `_` are permitted in the same scope. Each occurrence independently discards its value.

r[2.1:15]
```rue
fn main() -> i32 {
    let _ = 42;       // discards 42, no binding created
    let _ = 100;      // discards 100, no conflict with previous _
    0
}
```

## Underscore-Prefixed Identifiers

r[2.1:16#normative]
An identifier that begins with an underscore followed by one or more characters (e.g., `_unused`, `_x`) is a normal identifier that creates a binding. Such identifiers suppress unused variable warnings but can otherwise be used like any other identifier.

r[2.1:17]
```rue
fn main() -> i32 {
    let x = 1;
    let my_variable = 2;
    let _unused = 3;      // suppresses unused warning, but is a normal variable
    let x1 = 4;
    x + my_variable + _unused + x1
}
```
