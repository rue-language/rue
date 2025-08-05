//! Comprehensive integration tests for aggregate runtime behavior
//!
//! These tests verify the correctness of actual compiled Rue programs using aggregate types
//! (structs, tuples, arrays). Tests cover:
//! - Construction and field access
//! - Copy semantics and independence  
//! - Memory allocation strategies (stack vs heap)
//! - Bounds checking and error conditions
//! - Complex nesting scenarios
//! - Runtime helper functions (__rue_memcpy, __rue_memmove, __rue_memzero, __rue_alloc)
//!
//! ## Current Implementation Status
//!
//! **STATUS UPDATE**: Aggregate types are now largely implemented and functional!
//! Issues #112, #113, and #118 have been resolved, enabling basic struct operations.
//!
//! ### What's Implemented:
//! - ✅ Lexer and parser support for all aggregate syntax
//! - ✅ Semantic analysis for aggregate types (structs, tuples, arrays)
//! - ✅ HIR generation from parsed aggregate expressions
//! - ✅ HIR → MIR → PIR lowering infrastructure  
//! - ✅ MIR verification for aggregate operations
//! - ✅ Struct field access with correct offset calculation (Issue #118)
//! - ✅ Proper struct allocation size (Issue #113)
//! - ✅ Runtime helper functions (__rue_memcpy, __rue_memmove, __rue_memzero, __rue_alloc)
//! - ✅ Type layout and alignment calculations
//! - ✅ Memory allocation strategies
//!
//! ### Known Limitations:
//! - 🔄 Struct field names are mapped to offsets via heuristics (x=0, y=8, z=16, etc.)
//! - 🔄 Struct size assumes 2 x i64 fields (16 bytes) - needs proper struct definition lookup
//!
//! ### Testing Strategy:
//!
//! All tests are currently marked with `#[ignore]` and will be enabled once the semantic
//! analysis phase is complete. To run these tests when aggregate support is ready:
//!
//! ```bash
//! cargo test -p rue aggregate_runtime_integration_tests -- --ignored
//! ```
//!
//! ### Test Organization:
//!
//! Tests are organized by aggregate type and operation category:
//! - **Struct Operations**: Basic construction, field access, copy semantics
//! - **Tuple Operations**: Mixed types, size variations, element access  
//! - **Array Operations**: Indexing, bounds checking, different sizes
//! - **Memory Management**: Stack vs heap allocation, size thresholds
//! - **Complex Scenarios**: Nested aggregates, deep structures
//! - **Edge Cases**: Zero-sized types, alignment, error conditions

mod common;

use common::get_project_root;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Compile a Rue program from a string
fn compile_rue_code(code: &str, output_path: &Path) -> Result<(), String> {
    let project_root = get_project_root();

    // Wrap code in main function if not already wrapped
    let wrapped_code = if code.trim().starts_with("fn main()")
        || code.trim().starts_with("fn ")
        || code.trim().starts_with("struct ")
    {
        // Already has top-level definitions (functions or structs), assume complete program
        if !code.contains("fn main()") {
            // Has definitions but no main, add main that returns 0
            format!("{code}\n\nfn main() -> i64 {{\n    0\n}}")
        } else {
            code.to_string()
        }
    } else {
        // No top-level definitions, wrap in main
        format!("fn main() -> i64 {{\n{code}\n0\n}}")
    };

    // Create a temporary source file with unique name to avoid conflicts
    let pid = std::process::id();
    let tid = std::thread::current().id();
    let temp_source = project_root.join(format!("test_temp_{pid}_{tid:?}.rue"));
    fs::write(&temp_source, wrapped_code)
        .map_err(|e| format!("Failed to write source file: {e}"))?;

    // Clean up any existing executable
    if output_path.exists() {
        fs::remove_file(output_path)
            .map_err(|e| format!("Failed to remove existing executable: {e}"))?;
    }

    // Compile the rue program
    let compile_output = Command::new("cargo")
        .args(["run", "-p", "rue", "--quiet", "--"])
        .arg(&temp_source)
        .arg("-o")
        .arg(output_path)
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("Failed to execute rue compiler: {e}"))?;

    // Clean up temp source
    fs::remove_file(&temp_source).ok();

    if !compile_output.status.success() {
        return Err(format!(
            "Compilation failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&compile_output.stdout),
            String::from_utf8_lossy(&compile_output.stderr)
        ));
    }

    Ok(())
}

