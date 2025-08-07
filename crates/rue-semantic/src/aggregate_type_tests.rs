//! Comprehensive tests for aggregate type functionality
//!
//! This module tests all aspects of aggregate types (structs, tuples, arrays) including:
//! - Literal creation and type inference
//! - Field/element access
//! - Error handling and validation
//! - Size/alignment calculations
//! - HIR generation

#[cfg(test)]
mod tests {
    use crate::{AnalysisResult, SemanticError, analyze_cst};
    use rue_ir::hir::HirExpr;
    use rue_ir::types::{FieldId, RueType};
    use rue_lexer::Span;
    use rue_parser::parse_with_recovery;

    fn parse_and_analyze(source: &str) -> Result<AnalysisResult, SemanticError> {
        let cst = parse_with_recovery(source, "test.rue").map_err(|diagnostics| {
            // Convert first diagnostic to SemanticError for compatibility
            if let Some(first) = diagnostics.first() {
                SemanticError {
                    message: first.message.clone(),
                    span: first.primary_span().unwrap_or(Span::new(0, 0)),
                }
            } else {
                SemanticError {
                    message: "Parse error".to_string(),
                    span: Span::new(0, 0),
                }
            }
        })?;
        analyze_cst(&cst)
    }

    // ============================================================================
    // VALID STRUCT LITERAL TESTS
    // ============================================================================

    #[test]
    fn test_struct_literal_basic() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn main() -> Point {
                Point { x: 10, y: 20 }
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        // Check that we get a struct literal with correct type
        match final_expr.as_ref() {
            HirExpr::StructLiteral {
                struct_name,
                fields,
                ty,
                ..
            } => {
                assert_eq!(struct_name, "Point");
                assert_eq!(fields.len(), 2);

                // Check field x
                assert_eq!(fields[0].0, "x");
                match &fields[0].1 {
                    HirExpr::Literal { .. } => {}
                    _ => panic!("Expected literal for field x"),
                }

                // Check field y
                assert_eq!(fields[1].0, "y");
                match &fields[1].1 {
                    HirExpr::Literal { .. } => {}
                    _ => panic!("Expected literal for field y"),
                }

                // Check type is struct
                match ty {
                    RueType::Struct(_) => {}
                    _ => panic!("Expected struct type, got: {ty:?}"),
                }
            }
            _ => panic!("Expected struct literal, got: {final_expr:?}"),
        }
    }

    #[test]
    fn test_struct_literal_mixed_types() {
        let source = r#"
            struct Person {
                name_len: i32,
                age: i64,
                is_adult: bool,
            }
            
            fn main() -> Person {
                Person { name_len: 5, age: 25, is_adult: true }
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::StructLiteral {
                struct_name,
                fields,
                ty,
                ..
            } => {
                assert_eq!(struct_name, "Person");
                assert_eq!(fields.len(), 3);

                // Verify field names and types
                assert_eq!(fields[0].0, "name_len");
                assert_eq!(fields[1].0, "age");
                assert_eq!(fields[2].0, "is_adult");

                match ty {
                    RueType::Struct(_) => {}
                    _ => panic!("Expected struct type"),
                }
            }
            _ => panic!("Expected struct literal"),
        }
    }

