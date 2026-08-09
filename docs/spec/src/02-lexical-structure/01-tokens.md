+++
title = "Tokens"
weight = 1
template = "spec/page.html"
+++

# Tokens

{{ rule(id="2.1:1", cat="normative") }}

Tokens are the atomic units of syntax in a Rue program. The text of a source file is decomposed into a sequence of tokens (2.0:1).

## Token Categories

{{ rule(id="2.1:2") }}

Rue tokens fall into the following categories:

| Category | Examples |
|----------|----------|
| Keywords | `fn`, `let`, `mut`, `if`, `else`, `while`, `match`, `return`, `break`, `continue`, `true`, `false` |
| Identifiers | `main`, `x`, `my_var`, `_unused` |
| Integer literals | `0`, `42`, `1_000_000`, `0xFF`, `0o17`, `0b1010` |
| Byte literals | `b'a'`, `b'0'`, `b'\n'`, `b'\''` |
| String literals | `"hello"`, `"world"`, `"with \"escapes\""` |
| Operators | `+`, `-`, `*`, `/`, `%`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `\|\|`, `!`, `&`, `\|`, `^`, `~`, `<<`, `>>` |
| Compound assignment | `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `\|=`, `^=`, `<<=`, `>>=` |
| Delimiters | `(`, `)`, `{`, `}`, `[`, `]`, `,`, `;`, `:`, `->`, `=>` |

## Integer Literals

{{ rule(id="2.1:3", cat="normative") }}

An integer literal is a decimal literal, a hexadecimal literal (prefix `0x`), an octal literal (prefix `0o`), or a binary literal (prefix `0b`). A decimal literal begins with a decimal digit; a based literal begins with its lowercase base prefix and contains at least one digit of that base. Hexadecimal digits are case-insensitive: `0xff`, `0xFF`, and `0xfF` denote the same value.

```ebnf
integer_literal = dec_literal | hex_literal | oct_literal | bin_literal ;
dec_literal = dec_digit { dec_digit | "_" } ;
hex_literal = "0x" { hex_digit | "_" } ;   (* at least one hex_digit *)
oct_literal = "0o" { oct_digit | "_" } ;   (* at least one oct_digit *)
bin_literal = "0b" { bin_digit | "_" } ;   (* at least one bin_digit *)
dec_digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
oct_digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" ;
bin_digit = "0" | "1" ;
hex_digit = dec_digit | "a" | ... | "f" | "A" | ... | "F" ;
```

{{ rule(id="2.1:4", cat="normative") }}

Integer literals **MUST** be representable in their target type. An unadorned integer literal defaults to type `i32`.

{{ rule(id="2.1:19", cat="normative") }}

Underscores (`_`) may appear as digit separators anywhere among the digits of an integer literal — including immediately after a base prefix and trailing — and have no effect on the literal's value. An integer literal cannot *begin* with an underscore: a token such as `_1` is an identifier, not a literal.

{{ rule(id="2.1:20", cat="legality-rule") }}

A base prefix with no digits after it (e.g. `0x`, `0b_`) is a compile-time error.

{{ rule(id="2.1:21", cat="legality-rule") }}

A digit that is not valid in the literal's base (e.g. `0b2`, `0o9`, `0xG`) is a compile-time error.

{{ rule(id="2.1:22", cat="legality-rule") }}

Base prefixes are lowercase. An uppercase base prefix (`0X`, `0O`, `0B`) is a compile-time error.

{{ rule(id="2.1:5") }}

```rue
fn main() -> i32 {
    @dbg(0);            // zero
    @dbg(42);           // decimal integer
    @dbg(255);          // maximum u8 value
    @dbg(1_000_000);    // underscore separators
    @dbg(0xFF);         // hexadecimal, value 255
    @dbg(0x_FF_);       // underscores legal after the prefix and trailing
    @dbg(0o17);         // octal, value 15
    @dbg(0b1010);       // binary, value 10
    0
}
```

## Byte Literals

{{ rule(id="2.1:26", cat="normative") }}

A byte literal is written `b'c'`, where `c` is a single ASCII character (other than `'` or `\`) or an escape sequence, and denotes the integer value of that byte (0–255). A byte literal *is* an integer literal: the integer-literal typing and representability rules (2.1:4) apply, so `b'a'` and `97` are interchangeable and a byte literal is typically written where a `u8` is expected. Byte literals accept the string-literal escape sequences (2.1:7) and additionally `\'` (a single quote).

```ebnf
byte_literal = "b'" ( byte_char | escape_sequence | "\'" ) "'" ;
byte_char = ? any ASCII character except "'" or "\" ? ;
```

{{ rule(id="2.1:27", cat="legality-rule") }}

A byte literal that is empty (`b''`), contains more than one byte, contains a non-ASCII character, uses an unknown escape sequence, or is unterminated (reaching end-of-file or end-of-line before the closing `'`) is a compile-time error.

{{ rule(id="2.1:28") }}