/// Test a Rue program with expected output and exit code
fn test_runtime_program(code: &str, expected_output: &str, expected_exit_code: i32) {
    let project_root = get_project_root();
    let pid = std::process::id();
    let tid = std::thread::current().id();
    let executable_path = project_root.join(format!("test_aggregate_exe_{pid}_{tid:?}"));

    // Compile the program
    if let Err(e) = compile_rue_code(code, &executable_path) {
        panic!("Failed to compile program: {e}");
    }

    // Run the compiled executable
    let output = Command::new(&executable_path)
        .current_dir(project_root)
        .output()
        .expect("Failed to execute compiled program");

    // Check output
    let actual_output = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        actual_output, expected_output,
        "Output mismatch.\nExpected:\n{expected_output}\nActual:\n{actual_output}"
    );

    // Check exit code
    let actual_exit_code = output.status.code().unwrap_or(-1);
    assert_eq!(
        actual_exit_code, expected_exit_code,
        "Exit code mismatch. Expected: {expected_exit_code}, Actual: {actual_exit_code}"
    );

    // Clean up
    fs::remove_file(&executable_path).ok();
}

// =============================================================================
// INFRASTRUCTURE VERIFICATION TESTS (always enabled)
// =============================================================================

/// Verify that the test infrastructure works correctly
#[test]
fn test_infrastructure_basic_program() {
    let code = r#"
fn main() -> i64 {
    42
}
"#;
    test_runtime_program(code, "", 42);
}

/// Verify that the test infrastructure can handle compilation errors
#[test]
fn test_infrastructure_compilation_error() {
    let code = r#"
fn main() -> i64 {
    invalid_syntax_here
}
"#;

    let project_root = get_project_root();
    let pid = std::process::id();
    let tid = std::thread::current().id();
    let executable_path = project_root.join(format!("test_aggregate_exe_{pid}_{tid:?}"));

    // This should fail to compile
    let result = compile_rue_code(code, &executable_path);
    assert!(
        result.is_err(),
        "Expected compilation to fail for invalid syntax"
    );
}

/// Verify that basic arithmetic and output works
#[test]
fn test_infrastructure_arithmetic_and_output() {
    let code = r#"
fn main() -> i64 {
    let x: i64 = 10;
    let y: i64 = 20;
    println_i64(x + y);
    x + y
}
"#;
    test_runtime_program(code, "30\n", 30);
}

// =============================================================================
// STRUCT OPERATION TESTS (disabled until aggregate support is complete)
// =============================================================================

#[test]
fn test_struct_basic_construction_and_access() {
    let code = r#"
struct Point {
    x: i64,
    y: i64,
}

fn main() -> i64 {
    let p: Point = Point { x: 10, y: 20 };
    println_i64(p.x);
    println_i64(p.y);
    p.x + p.y
}
"#;
    test_runtime_program(code, "10\n20\n", 30);
}

#[test]
fn test_struct_copy_independence() {
    let code = r#"
struct Counter {
    value: i64,
}

fn main() -> i64 {
    let original: Counter = Counter { value: 5 };
    let copy: Counter = original;
    
    // Modify copy's field (simulate by creating new struct)
    let modified_copy: Counter = Counter { value: copy.value + 10 };
    
    // Verify original is unchanged
    println_i64(original.value);
    println_i64(modified_copy.value);
    
    // Return difference to verify independence
    modified_copy.value - original.value
}
"#;
    test_runtime_program(code, "5\n15\n", 10);
}

