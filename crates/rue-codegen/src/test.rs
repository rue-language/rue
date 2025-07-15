use super::*;
use rue_lexer::Lexer;

fn compile_program(source: &str) -> Result<Vec<Instruction>, CodegenError> {
    // Parse
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();
    let ast = rue_parser::parse(tokens).map_err(|e| CodegenError {
        message: format!("Parse error: {}", e.message),
    })?;

    // Semantic analysis
    let scope = rue_semantic::analyze_cst(&ast).map_err(|e| CodegenError {
        message: format!("Semantic error: {}", e.message),
    })?;

    // Code generation
    let mut codegen = Codegen::new();
    codegen.generate(&ast, &scope)
}

#[test]
fn test_simple_main() {
    let instructions = compile_program(
        r#"
fn main() -> i32 {
42
}
"#,
    );
    assert!(instructions.is_ok());
    let instrs = instructions.unwrap();

    // Should have program setup and main function
    // Look for _start label (second label, ID = 1)
    assert!(
        instrs
            .iter()
            .any(|i| matches!(i, Instruction::Label(LabelId(1)))),
        "expected a start label with ID 1"
    );
    // Should have a Copy instruction with immediate value 42
    assert!(instrs.iter().any(|i| matches!(
        i,
        Instruction::Copy {
            src: Value::Immediate(42),
            ..
        }
    )));
}

#[test]
fn test_arithmetic() {
    let instructions = compile_program(
        r#"
fn main() -> i32 {
2 + 3
}
"#,
    );
    assert!(instructions.is_ok());
    let instrs = instructions.unwrap();

    // Should contain arithmetic operations
    assert!(
        instrs
            .iter()
            .any(|i| matches!(i, Instruction::BinaryOp { op: BinOp::Add, .. }))
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
test(5)
}
"#,
    );
    assert!(instructions.is_ok());
}

#[test]
fn test_lowering_and_emitter_simple() {
    let vreg0 = VReg(0);
    let vreg1 = VReg(1);
    let vreg2 = VReg(2);
    let vreg3 = VReg(3);

    let instructions = vec![
        Instruction::Label(LabelId(999)), // _start
        Instruction::Copy {
            dest: vreg0,
            src: Value::Immediate(42),
        },
        Instruction::Copy {
            dest: vreg1,
            src: Value::VReg(vreg0),
        },
        Instruction::Copy {
            dest: vreg2,
            src: Value::Immediate(60),
        },
        Instruction::Syscall {
            result: vreg3,
            syscall_num: vreg2,
            args: vec![vreg1],
        },
    ];

    // Allocate registers
    let mut regalloc = RegisterAllocator::new();
    regalloc.allocate(vreg0).unwrap();
    regalloc.allocate(vreg1).unwrap();
    regalloc.allocate(vreg2).unwrap();
    regalloc.allocate(vreg3).unwrap();

    // Lower to machine instructions
    let mut lowering = Lowering::new(&mut regalloc, 0);
    let machine_instrs = lowering.lower(&instructions).unwrap();

    // Emit machine code
    let mut x86_emitter = X86Emitter::new();
    let machine_code = x86_emitter.emit_all(&machine_instrs).unwrap();

    assert!(!machine_code.is_empty());
}

#[test]
fn test_elf_generation() {
    let machine_code = vec![
        0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, // mov rax, 42
        0x48, 0x89, 0xc7, // mov rdi, rax
        0x48, 0xc7, 0xc0, 0x3c, 0x00, 0x00, 0x00, // mov rax, 60
        0x0f, 0x05, // syscall
    ];

    let elf_writer = ElfWriter::new();
    let symbols = HashMap::new(); // Empty symbol table for this test
    let elf = elf_writer.generate_elf(&machine_code, &symbols);

    // Check ELF magic
    assert_eq!(&elf[0..4], &[0x7f, 0x45, 0x4c, 0x46]);
    // Check that machine code is included
    assert!(elf.len() > machine_code.len());
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

    // Parse
    let mut lexer = Lexer::new(factorial_source);
    let tokens = lexer.tokenize();
    let ast = rue_parser::parse(tokens).expect("Parse failed");

    // Semantic analysis
    let scope = rue_semantic::analyze_cst(&ast).expect("Semantic analysis failed");

    // Code generation should now succeed with stack spilling
    let executable = compile_to_executable(&ast, &scope);
    assert!(
        executable.is_ok(),
        "Factorial should now compile successfully with stack spilling: {:?}",
        executable.err()
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
    );
    assert!(instructions.is_ok());
    let instrs = instructions.unwrap();

    // Should contain multiple copy operations (for let and assignment)
    let copy_count = instrs
        .iter()
        .filter(|i| matches!(i, Instruction::Copy { .. }))
        .count();
    assert!(copy_count >= 3); // At least initial value, assignment, and return loading
}

#[test]
fn test_physical_reg_error_in_binary_ops() {
    let mut regalloc = RegisterAllocator::new();
    let dest_vreg = VReg(0);
    let rhs_vreg = VReg(1);
    regalloc.allocate(dest_vreg).unwrap();
    regalloc.allocate(rhs_vreg).unwrap();

    // Test that using PhysicalReg in binary operations is handled properly by lowering
    let instr = vec![Instruction::BinaryOp {
        dest: dest_vreg,
        lhs: Value::PhysicalReg(Register::Rax),
        rhs: Value::VReg(rhs_vreg),
        op: BinOp::Add,
    }];

    let mut lowering = Lowering::new(&mut regalloc, 0);
    let result = lowering.lower(&instr);

    // The lowering layer should handle PhysicalReg values now
    // If it doesn't support them, it will return an error
    assert!(result.is_ok() || result.unwrap_err().contains("PhysicalReg"));
}

