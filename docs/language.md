# Rue Language Reference

This document provides an overview of the Rue programming language. For the authoritative specification with test cases, see `crates/rue-spec/cases/`.

## Overview

Rue is a systems programming language aiming for:
- Memory safety without garbage collection
- Higher-level ergonomics than Rust/Zig
- Rust-like syntax (initially)

## Current Status

Rue is in early development. The implemented feature set is minimal:

| Feature | Status |
|---------|--------|
| Integer literals | ✓ Implemented |
| Boolean literals | ✓ Implemented |
| Functions | ✓ With parameters and calls |
| Line comments | ✓ Implemented |
| Arithmetic operators | ✓ Implemented |
| Comparison operators | ✓ Implemented |
| Variables | ✓ Implemented |
| If/else expressions | ✓ Implemented |
| Match expressions | ✓ Implemented |
| Logical operators | ✓ Implemented |
| While loops | ✓ Implemented |
| Break/continue | ✓ Implemented |
| Return statement | ✓ Implemented |
| Arrays | ✓ Fixed-size arrays with value semantics |

## Specification Tests

The executable specification lives in `crates/rue-spec/cases/`:

| File | Section | Description |
|------|---------|-------------|
| `01-integers.toml` | 1.1 | Integer literals and exit codes |
| `02-comments.toml` | 1.3 | Line comments |
| `03-whitespace.toml` | 1.4 | Whitespace handling |
| `04-functions.toml` | 2.1 | Function declarations, parameters, and calls |
| `05-errors.toml` | 3.1 | Compilation errors |
| `06-ir-dumps.toml` | 4.1 | IR output golden tests |
| `07-error-golden.toml` | 5.1 | Error message golden tests |
| `08-arithmetic.toml` | 1.2 | Arithmetic operators |
| `09-variables.toml` | 2.2 | Local variables |
| `10-conditionals.toml` | 10.1 | If/else expressions and comparisons |
| `11-logical-operators.toml` | 11.1 | Logical operators (!, &&, \|\|) |
| `11-match.toml` | 11.1 | Match expressions |
| `12-while-loops.toml` | 12.1 | While loops |
| `13-break-continue.toml` | 13.1 | Break and continue statements |
| `14-return.toml` | 14.1 | Return statement |
| `19-arrays.toml` | 8.1 | Fixed-size arrays |

Each `.toml` file contains test cases that define expected behavior:

```toml
[[case]]
name = "simple_return"
source = "fn main() -> i32 { 42 }"
exit_code = 42
```

Run specs with `./test.sh` or directly with `buck2 run //crates/rue-spec:rue-spec`.

## Quick Reference

### Minimal Program

```rue
fn main() -> i32 {
    0
}
```

### Functions

Functions are declared with the `fn` keyword, followed by the function name, parameters in parentheses, a return type, and a body:

```rue
fn add(x: i32, y: i32) -> i32 {
    x + y
}

fn main() -> i32 {
    add(40, 2)
}
```

Parameters must have explicit type annotations. The return type is also required.

Functions can call other functions:

```rue
fn double(x: i32) -> i32 {
    x + x
}

fn quadruple(x: i32) -> i32 {
    double(double(x))
}

fn main() -> i32 {
    quadruple(10)  // returns 40
}
```

Functions must be defined before `main`. The `main` function is the program's entry point and must return `i32`.

Recursion is supported:

```rue
fn factorial(n: i32) -> i32 {
    if n <= 1 { 1 }
    else { n * factorial(n - 1) }
}

fn main() -> i32 {
    factorial(5)  // returns 120
}
```

### With Comments

```rue
// A comment
fn main() -> i32 {
    42  // inline comment
}
```

### Arithmetic

```rue
fn main() -> i32 {
    1 + 2 * 3  // = 7 (multiplication binds tighter)
}
```

```rue
fn main() -> i32 {
    (1 + 2) * 3  // = 9 (parentheses override)
}
```

```rue
fn main() -> i32 {
    -42  // unary negation
}
```

### Variables

```rue
fn main() -> i32 {
    let x = 40;
    let y = 2;
    x + y
}
```

Variables are immutable by default. Use `let mut` for mutable bindings:

```rue
fn main() -> i32 {
    let mut counter = 0;
    counter = counter + 1;
    counter
}
```

Type annotations are optional:

```rue
fn main() -> i32 {
    let x: i32 = 42;
    x
}
```

Shadowing is allowed:

```rue
fn main() -> i32 {
    let x = 10;
    let x = x + 5;  // shadows previous x
    x  // returns 15
}
```

### Arrays

