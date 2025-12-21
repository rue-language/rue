# Tokens

r[2.1.1#normative]
Tokens are the atomic units of syntax in a Rue program. The lexer processes source text and produces a sequence of tokens.

## Token Categories

r[2.1.2]
Rue tokens fall into the following categories:

| Category | Examples |
|----------|----------|
| Keywords | `fn`, `let`, `mut`, `if`, `else`, `while`, `match`, `return`, `break`, `continue`, `true`, `false` |
| Identifiers | `main`, `x`, `my_var`, `_unused` |
| Integer literals | `0`, `42`, `255`, `2147483647` |
| String literals | `"hello"`, `"world"`, `"with \"escapes\""` |
| Operators | `+`, `-`, `*`, `/`, `%`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `\|\|`, `!` |
| Delimiters | `(`, `)`, `{`, `}`, `[`, `]`, `,`, `;`, `:`, `->`, `=>` |

## Integer Literals

r[2.1.3#normative]
An integer literal is a sequence of decimal digits.

```ebnf
integer_literal = digit { digit } ;
digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

r[2.1.4#normative]
Integer literals must be representable in their target type. An unadorned integer literal defaults to type `i32`.

r[2.1.5]
```rue
fn main() -> i32 {
    0        // zero
    42       // decimal integer
    255      // maximum u8 value
}
```

## String Literals

r[2.1.6#normative]
A string literal is a sequence of characters enclosed in double quotes (`"`).

```ebnf
string_literal = '"' { string_char } '"' ;
string_char = any_char_except_quote_or_backslash | escape_sequence ;
escape_sequence = "\\" | "\"" ;
```

r[2.1.7#normative]
String literals support escape sequences: `\\` for a backslash and `\"` for a double quote.

r[2.1.8#normative]
An invalid escape sequence in a string literal is a compile-time error.

r[2.1.9]
```rue
fn main() -> i32 {
    let a = "hello world";
    let b = "with \"quotes\"";
    let c = "with \\ backslash";
    0
}
```

## Identifiers

r[2.1.10#normative]
An identifier starts with a letter or underscore, followed by any number of letters, digits, or underscores.

```ebnf
identifier = (letter | "_") { letter | digit | "_" } ;
letter = "a" | ... | "z" | "A" | ... | "Z" ;
```

r[2.1.11#normative]
Identifiers cannot be keywords. The identifier `_` (single underscore) is special and indicates an unused binding.

r[2.1.12]
```rue
fn main() -> i32 {
    let x = 1;
    let my_variable = 2;
    let _unused = 3;
    let x1 = 4;
    x + my_variable + x1
}
```
