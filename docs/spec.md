# Rue Language Specification

## 1. Introduction

Rue is a minimal programming language with a Rust-like syntax. The language supports multiple primitive types including integers and booleans. Programs are compiled to native executables that return their result as the process exit code.

## 2. Lexical Structure

### 2.1 Character Set
Rue source code is UTF-8 encoded text.

### 2.2 Tokens

#### 2.2.1 Keywords
```
fn let if else while i32 i64 bool true false
```

#### 2.2.2 Identifiers
An identifier is a sequence of letters, digits, and underscores that does not start with a digit and is not a keyword.

```
identifier ::= (letter | '_') (letter | digit | '_')*
letter     ::= 'a'..'z' | 'A'..'Z'
digit      ::= '0'..'9'
```

#### 2.2.3 Literals
Integer literals are sequences of decimal digits. Boolean literals are the keywords `true` and `false`. Unit literals are empty parentheses `()`.

```
integer_literal ::= digit+
boolean_literal ::= "true" | "false"
unit_literal    ::= "()"
```

#### 2.2.4 Operators
```
+ - * / % <= >= < > == != = -> :
```

#### 2.2.5 Delimiters
```
( ) { } , ;
```

#### 2.2.6 Whitespace
Whitespace consists of spaces, tabs, and newlines. Whitespace is ignored except as a token separator.

#### 2.2.7 Comments
Rue supports two styles of comments:

**Single-line comments** begin with `//` and extend to the end of the line:
```
// This is a single-line comment
let x: i32 = 42; // Comments can appear after code
```

**Multi-line comments** begin with `/*` and end with `*/`. Multi-line comments can be nested:
```
/* This is a multi-line comment
   that spans multiple lines */

/* Nested /* comments are */ supported */
```

Comments are treated as whitespace and can appear anywhere whitespace is allowed.

## 3. Syntax

### 3.1 Grammar
The following grammar is presented in EBNF notation:

```ebnf
program ::= function*

function ::= "fn" identifier "(" parameters? ")" return_type? block

parameters ::= parameter ("," parameter)*

parameter ::= identifier ":" type

return_type ::= "->" type

type ::= "i32" | "i64" | "bool" | "()"

block ::= "{" statement* expression? "}"

statement ::= let_statement | assignment_statement | expression_statement

let_statement ::= "let" identifier ":" type "=" expression ";"

assignment_statement ::= identifier "=" expression ";"

expression_statement ::= expression ";"

expression ::= if_expression | while_expression | binary_expression | call_expression | primary_expression

if_expression ::= "if" expression block else_clause?

else_clause ::= "else" (block | if_expression)

while_expression ::= "while" expression block

binary_expression ::= expression binary_operator expression

call_expression ::= identifier "(" arguments? ")"

arguments ::= expression ("," expression)*

primary_expression ::= identifier | integer_literal | boolean_literal | unit_literal | "(" expression ")"

binary_operator ::= "+" | "-" | "*" | "/" | "%" | "<=" | ">=" | "<" | ">" | "==" | "!="
```

### 3.2 Operator Precedence
Operators are listed from highest to lowest precedence:

1. Function calls: `f(x)`
2. Multiplicative: `*`, `/`, `%`
3. Additive: `+`, `-`
4. Comparison: `<=`, `>=`, `<`, `>`, `==`, `!=`

Operators of the same precedence are left-associative.

## 4. Static Semantics

### 4.1 Scoping Rules

#### 4.1.1 Basic Scoping Principles
- Function parameters are scoped to their function body
- Variables declared with `let` are scoped to the block in which they are declared
- Functions are globally scoped and visible throughout the program
- Variable shadowing is permitted both within the same scope and across nested scopes

#### 4.1.2 Block Scopes
A new scope is created when entering:
- Function bodies
- If expression blocks (both `then` and `else` branches)
- While expression bodies

When a block is exited, all variables declared within that block become inaccessible.

#### 4.1.3 Variable Shadowing
Variable shadowing occurs when a new variable declaration has the same name as an existing variable:
- Variables can shadow other variables in the same scope or in outer scopes
- The new variable "hides" all previous variables with the same name
- The previous variables become permanently inaccessible (they cannot be "unshadowed")
- Each `let` declaration creates a new variable, even if the name already exists

Example of cross-scope shadowing:
```rue
fn main() -> i32 {
    let x: i32 = 10;      // Outer x
    if true {
        let x: i32 = 20;  // Inner x shadows outer x
        x                 // Returns 20
    } else {
        x                 // Would return 10 (outer x)
    }
}
```