#[test]
fn test_struct_with_mixed_types() {
    let code = r#"
struct Record {
    id: i64,
    active: bool,
    score: i32,
}

fn main() -> i64 {
    let rec: Record = Record { id: 12345, active: true, score: 98 };
    
    println_i64(rec.id);
    println_bool(rec.active);
    println_i32(rec.score);
    
    if rec.active {
        rec.id
    } else {
        0
    }
}
"#;
    test_runtime_program(code, "12345\ntrue\n98\n", 57); // 12345 % 256 = 57
}

#[test]
fn test_multiple_struct_instances() {
    let code = r#"
struct Vector2 {
    x: i32,
    y: i32,
}

fn main() -> i64 {
    let v1: Vector2 = Vector2 { x: 1, y: 2 };
    let v2: Vector2 = Vector2 { x: 3, y: 4 };
    let v3: Vector2 = Vector2 { x: v1.x + v2.x, y: v1.y + v2.y };
    
    println_i32(v3.x);
    println_i32(v3.y);
    
    to_i64(v3.x + v3.y)
}
"#;
    test_runtime_program(code, "4\n6\n", 10);
}

// =============================================================================
// TUPLE OPERATION TESTS
// =============================================================================

#[test]
fn test_tuple_basic_operations() {
    let code = r#"
fn main() -> i64 {
    let coords: (i64, i64) = (100, 200);
    println_i64(coords.0);
    println_i64(coords.1);
    return coords.0;  // Return 100 instead of 300 to avoid exit code truncation
}
"#;
    test_runtime_program(code, "100\n200\n", 100);
}

#[test]
fn test_tuple_mixed_types() {
    let code = r#"
fn main() -> i64 {
    let data: (i64, bool, i32) = (42, true, 99);
    
    println_i64(data.0);
    println_bool(data.1);
    println_i32(data.2);
    
    if data.1 {
        data.0 + to_i64(data.2)
    } else {
        data.0
    }
}
"#;
    test_runtime_program(code, "42\ntrue\n99\n", 141);
}

#[test]
fn test_tuple_copy_semantics() {
    let code = r#"
fn main() -> i64 {
    let original: (i64, i32) = (10, 20);
    let copy: (i64, i32) = original;
    
    // Verify both have same values
    println_i64(original.0);
    println_i32(original.1);
    println_i64(copy.0);
    println_i32(copy.1);
    
    // Test that they're independent copies by using in different contexts
    let sum_original: i64 = original.0 + to_i64(original.1);
    let sum_copy: i64 = copy.0 + to_i64(copy.1);
    
    return sum_original + sum_copy;
}
"#;
    test_runtime_program(code, "10\n20\n10\n20\n", 60);
}

#[test]
fn test_single_element_tuple() {
    let code = r#"
fn main() -> i64 {
    let single: (i64,) = (42,);
    println_i64(single.0);
    return single.0;
}
"#;
    test_runtime_program(code, "42\n", 42);
}

#[test]
fn test_unit_tuple() {
    let code = r#"
fn main() -> i64 {
    let unit: () = ();
    println_unit(unit);
    0
}
"#;
    test_runtime_program(code, "()\n", 0);
}

#[test]
fn test_large_tuple() {
    let code = r#"
fn main() -> i64 {
    let big: (i64, i64, i64, i64, i64) = (1, 2, 3, 4, 5);
    
    let sum: i64 = big.0 + big.1 + big.2 + big.3 + big.4;
    println_i64(sum);
    sum
}
"#;
    test_runtime_program(code, "15\n", 15);
}

// =============================================================================
// ARRAY OPERATION TESTS
// =============================================================================

#[test]
fn test_array_basic_operations() {
    let code = r#"
fn main() -> i64 {
    let arr: [i64; 3] = [10, 20, 30];
    
    println_i64(arr[0]);
    println_i64(arr[1]);
    println_i64(arr[2]);
    
    arr[0] + arr[1] + arr[2]
}
"#;
    test_runtime_program(code, "10\n20\n30\n", 60);
}