```rue
fn is_digit(c: u8) -> bool {
    c >= b'0' && c <= b'9'   // b'0' is 48, b'9' is 57
}

fn main() -> i32 {
    let newline: u8 = b'\n';   // 10
    let quote: u8 = b'\'';     // 39
    0
}
```

## String Literals

{{ rule(id="2.1:6", cat="normative") }}

A string literal is a sequence of characters enclosed in double quotes (`"`).

```ebnf
string_literal = '"' { string_char } '"' ;
string_char = any_char_except_quote_or_backslash | escape_sequence ;
escape_sequence = "\\" | "\"" | "\n" | "\t" | "\r" | "\0" ;
```

{{ rule(id="2.1:7", cat="normative") }}

String literals support the following escape sequences:

| Escape | Character |
|--------|-----------|
| `\\` | Backslash |
| `\"` | Double quote |
| `\n` | Newline (line feed, U+000A) |
| `\t` | Horizontal tab (U+0009) |
| `\r` | Carriage return (U+000D) |
| `\0` | Null character (U+0000) |

{{ rule(id="2.1:8", cat="legality-rule") }}

An invalid escape sequence in a string literal is a compile-time error.

{{ rule(id="2.1:9", cat="legality-rule") }}

An unterminated string literal (reaching end-of-file or end-of-line without a closing quote) is a compile-time error.

{{ rule(id="2.1:10") }}

```rue
fn main() -> i32 {
    let a = "hello world";
    let b = "with \"quotes\"";
    let c = "with \\ backslash";
    let d = "line1\nline2";   // newline
    let e = "col1\tcol2";     // tab
    0
}
```

## Identifiers

{{ rule(id="2.1:11", cat="normative") }}

An identifier starts with a letter or underscore, followed by any number of letters, digits, or underscores.

```ebnf
identifier = (letter | "_") { letter | digit | "_" } ;
letter = "a" | ... | "z" | "A" | ... | "Z" ;
```

{{ rule(id="2.1:12", cat="normative") }}

Identifiers cannot be keywords.

## Underscore Identifier

{{ rule(id="2.1:13", cat="normative") }}

The identifier `_` (single underscore) is a *wildcard* that discards its value without creating a binding. When used in a let statement, the initializer expression is evaluated for its side effects, but no variable is created and no storage is allocated.

{{ rule(id="2.1:14", cat="normative") }}

A reference to `_` as an expression is a compile-time error. The wildcard identifier cannot be used to retrieve a previously discarded value.

{{ rule(id="2.1:15", cat="normative") }}

Multiple occurrences of `_` are permitted in the same scope. Each occurrence independently discards its value.

{{ rule(id="2.1:16") }}

```rue
fn main() -> i32 {
    let _ = 42;       // discards 42, no binding created
    let _ = 100;      // discards 100, no conflict with previous _
    0
}
```

## Underscore-Prefixed Identifiers

{{ rule(id="2.1:17", cat="normative") }}

An identifier that begins with an underscore followed by one or more characters (e.g., `_unused`, `_x`) is a normal identifier that creates a binding. Such identifiers suppress unused variable warnings but can otherwise be used like any other identifier.

{{ rule(id="2.1:18") }}

```rue
fn main() -> i32 {
    let x = 1;
    let my_variable = 2;
    let _unused = 3;      // suppresses unused warning, but is a normal variable
    let x1 = 4;
    x + my_variable + _unused + x1
}
```

## Comma-Separated Lists

{{ rule(id="2.1:23", cat="normative") }}

Every comma-separated list in the grammar accepts a single optional *trailing
comma* after its final element. This applies uniformly to all list forms —
call arguments, intrinsic and `@import` arguments, function and method
parameters, array-literal elements, struct-literal field initializers, struct
and enum declaration fields, enum tuple-variant payloads, match arms, and
pattern bindings. A trailing comma is accepted only after at least one element;
it has no semantic effect and never changes the number of elements. An empty
list (`f()`, `[]`) followed by a comma remains a syntax error, because there is
no final element for the comma to follow.

{{ rule(id="2.1:24") }}

```rue
fn add(a: i32, b: i32,) -> i32 {   // trailing comma in parameters
    a + b
}
fn main() -> i32 {
    let xs = [1, 2, 3,];           // trailing comma in an array literal
    add(xs[0], xs[1],)             // trailing comma in call arguments
}
```

## Symbol Interning in the Reference Token Dump

{{ rule(id="2.1:25", cat="informative") }}

The reference implementation's token dump (`rue --emit tokens`) prints identifier and string tokens as `IDENT(sym:N)` and `STRING(sym:N)`, where `N` indexes a single symbol table shared by identifiers and string literals and keyed by *decoded* value: the string literal `"ab"` and the identifier `ab` receive the same symbol, and an escape such as `"\t"` interns identically to a string containing a literal tab. Symbols are numbered in first-occurrence order over the token stream. Primitive type names print as `TYPE(name)` and are not interned, while `usize` and `isize` lex as ordinary identifiers. These are properties of the reference tooling's presentation, not language semantics; they are recorded here so a second implementation can reproduce the dump byte-for-byte without reverse-engineering it.