Example of same-scope shadowing:
```rue
fn main() -> i32 {
    let x: i32 = 10;      // First x
    let x: i32 = 20;      // Second x shadows first x
    x                     // Returns 20 (first x is no longer accessible)
}
```

Shadowing with different types:
```rue
fn main() -> i32 {
    let x: i32 = 42;      // x is i32
    let x: bool = true;   // x is now bool, previous x is shadowed
    if x {                // Uses the bool x
        let x: i32 = 100; // x is i32 again in this scope
        x                 // Returns 100
    } else {
        0
    }
}

#### 4.1.4 Scope Resolution
When resolving a variable reference:
1. Search starts in the current (innermost) scope
2. If not found, search proceeds to the next outer scope
3. Continue until the variable is found or all scopes are exhausted
4. If the variable is not found in any accessible scope, a compile error occurs

#### 4.1.5 If Expression Scopes
Each branch of an if expression creates its own scope:
```rue
fn main() -> i32 {
    let x: i32 = 1;
    if condition {
        let y: i32 = 2;   // y is only accessible in this branch
        x + y             // Can access outer x and inner y
    } else {
        let z: i32 = 3;   // z is only accessible in this branch
        x + z             // Can access outer x and inner z
    }
    // Neither y nor z are accessible here
}
```

Nested if expressions (else if) each create their own scope:
```rue
fn main() -> i32 {
    let x: i32 = 1;
    if condition1 {
        let a: i32 = 10;
        a
    } else if condition2 {
        let b: i32 = 20;  // b is only accessible in this branch
        b
    } else {
        let c: i32 = 30;  // c is only accessible in this branch
        c
    }
    // None of a, b, or c are accessible here
}
```

#### 4.1.6 While Expression Scopes
The body of a while expression creates a new scope for each iteration:
```rue
fn main() -> i32 {
    let x: i32 = 0;
    while x < 10 {
        let y: i32 = x * 2;  // y is created fresh each iteration
        x = x + 1;           // Modifies outer x
    };
    // y is not accessible here
    x
}
```

#### 4.1.7 Function Parameter Shadowing
Function parameters can be shadowed within the function body:
```rue
fn process(x: i32) -> i32 {
    if x > 10 {
        let x: i32 = 10;  // Shadows the parameter x
        x                 // Returns 10
    } else {
        x                 // Returns the parameter value
    }
}
```

#### 4.1.8 Assignment and Scoping
Assignment statements always modify the variable in the scope where it was declared:
```rue
fn main() -> i32 {
    let x: i32 = 10;
    if true {
        x = 20;           // Modifies the outer x
        let x: i32 = 30;  // Creates a new inner x
        x = 40;           // Modifies the inner x
    };
    x                     // Returns 20 (outer x was modified)
}
```

Note that assignment requires the variable to already exist - you cannot create a new variable with assignment:
```rue
fn main() -> i32 {
    y = 10;  // Error: undefined variable y
    0
}
```

#### 4.1.9 Comprehensive Scoping Example
This example demonstrates multiple scoping concepts:
```rue
fn calculate(n: i32) -> i32 {
    let result: i32 = 0;        // Function scope
    let multiplier: i32 = 2;    // Function scope
    
    if n > 0 {
        let temp: i32 = n * multiplier;  // If-block scope
        result = temp;                    // Modifies function-scope result
        
        if n > 10 {
            let multiplier: i32 = 3;      // Shadows function-scope multiplier
            let temp: i32 = n * multiplier;  // Shadows if-block temp
            result = temp;                // Still modifies function-scope result
        };                                // Inner multiplier and temp go out of scope
        
        // Here, temp refers to the if-block temp, not the inner one
        result = result + temp;
    } else {
        let temp: i32 = -1;              // Different temp in else-block
        result = temp;
    };
    // No temp variable is accessible here
    
    result * multiplier  // Uses function-scope multiplier (2)
}
```

### 4.2 Name Resolution
- All identifiers must be declared before use
- Function calls must reference declared functions
- Variable references must reference declared variables or parameters

### 4.3 Type System

#### 4.3.1 Supported Types
Rue supports the following primitive types:
- `i32`: 32-bit signed integer
- `i64`: 64-bit signed integer  
- `bool`: Boolean type (true or false)
- `()`: Unit type (represents no value)

#### 4.3.2 Type Annotations
- Variables must be explicitly typed at declaration: `let x: i32 = 42;`
- Function parameters must be explicitly typed: `fn add(a: i32, b: i32)`
- Function return types are optional and default to unit: `fn foo() -> i32`
- No implicit type conversions are allowed

#### 4.3.3 Type Inference
- Numeric literals without explicit type context default to `i32`
- Boolean literals (`true` and `false`) are always type `bool`
- Expression types are derived from their operands

#### 4.3.4 Type Checking Rules
- Binary arithmetic operators (`+`, `-`, `*`, `/`, `%`) require both operands to have the same numeric type (`i32` or `i64`)
- Comparison operators (`<`, `>`, `<=`, `>=`, `==`, `!=`) require both operands to have the same type and produce a `bool` result
- Conditional expressions (`if` and `while`) require their condition to be of type `bool`
- Assignment requires the expression type to match the variable's declared type
- Function arguments must match the declared parameter types exactly
- Function return values must match the declared return type (or unit if none specified)

## 5. Dynamic Semantics

### 5.1 Program Execution
- Program execution begins with a call to the `main` function
- The `main` function must be defined with no parameters
- The return type of `main` determines the exit code behavior:
  - `fn main() -> ()`: Always exits with code 0
  - `fn main() -> i32` or `fn main() -> i64`: The returned value becomes the exit code
  - `fn main() -> bool`: Returns 0 for `false`, 1 for `true`

### 5.2 Expression Evaluation

#### 5.2.1 Literals
Integer literals evaluate to their numeric value. Boolean literals evaluate to their boolean value. Unit literals evaluate to the unit value.

#### 5.2.2 Variables
Variable references evaluate to the current value of the variable.

#### 5.2.3 Binary Operations
Binary operations are evaluated left-to-right according to precedence:

**Arithmetic Operations** (require matching numeric types):
- `+`: Addition (wrapping on overflow)
- `-`: Subtraction (wrapping on overflow)  
- `*`: Multiplication (wrapping on overflow)
- `/`: Division (program aborts on division by zero)
- `%`: Modulo (program aborts on division by zero)
  - TODO: Specify that modulo uses truncated division (same as Rust/C)

**Comparison Operations** (require matching types, return `bool`):
- `<=`, `>=`, `<`, `>`: Comparison (returns `true` or `false`)
- `==`, `!=`: Equality (returns `true` or `false`)

#### 5.2.4 Function Calls
Function calls:
1. Evaluate the argument expression (if present)
2. Create a new scope for the function body
3. Bind the parameter (if present) to the argument value
4. Execute the function body
5. Return the value of the final expression

#### 5.2.5 Conditional Expressions
`if` expressions:
1. Evaluate the condition expression (must be type `bool`)
2. If the condition is `true`, execute the `then` block
3. If the condition is `false` and an `else` block exists, execute the `else` block
4. Return the value of the executed block, or unit if no `else` block exists
5. Both branches must have the same return type

#### 5.2.6 While Loops
`while` expressions:
1. Evaluate the condition expression (must be type `bool`)
2. If the condition is `false`, return unit
3. If the condition is `true`, execute the loop body and repeat from step 1
4. The loop body value is discarded; the loop always returns unit

### 5.3 Statements

#### 5.3.1 Let Statements
`let` statements declare a new variable in the current scope and initialize it with the value of the expression. They are terminated with a semicolon.

#### 5.3.2 Assignment Statements
Assignment statements update the value of an existing variable. The variable must be previously declared in an accessible scope. They are terminated with a semicolon.

#### 5.3.3 Expression Statements
Expression statements evaluate an expression and discard the result. They are terminated with a semicolon.

### 5.4 Blocks
Blocks execute their statements in order, then evaluate their final expression (if present). Statements are terminated with semicolons and executed for their side effects. The optional final expression has no semicolon and its value becomes the block's value. If there is no final expression, the block evaluates to unit.

## 6. Standard Library

### 6.1 Built-in Functions

Rue provides the following built-in functions for I/O and program control:

#### 6.1.1 exit
```
exit(code: i64) -> ()
```
Terminates the program immediately with the specified exit code. The exit code is truncated to fit the system's exit code range (typically 0-255).

#### 6.1.2 println_i64
```
println_i64(value: i64) -> ()
```
Prints the given 64-bit integer to standard output followed by a newline.

#### 6.1.3 println_i32
```
println_i32(value: i32) -> ()
```
Prints the given 32-bit integer to standard output followed by a newline.

#### 6.1.4 println_bool
```
println_bool(value: bool) -> ()
```
Prints the given boolean value as "true" or "false" to standard output followed by a newline.

#### 6.1.5 println_unit
```
println_unit(value: ()) -> ()
```
Prints "()" to standard output followed by a newline.

#### 6.1.6 input
```
input() -> i64
```
Reads a line from standard input and attempts to parse it as a 64-bit integer. Returns:
- The parsed integer value if successful
- 0 if parsing fails or on EOF
- Leading and trailing whitespace is ignored
- Parsing stops at the first non-digit character after optional sign

#### 6.1.7 to_i32
```
to_i32(value: i64) -> i32
```
Casts a 64-bit integer to a 32-bit integer by truncating the upper 32 bits. The lower 32 bits are preserved unchanged.

#### 6.1.8 to_i64
```
to_i64(value: i32) -> i64
```
Casts a 32-bit integer to a 64-bit integer by sign extension. The sign bit of the 32-bit value is extended to fill the upper 32 bits, preserving the numeric value for both positive and negative numbers.

### 6.2 Runtime Behavior
- Integer overflow wraps using two's complement arithmetic for both `i32` and `i64`
- Division by zero causes program termination with exit code 250
- Modulo by zero causes program termination with exit code 250
- Boolean values are represented as 0 (`false`) and 1 (`true`) at runtime
- All memory management is handled by the runtime
- Stack overflow causes program termination with exit code 251 (when implemented)

## 7. Examples

### 7.1 Hello World (Return 42)
```rue
fn main() -> i32 {
    42
}
```

### 7.2 Factorial Function
```rue
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(5)
}
```

### 7.3 Let and Assignment Statements
```rue
fn main() -> i32 {
    let x: i32 = 42;
    x = x + 58;
    x
}
```

### 7.4 While Loop
```rue
fn countdown(n: i32) -> i32 {
    let count: i32 = n;
    while count > 0 {
        count = count - 1;
    };
    count
}

