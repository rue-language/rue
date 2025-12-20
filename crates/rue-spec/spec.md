# Rue Language Specification

This document describes the Rue programming language. Rue is a minimal, statically-typed language that compiles to native x86-64 ELF executables.

## 1. Lexical Elements

### 1.1 Integer Literals

An integer literal is a sequence of decimal digits representing a signed 64-bit integer.

**Syntax:**
```ebnf
integer = digit+ ;
digit   = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

**Semantics:**
- Integer literals evaluate to their numeric value
- Range: 0 to 2^63-1 (negative literals not yet supported)
- When returned from `main`, the value is truncated to 8 bits for the process exit code (i.e., `value & 0xFF`)

**Examples:**
```rue
fn main() -> i32 { 0 }    // exits with code 0
fn main() -> i32 { 42 }   // exits with code 42
fn main() -> i32 { 256 }  // exits with code 0 (256 & 0xFF = 0)
```

**See also:** [Test cases](cases/01-integers.toml)

### 1.2 Comments

Line comments start with `//` and extend to the end of the line.

**Syntax:**
```ebnf
comment = "//" { any character except newline } ;
```

**Semantics:**
- Comments are treated as whitespace
- Block comments (`/* */`) are not supported

**Examples:**
```rue
// This is a comment
fn main() -> i32 { 42 }  // trailing comment
```

**See also:** [Test cases](cases/02-comments.toml)

### 1.3 Whitespace

Whitespace (spaces, tabs, newlines) is ignored between tokens.

**Semantics:**
- Whitespace separates tokens but has no semantic meaning
- Multiple whitespace characters are equivalent to one
- Whitespace is required between keywords and identifiers

**See also:** [Test cases](cases/03-whitespace.toml)

## 2. Declarations

### 2.1 Functions

Functions are declared with the `fn` keyword.

**Syntax:**
```ebnf
function = "fn" identifier "(" ")" "->" type "{" expression "}" ;
identifier = letter ( letter | digit | "_" )* ;
letter = "a"..."z" | "A"..."Z" | "_" ;
type = "i32" ;
```

**Semantics:**
- The `main` function is the entry point of the program
- Return type annotation is required
- The function body is a single expression whose value is returned

**Examples:**
```rue
fn main() -> i32 { 42 }
```

**See also:** [Test cases](cases/04-functions.toml)

## 3. Errors

### 3.1 Compilation Errors

Certain programs are rejected by the compiler.

**Common errors:**
- Missing `main` function
- Negative integer literals (not yet supported)
- Unexpected characters
- Unterminated constructs (unclosed braces, etc.)

**See also:** [Test cases](cases/05-errors.toml)

## 8. Arrays

### 8.1 Fixed-Size Arrays

Arrays are fixed-size, homogeneous sequences of elements. The type `[T; N]` represents an array of `N` elements of type `T`.

**Syntax:**
```ebnf
array_type    = "[" type ";" integer "]" ;
array_literal = "[" [ expression { "," expression } ] "]" ;
index_expr    = expression "[" expression "]" ;
```

**Semantics:**
- Array length is part of the type: `[i32; 3]` is a different type from `[i32; 4]`
- Array elements are accessed via zero-based indexing: `arr[0]`, `arr[1]`, etc.
- Array literals must have exactly the number of elements specified in the type
- All elements must have the same type
- Mutable arrays (declared with `let mut`) allow element assignment
- Index expressions must evaluate to an integer type

**Examples:**
```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 12];
    arr[0] + arr[1] + arr[2]  // returns 42
}
```

```rue
fn main() -> i32 {
    let mut arr: [i32; 2] = [0, 0];
    arr[0] = 20;
    arr[1] = 22;
    arr[0] + arr[1]  // returns 42
}
```

**Compile-time errors:**
- Array length mismatch: `let arr: [i32; 3] = [1, 2]` (expected 3 elements, found 2)
- Element type mismatch: `let arr: [i32; 2] = [1, true]` (expected i32, found bool)
- Assignment to immutable array: `arr[0] = 5` where `arr` is not `mut`
- Index on non-array type: `let x = 5; x[0]`
- Empty array without type annotation: `let arr = []` (type annotation required)

**See also:** [Test cases](cases/19-arrays.toml)