Arrays are fixed-size sequences of elements of the same type. The syntax `[T; N]` declares an array of `N` elements of type `T`:

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 12];
    arr[0] + arr[1] + arr[2]  // returns 42
}
```

Array elements are accessed using index syntax `arr[index]`:

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [100, 42, 200];
    arr[1]  // returns 42
}
```

Mutable arrays allow element assignment:

```rue
fn main() -> i32 {
    let mut arr: [i32; 2] = [0, 0];
    arr[0] = 20;
    arr[1] = 22;
    arr[0] + arr[1]  // returns 42
}
```

Array indices can be variables or expressions:

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 42, 100];
    let idx = 1;
    arr[idx]  // returns 42
}
```

```rue
fn main() -> i32 {
    let arr: [i32; 5] = [5, 10, 42, 100, 200];
    let base = 1;
    arr[base + 1]  // returns 42
}
```

Array elements can be initialized with expressions:

```rue
fn double(x: i32) -> i32 { x * 2 }

fn main() -> i32 {
    let arr: [i32; 1] = [double(21)];
    arr[0]  // returns 42
}
```

**Compile-time checks:**
- Array length must match the declared size
- All elements must have the same type
- Immutable arrays cannot be assigned to
- Constant indices are bounds-checked at compile time

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2];     // ERROR: expected array of 3 elements
    let arr: [i32; 2] = [1, true];  // ERROR: type mismatch
    let arr: [i32; 1] = [42];
    arr[0] = 10;                    // ERROR: cannot assign to immutable array
    0
}
```

**Bounds checking:**

Array indices are checked against the array length. For constant indices, this check happens at compile time:

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    arr[5]  // ERROR: index out of bounds: the length is 3 but the index is 5
}
```

For variable indices, bounds checking happens at runtime:

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    let idx = 5;
    arr[idx]  // Runtime error: index out of bounds (exit code 101)
}
```