#[test]
fn test_array_different_types() {
    let code = r#"
fn main() -> i64 {
    let numbers: [i32; 4] = [1, 2, 3, 4];
    let bools: [bool; 2] = [true, false];
    
    println_i32(numbers[0]);
    println_i32(numbers[3]);
    println_bool(bools[0]);
    println_bool(bools[1]);
    
    if bools[0] {
        to_i64(numbers[0] + numbers[3])
    } else {
        0
    }
}
"#;
    test_runtime_program(code, "1\n4\ntrue\nfalse\n", 5);
}

#[test]
fn test_array_bounds_checking_valid() {
    let code = r#"
fn main() -> i64 {
    let arr: [i64; 5] = [10, 20, 30, 40, 50];
    
    // Access all valid indices
    let sum: i64 = arr[0] + arr[1] + arr[2] + arr[3] + arr[4];
    println_i64(sum);
    sum
}
"#;
    test_runtime_program(code, "150\n", 150);
}

#[test]
fn test_array_bounds_checking_out_of_bounds() {
    let code = r#"
fn main() -> i64 {
    let arr: [i64; 3] = [1, 2, 3];
    // This should trigger bounds check failure
    let invalid: i64 = arr[5];
    invalid
}
"#;
    test_runtime_program(code, "", 252); // Bounds check exit code
}

#[test]
fn test_array_bounds_checking_negative_index() {
    let code = r#"
fn main() -> i64 {
    let arr: [i64; 3] = [1, 2, 3];
    let neg_one: i64 = 0 - 1;
    // This should trigger bounds check failure  
    let invalid: i64 = arr[neg_one];
    invalid
}
"#;
    test_runtime_program(code, "", 252); // Bounds check exit code
}

#[test]
fn test_zero_length_array() {
    let code = r#"
fn main() -> i64 {
    let empty: [i64; 0] = [];
    // Can't access elements, but array exists
    println_i64(42);
    42
}
"#;
    test_runtime_program(code, "42\n", 42);
}

#[test]
fn test_large_array() {
    let code = r#"
fn main() -> i64 {
    let big: [i32; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    
    // Sum first and last elements
    let sum: i32 = big[0] + big[9];
    println_i32(sum);
    to_i64(sum)
}
"#;
    test_runtime_program(code, "11\n", 11);
}

// =============================================================================
// HEAP VS STACK ALLOCATION TESTS
// =============================================================================

#[test]
fn test_small_struct_allocation() {
    // Small struct (likely stack allocated)
    let code = r#"
struct Small {
    a: i32,
    b: i32,
}

fn main() -> i64 {
    let s1: Small = Small { a: 10, b: 20 };
    let s2: Small = Small { a: 30, b: 40 };
    
    println_i32(s1.a + s1.b);
    println_i32(s2.a + s2.b);
    
    to_i64(s1.a + s1.b + s2.a + s2.b)
}
"#;
    test_runtime_program(code, "30\n70\n", 100);
}

#[test]
fn test_large_array_allocation() {
    // Large array (should trigger heap allocation at 128+ bytes)
    let code = r#"
fn main() -> i64 {
    // 20 * 8 bytes = 160 bytes, should be heap allocated
    let big: [i64; 20] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
        11, 12, 13, 14, 15, 16, 17, 18, 19, 20
    ];
    
    // Access elements to verify allocation worked
    println_i64(big[0]);
    println_i64(big[10]);
    println_i64(big[19]);
    
    big[0] + big[10] + big[19]
}
"#;
    test_runtime_program(code, "1\n11\n20\n", 32);
}

