//! Comprehensive parser test coverage
//!
//! This test suite aims to cover all AST node types and edge cases

use rue_insta_utils::configure_insta;
use rue_parser::parse_with_diagnostics;

fn assert_parser_snapshot(name: &str, source: &str) {
    let result = parse_with_diagnostics(source, "test.rue");

    configure_insta("tests/snapshots").bind(|| {
        insta::assert_debug_snapshot!(name, result);
    });
}

// ===== Literal Tests =====

#[test]
fn test_integer_literals() {
    assert_parser_snapshot(
        "literals_integers",
        r#"
fn main() -> i32 {
    let a = 0;
    let b = 42;
    let c = 999999999;
    let d = -1;
    let e = -2147483648;  // i32::MIN
    42
}
"#,
    )
}

#[test]
fn test_boolean_literals() {
    assert_parser_snapshot(
        "literals_booleans",
        r#"
fn main() -> bool {
    let t = true;
    let f = false;
    true && false || true
}
"#,
    )
}

// ===== Binary Expression Tests =====

#[test]
fn test_arithmetic_operators() {
    assert_parser_snapshot(
        "operators_arithmetic",
        r#"
fn main() -> i32 {
    let sum = 1 + 2;
    let diff = 5 - 3;
    let prod = 4 * 5;
    let quot = 10 / 2;
    let rem = 7 % 3;
    sum + diff * prod - quot / rem
}
"#,
    )
}

#[test]
fn test_comparison_operators() {
    assert_parser_snapshot(
        "operators_comparison",
        r#"
fn main() -> bool {
    let a = 5;
    let b = 10;
    let eq = a == b;
    let ne = a != b;
    let lt = a < b;
    let le = a <= b;
    let gt = a > b;
    let ge = a >= b;
    lt && le || gt && ge
}
"#,
    )
}

#[test]
fn test_logical_operators() {
    assert_parser_snapshot(
        "operators_logical",
        r#"
fn main() -> bool {
    let a = true;
    let b = false;
    let and = a && b;
    let or = a || b;
    let complex = (a && b) || (!a && !b);
    complex
}
"#,
    )
}

#[test]
fn test_operator_precedence() {
    assert_parser_snapshot(
        "operators_precedence",
        r#"
fn main() -> i32 {
    // Test that multiplication has higher precedence than addition
    let a = 2 + 3 * 4;  // Should be 2 + (3 * 4) = 14, not (2 + 3) * 4 = 20
    
    // Test that comparison has lower precedence than arithmetic
    let b = 2 + 3 < 4 * 5;  // Should be (2 + 3) < (4 * 5)
    
    // Test that logical AND has higher precedence than OR
    let c = true || false && false;  // Should be true || (false && false) = true
    
    // Test parentheses override precedence
    let d = (2 + 3) * 4;
    let e = 2 * (3 + 4);
    
    a + d + e
}
"#,
    )
}

// ===== Control Flow Tests =====

#[test]
fn test_if_else_chain() {
    assert_parser_snapshot(
        "control_if_else_chain",
        r#"
fn main() -> i32 {
    let x = 10;
    
    if x < 5 {
        1
    } else if x < 10 {
        2
    } else if x == 10 {
        3
    } else {
        4
    }
}
"#,
    )
}

#[test]
fn test_nested_if() {
    assert_parser_snapshot(
        "control_nested_if",
        r#"
fn main() -> i32 {
    let x = 10;
    let y = 20;
    
    if x > 5 {
        if y > 15 {
            if x + y > 25 {
                100
            } else {
                200
            }
        } else {
            300
        }
    } else {
        400
    }
}
"#,
    )
}

#[test]
fn test_while_loop_variations() {
    assert_parser_snapshot(
        "control_while_variations",
        r#"
fn main() -> i32 {
    let mut x = 10;
    
    // Simple while
    while x > 0 {
        x = x - 1;
    };
    
    // Nested while
    let mut i = 3;
    while i > 0 {
        let mut j = 2;
        while j > 0 {
            j = j - 1;
        };
        i = i - 1;
    };
    
    // While with complex condition
    while x < 100 && (x % 2 == 0 || x % 3 == 0) {
        x = x + 1;
    };
    
    x
}
"#,
    )
}