Negative indices are also caught (they're treated as large unsigned values):

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    arr[-1]  // ERROR: index out of bounds
}
```

### Types

Rue supports the following primitive types:

| Type | Description | Literals |
|------|-------------|----------|
| `i8` | 8-bit signed integer | `0`, `42`, `-17` |
| `i16` | 16-bit signed integer | `0`, `42`, `-17` |
| `i32` | 32-bit signed integer | `0`, `42`, `-17` |
| `i64` | 64-bit signed integer | `0`, `42`, `-17` |
| `u8` | 8-bit unsigned integer | `0`, `42`, `255` |
| `u16` | 16-bit unsigned integer | `0`, `42` |
| `u32` | 32-bit unsigned integer | `0`, `42` |
| `u64` | 64-bit unsigned integer | `0`, `42` |
| `bool` | Boolean | `true`, `false` |

Integer literals without a type annotation default to `i32`.

Type annotations are optional when the type can be inferred:

```rue
fn main() -> i32 {
    let x = 42;        // inferred as i32
    let flag = true;   // inferred as bool
    let y: i32 = 10;   // explicit annotation
    let big: i64 = 1000000;  // 64-bit integer
    x + y
}
```

### Operators

Operators by precedence (highest to lowest):
1. `-` (unary negation), `!` (logical not)
2. `*`, `/`, `%` (multiplicative)
3. `+`, `-` (additive)
4. `==`, `!=`, `<`, `>`, `<=`, `>=` (comparison)
5. `&&` (logical and)
6. `||` (logical or)

All binary operators are left-associative: `10 - 3 - 2` equals `5` (not `9`).

Comparison operators return `bool` and use bidirectional type inference:

```rue
fn main() -> i32 {
    let a = 1 == 1;    // true
    let b = 2 < 3;     // true
    let c = 5 >= 5;    // true
    if a { 1 } else { 0 }
}
```

The type of a comparison is inferred from the left operand, and the right operand is checked against it:

```rue
fn main() -> i32 {
    let x: i64 = 100;
    if x == 100 { 1 } else { 0 }  // 100 is inferred as i64
}
```

Equality operators (`==`, `!=`) work on both integers and booleans. Ordering operators (`<`, `>`, `<=`, `>=`) only work on integers:

```rue
fn main() -> i32 {
    let a = true == false;  // ok: bool equality
    let b = 10 < 20;        // ok: integer ordering
    // let c = true < false; // error: ordering not allowed on bool
    if a { 0 } else { 1 }
}
```

Both operands must have the same type:

```rue
fn main() -> i32 {
    let x: i64 = 100;
    let y: i32 = 100;
    // if x == y { 1 } else { 0 }  // error: type mismatch
    0
}
```

### Logical Operators

Logical operators work on `bool` values:

```rue
fn main() -> i32 {
    let a = !false;           // true (negation)
    let b = true && true;     // true (and)
    let c = false || true;    // true (or)
    if a && b && c { 1 } else { 0 }
}
```

`&&` binds tighter than `||`, so `a || b && c` means `a || (b && c)`:

```rue
fn main() -> i32 {
    // true || false && false  =>  true || (false && false)  =>  true
    if true || false && false { 1 } else { 0 }
}
```

`&&` and `||` use short-circuit evaluation: the right operand is only evaluated if needed.

### Conditionals

If/else is an expression that returns a value:

```rue
fn main() -> i32 {
    let x = if true { 42 } else { 0 };
    x
}
```

Both branches must have the same type:

```rue
fn main() -> i32 {
    let n = 5;
    if n > 0 { 100 } else { 0 }
}
```

If/else can be nested:

```rue
fn main() -> i32 {
    let x = 5;
    if x < 3 { 1 }
    else { if x < 7 { 2 }
    else { 3 } }
}
```

The condition must be of type `bool`. Integer values are not implicitly converted to booleans:

```rue
fn main() -> i32 {
    if 1 { 42 } else { 0 }  // ERROR: expected bool, found i32
}
```

### Match Expressions

Match expressions provide multi-way branching on values:

```rue
fn main() -> i32 {
    match 2 {
        1 => 10,
        2 => 20,
        _ => 0,
    }
}
```

Match is an expression that returns a value. All arms must have the same type:

```rue
fn main() -> i32 {
    let x = 3;
    let result = match x {
        1 => 100,
        2 => 200,
        _ => 0,
    };
    result
}
```

#### Patterns

Match currently supports three kinds of patterns:

| Pattern | Description | Example |
|---------|-------------|---------|
| Integer literal | Matches a specific integer value | `1`, `42`, `0` |
| Boolean literal | Matches `true` or `false` | `true`, `false` |
| Wildcard | Matches any value | `_` |

The wildcard pattern `_` is a catch-all that matches any value not covered by previous arms.

#### Exhaustiveness Checking

Match expressions must be exhaustive—they must cover all possible values:

**For booleans**: You must cover both `true` and `false`, or use a wildcard:

```rue
fn main() -> i32 {
    // Both values covered explicitly
    match true {
        true => 1,
        false => 0,
    }
}
```

```rue
fn main() -> i32 {
    // Wildcard covers remaining cases
    match false {
        true => 1,
        _ => 0,
    }
}
```

**For integers**: You must include a wildcard pattern since integers have too many possible values:

```rue
fn main() -> i32 {
    match 42 {
        1 => 10,
        2 => 20,
        _ => 0,  // required for integers
    }
}
```

Non-exhaustive matches are compile-time errors:

```rue
fn main() -> i32 {
    match 1 {
        1 => 10,
        2 => 20,
        // ERROR: match is not exhaustive (integer match requires wildcard)
    }
}
```

#### Block Bodies

Match arms can have block bodies for more complex expressions:

```rue
fn main() -> i32 {
    match 2 {
        1 => {
            let a = 10;
            a
        },
        2 => {
            let b = 20;
            let c = 5;
            b + c
        },
        _ => 0,
    }
}
```

#### Match on Expressions

The scrutinee (value being matched) can be any expression:

```rue
fn main() -> i32 {
    let x = 5;
    match x > 3 {
        true => 100,
        false => 0,
    }
}
```

```rue
fn main() -> i32 {
    match 1 + 1 {
        2 => 42,
        _ => 0,
    }
}
```

#### Nested Match

Match expressions can be nested:

```rue
fn main() -> i32 {
    match 1 {
        1 => match 2 {
            2 => 12,
            _ => 10,
        },
        _ => 0,
    }
}
```

#### Trailing Commas

Trailing commas after the last arm are optional:

```rue
fn main() -> i32 {
    // With trailing comma
    match 1 {
        1 => 10,
        _ => 0,
    }
}
```

```rue
fn main() -> i32 {
    // Without trailing comma
    match 1 {
        1 => 10,
        _ => 0
    }
}
```

### While Loops

`while` loops repeat a block of code while a condition is true:

```rue
fn main() -> i32 {
    let mut sum = 0;
    let mut i = 1;
    while i <= 10 {
        sum = sum + i;
        i = i + 1;
    }
    sum  // returns 55
}
```

The condition must be of type `bool`. While loops evaluate to unit type `()`.

### Break and Continue

`break` exits the innermost loop immediately:

```rue
fn main() -> i32 {
    let mut x = 0;
    while true {
        x = x + 1;
        if x == 5 {
            break;
        }
    }
    x  // returns 5
}
```

`continue` skips to the next iteration of the innermost loop:

```rue
fn main() -> i32 {
    let mut sum = 0;
    let mut i = 0;
    while i < 10 {
        i = i + 1;
        if i % 2 == 0 {
            continue;  // skip even numbers
        }
        sum = sum + i;
    }
    sum  // returns 25 (1+3+5+7+9)
}
```

Both `break` and `continue` must appear inside a loop. Using them outside a loop is a compile-time error:

```rue
fn main() -> i32 {
    break;  // ERROR: 'break' outside of loop
    0
}
```

In nested loops, `break` and `continue` affect only the innermost loop:

```rue
fn main() -> i32 {
    let mut total = 0;
    let mut outer = 0;
    while outer < 3 {
        let mut inner = 0;
        while true {
            inner = inner + 1;
            total = total + 1;
            if inner == 2 {
                break;  // exits inner loop only
            }
        }
        outer = outer + 1;
    }
    total  // returns 6 (2 iterations * 3 outer loops)
}
```

### Return Statement

The `return` keyword explicitly returns a value from the current function:

```rue
fn main() -> i32 {
    return 42;
}
```

`return` is useful for early exits from functions:

```rue
fn abs(x: i32) -> i32 {
    if x < 0 {
        return 0 - x;
    }
    x
}