#[test]
fn test_stack_size_threshold() {
    // Test struct right at the threshold boundary (128 bytes)
    let code = r#"
struct AtThreshold {
    // 16 i64s = 128 bytes exactly
    f0: i64, f1: i64, f2: i64, f3: i64,
    f4: i64, f5: i64, f6: i64, f7: i64,
    f8: i64, f9: i64, f10: i64, f11: i64,
    f12: i64, f13: i64, f14: i64, f15: i64,
}

fn main() -> i64 {
    let s: AtThreshold = AtThreshold {
        f0: 1, f1: 2, f2: 3, f3: 4,
        f4: 5, f5: 6, f6: 7, f7: 8,
        f8: 9, f9: 10, f10: 11, f11: 12,
        f12: 13, f13: 14, f14: 15, f15: 16,
    };
    
    println_i64(s.f0);
    println_i64(s.f15);
    
    s.f0 + s.f15
}
"#;
    test_runtime_program(code, "1\n16\n", 17);
}

// =============================================================================
// NESTED AGGREGATE TESTS
// =============================================================================

#[test]
fn test_struct_containing_tuple() {
    let code = r#"
struct Container {
    data: (i64, i32),
    id: i64,
}

fn main() -> i64 {
    let c: Container = Container {
        data: (100, 50),
        id: 42,
    };
    
    println_i64(c.data.0);
    println_i32(c.data.1);
    println_i64(c.id);
    
    c.data.0 + to_i64(c.data.1) + c.id
}
"#;
    test_runtime_program(code, "100\n50\n42\n", 192);
}

#[test]
fn test_tuple_containing_struct() {
    let code = r#"
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i64 {
    let p: Point = Point { x: 10, y: 20 };
    let t: (Point, i64) = (p, 42);
    
    println_i32(t.0.x);
    println_i32(t.0.y);
    println_i64(t.1);
    
    to_i64(t.0.x + t.0.y) + t.1
}
"#;
    test_runtime_program(code, "10\n20\n42\n", 72);
}

#[test]
fn test_array_of_structs() {
    let code = r#"
struct Pair {
    first: i32,
    second: i32,
}

fn main() -> i64 {
    let arr: [Pair; 2] = [
        Pair { first: 1, second: 2 },
        Pair { first: 3, second: 4 },
    ];
    
    println_i32(arr[0].first);
    println_i32(arr[0].second);
    println_i32(arr[1].first);
    println_i32(arr[1].second);
    
    to_i64(arr[0].first + arr[0].second + arr[1].first + arr[1].second)
}
"#;
    test_runtime_program(code, "1\n2\n3\n4\n", 10);
}

#[test]
fn test_struct_with_array() {
    let code = r#"
struct Matrix {
    data: [i64; 4],
    rows: i32,
    cols: i32,
}

fn main() -> i64 {
    let m: Matrix = Matrix {
        data: [1, 2, 3, 4],
        rows: 2,
        cols: 2,
    };
    
    println_i64(m.data[0]);
    println_i64(m.data[3]);
    println_i32(m.rows);
    println_i32(m.cols);
    
    m.data[0] + m.data[3] + to_i64(m.rows) + to_i64(m.cols)
}
"#;
    test_runtime_program(code, "1\n4\n2\n2\n", 9);
}

#[test]
fn test_deeply_nested_aggregate() {
    let code = r#"
struct Inner {
    value: i64,
}

struct Middle {
    inner: Inner,
    flag: bool,
}

struct Outer {
    middle: Middle,
    count: i32,
}

fn main() -> i64 {
    let nested: Outer = Outer {
        middle: Middle {
            inner: Inner { value: 100 },
            flag: true,
        },
        count: 5,
    };
    
    println_i64(nested.middle.inner.value);
    println_bool(nested.middle.flag);
    println_i32(nested.count);
    
    if nested.middle.flag {
        nested.middle.inner.value + to_i64(nested.count)
    } else {
        0
    }
}
"#;
    test_runtime_program(code, "100\ntrue\n5\n", 105);
}

// =============================================================================
// MOVE SEMANTICS AND COPY INDEPENDENCE TESTS
// =============================================================================