#[test]
fn test_for_loop() {
    assert_parser_snapshot(
        "control_for_loop",
        r#"
fn main() -> i32 {
    let mut sum = 0;
    
    for i in 0..10 {
        sum = sum + i;
    };
    
    // Nested for loop
    for i in 0..3 {
        for j in 0..3 {
            sum = sum + i * j;
        };
    };
    
    sum
}
"#,
    )
}

// ===== Function Tests =====

#[test]
fn test_function_parameters() {
    assert_parser_snapshot(
        "functions_parameters",
        r#"
// No parameters
fn zero() -> i32 {
    42
}

// Single parameter
fn identity(x: i32) -> i32 {
    x
}

// Multiple parameters
fn add3(a: i32, b: i32, c: i32) -> i32 {
    a + b + c
}

// Maximum parameters (test limit)
fn many_params(
    p1: i32, p2: i32, p3: i32, p4: i32, p5: i32,
    p6: i32, p7: i32, p8: i32, p9: i32, p10: i32
) -> i32 {
    p1 + p2 + p3 + p4 + p5 + p6 + p7 + p8 + p9 + p10
}

fn main() -> i32 {
    zero() + identity(5) + add3(1, 2, 3)
}
"#,
    )
}

#[test]
fn test_recursive_functions() {
    assert_parser_snapshot(
        "functions_recursive",
        r#"
fn factorial(n: i32) -> i32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn fibonacci(n: i32) -> i32 {
    if n <= 1 {
        n
    } else {
        fibonacci(n - 1) + fibonacci(n - 2)
    }
}

fn main() -> i32 {
    factorial(5) + fibonacci(10)
}
"#,
    )
}

#[test]
fn test_nested_function_calls() {
    assert_parser_snapshot(
        "functions_nested_calls",
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn mul(a: i32, b: i32) -> i32 {
    a * b
}

fn complex(x: i32) -> i32 {
    add(mul(x, 2), add(x, mul(3, 4)))
}

fn main() -> i32 {
    complex(add(1, mul(2, 3)))
}
"#,
    )
}

// ===== Statement Tests =====

#[test]
fn test_let_statements() {
    assert_parser_snapshot(
        "statements_let",
        r#"
fn main() -> i32 {
    // Simple let
    let x = 42;
    
    // Let with type annotation
    let y: i32 = 10;
    
    // Let with complex expression
    let z = x + y * 2;
    
    // Shadowing
    let x = 100;
    let x = x + 1;
    
    x + y + z
}
"#,
    )
}

#[test]
fn test_assignment_statements() {
    assert_parser_snapshot(
        "statements_assignment",
        r#"
fn main() -> i32 {
    let x = 10;
    x = 20;
    x = x + 5;
    
    // Compound assignments
    x += 10;
    x -= 5;
    x *= 2;
    x /= 3;
    x %= 7;
    
    x
}
"#,
    )
}

#[test]
fn test_return_statements() {
    assert_parser_snapshot(
        "statements_return",
        r#"
fn early_return(x: i32) -> i32 {
    if x < 0 {
        return -x;
    };
    
    if x == 0 {
        return 0;
    };
    
    return x * 2;
}

fn implicit_return() -> i32 {
    42  // No semicolon, implicit return
}

fn explicit_return() -> i32 {
    return 42;  // Explicit return statement
}

fn main() -> i32 {
    early_return(-5) + implicit_return() + explicit_return()
}
"#,
    )
}

// ===== Type Tests =====

#[test]
fn test_type_annotations() {
    assert_parser_snapshot(
        "types_annotations",
        r#"
fn type_examples(
    a: i8,
    b: i16, 
    c: i32,
    d: i64,
    e: u8,
    f: u16,
    g: u32,
    h: u64,
    i: bool
) -> i32 {
    let x: i32 = 42;
    let y: bool = true;
    let z: i64 = 999999;
    
    x
}

fn main() -> i32 {
    type_examples(1, 2, 3, 4, 5, 6, 7, 8, true)
}
"#,
    )
}

#[test]
fn test_cast_expressions() {
    assert_parser_snapshot(
        "types_cast",
        r#"
fn main() -> i32 {
    let x: i64 = 1000;
    let y = x as i32;
    
    let a: i8 = 10;
    let b = a as i16 as i32 as i64;  // Chained casts
    
    let c = (5 + 3) as i64;  // Cast of expression
    
    y + (b as i32) + (c as i32)
}
"#,
    )
}