fn main() -> i32 {
    countdown(10)
}
```

### 7.5 Boolean Operations
```rue
fn is_even(n: i32) -> bool {
    n % 2 == 0
}

fn main() -> i32 {
    if is_even(42) {
        1
    } else {
        0
    }
}
```

### 7.6 Unit Type Function
```rue
fn print_value(n: i32) -> () {
    // In a real implementation, this might print to stdout
    // For now, it just demonstrates a unit-returning function
    n;
}

fn main() -> i32 {
    let x: () = ();  // Unit literal
    print_value(42);
    0
}
```

### 7.7 Using Built-in I/O Functions
```rue
fn print_unit() -> () {
    // Function that returns unit
}

fn main() -> i64 {
    let forty_two: i64 = 42;
    println_i64(forty_two);
    println_bool(true);
    println_i32(100);
    
    let unit_val = print_unit();
    println_unit(unit_val);
    
    0
}
```

### 7.8 Interactive Input Program
```rue
fn main() -> i64 {
    let prompt: i64 = 1234;
    println_i64(prompt);  // Print a prompt
    
    let x: i64 = input();
    let y: i64 = input();
    
    let sum: i64 = x + y;
    println_i64(sum);
    
    if x > y {
        println_bool(true);
        x - y
    } else {
        println_bool(false);
        y - x
    }
}
```

### 7.9 Error Handling Example
```rue
fn divide(a: i64, b: i64) -> i64 {
    a / b  // Will exit with code 250 if b is 0
}

fn main() -> i64 {
    let result: i64 = divide(10, 2);
    println_i64(result);  // Prints 5
    
    // This would cause program termination:
    // let bad: i64 = divide(10, 0);
    
    0
}
```

### 7.10 Type Casting Example
```rue
fn main() -> i64 {
    // Casting from i64 to i32 (truncation)
    let big: i64 = 4294967296;  // 2^32
    let small: i32 = to_i32(big);
    println_i32(small);  // Prints 0 (lower 32 bits)
    
    // Casting from i32 to i64 (sign extension)
    let negative: i32 = -42;
    let extended: i64 = to_i64(negative);
    println_i64(extended);  // Prints -42
    
    // Practical use: mixing i32 and i64 operations
    let a: i32 = 100;
    let b: i64 = 200;
    let sum: i64 = to_i64(a) + b;
    println_i64(sum);  // Prints 300
    
    0
}
```