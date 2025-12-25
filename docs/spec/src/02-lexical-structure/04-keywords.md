+++
title = "Keywords and Reserved Words"
weight = 4
template = "spec/page.html"
+++

# Keywords and Reserved Words

{{ rule(id="2.4:1", cat="normative") }}

Keywords are reserved words that have special meaning in the language.

## Keywords

{{ rule(id="2.4:2", cat="normative") }}

The following words are keywords and cannot be used as identifiers:

| Keyword | Description |
|---------|-------------|
| `fn` | Function declaration |
| `let` | Variable binding |
| `mut` | Mutable binding modifier |
| `if` | Conditional expression |
| `else` | Alternative branch |
| `while` | While loop expression |
| `loop` | Infinite loop expression |
| `match` | Pattern matching expression |
| `return` | Return from function |
| `break` | Exit loop |
| `continue` | Skip to next iteration |
| `true` | Boolean literal |
| `false` | Boolean literal |
| `struct` | Struct definition |
| `enum` | Enum definition |
| `impl` | Impl block |
| `self` | Self parameter in methods |
| `drop` | Destructor declaration |

## Type Names

{{ rule(id="2.4:3", cat="normative") }}

The following are type names and are reserved:

| Type | Description |
|------|-------------|
| `i8` | 8-bit signed integer |
| `i16` | 16-bit signed integer |
| `i32` | 32-bit signed integer |
| `i64` | 64-bit signed integer |
| `u8` | 8-bit unsigned integer |
| `u16` | 16-bit unsigned integer |
| `u32` | 32-bit unsigned integer |
| `u64` | 64-bit unsigned integer |
| `bool` | Boolean type |
