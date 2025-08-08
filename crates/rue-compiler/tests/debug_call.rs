use rue_compiler::pipeline::compile_hir2_via_mir_to_assembly;
use rue_parser::parse_with_recovery;
use rue_semantic::{analyze_cst, analyze_cst_v2, scope_to_type_context};

#[test]
fn debug_function_calls() {
    let source = r#"
        fn add(x: i32, y: i32) -> i32 {
            x + y
        }
        
        fn main() -> i32 {
            add(3, 4)
        }
    "#;

    // Parse CST
    let cst = parse_with_recovery(source, "test.rue").unwrap();

    // Path 2: HIR2
    let analysis1 = analyze_cst(&cst).unwrap();
    let hir2 = analyze_cst_v2(&cst).unwrap();

    println!("\n=== HIR2 Instructions ===");
    println!("{}", hir2);

    let type_context = scope_to_type_context(&analysis1.scope);
    let asm2 = compile_hir2_via_mir_to_assembly(&hir2, type_context, false).unwrap();

    println!("\n=== HIR2 Assembly ===");
    println!("{}", asm2);

    assert!(asm2.contains("add:"), "Missing add function");
    assert!(asm2.contains("main:"), "Missing main function");
}
