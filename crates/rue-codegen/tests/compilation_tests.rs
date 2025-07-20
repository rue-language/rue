use rue_codegen::{Codegen, CodegenError, Instruction};
use rue_lexer::Lexer;

fn compile_program(source: &str) -> Result<Vec<Instruction>, CodegenError> {
    // Parse
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| CodegenError {
        message: format!("Lexical error: {}", e.message),
    })?;
    let ast = rue_parser::parse(tokens).map_err(|e| CodegenError {
        message: format!("Parse error: {}", e.message),
    })?;

    // Semantic analysis
    let scope = rue_semantic::analyze_cst(&ast).map_err(|e| CodegenError {
        message: format!("Semantic error: {}", e.message),
    })?;

    // Code generation
    let mut codegen = Codegen::new();
    let instructions = codegen.generate(&ast, &scope)?;
    Ok(instructions)
}

#[test]
fn test_simple_main() {
    let instructions = compile_program(
        r#"
fn main() -> i32 {
    42
}
"#,
    )
    .unwrap();

    // Should have at least a Copy instruction for the immediate value
    assert!(!instructions.is_empty());
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::Copy { .. }))
    );
}

#[test]
fn test_arithmetic() {
    let instructions = compile_program(
        r#"
fn main() -> i32 {
    2 + 3
}
"#,
    )
    .unwrap();

    // Should have Copy instructions and a BinaryOp
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::Copy { .. }))
    );
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::BinaryOp { .. }))
    );
}

#[test]
fn test_function_with_parameter() {
    let instructions = compile_program(
        r#"
fn test(x: i32) -> i32 {
    x
}

fn main() -> i32 {
    test(42)
}
"#,
    )
    .unwrap();

    // Should have function call instruction
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::Call { .. }))
    );
}

#[test]
fn test_factorial_compilation() {
    let factorial_source = r#"
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
"#;

    let result = compile_program(factorial_source);
    assert!(result.is_ok(), "Factorial should compile successfully");

    let instructions = result.unwrap();
    // Should have control flow and function calls
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::Branch { .. }))
    );
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::Call { .. }))
    );
}

#[test]
fn test_assignment_compilation() {
    let instructions = compile_program(
        r#"
fn main() -> i32 {
    let x: i32 = 42;
    x = 100;
    x
}
"#,
    )
    .unwrap();

    // Should have assignment operations
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::Copy { .. }))
    );
}

#[test]
fn test_boolean_literals() {
    let instructions = compile_program(
        r#"
fn main() -> bool {
    true
}
"#,
    )
    .unwrap();

    // Should compile without error and have a copy instruction
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::Copy { .. }))
    );
}

#[test]
fn test_boolean_false_literal() {
    let instructions = compile_program(
        r#"
fn main() -> bool {
    false
}
"#,
    )
    .unwrap();

    // Should compile without error and have a copy instruction
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::Copy { .. }))
    );
}

#[test]
fn test_comparison_operators() {
    let instructions = compile_program(
        r#"
fn main() -> bool {
    5 < 10
}
"#,
    )
    .unwrap();

    // Should have comparison operation
    assert!(
        instructions
            .iter()
            .any(|i| matches!(i, Instruction::BinaryOp { .. }))
    );
}

#[test]
fn test_unit_return() {
    let instructions = compile_program(
        r#"
fn helper() {
}

fn main() -> i32 {
    helper();
    42
}
"#,
    )
    .unwrap();

    // Should compile successfully
    assert!(!instructions.is_empty());
}

#[test]
fn test_all_comparison_ops() {
    let ops = vec![
        ("less_than", "<"),
        ("less_equal", "<="),
        ("greater_than", ">"),
        ("greater_equal", ">="),
        ("equal", "=="),
        ("not_equal", "!="),
    ];

    for (op_name, op) in ops {
        let program = format!(
            r#"
fn main() -> bool {{
    5 {op} 10
}}
"#
        );

        let instructions = compile_program(&program);
        assert!(instructions.is_ok(), "Failed to compile: {op}");
        let instrs = instructions.unwrap();

        assert!(
            instrs
                .iter()
                .any(|i| matches!(i, Instruction::BinaryOp { .. })),
            "No BinaryOp found for {op_name}"
        );
    }
}

#[test]
fn test_division_by_immediate() {
    for (op_name, expr) in &[("div", "100 / 5"), ("mod", "100 % 7")] {
        let program = format!(
            r#"
fn main() -> i32 {{
    {expr}
}}
"#
        );

        let instructions = compile_program(&program);
        assert!(instructions.is_ok(), "Failed to compile: {expr}");
        let instrs = instructions.unwrap();

        assert!(
            instrs
                .iter()
                .any(|i| matches!(i, Instruction::BinaryOp { .. })),
            "No BinaryOp found for {op_name}"
        );
    }
}

#[test]
fn test_complex_division_immediate() {
    for expr in &["123456789 / 987654321", "999999999 % 123456789"] {
        let program = format!(
            r#"
fn main() -> i32 {{
    {expr}
}}
"#
        );

        let result = compile_program(&program);
        assert!(result.is_ok(), "{expr} comparison failed to compile");

        let instrs = result.unwrap();
        // Verify we have a BinaryOp instruction
        assert!(
            instrs
                .iter()
                .any(|i| matches!(i, Instruction::BinaryOp { .. })),
            "No BinaryOp found for expression: {expr}"
        );
    }
}

#[test]
fn test_pre_allocation_no_panic() {
    // Generate a large program with many variables to test pre-allocation doesn't panic
    let mut program = String::from("fn main() -> i32 {\n");
    for i in 0..1000 {
        program.push_str(&format!("    let x{i}: i32 = {i};\n"));
    }
    program.push_str("    x999\n}\n");

    let result = compile_program(&program);
    assert!(
        result.is_ok(),
        "Large program should compile without allocation panics"
    );
}