#[test]
fn test_assignment_creates_independent_copy() {
    let code = r#"
struct Value {
    num: i64,
}

fn main() -> i64 {
    let original: Value = Value { num: 10 };
    let assigned: Value = original;
    
    // Both should have the same value initially
    println_i64(original.num);
    println_i64(assigned.num);
    
    // Simulate modification by creating new instances
    let modified_original: Value = Value { num: original.num + 5 };
    let modified_assigned: Value = Value { num: assigned.num + 20 };
    
    println_i64(modified_original.num);
    println_i64(modified_assigned.num);
    
    // Verify they can be modified independently
    modified_assigned.num - modified_original.num
}
"#;
    test_runtime_program(code, "10\n10\n15\n30\n", 15);
}

#[test]
fn test_function_parameter_copying() {
    let code = r#"
struct Data {
    x: i64,
    y: i64,
}

fn process_data(d: Data) -> i64 {
    println_i64(d.x);
    println_i64(d.y);
    d.x * d.y
}

fn main() -> i64 {
    let original: Data = Data { x: 6, y: 7 };
    
    // Pass to function (should copy)
    let result: i64 = process_data(original);
    
    // Original should still be accessible
    println_i64(original.x);
    println_i64(original.y);
    
    result
}
"#;
    test_runtime_program(code, "6\n7\n6\n7\n", 42);
}

#[test]
fn test_return_value_copying() {
    let code = r#"
struct Result {
    success: bool,
    value: i64,
}

fn create_result() -> Result {
    Result { success: true, value: 42 }
}

fn main() -> i64 {
    let r: Result = create_result();
    
    println_bool(r.success);
    println_i64(r.value);
    
    if r.success {
        r.value
    } else {
        0
    }
}
"#;
    test_runtime_program(code, "true\n42\n", 42);
}

// =============================================================================
// EDGE CASE AND ERROR CONDITION TESTS
// =============================================================================

#[test]
fn test_zero_sized_struct() {
    let code = r#"
struct Empty {
}

fn main() -> i64 {
    let e1: Empty = Empty {};
    let e2: Empty = e1; // Copy
    
    println_i64(42); // Just to show program runs
    42
}
"#;
    test_runtime_program(code, "42\n", 42);
}

#[test]
fn test_alignment_sensitive_struct() {
    let code = r#"
struct Mixed {
    small: bool,    // 1 byte  
    big: i64,       // 8 bytes (needs alignment)
    medium: i32,    // 4 bytes
}

fn main() -> i64 {
    let m: Mixed = Mixed {
        small: true,
        big: 1000000000,
        medium: 999,
    };
    
    println_bool(m.small);
    println_i64(m.big);
    println_i32(m.medium);
    
    if m.small {
        m.big / 1000000 + to_i64(m.medium)
    } else {
        0
    }
}
"#;
    test_runtime_program(code, "true\n1000000000\n999\n", 207); // 1999 % 256 = 207
}

#[test]
fn test_complex_tuple_types() {
    let code = r#"
fn main() -> i64 {
    // Complex tuple with different alignments
    let complex: (bool, i64, i32, bool, i64) = (true, 100, 50, false, 200);
    
    println_bool(complex.0);
    println_i64(complex.1);
    println_i32(complex.2);
    println_bool(complex.3);
    println_i64(complex.4);
    
    let sum: i64 = complex.1 + to_i64(complex.2) + complex.4;
    sum
}
"#;
    test_runtime_program(code, "true\n100\n50\nfalse\n200\n", 94); // 350 % 256 = 94
}

#[test]
fn test_maximum_reasonable_array_size() {
    let code = r#"
fn main() -> i64 {
    // Large but reasonable array (should work)
    let arr: [i32; 50] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
        11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
        21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
        31, 32, 33, 34, 35, 36, 37, 38, 39, 40,
        41, 42, 43, 44, 45, 46, 47, 48, 49, 50
    ];
    
    // Access some elements to verify it works
    println_i32(arr[0]);
    println_i32(arr[25]);
    println_i32(arr[49]);
    
    to_i64(arr[0] + arr[25] + arr[49])
}
"#;
    test_runtime_program(code, "1\n26\n50\n", 77);
}
