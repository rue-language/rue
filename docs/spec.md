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
Integer literals are sequences of decimal digits. Boolean literals are the keywords `true` and `false`.

```
integer_literal ::= digit+
boolean_literal ::= "true" | "false"
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
Currently, Rue does not support comments.

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

if_expression ::= "if" expression block ("else" block)?

while_expression ::= "while" expression block

binary_expression ::= expression binary_operator expression

call_expression ::= identifier "(" arguments? ")"

arguments ::= expression ("," expression)*

primary_expression ::= identifier | integer_literal | boolean_literal | "(" expression ")"

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
- Function parameters are scoped to their function body
- Variables declared with `let` are scoped to the block in which they are declared
- Functions are globally scoped
- Variable shadowing is not permitted within the same scope

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
- The `main` function must be defined and take either zero or one parameter
- The return type of `main` determines the exit code behavior:
  - `fn main() -> ()`: Always exits with code 0
  - `fn main() -> i32` or `fn main() -> i64`: The returned value becomes the exit code
  - `fn main() -> bool`: Returns 0 for `false`, 1 for `true`

### 5.2 Expression Evaluation

#### 5.2.1 Literals
Integer literals evaluate to their numeric value.

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
Currently, Rue has no built-in functions.

### 6.2 Runtime Behavior
- Integer overflow wraps using two's complement arithmetic for both `i32` and `i64`
- Division by zero causes program termination
- Boolean values are represented as 0 (`false`) and 1 (`true`) at runtime
- All memory management is handled by the runtime

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
    print_value(42);
    0
}
```