// ============================================================================
// Integration Unit Tests
// ============================================================================
//
// These tests verify the compilation pipeline without execution. They test:
// - Type checking and semantic analysis
// - CFG construction
// - Error message quality
//
// Benefits:
// - Fast: No file I/O, no process spawning, no execution
// - Comprehensive: Tests full parse→sema→codegen pipeline
// - Debuggable: Can inspect intermediate IRs in tests

use crate::*;

#[cfg(test)]
mod integration_tests {
    use super::*;

    // ========================================================================
    // Integer Types
    // ========================================================================

    mod integer_types {
        use super::*;

        #[test]
        fn signed_integer_return() {
            assert!(test_air("fn main() -> i8 { 42 }").is_ok());
            assert!(test_air("fn main() -> i16 { 42 }").is_ok());
            assert!(test_air("fn main() -> i32 { 42 }").is_ok());
            assert!(test_air("fn main() -> i64 { 42 }").is_ok());
        }

        #[test]
        fn unsigned_integer_return() {
            assert!(test_air("fn main() -> u8 { 42 }").is_ok());
            assert!(test_air("fn main() -> u16 { 42 }").is_ok());
            assert!(test_air("fn main() -> u32 { 42 }").is_ok());
            assert!(test_air("fn main() -> u64 { 42 }").is_ok());
        }

        #[test]
        fn integer_type_mismatch() {
            let result = test_air("fn main() -> i32 { let x: i64 = 1; x }");
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("type mismatch") || err.contains("expected"));
        }

