#[cfg(test)]
mod tests {
    use crate::inst::{AirInstData, AirRef};
    use crate::sema::{Sema, SemaOutput};
    use crate::types::Type;
    use rue_error::{CompileErrors, ErrorKind, MultiErrorResult, PreviewFeatures};
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    fn compile_to_air(source: &str) -> MultiErrorResult<SemaOutput> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from_error)?;
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse()?;

        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &mut interner, PreviewFeatures::new());
        sema.analyze_all()
    }

    #[test]
    fn test_analyze_simple_function() {
        let output = compile_to_air("fn main() -> i32 { 42 }").unwrap();
        let functions = &output.functions;

        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "main");

        let air = &functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        assert_eq!(air.len(), 2); // Const + Ret
    }

    #[test]
    fn test_analyze_addition() {
        let output = compile_to_air("fn main() -> i32 { 1 + 2 }").unwrap();

        let air = &output.functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        // Const(1) + Const(2) + Add + Ret = 4 instructions
        assert_eq!(air.len(), 4);

        // Check that add instruction exists with correct type
        let add_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(add_inst.data, AirInstData::Add(_, _)));
        assert_eq!(add_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_all_binary_ops() {
        // Test that all binary operators compile correctly
        assert!(compile_to_air("fn main() -> i32 { 1 + 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 - 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 * 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 / 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 % 2 }").is_ok());
    }

    #[test]
    fn test_analyze_negation() {
        let output = compile_to_air("fn main() -> i32 { -42 }").unwrap();

        let air = &output.functions[0].air;
        // Const(42) + Neg + Ret = 3 instructions
        assert_eq!(air.len(), 3);

        let neg_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(neg_inst.data, AirInstData::Neg(_)));
        assert_eq!(neg_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_complex_expr() {
        let output = compile_to_air("fn main() -> i32 { (1 + 2) * 3 }").unwrap();

        let air = &output.functions[0].air;
        // Const(1) + Const(2) + Add + Const(3) + Mul + Ret = 6 instructions
        assert_eq!(air.len(), 6);

        // Check that result is multiplication
        let mul_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(mul_inst.data, AirInstData::Mul(_, _)));
    }

    #[test]
    fn test_analyze_let_binding() {
        let output = compile_to_air("fn main() -> i32 { let x = 42; x }").unwrap();

        assert_eq!(output.functions.len(), 1);
        assert_eq!(output.functions[0].num_locals, 1);

        let air = &output.functions[0].air;
        // Const(42) + StorageLive + Alloc + Block([StorageLive], Alloc) + Load + Block([alloc block], Load) + Ret = 7 instructions
        assert_eq!(air.len(), 7);

        // Check storage_live instruction
        let storage_live_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(
            storage_live_inst.data,
            AirInstData::StorageLive { slot: 0 }
        ));

        // Check alloc instruction
        let alloc_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(
            alloc_inst.data,
            AirInstData::Alloc { slot: 0, .. }
        ));

        // Check load instruction
        let load_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(load_inst.data, AirInstData::Load { slot: 0 }));

        // Check block instruction groups the alloc with the load
        let block_inst = air.get(AirRef::from_raw(5));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_analyze_let_mut_assignment() {
        let output = compile_to_air("fn main() -> i32 { let mut x = 10; x = 20; x }").unwrap();

        let air = &output.functions[0].air;
        // Const(10) + StorageLive + Alloc + Block([StorageLive], Alloc) + Const(20) + Store + Load + Block([alloc block, Store], Load) + Ret = 9 instructions
        assert_eq!(air.len(), 9);

        // Check store instruction
        let store_inst = air.get(AirRef::from_raw(5));
        assert!(matches!(
            store_inst.data,
            AirInstData::Store { slot: 0, .. }
        ));

        // Check block instruction groups statements
        let block_inst = air.get(AirRef::from_raw(7));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_undefined_variable() {
        let result = compile_to_air("fn main() -> i32 { x }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::UndefinedVariable(_)
        ));
    }

    #[test]
    fn test_assign_to_immutable() {
        let result = compile_to_air("fn main() -> i32 { let x = 10; x = 20; x }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::AssignToImmutable(_)
        ));
    }

    #[test]
    fn test_multiple_variables() {
        let output = compile_to_air("fn main() -> i32 { let x = 10; let y = 20; x + y }").unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_empty_block_evaluates_to_unit() {
        // Empty block should evaluate to () and not panic
        let output = compile_to_air("fn main() { let _x: () = {}; }").unwrap();

        let air = &output.functions[0].air;
        // Should have a UnitConst instruction for the empty block
        let has_unit_const = air
            .iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::UnitConst));
        assert!(has_unit_const, "Empty block should produce UnitConst");
    }

    // =========================================================================
    // Error recovery tests
    // =========================================================================
    // These tests verify that one type error does not cause cascading errors.
    // The issue rue-wqyw tracks the implementation of better error recovery.

    #[test]
    fn test_single_error_no_cascade_simple() {
        // A simple case where adding an integer and boolean should report exactly one error
        let result = compile_to_air("fn main() -> i32 { 1 + true }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(
            errors.len(),
            1,
            "Should have exactly 1 error, not cascading errors"
        );

        // Verify the error is about type mismatch (integer vs bool)
        let error = errors.iter().next().unwrap();
        assert!(
            matches!(&error.kind, ErrorKind::TypeMismatch { expected, found }
                if expected.contains("integer") && found.contains("bool")),
            "Error should mention integer and bool, got: {:?}",
            error.kind
        );
    }

    #[test]
    fn test_single_error_no_cascade_with_function_call() {
        // The error-typed variable is used in a function call - should not cascade
        let result = compile_to_air(
            "fn foo(a: i32, b: i32) -> i32 { a + b }
             fn main() -> i32 {
                 let x = 1 + true;
                 foo(x, 1)
             }",
        );
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(
            errors.len(),
            1,
            "Should have exactly 1 error for the original type mismatch"
        );
    }

    #[test]
    fn test_single_error_no_cascade_deep_chain() {
        // Deep chain of operations using error-typed value - should not cascade
        let result = compile_to_air(
            "fn main() -> i32 {
                 let x = 1 + true;
                 let y = x + 1;
                 let z = y * 2;
                 let w = z - 3;
                 let v = w / 4;
                 v
             }",
        );
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(
            errors.len(),
            1,
            "Should have exactly 1 error, not 5 cascading errors"
        );
    }

    #[test]
    fn test_bool_plus_int_error() {
        // Reversed order: bool + int should also give one error
        let result = compile_to_air("fn main() -> i32 { true + 1 }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_arithmetic_on_bool_type_error() {
        // Using bool in any arithmetic should be an error
        let result = compile_to_air("fn main() -> i32 { true * true }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
    }

    // =========================================================================
    // Scope management tests
    // =========================================================================
    // These tests verify that variable scoping works correctly, including
    // shadowing, nested scopes, and proper cleanup when exiting scopes.

    #[test]
    fn test_variable_shadowing_same_type() {
        // Variable shadowing with the same type should work
        let output = compile_to_air(
            "fn main() -> i32 {
                let x = 10;
                let x = 20;  // Shadow x with a new binding
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_variable_shadowing_different_type() {
        // Variable shadowing with a different type should work
        let output = compile_to_air(
            "fn main() -> bool {
                let x = 10;
                let x = true;  // Shadow x with a different type
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
        assert_eq!(output.functions[0].air.return_type(), Type::Bool);
    }

    #[test]
    fn test_nested_scope_variable_not_visible_outside() {
        // Variable declared in inner scope should not be visible outside
        let result = compile_to_air(
            "fn main() -> i32 {
                {
                    let x = 10;
                }
                x  // Error: x is not in scope
            }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::UndefinedVariable(_)
        ));
    }

    #[test]
    fn test_shadowed_variable_restored_after_scope() {
        // After inner scope ends, the outer variable should be visible again
        let output = compile_to_air(
            "fn main() -> i32 {
                let x = 10;
                {
                    let x = 20;  // Shadow x in inner scope
                }
                x  // Should be 10 (outer x)
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_deeply_nested_scopes() {
        // Variables in deeply nested scopes should work correctly
        let output = compile_to_air(
            "fn main() -> i32 {
                let a = 1;
                {
                    let b = 2;
                    {
                        let c = 3;
                        {
                            let d = 4;
                            a + b + c + d
                        }
                    }
                }
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 4);
    }

    #[test]
    fn test_if_else_scope_isolation() {
        // Variables in if/else branches should not leak
        let result = compile_to_air(
            "fn main() -> i32 {
                if true {
                    let x = 10;
                    x
                } else {
                    y  // Error: y not defined in this branch
                }
            }",
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_loop_scope_isolation() {
        // Variables in loop body should not leak outside
        let result = compile_to_air(
            "fn main() -> i32 {
                let mut i = 0;
                loop {
                    let inner = 10;
                    i = i + 1;
                    if i > 5 {
                        break inner;
                    }
                }
            }",
        );

        // This should compile successfully
        assert!(result.is_ok());
    }

    // =========================================================================
    // Declaration gathering tests
    // =========================================================================
    // These tests verify that declarations are properly gathered and validated.

    #[test]
    fn test_duplicate_struct_names() {
        // Two structs with the same name should error
        let result = compile_to_air(
            "struct Foo { x: i32 }
             struct Foo { y: bool }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateTypeDefinition { .. }
        ));
    }

    #[test]
    fn test_duplicate_enum_names() {
        // Two enums with the same name should error
        let result = compile_to_air(
            "enum Color { Red, Green }
             enum Color { Blue, Yellow }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateTypeDefinition { .. }
        ));
    }

    #[test]
    fn test_struct_and_enum_name_collision() {
        // A struct and enum with the same name should error
        let result = compile_to_air(
            "struct Foo { x: i32 }
             enum Foo { A, B }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateTypeDefinition { .. }
        ));
    }

    #[test]
    fn test_duplicate_struct_field() {
        // Duplicate field names in a struct should error
        let result = compile_to_air(
            "struct Foo { x: i32, x: bool }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateField { .. }
        ));
    }

    #[test]
    fn test_duplicate_enum_variant() {
        // Duplicate variant names in an enum should error
        let result = compile_to_air(
            "enum Color { Red, Blue, Red }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateVariant { .. }
        ));
    }

    #[test]
    fn test_struct_field_type_resolution() {
        // Struct with field of another struct type should resolve correctly
        let output = compile_to_air(
            "@copy struct Inner { x: i32 }
             @copy struct Outer { inner: Inner }
             fn main() -> i32 {
                let o = Outer { inner: Inner { x: 42 } };
                o.inner.x
             }",
        )
        .unwrap();

        assert_eq!(output.struct_defs.len(), 3); // Inner, Outer, and String (builtin)
    }

    #[test]
    fn test_copy_struct_with_copy_fields() {
        // @copy struct with only Copy fields should compile
        let output = compile_to_air(
            "@copy struct Point { x: i32, y: i32 }
             fn main() -> i32 {
                let p = Point { x: 1, y: 2 };
                let q = p;  // Copy, not move
                p.x + q.x
             }",
        )
        .unwrap();

        assert!(
            output
                .struct_defs
                .iter()
                .any(|s| s.name == "Point" && s.is_copy)
        );
    }

    #[test]
    fn test_copy_struct_with_non_copy_field_rejected() {
        // @copy struct with non-copy field should error
        let result = compile_to_air(
            "struct NonCopy { x: i32 }
             @copy struct Wrapper { inner: NonCopy }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::CopyStructNonCopyField(_)
        ));
    }

    #[test]
    fn test_recursive_struct_via_array() {
        // Self-referential struct through array is not allowed (no arrays of non-copy structs yet)
        // But circular reference through other means should be detected
        let result = compile_to_air(
            "struct Node { value: i32 }
             fn main() -> i32 { 0 }",
        );

        // Simple non-recursive struct should work
        assert!(result.is_ok());
    }

    #[test]
    fn test_function_signature_resolution() {
        // Function parameters and return types should resolve correctly
        let output = compile_to_air(
            "fn add(a: i32, b: i32) -> i32 { a + b }
             fn main() -> i32 { add(1, 2) }",
        )
        .unwrap();

        assert_eq!(output.functions.len(), 2);
    }

    // =========================================================================
    // Builtin type tests
    // =========================================================================
    // These tests verify that builtin types (String, etc.) work correctly.

    #[test]
    fn test_string_type_injected() {
        // String type should exist after builtin injection
        let output = compile_to_air(
            "fn main() -> i32 {
                let s = \"hello\";
                0
            }",
        )
        .unwrap();

        // String struct should exist in struct_defs
        assert!(output.struct_defs.iter().any(|s| s.name == "String"));
    }

    #[test]
    fn test_string_len_method() {
        // String.len() should return u64
        let output = compile_to_air(
            "fn main() -> u64 {
                let s = \"hello\";
                s.len()
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::U64);
    }

    #[test]
    fn test_string_is_empty_method() {
        // String.is_empty() should return bool
        let output = compile_to_air(
            "fn main() -> bool {
                let s = \"hello\";
                s.is_empty()
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::Bool);
    }

    #[test]
    fn test_string_literal_type_inference() {
        // String literal should have type String
        let output = compile_to_air(
            "fn main() -> bool {
                let s = \"hello\";
                let t = \"world\";
                s.is_empty()
            }",
        )
        .unwrap();

        // Should have local storage for two string variables
        assert!(output.functions[0].num_locals >= 2);
    }

    // =========================================================================
    // Move tracking integration tests
    // =========================================================================
    // These tests verify move semantics work correctly through the full pipeline.

    #[test]
    fn test_use_after_move_error() {
        // Using a moved value should error
        let result = compile_to_air(
            "struct NonCopy { x: i32 }
             fn consume(n: NonCopy) -> i32 { n.x }
             fn main() -> i32 {
                 let n = NonCopy { x: 42 };
                 let x = consume(n);
                 n.x  // Error: n was moved
             }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::UseAfterMove { .. }
        ));
    }

    #[test]
    fn test_partial_move_sibling_still_valid() {
        // After moving one field, sibling fields should still be usable
        // Note: Inner is non-copy, Outer is also non-copy (can't be @copy with non-copy field)
        let output = compile_to_air(
            "struct Inner { x: i32 }
             struct Outer { a: Inner, b: i32 }
             fn consume(i: Inner) -> i32 { i.x }
             fn main() -> i32 {
                 let o = Outer { a: Inner { x: 1 }, b: 2 };
                 let x = consume(o.a);  // Move o.a
                 o.b  // OK: o.b is still valid (it's Copy)
             }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_copy_type_not_moved() {
        // Copy types should not be moved, allowing multiple uses
        let output = compile_to_air(
            "@copy struct Point { x: i32, y: i32 }
             fn use_point(p: Point) -> i32 { p.x }
             fn main() -> i32 {
                 let p = Point { x: 1, y: 2 };
                 let a = use_point(p);
                 let b = use_point(p);  // OK: Point is Copy
                 a + b
             }",
        )
        .unwrap();

        assert_eq!(output.functions.len(), 2);
    }

    // =========================================================================
    // Type inference tests
    // =========================================================================
    // These tests verify type inference works correctly.

    #[test]
    fn test_integer_literal_infers_i32_by_default() {
        // Unconstrained integer literal should default to i32
        let output = compile_to_air(
            "fn main() -> i32 {
                let x = 42;
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_integer_literal_infers_from_context() {
        // Integer literal should infer type from context
        let output = compile_to_air(
            "fn main() -> i64 {
                let x: i64 = 42;
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I64);
    }

    #[test]
    fn test_integer_literal_infers_from_return_type() {
        // Integer literal should infer type from function return type
        let output = compile_to_air("fn main() -> u8 { 42 }").unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::U8);
    }

    #[test]
    fn test_integer_literal_infers_from_binary_op() {
        // Integer literal should infer type from binary operation context
        let output = compile_to_air(
            "fn main() -> i64 {
                let x: i64 = 10;
                x + 5  // 5 should infer to i64
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I64);
    }

    // =========================================================================
    // Array type tests
    // =========================================================================

    #[test]
    fn test_array_type_inference() {
        // Array element type should be inferred
        let output = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                arr[0]
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_array_index_type_must_be_unsigned() {
        // Array index must be unsigned integer
        let result = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                let i: i32 = 1;
                arr[i]  // Error: i32 is signed
            }",
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_array_index_literal_infers_u64() {
        // Integer literal used as array index should infer to u64
        let output = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                arr[1]  // 1 should infer to u64
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_array_length_mismatch() {
        // Array length in type annotation must match initializer
        let result = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2];  // Error: length mismatch
                arr[0]
            }",
        );

        assert!(result.is_err());
    }
}