// ===== Complex/Edge Case Tests =====

#[test]
fn test_deeply_nested_expressions() {
    assert_parser_snapshot(
        "edge_deeply_nested",
        r#"
fn main() -> i32 {
    // Deeply nested arithmetic
    let x = ((((1 + 2) * 3) - 4) / 5) % 6;
    
    // Deeply nested function calls
    fn f(x: i32) -> i32 { x + 1 }
    let y = f(f(f(f(f(f(f(f(f(f(10))))))))));
    
    // Deeply nested if expressions
    let z = if true {
        if false {
            if true {
                if false {
                    1
                } else {
                    2
                }
            } else {
                3
            }
        } else {
            4
        }
    } else {
        5
    };
    
    x + y + z
}
"#,
    )
}

#[test]
fn test_empty_blocks() {
    assert_parser_snapshot(
        "edge_empty_blocks",
        r#"
fn empty_function() -> i32 {
    // Function with just a return value
    42
}

fn main() -> i32 {
    // Empty if blocks
    if true { } else { };
    
    // Empty while block
    while false { };
    
    // Nested empty blocks
    if true {
        if false { } else { };
    } else { };
    
    42
}
"#,
    )
}

#[test]
fn test_single_expression_blocks() {
    assert_parser_snapshot(
        "edge_single_expression",
        r#"
fn main() -> i32 {
    // Block with single expression (no semicolon)
    let x = { 42 };
    
    // Block with single statement (with semicolon)
    let y = { 42; };
    
    // Nested blocks
    let z = {
        {
            {
                100
            }
        }
    };
    
    x + z
}
"#,
    )
}

// ===== Error Recovery Tests =====

#[test]
fn test_error_recovery_missing_semicolon() {
    assert_parser_snapshot(
        "error_recovery_missing_semi",
        r#"
fn main() -> i32 {
    let x = 42  // Missing semicolon
    let y = 10;
    x + y
}
"#,
    )
}

#[test]
fn test_error_recovery_missing_type() {
    assert_parser_snapshot(
        "error_recovery_missing_type",
        r#"
fn main() -> {  // Missing return type
    42
}

fn add(a: i32, b) -> i32 {  // Missing parameter type
    a + b
}
"#,
    )
}

#[test]
fn test_error_recovery_unclosed_delimiter() {
    assert_parser_snapshot(
        "error_recovery_unclosed",
        r#"
fn main() -> i32 {
    let x = (1 + 2;  // Unclosed parenthesis
    let y = [1, 2, 3;  // Unclosed bracket
    42
"#,
    )
}

#[test]
fn test_error_recovery_multiple_errors() {
    assert_parser_snapshot(
        "error_recovery_multiple",
        r#"
fn main() -> i32 {
    let x = ;  // Missing expression
    let y = 10 20;  // Extra token
    if x > {  // Missing expression after operator
        42
    } else
        100  // Missing block braces
}
"#,
    )
}

// ===== Unicode and Special Character Tests =====

#[test]
fn test_unicode_identifiers() {
    assert_parser_snapshot(
        "unicode_identifiers",
        r#"
fn main() -> i32 {
    let café = 10;
    let π = 3;
    let 你好 = 42;
    let змінна = 100;
    
    café + π + 你好 + змінна
}
"#,
    )
}

#[test]
fn test_string_literals() {
    assert_parser_snapshot(
        "string_literals",
        r#"
fn main() -> i32 {
    let empty = "";
    let simple = "Hello, World!";
    let escaped = "Line 1\nLine 2\tTabbed";
    let quotes = "He said \"Hello\"";
    let unicode = "Unicode: 你好 🦀 π";
    
    42
}
"#,
    )
}

// ===== Comments Tests =====

#[test]
fn test_comments() {
    assert_parser_snapshot(
        "comments",
        r#"
// Single-line comment at file level

fn main() -> i32 {
    // Comment before statement
    let x = 42; // Comment after statement
    
    /* Multi-line comment
       spanning multiple
       lines */
    
    let y = /* inline comment */ 10;
    
    // Nested /* comments */ in single-line
    
    /* Nested // comment in multi-line */
    
    x + y
}
"#,
    )
}
