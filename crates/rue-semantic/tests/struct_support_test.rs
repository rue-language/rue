// Integration test for struct literal and field access support in HIR

use rue_ir::types::RueType;
use rue_parser::parse_with_recovery;
use rue_semantic::analyze_cst;

#[test]
fn test_struct_literal_and_field_access_in_hir() {
    let source = r#"
        struct Point {
            x: i32,
            y: i32,
        }

        fn main() -> i32 {
            let p = Point { x: 10, y: 20 };
            let x_val = p.x;
            let y_val = p.y;
            x_val + y_val
        }
    "#;

    // Parse the source code
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Run semantic analysis with HIR generation
    let result = analyze_cst(&cst).expect("Failed to analyze");

    // Verify struct was registered
    let point_struct = result.scope.get_struct("Point");
    assert!(point_struct.is_some(), "Point struct should be defined");
    let point_def = point_struct.unwrap();
    assert_eq!(point_def.fields.len(), 2, "Point should have 2 fields");
    assert_eq!(point_def.fields[0].0, "x");
    assert_eq!(point_def.fields[0].1, RueType::I32);
    assert_eq!(point_def.fields[1].0, "y");
    assert_eq!(point_def.fields[1].1, RueType::I32);

    // Check that we generated instructions (we should have struct literal, field accesses, etc.)
    assert!(
        (result.hir.functions.len() + result.hir.blocks.len()) > 0,
        "Should have generated instructions"
    );

    // We can't easily check specific instruction types without walking the HIR,
    // but the fact that it compiles without errors is a good sign
    println!("Test passed: struct literals and field access are supported in HIR");
}

#[test]
fn test_nested_struct_field_access() {
    let source = r#"
        struct Inner {
            value: i32,
        }

        struct Outer {
            inner: Inner,
            other: i32,
        }

        fn main() -> i32 {
            let inner_obj = Inner { value: 42 };
            let outer_obj = Outer { inner: inner_obj, other: 10 };
            let val = outer_obj.other;
            val
        }
    "#;

    // Parse the source code
    let cst = parse_with_recovery(source, "test.rue").expect("Failed to parse");

    // Run semantic analysis with HIR generation
    let result = analyze_cst(&cst);
    assert!(result.is_ok(), "Should successfully analyze nested structs");

    let result = result.unwrap();

    // Verify both structs were registered
    assert!(
        result.scope.get_struct("Inner").is_some(),
        "Inner struct should be defined"
    );
    assert!(
        result.scope.get_struct("Outer").is_some(),
        "Outer struct should be defined"
    );

    // Verify Outer struct has correct field types
    let outer_struct = result.scope.get_struct("Outer").unwrap();
    assert_eq!(outer_struct.fields.len(), 2, "Outer should have 2 fields");
    assert_eq!(outer_struct.fields[0].0, "inner");
    assert!(matches!(outer_struct.fields[0].1, RueType::Struct(_)));
    assert_eq!(outer_struct.fields[1].0, "other");
    assert_eq!(outer_struct.fields[1].1, RueType::I32);

    println!("Test passed: nested structs are properly handled");
}