#[test]
fn test_boolean_literals() {
    let instructions = compile_program(
        r#"
fn main() -> bool {
true
}
"#,
    );
    assert!(instructions.is_ok());
    let instrs = instructions.unwrap();

    // Should have a Copy instruction with immediate value 1 for true
    assert!(instrs.iter().any(|i| matches!(
        i,
        Instruction::Copy {
            src: Value::Immediate(1),
            ..
        }
    )));
}

#[test]
fn test_boolean_false_literal() {
    let instructions = compile_program(
        r#"
fn main() -> bool {
false
}
"#,
    );
    assert!(instructions.is_ok());
    let instrs = instructions.unwrap();

    // Should have a Copy instruction with immediate value 0 for false
    assert!(instrs.iter().any(|i| matches!(
        i,
        Instruction::Copy {
            src: Value::Immediate(0),
            ..
        }
    )));
}

#[test]
fn test_comparison_operators() {
    let instructions = compile_program(
        r#"
fn main() -> bool {
5 < 10
}
"#,
    );
    assert!(instructions.is_ok());
    let instrs = instructions.unwrap();

    // Should contain a less than comparison
    assert!(
        instrs
            .iter()
            .any(|i| matches!(i, Instruction::BinaryOp { op: BinOp::Lt, .. }))
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
    );
    assert!(instructions.is_ok());
    let instrs = instructions.unwrap();

    // Should have return instructions - one with None (unit) and one with Some (i32)
    let return_count = instrs
        .iter()
        .filter(|i| matches!(i, Instruction::Return { .. }))
        .count();
    assert!(return_count >= 2);

    // Check for unit return
    assert!(
        instrs
            .iter()
            .any(|i| matches!(i, Instruction::Return { value: None }))
    );
}

#[test]
fn test_all_comparison_ops() {
    // Test that all comparison operators are properly mapped
    let test_cases = vec![
        ("1 < 2", BinOp::Lt),
        ("1 <= 2", BinOp::Le),
        ("1 > 2", BinOp::Gt),
        ("1 >= 2", BinOp::Ge),
        ("1 == 2", BinOp::Eq),
        ("1 != 2", BinOp::Ne),
    ];

    for (expr, expected_op) in test_cases {
        let program = format!(
            r#"
fn main() -> bool {{
{expr}
}}
"#
        );
        let instructions = compile_program(&program);
        assert!(instructions.is_ok(), "Failed to compile: {expr}");
        let instrs = instructions.unwrap();

        assert!(
            instrs.iter().any(|i| matches!(
                i,
                Instruction::BinaryOp { op, .. } if *op == expected_op
            )),
            "Expected {expected_op:?} operation for expression: {expr}"
        );
    }
}

#[test]
fn test_division_by_immediate() {
    let source = r#"
fn main() -> i32 {
42 / 3
}
"#;

    // Parse
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();
    let ast = rue_parser::parse(tokens).expect("Parse failed");

    // Semantic analysis
    let scope = rue_semantic::analyze_cst(&ast).expect("Semantic analysis failed");

    // Code generation should succeed now
    let result = compile_to_executable(&ast, &scope);
    assert!(
        result.is_ok(),
        "Division by immediate should now be supported"
    );
}

#[test]
fn test_complex_division_immediate() {
    let source = r#"
fn main() -> i32 {
let x: i32 = 100;
x / 5 + 10 / 2
}
"#;

    // Parse
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();
    let ast = rue_parser::parse(tokens).expect("Parse failed");

    // Semantic analysis
    let scope = rue_semantic::analyze_cst(&ast).expect("Semantic analysis failed");

    // Code generation should succeed
    let result = compile_to_executable(&ast, &scope);
    assert!(
        result.is_ok(),
        "Complex division by immediate should be supported"
    );
}

#[test]
fn test_emit_cmp_set_consistency() {
    // Test that all comparison operations produce the same code structure
    let programs = vec![
        ("Lt", "fn main() -> bool { 5 < 10 }"),
        ("Le", "fn main() -> bool { 5 <= 10 }"),
        ("Gt", "fn main() -> bool { 10 > 5 }"),
        ("Ge", "fn main() -> bool { 10 >= 5 }"),
        ("Eq", "fn main() -> bool { 5 == 5 }"),
        ("Ne", "fn main() -> bool { 5 != 10 }"),
    ];

    for (op_name, source) in programs {
        let result = compile_program(source);
        assert!(result.is_ok(), "{op_name} comparison failed to compile");

        let instrs = result.unwrap();
        // Verify we have a BinaryOp instruction
        assert!(
            instrs
                .iter()
                .any(|i| matches!(i, Instruction::BinaryOp { .. })),
            "{op_name} should generate BinaryOp instruction"
        );
    }
}

#[test]
fn test_pre_allocation_no_panic() {
    // Generate a large program to test pre-allocation
    let mut large_program = String::from("fn main() -> i32 {\n");
    for i in 0..1000 {
        large_program.push_str(&format!("    let x{i}: i32 = {i};\n"));
    }
    large_program.push_str("    x999\n}\n");

    let result = compile_program(&large_program);
    assert!(
        result.is_ok(),
        "Large program should compile without allocation panics"
    );
}