fn main() -> i32 {
    abs(-5)  // returns 5
}
```

The returned expression must match the function's declared return type:

```rue
fn main() -> i32 {
    return true;  // ERROR: type mismatch: expected i32, found bool
}
```

`return` can be used inside loops to exit the function:

```rue
fn find_first_even(start: i32) -> i32 {
    let mut x = start;
    while x < 100 {
        if x % 2 == 0 {
            return x;
        }
        x = x + 1;
    }
    return 0;  // not found
}

fn main() -> i32 {
    find_first_even(5)  // returns 6
}
```

The `return` expression has the never type (`!`) because it diverges - it never produces a local value. This allows `return` to be used in either branch of an if/else:

```rue
fn test(x: i32) -> i32 {
    let y = if x > 5 { return 100 } else { x };
    y * 2
}

fn main() -> i32 {
    test(3) + test(10)  // 6 + 100 = 106
}
```

### Runtime Errors

Division by zero, integer overflow, and array bounds violations cause runtime errors (exit code 101):

```rue
fn main() -> i32 { 10 / 0 }           // runtime error: division by zero
fn main() -> i32 { 2147483647 + 1 }   // runtime error: integer overflow
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    let idx = 10;
    arr[idx]                          // runtime error: index out of bounds
}
```

### Grammar (Current)

```ebnf
program        = { function } ;
function       = "fn" IDENT "(" [ params ] ")" "->" type "{" block "}" ;
params         = param { "," param } ;
param          = IDENT ":" type ;
block          = { statement } expression ;
statement      = let_stmt | assign_stmt ;
let_stmt       = "let" [ "mut" ] IDENT [ ":" type ] "=" expression ";" ;
assign_stmt    = IDENT "=" expression ";" | IDENT "[" expression "]" "=" expression ";" ;
type           = "i32" | "bool" | "[" type ";" INTEGER "]" ;
expression     = or_expr ;
or_expr        = and_expr { "||" and_expr } ;
and_expr       = comparison { "&&" comparison } ;
comparison     = additive { ("==" | "!=" | "<" | ">" | "<=" | ">=") additive } ;
additive       = multiplicative { ("+" | "-") multiplicative } ;
multiplicative = unary { ("*" | "/" | "%") unary } ;
unary          = "-" unary | "!" unary | postfix ;
postfix        = primary { "[" expression "]" | "(" [ args ] ")" } ;
args           = expression { "," expression } ;
primary        = INTEGER | BOOL | IDENT | "(" expression ")" | block_expr
               | if_expr | match_expr | while_expr | "break" | "continue" | return_expr
               | array_literal ;
array_literal  = "[" [ expression { "," expression } ] "]" ;
return_expr    = "return" expression ;
block_expr     = "{" block "}" ;
if_expr        = "if" expression "{" block "}" [ "else" "{" block "}" ] ;
match_expr     = "match" expression "{" { match_arm "," } [ match_arm ] "}" ;
match_arm      = pattern "=>" expression ;
pattern        = "_" | INTEGER | BOOL ;
while_expr     = "while" expression "{" block "}" ;

BOOL           = "true" | "false" ;
```

## Planned Features

See `docs/design-decisions.md` (ADR-009) for language philosophy. Planned additions include:

- `loop` keyword (infinite loop)
- Labeled loops and labeled break/continue
- Structs and user-defined types
- Memory safety model (influenced by Hylo/Swift/Rust)