        #[test]
        fn integer_literal_type_inference() {
            // Type inferred from return type
            assert!(test_air("fn main() -> i64 { 100 }").is_ok());
            // Type inferred from annotation
            assert!(test_air("fn main() -> i32 { let x: i64 = 100; 0 }").is_ok());
        }
    }

    // ========================================================================
    // Boolean Type
    // ========================================================================

    mod boolean_type {
        use super::*;

        #[test]
        fn boolean_literals() {
            assert!(test_air("fn main() -> bool { true }").is_ok());
            assert!(test_air("fn main() -> bool { false }").is_ok());
        }

        #[test]
        fn boolean_in_condition() {
            assert!(test_cfg("fn main() -> i32 { if true { 1 } else { 0 } }").is_ok());
        }

        #[test]
        fn non_boolean_condition_rejected() {
            let result = test_air("fn main() -> i32 { if 1 { 1 } else { 0 } }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Unit Type
    // ========================================================================

    mod unit_type {
        use super::*;

        #[test]
        fn unit_return_type() {
            assert!(test_air("fn main() -> () { () }").is_ok());
        }

        #[test]
        fn unit_in_expression() {
            assert!(test_air("fn main() -> () { let _x = (); () }").is_ok());
        }

        #[test]
        fn implicit_unit_return() {
            assert!(test_air("fn foo() -> () { } fn main() -> i32 { foo(); 0 }").is_ok());
        }
    }

    // ========================================================================
    // Arithmetic Operations
    // ========================================================================

    mod arithmetic {
        use super::*;

        #[test]
        fn basic_addition() {
            assert!(test_air("fn main() -> i32 { 1 + 2 }").is_ok());
        }

        #[test]
        fn basic_subtraction() {
            assert!(test_air("fn main() -> i32 { 5 - 3 }").is_ok());
        }

        #[test]
        fn basic_multiplication() {
            assert!(test_air("fn main() -> i32 { 3 * 4 }").is_ok());
        }

        #[test]
        fn basic_division() {
            assert!(test_air("fn main() -> i32 { 10 / 2 }").is_ok());
        }

        #[test]
        fn basic_modulo() {
            assert!(test_air("fn main() -> i32 { 10 % 3 }").is_ok());
        }

        #[test]
        fn unary_negation() {
            assert!(test_air("fn main() -> i32 { -42 }").is_ok());
        }

        #[test]
        fn operator_precedence() {
            // Multiplication before addition
            let state = test_cfg("fn main() -> i32 { 1 + 2 * 3 }").unwrap();
            assert_eq!(state.functions.len(), 1);
        }

        #[test]
        fn chained_operations() {
            assert!(test_air("fn main() -> i32 { 1 + 2 + 3 + 4 }").is_ok());
        }

        #[test]
        fn mixed_type_arithmetic_rejected() {
            let result = test_air("fn main() -> i32 { 1 + true }");
            assert!(result.is_err());
        }

        #[test]
        fn unsigned_arithmetic() {
            assert!(test_air("fn main() -> u32 { 10 + 5 }").is_ok());
            assert!(test_air("fn main() -> u32 { 10 - 5 }").is_ok());
            assert!(test_air("fn main() -> u32 { 10 * 5 }").is_ok());
        }
    }

    // ========================================================================
    // Comparison Operations
    // ========================================================================

    mod comparison {
        use super::*;

        #[test]
        fn equality_comparison() {
            assert!(test_air("fn main() -> bool { 1 == 1 }").is_ok());
            assert!(test_air("fn main() -> bool { 1 != 2 }").is_ok());
        }

        #[test]
        fn ordering_comparison() {
            assert!(test_air("fn main() -> bool { 1 < 2 }").is_ok());
            assert!(test_air("fn main() -> bool { 2 > 1 }").is_ok());
            assert!(test_air("fn main() -> bool { 1 <= 2 }").is_ok());
            assert!(test_air("fn main() -> bool { 2 >= 1 }").is_ok());
        }

        #[test]
        fn boolean_equality() {
            assert!(test_air("fn main() -> bool { true == true }").is_ok());
            assert!(test_air("fn main() -> bool { true != false }").is_ok());
        }

        #[test]
        fn comparison_returns_bool() {
            let result = test_air("fn main() -> i32 { 1 < 2 }");
            assert!(result.is_err()); // Type mismatch: bool vs i32
        }

        #[test]
        fn mixed_type_comparison_rejected() {
            let result = test_air("fn main() -> bool { 1 == true }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Logical Operations
    // ========================================================================

    mod logical {
        use super::*;

        #[test]
        fn logical_and() {
            assert!(test_cfg("fn main() -> bool { true && false }").is_ok());
        }

        #[test]
        fn logical_or() {
            assert!(test_cfg("fn main() -> bool { true || false }").is_ok());
        }

        #[test]
        fn logical_not() {
            assert!(test_air("fn main() -> bool { !true }").is_ok());
        }

        #[test]
        fn chained_logical() {
            assert!(test_cfg("fn main() -> bool { true && false || true }").is_ok());
        }

        #[test]
        fn logical_with_non_bool_rejected() {
            let result = test_air("fn main() -> bool { 1 && true }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Bitwise Operations
    // ========================================================================

    mod bitwise {
        use super::*;

        #[test]
        fn bitwise_and() {
            assert!(test_air("fn main() -> i32 { 5 & 3 }").is_ok());
        }

        #[test]
        fn bitwise_or() {
            assert!(test_air("fn main() -> i32 { 5 | 3 }").is_ok());
        }

        #[test]
        fn bitwise_xor() {
            assert!(test_air("fn main() -> i32 { 5 ^ 3 }").is_ok());
        }

        #[test]
        fn bitwise_not() {
            assert!(test_air("fn main() -> i32 { ~5 }").is_ok());
        }

        #[test]
        fn shift_left() {
            assert!(test_air("fn main() -> i32 { 1 << 4 }").is_ok());
        }

        #[test]
        fn shift_right() {
            assert!(test_air("fn main() -> i32 { 16 >> 2 }").is_ok());
        }

        #[test]
        fn bitwise_on_bool_rejected() {
            let result = test_air("fn main() -> bool { true & false }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Control Flow - If Expressions
    // ========================================================================

    mod if_expressions {
        use super::*;

        #[test]
        fn basic_if_else() {
            assert!(test_cfg("fn main() -> i32 { if true { 1 } else { 0 } }").is_ok());
        }

        #[test]
        fn if_with_condition_expr() {
            assert!(test_cfg("fn main() -> i32 { if 1 < 2 { 1 } else { 0 } }").is_ok());
        }

        #[test]
        fn nested_if() {
            let src = "fn main() -> i32 { if true { if false { 1 } else { 2 } } else { 3 } }";
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn if_branches_must_match_type() {
            let result = test_air("fn main() -> i32 { if true { 1 } else { true } }");
            assert!(result.is_err());
        }

        #[test]
        fn if_result_type_checked() {
            let result = test_air("fn main() -> bool { if true { 1 } else { 0 } }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Control Flow - Match Expressions
    // ========================================================================

    mod match_expressions {
        use super::*;

        #[test]
        fn match_on_integer() {
            let src = r#"
                fn main() -> i32 {
                    let x = 1;
                    match x {
                        1 => 10,
                        2 => 20,
                        _ => 0,
                    }
                }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn match_on_boolean() {
            let src = r#"
                fn main() -> i32 {
                    match true {
                        true => 1,
                        false => 0,
                    }
                }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn match_exhaustiveness_required() {
            // Missing case should error
            let result = test_air(
                r#"
                fn main() -> i32 {
                    match 1 {
                        1 => 10,
                    }
                }
            "#,
            );
            assert!(result.is_err());
        }

        #[test]
        fn match_branches_must_match_type() {
            let result = test_air(
                r#"
                fn main() -> i32 {
                    match true {
                        true => 1,
                        false => true,
                    }
                }
            "#,
            );
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Control Flow - Loops
    // ========================================================================

    mod loops {
        use super::*;

        #[test]
        fn while_loop_basic() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    while x < 10 {
                        x = x + 1;
                    }
                    x
                }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn while_with_break() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    while true {
                        x = x + 1;
                        if x == 5 {
                            break;
                        }
                    }
                    x
                }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn while_with_continue() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    let mut sum = 0;
                    while x < 10 {
                        x = x + 1;
                        if x == 5 {
                            continue;
                        }
                        sum = sum + x;
                    }
                    sum
                }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn break_outside_loop_rejected() {
            let result = test_air("fn main() -> i32 { break; 0 }");
            assert!(result.is_err());
        }

        #[test]
        fn continue_outside_loop_rejected() {
            let result = test_air("fn main() -> i32 { continue; 0 }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Let Bindings
    // ========================================================================

    mod let_bindings {
        use super::*;

        #[test]
        fn basic_let() {
            assert!(test_air("fn main() -> i32 { let x = 42; x }").is_ok());
        }

        #[test]
        fn let_with_type_annotation() {
            assert!(test_air("fn main() -> i32 { let x: i32 = 42; x }").is_ok());
        }

        #[test]
        fn mutable_let() {
            let src = "fn main() -> i32 { let mut x = 1; x = 2; x }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn immutable_assignment_rejected() {
            let result = test_air("fn main() -> i32 { let x = 1; x = 2; x }");
            assert!(result.is_err());
        }

        #[test]
        fn shadowing_allowed() {
            let src = "fn main() -> i32 { let x = 1; let x = 2; x }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn shadowing_can_change_type() {
            let src = "fn main() -> bool { let x = 1; let x = true; x }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn undefined_variable_rejected() {
            let result = test_air("fn main() -> i32 { x }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Functions
    // ========================================================================

    mod functions {
        use super::*;

        #[test]
        fn function_call() {
            let src = r#"
                fn add(a: i32, b: i32) -> i32 { a + b }
                fn main() -> i32 { add(1, 2) }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn function_forward_reference() {
            let src = r#"
                fn main() -> i32 { foo() }
                fn foo() -> i32 { 42 }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn recursion() {
            let src = r#"
                fn factorial(n: i32) -> i32 {
                    if n <= 1 { 1 } else { n * factorial(n - 1) }
                }
                fn main() -> i32 { factorial(5) }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn mutual_recursion() {
            let src = r#"
                fn is_even(n: i32) -> bool {
                    if n == 0 { true } else { is_odd(n - 1) }
                }
                fn is_odd(n: i32) -> bool {
                    if n == 0 { false } else { is_even(n - 1) }
                }
                fn main() -> i32 { if is_even(4) { 1 } else { 0 } }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn wrong_argument_count_rejected() {
            let src = r#"
                fn add(a: i32, b: i32) -> i32 { a + b }
                fn main() -> i32 { add(1) }
            "#;
            let result = test_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn wrong_argument_type_rejected() {
            let src = r#"
                fn foo(x: i32) -> i32 { x }
                fn main() -> i32 { foo(true) }
            "#;
            let result = test_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn undefined_function_rejected() {
            let result = test_air("fn main() -> i32 { unknown() }");
            assert!(result.is_err());
        }

        #[test]
        fn return_type_mismatch_rejected() {
            let result = test_air("fn main() -> i32 { true }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Structs
    // ========================================================================

    mod structs {
        use super::*;

        #[test]
        fn struct_definition() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 { 0 }
            "#;
            let result = test_air(src).unwrap();
            // type_pool includes builtin types (StrBuf) plus user-defined
            // structs. There's 1 builtin (StrBuf) + 1 user-defined (Point) = 2
            // distinct structs.
            let all_struct_ids = result.type_pool.all_struct_ids();
            assert_eq!(all_struct_ids.len(), 2);
            // Verify Point is present
            let point_name = result.interner.get_or_intern("Point");
            let point_interned = result
                .type_pool
                .get_struct_by_file_name(rue_span::FileId::DEFAULT, point_name);
            assert!(
                point_interned.is_some(),
                "Point struct should exist in pool"
            );
        }

        #[test]
        fn struct_literal() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let _p = Point { x: 1, y: 2 };
                    0
                }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn struct_field_access() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { x: 10, y: 20 };
                    p.x + p.y
                }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn struct_field_order_independent() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { y: 2, x: 1 };
                    p.x
                }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn struct_unknown_field_rejected() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { x: 1, z: 2 };
                    0
                }
            "#;
            let result = test_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn struct_equality() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> bool {
                    let a = Point { x: 1, y: 2 };
                    let b = Point { x: 1, y: 2 };
                    a == b
                }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn struct_move_semantics() {
            // After moving a struct, it should not be usable
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn consume(p: Point) -> i32 { p.x }
                fn main() -> i32 {
                    let p = Point { x: 1, y: 2 };
                    let _a = consume(p);
                    p.x
                }
            "#;
            let result = test_air(src);
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Enums
    // ========================================================================

    mod enums {
        use super::*;

        #[test]
        fn enum_definition() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 { 0 }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn enum_variant_access() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 {
                    let _c = Color.Red;
                    0
                }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn enum_match() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 {
                    let c = Color.Green;
                    match c {
                        Color.Red => 1,
                        Color.Green => 2,
                        Color.Blue => 3,
                    }
                }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn enum_comparison_via_match() {
            // Enum equality comparison is done via match, not ==
            // (== is not yet implemented for enums)
            let src = r#"
                enum Color { Red, Green, Blue }
                fn eq(a: Color, b: Color) -> bool {
                    match a {
                        Color.Red => match b { Color.Red => true, _ => false },
                        Color.Green => match b { Color.Green => true, _ => false },
                        Color.Blue => match b { Color.Blue => true, _ => false },
                    }
                }
                fn main() -> i32 { if eq(Color.Red, Color.Red) { 1 } else { 0 } }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn unknown_enum_variant_rejected() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 {
                    let _c = Color.Yellow;
                    0
                }
            "#;
            let result = test_air(src);
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Arrays
    // ========================================================================

    mod arrays {
        use super::*;

        #[test]
        fn array_literal() {
            let src = "fn main() -> i32 { let _arr: [i32; 3] = [1, 2, 3]; 0 }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn array_indexing() {
            let src = "fn main() -> i32 { let arr: [i32; 3] = [1, 2, 3]; arr[1] }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn array_element_assignment() {
            let src = r#"
                fn main() -> i32 {
                    let mut arr: [i32; 3] = [1, 2, 3];
                    arr[0] = 10;
                    arr[0]
                }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn array_wrong_length_rejected() {
            let src = "fn main() -> i32 { let _arr: [i32; 3] = [1, 2]; 0 }";
            let result = test_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn array_mixed_types_rejected() {
            let src = "fn main() -> i32 { let _arr: [i32; 2] = [1, true]; 0 }";
            let result = test_air(src);
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Strings
    // ========================================================================

    mod strings {
        use super::*;

        #[test]
        fn string_literal() {
            let src = r#"fn main() -> i32 { let _s = "hello"; 0 }"#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn string_with_quote_escape() {
            // String escape sequences: \" is supported
            let src = r#"fn main() -> i32 { let _s = "hello\"world"; 0 }"#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn string_with_backslash_escape() {
            // String escape sequences: \\ is supported
            let src = r#"fn main() -> i32 { let _s = "hello\\world"; 0 }"#;
            assert!(test_air(src).is_ok());
        }
    }

    // ========================================================================
    // Block Expressions
    // ========================================================================

    mod blocks {
        use super::*;

        #[test]
        fn block_returns_final_expression() {
            let src = "fn main() -> i32 { { 1; 2; 3 } }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn block_with_let_bindings() {
            let src = "fn main() -> i32 { { let x = 1; let y = 2; x + y } }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn nested_blocks() {
            let src = "fn main() -> i32 { { { { 42 } } } }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn block_scoping() {
            // Variable should not be accessible outside block
            let result = test_air("fn main() -> i32 { { let x = 1; } x }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Never Type
    // ========================================================================

    mod never_type {
        use super::*;

        #[test]
        fn return_is_never() {
            let src = "fn main() -> i32 { return 42; }";
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn break_is_never() {
            let src = r#"
                fn main() -> i32 {
                    while true {
                        break;
                    }
                    0
                }
            "#;
            assert!(test_cfg(src).is_ok());
        }

        #[test]
        fn never_in_if_branch() {
            let src = "fn main() -> i32 { if true { 1 } else { return 2; } }";
            assert!(test_cfg(src).is_ok());
        }
    }

    // ========================================================================
    // Type Intrinsics
    // ========================================================================

    mod intrinsics {
        use super::*;

        #[test]
        fn panic_diverges_through_cfg_and_lowers() {
            // `@panic` is never-typed (RUE-512): the CFG carries a NEVER-typed
            // intrinsic and the block ends in `Unreachable`, not a return, just
            // like a `-> !` call. It still lowers for every backend.
            for (name, body) in [
                ("trailing", "@panic()"),
                ("trailing_message", "@panic(\"boom\")"),
                ("semicolon", "@panic();"),
            ] {
                let source = format!("fn probe() {{ {body} }} fn main() -> i32 {{ probe(); 0 }}");
                let state = test_cfg(&source)
                    .expect("every panic form must reach a verified, terminated CFG");
                let cfg = &state
                    .functions
                    .iter()
                    .find(|function| function.cfg.fn_name() == "probe")
                    .unwrap_or_else(|| panic!("missing CFG for {name}"))
                    .cfg;
                let intrinsic_types: Vec<_> = cfg
                    .blocks()
                    .iter()
                    .flat_map(|block| block.insts.iter())
                    .filter_map(|value| {
                        let inst = cfg.get_inst(*value);
                        matches!(inst.data, rue_cfg::CfgInstData::Intrinsic { .. })
                            .then_some(inst.ty)
                    })
                    .collect();
                assert_eq!(
                    intrinsic_types,
                    vec![Type::NEVER],
                    "{name} must carry the never result into CFG"
                );

                // A diverging panic has no reachable return; the block that
                // evaluates it ends in `Unreachable`.
                assert!(
                    cfg.blocks()
                        .iter()
                        .any(|block| matches!(block.terminator, rue_cfg::Terminator::Unreachable)),
                    "{name} must diverge into an Unreachable terminator"
                );
                assert!(
                    !cfg.blocks().iter().any(|block| matches!(
                        block.terminator,
                        rue_cfg::Terminator::Return { .. }
                    )),
                    "{name} must not synthesize a return past a diverging @panic"
                );

                for &target in Target::all() {
                    generate_mir(cfg, &state.type_pool, &state.interner, target)
                        .unwrap_or_else(|error| panic!("{name} must lower for {target}: {error}"));
                }
                test_compile_source(&source).unwrap_or_else(|error| {
                    panic!("{name} must compile and link natively: {error}")
                });
            }
        }

        #[test]
        fn assert_uses_the_unit_contract_through_cfg() {
            // `@assert` is unit-typed: it returns on the success path, so the CFG
            // reuses the `UnitConst`-style dummy value for the trailing return.
            for (name, body) in [
                ("assertion", "@assert(true)"),
                ("assertion_with_message", "@assert(true, \"ok\")"),
            ] {
                let source = format!("fn probe() {{ {body} }} fn main() -> i32 {{ probe(); 0 }}");
                let state = test_cfg(&source)
                    .expect("every unit-valued assert form must reach a verified, terminated CFG");
                let cfg = &state
                    .functions
                    .iter()
                    .find(|function| function.cfg.fn_name() == "probe")
                    .unwrap_or_else(|| panic!("missing CFG for {name}"))
                    .cfg;
                let intrinsic_types: Vec<_> = cfg
                    .blocks()
                    .iter()
                    .flat_map(|block| block.insts.iter())
                    .filter_map(|value| {
                        let inst = cfg.get_inst(*value);
                        matches!(inst.data, rue_cfg::CfgInstData::Intrinsic { .. })
                            .then_some(inst.ty)
                    })
                    .collect();
                assert_eq!(
                    intrinsic_types,
                    vec![Type::UNIT],
                    "{name} must carry the unit result into CFG"
                );

                let return_values: Vec<_> = cfg
                    .blocks()
                    .iter()
                    .filter_map(|block| match block.terminator {
                        rue_cfg::Terminator::Return { value } => Some(value),
                        _ => None,
                    })
                    .collect();
                assert_eq!(return_values.len(), 1, "{name} must have one return");
                let return_value = return_values[0].expect("unit return must use a dummy value");
                let return_inst = cfg.get_inst(return_value);
                assert_eq!(return_inst.ty, Type::UNIT, "{name} return value type");
                assert!(
                    matches!(return_inst.data, rue_cfg::CfgInstData::Const(0)),
                    "{name} must return the established dummy unit value, not a side-effect-only intrinsic"
                );

                for &target in Target::all() {
                    generate_mir(cfg, &state.type_pool, &state.interner, target)
                        .unwrap_or_else(|error| panic!("{name} must lower for {target}: {error}"));
                }
                test_compile_source(&source).unwrap_or_else(|error| {
                    panic!("{name} must compile and link natively: {error}")
                });
            }
        }

        #[test]
        fn panic_participates_in_never_coercion() {
            // `@panic` is `!`, so it coerces into any expected type — a bare
            // function tail, a typed `let` initializer, and a typed `if`/`else`
            // arm all type-check and compile (RUE-512).
            for src in [
                "fn main() -> i32 { @panic() }",
                "fn main() -> i32 { let value: i32 = @panic(); value }",
                "fn main() -> i32 { if true { 1 } else { @panic() } }",
            ] {
                test_cfg(src).unwrap_or_else(|error| {
                    panic!(
                        "never-typed @panic must coerce into a non-unit context ({src}): {error}"
                    )
                });
            }
        }

        #[test]
        fn never_operands_coerce_to_abort_intrinsic_parameters() {
            for (name, body) in [
                ("assert condition", "@assert(diverge())"),
                ("panic message", "@panic(diverge())"),
            ] {
                let source = format!(
                    "fn diverge() -> ! {{ loop {{}} }} fn probe() {{ {body} }} fn main() -> i32 {{ probe(); 0 }}"
                );
                let state = test_cfg(&source)
                    .unwrap_or_else(|error| panic!("never must coerce to {name}: {error}"));
                for &target in Target::all() {
                    for function in &state.functions {
                        generate_mir(&function.cfg, &state.type_pool, &state.interner, target)
                            .unwrap_or_else(|error| {
                                panic!("{name} must lower for {target}: {error}")
                            });
                    }
                }
                test_compile_source(&source)
                    .unwrap_or_else(|error| panic!("{name} must compile and link: {error}"));
            }
        }

        #[test]
        fn size_of_intrinsic() {
            // @size_of returns i32
            let src = "fn main() -> i32 { @size_of(i32) }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn align_of_intrinsic() {
            // @align_of returns i32
            let src = "fn main() -> i32 { @align_of(i64) }";
            assert!(test_air(src).is_ok());
        }
    }

    // ========================================================================
    // CFG Construction
    // ========================================================================

    mod cfg_construction {
        use super::*;

        #[test]
        fn cfg_has_correct_function_count() {
            let src = r#"
                fn foo() -> i32 { 1 }
                fn bar() -> i32 { 2 }
                fn main() -> i32 { foo() + bar() }
            "#;
            let state = test_cfg(src).unwrap();
            assert_eq!(state.functions.len(), 3);
        }

        #[test]
        fn cfg_branches_for_if() {
            let src = "fn main() -> i32 { if true { 1 } else { 0 } }";
            let state = test_cfg(src).unwrap();
            // CFG should have multiple blocks for branching
            let main_cfg = &state.functions[0].cfg;
            assert!(main_cfg.blocks().len() >= 3); // entry, then, else, merge
        }

        #[test]
        fn cfg_loop_for_while() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    while x < 10 { x = x + 1; }
                    x
                }
            "#;
            let state = test_cfg(src).unwrap();
            let main_cfg = &state.functions[0].cfg;
            assert!(main_cfg.blocks().len() >= 3); // header, body, exit
        }
    }

    // ========================================================================
    // Error Messages
    // ========================================================================

    mod error_messages {
        use super::*;

        #[test]
        fn type_mismatch_error_is_descriptive() {
            let result = test_air("fn main() -> i32 { true }");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("type mismatch") || err.contains("expected"));
            assert!(err.contains("i32") || err.contains("bool"));
        }

        #[test]
        fn undefined_variable_error_is_descriptive() {
            let result = test_air("fn main() -> i32 { unknown_var }");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("undefined") || err.contains("unknown"));
        }

        #[test]
        fn missing_field_error_is_descriptive() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { x: 1 };
                    0
                }
            "#;
            let result = test_air(src);
            let err = result.unwrap_err().to_string();
            assert!(err.contains("missing") || err.contains("field"));
        }
    }

    // ========================================================================
    // Warnings
    // ========================================================================

    mod warnings {
        use super::*;

        #[test]
        fn unused_variable_warning() {
            let result = test_air("fn main() -> i32 { let x = 42; 0 }").unwrap();
            assert_eq!(result.warnings.len(), 1);
            assert!(result.warnings[0].to_string().contains("unused"));
        }

        #[test]
        fn underscore_prefix_suppresses_warning() {
            let result = test_air("fn main() -> i32 { let _x = 42; 0 }").unwrap();
            assert_eq!(result.warnings.len(), 0);
        }

        #[test]
        fn used_variable_no_warning() {
            let result = test_air("fn main() -> i32 { let x = 42; x }").unwrap();
            assert_eq!(result.warnings.len(), 0);
        }
    }

    // ========================================================================
    // Edge Cases
    // ========================================================================

    mod edge_cases {
        use super::*;

        #[test]
        fn empty_function_body() {
            assert!(test_air("fn main() -> () { }").is_ok());
        }

        #[test]
        fn deeply_nested_expressions() {
            let src = "fn main() -> i32 { ((((((1 + 2)))))) }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn many_parameters() {
            let src = r#"
                fn many(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
                    a + b + c + d + e + f
                }
                fn main() -> i32 { many(1, 2, 3, 4, 5, 6) }
            "#;
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn long_chain_of_operations() {
            let src = "fn main() -> i32 { 1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 }";
            assert!(test_air(src).is_ok());
        }

        #[test]
        fn multiple_functions_same_local_names() {
            let src = r#"
                fn foo() -> i32 { let x = 1; x }
                fn bar() -> i32 { let x = 2; x }
                fn main() -> i32 { foo() + bar() }
            "#;
            assert!(test_air(src).is_ok());
        }
    }
}