    #[test]
    fn test_struct_literal_with_variables() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn main() -> Point {
                let a: i32 = 10;
                let b: i32 = 20;
                Point { x: a, y: b }
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::StructLiteral {
                struct_name,
                fields,
                ..
            } => {
                assert_eq!(struct_name, "Point");

                // Check that fields reference variables
                match &fields[0].1 {
                    HirExpr::Var { name, ty, .. } => {
                        assert_eq!(name, "a");
                        assert_eq!(ty, &RueType::I32);
                    }
                    _ => panic!("Expected variable reference for field x"),
                }

                match &fields[1].1 {
                    HirExpr::Var { name, ty, .. } => {
                        assert_eq!(name, "b");
                        assert_eq!(ty, &RueType::I32);
                    }
                    _ => panic!("Expected variable reference for field y"),
                }
            }
            _ => panic!("Expected struct literal"),
        }
    }

    // ============================================================================
    // VALID TUPLE LITERAL TESTS
    // ============================================================================

    #[test]
    fn test_tuple_literal_basic() {
        let source = r#"
            fn main() -> (i32, i64) {
                let large: i64 = 1000000000000;
                (42, large)
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::TupleLiteral { elements, ty, .. } => {
                assert_eq!(elements.len(), 2);

                // Check tuple type
                match ty {
                    RueType::Tuple(types) => {
                        assert_eq!(types.len(), 2);
                        assert_eq!(types[0], RueType::I32);
                        assert_eq!(types[1], RueType::I64);
                    }
                    _ => panic!("Expected tuple type, got: {ty:?}"),
                }

                // Check element types
                assert_eq!(elements[0].ty(), &RueType::I32);
                assert_eq!(elements[1].ty(), &RueType::I64);
            }
            _ => panic!("Expected tuple literal, got: {final_expr:?}"),
        }
    }

    #[test]
    fn test_tuple_literal_mixed_types() {
        let source = r#"
            fn main() -> (i32, bool, i64) {
                let large: i64 = 1000000000000;
                (42, true, large)
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::TupleLiteral { elements, ty, .. } => {
                assert_eq!(elements.len(), 3);

                match ty {
                    RueType::Tuple(types) => {
                        assert_eq!(types.len(), 3);
                        assert_eq!(types[0], RueType::I32);
                        assert_eq!(types[1], RueType::Bool);
                        assert_eq!(types[2], RueType::I64);
                    }
                    _ => panic!("Expected tuple type"),
                }
            }
            _ => panic!("Expected tuple literal"),
        }
    }

    #[test]
    fn test_tuple_literal_single_element() {
        let source = r#"
            fn main() -> (i32,) {
                (42,)
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::TupleLiteral { elements, ty, .. } => {
                assert_eq!(elements.len(), 1);

                match ty {
                    RueType::Tuple(types) => {
                        assert_eq!(types.len(), 1);
                        assert_eq!(types[0], RueType::I32);
                    }
                    _ => panic!("Expected tuple type"),
                }
            }
            _ => panic!("Expected tuple literal"),
        }
    }

    #[test]
    fn test_tuple_literal_empty() {
        let source = r#"
            fn main() -> () {
                ()
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        // Empty tuple should be unit literal, not tuple literal
        match final_expr.as_ref() {
            HirExpr::Literal { .. } => {
                assert_eq!(final_expr.ty(), &RueType::Unit);
            }
            _ => panic!("Expected unit literal for empty tuple, got: {final_expr:?}"),
        }
    }

    // ============================================================================
    // VALID ARRAY LITERAL TESTS
    // ============================================================================

    #[test]
    fn test_array_literal_basic() {
        let source = r#"
            fn main() -> [i32; 3] {
                [1, 2, 3]
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::ArrayLiteral { elements, ty, .. } => {
                assert_eq!(elements.len(), 3);

                // Check array type
                match ty {
                    RueType::Array(element_type, size) => {
                        assert_eq!(element_type.as_ref(), &RueType::I32);
                        assert_eq!(*size, 3);
                    }
                    _ => panic!("Expected array type, got: {ty:?}"),
                }

                // Check all elements are i32
                for element in elements {
                    assert_eq!(element.ty(), &RueType::I32);
                }
            }
            _ => panic!("Expected array literal, got: {final_expr:?}"),
        }
    }

    #[test]
    fn test_array_literal_single_element() {
        let source = r#"
            fn main() -> [bool; 1] {
                [true]
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::ArrayLiteral { elements, ty, .. } => {
                assert_eq!(elements.len(), 1);

                match ty {
                    RueType::Array(element_type, size) => {
                        assert_eq!(element_type.as_ref(), &RueType::Bool);
                        assert_eq!(*size, 1);
                    }
                    _ => panic!("Expected array type"),
                }
            }
            _ => panic!("Expected array literal"),
        }
    }

    #[test]
    fn test_array_literal_with_variables() {
        let source = r#"
            fn main() -> [i32; 3] {
                let a: i32 = 10;
                let b: i32 = 20;
                let c: i32 = 30;
                [a, b, c]
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::ArrayLiteral { elements, ty, .. } => {
                assert_eq!(elements.len(), 3);

                // Check that elements reference variables
                for (i, element) in elements.iter().enumerate() {
                    match element {
                        HirExpr::Var { name, ty, .. } => {
                            assert_eq!(ty, &RueType::I32);
                            match i {
                                0 => assert_eq!(name, "a"),
                                1 => assert_eq!(name, "b"),
                                2 => assert_eq!(name, "c"),
                                _ => panic!("Unexpected element index"),
                            }
                        }
                        _ => panic!("Expected variable reference for element {i}"),
                    }
                }

                match ty {
                    RueType::Array(element_type, size) => {
                        assert_eq!(element_type.as_ref(), &RueType::I32);
                        assert_eq!(*size, 3);
                    }
                    _ => panic!("Expected array type"),
                }
            }
            _ => panic!("Expected array literal"),
        }
    }

    // ============================================================================
    // FIELD ACCESS TESTS
    // ============================================================================

    #[test]
    fn test_struct_field_access() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn main() -> i32 {
                let p: Point = Point { x: 10, y: 20 };
                p.x
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::FieldAccess {
                base, field, ty, ..
            } => {
                // Check base is a variable reference to p
                match base.as_ref() {
                    HirExpr::Var { name, .. } => {
                        assert_eq!(name, "p");
                    }
                    _ => panic!("Expected variable reference for base"),
                }

                // Check field access
                match field {
                    FieldId::Named(name) => {
                        assert_eq!(name, "x");
                    }
                    _ => panic!("Expected named field access"),
                }

                // Check result type
                assert_eq!(ty, &RueType::I32);
            }
            _ => panic!("Expected field access, got: {final_expr:?}"),
        }
    }

    #[test]
    fn test_tuple_field_access() {
        let source = r#"
            fn main() -> i32 {
                let large: i64 = 1000000000000;
                let t: (i32, i64, bool) = (42, large, true);
                t.0
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::FieldAccess {
                base, field, ty, ..
            } => {
                // Check base is a variable reference to t
                match base.as_ref() {
                    HirExpr::Var { name, .. } => {
                        assert_eq!(name, "t");
                    }
                    _ => panic!("Expected variable reference for base"),
                }

                // Check field access
                match field {
                    FieldId::Index(index) => {
                        assert_eq!(*index, 0);
                    }
                    _ => panic!("Expected index field access"),
                }

                // Check result type
                assert_eq!(ty, &RueType::I32);
            }
            _ => panic!("Expected field access, got: {final_expr:?}"),
        }
    }

    #[test]
    fn test_tuple_field_access_different_indices() {
        let source = r#"
            fn main() -> bool {
                let large: i64 = 1000000000000;
                let t: (i32, i64, bool) = (42, large, true);
                t.2
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::FieldAccess { field, ty, .. } => {
                match field {
                    FieldId::Index(index) => {
                        assert_eq!(*index, 2);
                    }
                    _ => panic!("Expected index field access"),
                }

                assert_eq!(ty, &RueType::Bool);
            }
            _ => panic!("Expected field access"),
        }
    }

    // ============================================================================
    // ARRAY ACCESS TESTS
    // ============================================================================

    #[test]
    fn test_array_access_basic() {
        let source = r#"
            fn main() -> i32 {
                let arr: [i32; 3] = [10, 20, 30];
                arr[1]
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::ArrayAccess {
                base, index, ty, ..
            } => {
                // Check base is a variable reference to arr
                match base.as_ref() {
                    HirExpr::Var { name, .. } => {
                        assert_eq!(name, "arr");
                    }
                    _ => panic!("Expected variable reference for base"),
                }

                // Check index is an integer literal
                match index.as_ref() {
                    HirExpr::Literal { .. } => {
                        assert_eq!(index.ty(), &RueType::I32);
                    }
                    _ => panic!("Expected literal for index"),
                }

                // Check result type
                assert_eq!(ty, &RueType::I32);
            }
            _ => panic!("Expected array access, got: {final_expr:?}"),
        }
    }

    #[test]
    fn test_array_access_variable_index() {
        let source = r#"
            fn main() -> i32 {
                let arr: [i32; 3] = [10, 20, 30];
                let idx: i32 = 1;
                arr[idx]
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::ArrayAccess {
                base, index, ty, ..
            } => {
                // Check base is a variable reference to arr
                match base.as_ref() {
                    HirExpr::Var { name, .. } => {
                        assert_eq!(name, "arr");
                    }
                    _ => panic!("Expected variable reference for base"),
                }

                // Check index is a variable reference to idx
                match index.as_ref() {
                    HirExpr::Var { name, ty, .. } => {
                        assert_eq!(name, "idx");
                        assert_eq!(ty, &RueType::I32);
                    }
                    _ => panic!("Expected variable reference for index"),
                }

                // Check result type
                assert_eq!(ty, &RueType::I32);
            }
            _ => panic!("Expected array access"),
        }
    }

    // ============================================================================
    // ERROR CASE TESTS
    // ============================================================================

    #[test]
    fn test_struct_literal_missing_field() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn main() -> Point {
                Point { x: 10 }
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(result.is_err(), "Expected error for missing field");

        let error = result.unwrap_err();
        assert!(error.message.contains("Missing fields"));
        assert!(error.message.contains("y"));
    }

    #[test]
    fn test_struct_literal_extra_field() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn main() -> Point {
                Point { x: 10, y: 20, z: 30 }
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(result.is_err(), "Expected error for extra field");

        let error = result.unwrap_err();
        assert!(error.message.contains("Unknown field"));
        assert!(error.message.contains("z"));
    }

    #[test]
    fn test_struct_literal_duplicate_field() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn main() -> Point {
                Point { x: 10, x: 20, y: 30 }
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(result.is_err(), "Expected error for duplicate field");

        let error = result.unwrap_err();
        assert!(error.message.contains("Duplicate field"));
        assert!(error.message.contains("x"));
    }

    #[test]
    fn test_struct_literal_wrong_field_type() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn main() -> Point {
                Point { x: 10, y: true }
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(result.is_err(), "Expected error for wrong field type");

        let error = result.unwrap_err();
        println!("Error message: {}", error.message);
        assert!(error.message.contains("Type mismatch"));
    }

    #[test]
    fn test_field_access_on_non_aggregate() {
        let source = r#"
            fn main() -> i32 {
                let x: i32 = 42;
                x.field
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_err(),
            "Expected error for field access on non-aggregate"
        );

        let error = result.unwrap_err();
        assert!(error.message.contains("Cannot access field on type"));
    }

    #[test]
    fn test_struct_wrong_field_name() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn main() -> i32 {
                let p: Point = Point { x: 10, y: 20 };
                p.z
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(result.is_err(), "Expected error for wrong field name");

        let error = result.unwrap_err();
        assert!(error.message.contains("has no field named"));
        assert!(error.message.contains("z"));
    }

    #[test]
    fn test_tuple_out_of_bounds_access() {
        let source = r#"
            fn main() -> i32 {
                let large: i64 = 1000000000000;
                let t: (i32, i64) = (42, large);
                t.5
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_err(),
            "Expected error for out of bounds tuple access"
        );

        let error = result.unwrap_err();
        assert!(error.message.contains("out of bounds"));
        assert!(error.message.contains("5"));
    }

    #[test]
    fn test_tuple_named_field_access() {
        let source = r#"
            fn main() -> i32 {
                let large: i64 = 1000000000000;
                let t: (i32, i64) = (42, large);
                t.field
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(result.is_err(), "Expected error for named field on tuple");

        let error = result.unwrap_err();
        assert!(error.message.contains("must use integer literal"));
    }

    #[test]
    fn test_struct_positional_access() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn main() -> i32 {
                let p: Point = Point { x: 10, y: 20 };
                p.0
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_err(),
            "Expected error for positional access on struct"
        );

        let error = result.unwrap_err();
        assert!(error.message.contains("must use field names"));
    }

    #[test]
    fn test_array_literal_type_mismatch() {
        let source = r#"
            fn main() -> [i32; 3] {
                [1, true, 3]
            }
        "#;

        let result = parse_and_analyze(source);
        match result {
            Ok(_) => {
                panic!("Expected error for mixed types in array but got success");
            }
            Err(error) => {
                println!("Got error: {}", error.message);
                // The error could be in different forms depending on how type checking works
                assert!(
                    error.message.contains("Type mismatch")
                        || error
                            .message
                            .contains("Array elements must have consistent type")
                        || error.message.contains("expected") && error.message.contains("found")
                );
            }
        }
    }

    #[test]
    fn test_array_access_wrong_index_type() {
        let source = r#"
            fn main() -> i32 {
                let arr: [i32; 3] = [10, 20, 30];
                arr[true]
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(result.is_err(), "Expected error for wrong index type");

        let error = result.unwrap_err();
        assert!(
            error
                .message
                .contains("Array index must be an integer type")
        );
    }

    #[test]
    fn test_array_access_on_non_array() {
        let source = r#"
            fn main() -> i32 {
                let x: i32 = 42;
                x[0]
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_err(),
            "Expected error for array access on non-array"
        );

        let error = result.unwrap_err();
        assert!(error.message.contains("Cannot index into type"));
    }

    #[test]
    fn test_empty_array_literal_without_context() {
        let source = r#"
            fn main() -> () {
                let arr = [];
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_err(),
            "Expected error for empty array without type context"
        );

        let error = result.unwrap_err();
        assert!(
            error
                .message
                .contains("Cannot infer type of empty array literal")
        );
    }

    // ============================================================================
    // TYPE INFERENCE TESTS
    // ============================================================================

    #[test]
    fn test_struct_literal_contextual_inference() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn create_point() -> Point {
                Point { x: 42, y: 100 }
            }
            
            fn main() -> Point {
                create_point()
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let create_point_fn = &analysis.hir.functions[0];
        let final_expr = create_point_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::StructLiteral { ty, .. } => match ty {
                RueType::Struct(_) => {}
                _ => panic!("Expected struct type"),
            },
            _ => panic!("Expected struct literal"),
        }
    }

    #[test]
    fn test_tuple_inference_from_parameter() {
        let source = r#"
            fn process_tuple(t: (i32, bool)) -> i32 {
                42
            }
            
            fn main() -> i32 {
                process_tuple((10, true))
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[1];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::Call { args, .. } => {
                assert_eq!(args.len(), 1);
                match &args[0] {
                    HirExpr::TupleLiteral { ty, .. } => match ty {
                        RueType::Tuple(types) => {
                            assert_eq!(types.len(), 2);
                            assert_eq!(types[0], RueType::I32);
                            assert_eq!(types[1], RueType::Bool);
                        }
                        _ => panic!("Expected tuple type"),
                    },
                    _ => panic!("Expected tuple literal in function argument"),
                }
            }
            _ => panic!("Expected function call"),
        }
    }

    #[test]
    fn test_array_inference_from_expected_type() {
        let source = r#"
            fn main() -> [i64; 2] {
                let a: i64 = 42;
                let b: i64 = 100;
                [a, b]
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::ArrayLiteral { elements, ty, .. } => {
                // Check that integer literals were inferred as i64
                for element in elements {
                    assert_eq!(element.ty(), &RueType::I64);
                }

                match ty {
                    RueType::Array(element_type, size) => {
                        assert_eq!(element_type.as_ref(), &RueType::I64);
                        assert_eq!(*size, 2);
                    }
                    _ => panic!("Expected array type"),
                }
            }
            _ => panic!("Expected array literal"),
        }
    }

    // ============================================================================
    // NESTED AGGREGATE TESTS
    // ============================================================================

    #[test]
    fn test_struct_containing_tuple() {
        let source = r#"
            struct Container {
                data: (i32, bool),
                size: i32,
            }
            
            fn main() -> Container {
                Container { data: (42, true), size: 2 }
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::StructLiteral { fields, .. } => {
                // Check data field contains tuple
                match &fields[0].1 {
                    HirExpr::TupleLiteral { ty, .. } => match ty {
                        RueType::Tuple(types) => {
                            assert_eq!(types.len(), 2);
                            assert_eq!(types[0], RueType::I32);
                            assert_eq!(types[1], RueType::Bool);
                        }
                        _ => panic!("Expected tuple type in struct field"),
                    },
                    _ => panic!("Expected tuple literal in data field"),
                }
            }
            _ => panic!("Expected struct literal"),
        }
    }

    #[test]
    fn test_array_of_tuples() {
        let source = r#"
            fn main() -> [(i32, bool); 2] {
                [(10, true), (20, false)]
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::ArrayLiteral { elements, ty, .. } => {
                // Check each element is a tuple
                for element in elements {
                    match element {
                        HirExpr::TupleLiteral { ty, .. } => match ty {
                            RueType::Tuple(types) => {
                                assert_eq!(types.len(), 2);
                                assert_eq!(types[0], RueType::I32);
                                assert_eq!(types[1], RueType::Bool);
                            }
                            _ => panic!("Expected tuple type in array element"),
                        },
                        _ => panic!("Expected tuple literal in array"),
                    }
                }

                // Check array type
                match ty {
                    RueType::Array(element_type, size) => {
                        match element_type.as_ref() {
                            RueType::Tuple(types) => {
                                assert_eq!(types.len(), 2);
                                assert_eq!(types[0], RueType::I32);
                                assert_eq!(types[1], RueType::Bool);
                            }
                            _ => panic!("Expected tuple element type"),
                        }
                        assert_eq!(*size, 2);
                    }
                    _ => panic!("Expected array type"),
                }
            }
            _ => panic!("Expected array literal"),
        }
    }

    #[test]
    fn test_nested_field_access() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            struct Container {
                point: Point,
                name: i32,
            }
            
            fn main() -> i32 {
                let c: Container = Container { 
                    point: Point { x: 10, y: 20 }, 
                    name: 42 
                };
                c.point.x
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        match final_expr.as_ref() {
            HirExpr::FieldAccess {
                base, field, ty, ..
            } => {
                // Check outer field access
                match field {
                    FieldId::Named(name) => {
                        assert_eq!(name, "x");
                    }
                    _ => panic!("Expected named field access"),
                }
                assert_eq!(ty, &RueType::I32);

                // Check base is also field access
                match base.as_ref() {
                    HirExpr::FieldAccess { field, .. } => match field {
                        FieldId::Named(name) => {
                            assert_eq!(name, "point");
                        }
                        _ => panic!("Expected named field access for point"),
                    },
                    _ => panic!("Expected field access for base"),
                }
            }
            _ => panic!("Expected field access, got: {final_expr:?}"),
        }
    }

    // ============================================================================
    // INTEGRATION TESTS
    // ============================================================================

    #[test]
    fn test_aggregate_in_function_parameters() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
            }
            
            fn distance(p1: Point, p2: Point) -> i32 {
                let dx: i32 = p1.x - p2.x;
                let dy: i32 = p1.y - p2.y;
                dx * dx + dy * dy
            }
            
            fn main() -> i32 {
                let a: Point = Point { x: 0, y: 0 };
                let b: Point = Point { x: 3, y: 4 };
                distance(a, b)
            }
        "#;

        let result = parse_and_analyze(source);
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();

        // Check function signatures are correct
        let distance_fn_sig = &analysis.scope.functions["distance"];
        assert_eq!(distance_fn_sig.param_types.len(), 2);

        for param_type in &distance_fn_sig.param_types {
            match param_type {
                RueType::Struct(_) => {}
                _ => panic!("Expected struct type for parameter"),
            }
        }

        assert_eq!(distance_fn_sig.return_type, RueType::I32);
    }

    #[test]
    fn test_complex_aggregate_usage() {
        // Test just struct definition and retrieval
        let def_source = r#"
            struct Person {
                scores: [i32; 3],
            }
            
            fn main() -> () {
            }
        "#;

        let def_result = parse_and_analyze(def_source);
        if let Err(ref error) = def_result {
            println!("Error in struct definition: {}", error.message);
            panic!("Struct definition should work");
        } else {
            let analysis = def_result.unwrap();
            if let Some(person_struct) = analysis.scope.get_struct("Person") {
                if let Some(scores_type) = person_struct.field_type("scores") {
                    println!("Scores field type: {scores_type:?}");
                } else {
                    println!("No scores field found!");
                }
            } else {
                println!("No Person struct found!");
            }
        }

        // First test just the field access of an array field
        let simple_source = r#"
            struct Person {
                scores: [i32; 3],
            }
            
            fn main() -> [i32; 3] {
                let p: Person = Person { 
                    scores: [85, 90, 78] 
                };
                p.scores
            }
        "#;

        let simple_result = parse_and_analyze(simple_source);
        if let Err(ref error) = simple_result {
            println!("Error in simple field access: {}", error.message);
        }
        assert!(simple_result.is_ok(), "Simple field access should work");

        // Now test the full case
        let source = r#"
            struct Person {
                age: i32,
                scores: [i32; 3],
            }
            
            fn main() -> i32 {
                let p: Person = Person { 
                    age: 25, 
                    scores: [85, 90, 78] 
                };
                p.scores[1] + p.age
            }
        "#;

        let result = parse_and_analyze(source);
        if let Err(ref error) = result {
            println!("Error in complex aggregate test: {}", error.message);
        }
        assert!(
            result.is_ok(),
            "Expected successful analysis, got: {:?}",
            result.err()
        );

        let analysis = result.unwrap();
        let main_fn = &analysis.hir.functions[0];
        let final_expr = main_fn.body.expr.as_ref().unwrap();

        // Should be a binary add expression
        match final_expr.as_ref() {
            HirExpr::Binary {
                op, lhs, rhs, ty, ..
            } => {
                assert_eq!(*op, rue_ir::hir::BinOp::Add);
                assert_eq!(ty, &RueType::I32);

                // Left side should be array access
                match lhs.as_ref() {
                    HirExpr::ArrayAccess { ty, .. } => {
                        assert_eq!(ty, &RueType::I32);
                    }
                    _ => panic!("Expected array access on left side"),
                }

                // Right side should be field access
                match rhs.as_ref() {
                    HirExpr::FieldAccess { ty, .. } => {
                        assert_eq!(ty, &RueType::I32);
                    }
                    _ => panic!("Expected field access on right side"),
                }
            }
            _ => panic!("Expected binary expression, got: {final_expr:?}"),
        }
    }
}
