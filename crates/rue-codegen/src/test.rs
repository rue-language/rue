use super::*;
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
    let tokens = lexer.tokenize().expect("Lexer failed");
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
    let tokens = lexer.tokenize().expect("Lexer failed");
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
    let tokens = lexer.tokenize().expect("Lexer failed");
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

#[test]
fn test_deeply_nested_function_calls() {
    // Test that deeply nested function calls don't cause stack overflow or other issues
    let mut program = String::from("fn f0() -> i32 { 42 }\n");

    // Create a chain of functions, each calling the previous
    for i in 1..50 {
        program.push_str(&format!("fn f{i}() -> i32 {{ f{}() }}\n", i - 1));
    }

    program.push_str("fn main() -> i32 { f49() }");

    let result = compile_program(&program);
    assert!(result.is_ok(), "Deep function call chain should compile");
}

#[test]
fn test_maximum_function_parameters() {
    // Test that functions with too many parameters fail gracefully
    let mut params = String::new();
    let mut args = String::new();
    let mut body = String::new();

    // Create a function with more than 6 parameters (the current limit)
    for i in 0..10 {
        if i > 0 {
            params.push_str(", ");
            args.push_str(", ");
            body.push_str(" + ");
        }
        params.push_str(&format!("x{i}: i32"));
        args.push_str(&format!("{i}"));
        body.push_str(&format!("x{i}"));
    }

    let program = format!(
        "fn many_params({params}) -> i32 {{\n    {body}\n}}\n\nfn main() -> i32 {{\n    many_params({args})\n}}"
    );

    let result = compile_program(&program);
    assert!(
        result.is_err(),
        "Function with more than 6 parameters should fail"
    );
    assert!(
        result.unwrap_err().message.contains("Too many parameters"),
        "Error should mention parameter limit"
    );
}

#[test]
fn test_function_with_six_parameters() {
    // Test that functions with exactly 6 parameters work
    let program = r#"
fn six_params(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
    a + b + c + d + e + f
}

fn main() -> i32 {
    six_params(1, 2, 3, 4, 5, 6)
}
"#;

    let result = compile_program(program);
    assert!(result.is_ok(), "Function with 6 parameters should compile");
}

#[test]
fn test_deeply_nested_expressions() {
    // Test deeply nested arithmetic expressions
    let mut expr = String::from("1");
    for _ in 0..30 {
        expr = format!("({expr} + 1)");
    }

    let program = format!(
        r#"
fn main() -> i32 {{
    {expr}
}}
"#
    );

    let result = compile_program(&program);
    assert!(result.is_ok(), "Deeply nested expressions should compile");
}

#[test]
fn test_register_spilling_stress() {
    // Test that forces register spilling by using more values than available registers
    let program = r#"
fn spill_test() -> i32 {
    let a1: i32 = 1;
    let a2: i32 = 2;
    let a3: i32 = 3;
    let a4: i32 = 4;
    let a5: i32 = 5;
    let a6: i32 = 6;
    let a7: i32 = 7;
    let a8: i32 = 8;
    let a9: i32 = 9;
    let a10: i32 = 10;
    let a11: i32 = 11;
    let a12: i32 = 12;
    let a13: i32 = 13;
    let a14: i32 = 14;
    let a15: i32 = 15;
    let a16: i32 = 16;
    
    // Use all variables in a single expression to force them to be live simultaneously
    a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + a10 + a11 + a12 + a13 + a14 + a15 + a16
}

fn main() -> i32 {
    spill_test()
}
"#;

    let result = compile_program(program);
    assert!(
        result.is_ok(),
        "Register spilling should be handled correctly"
    );
}

#[test]
fn test_complex_control_flow() {
    // Test complex nested control flow
    let program = r#"
fn complex_flow(a: i32, b: i32, c: i32) -> i32 {
    if a > 0 {
        if b > 0 {
            if c > 0 {
                a + b + c
            } else {
                if a > b {
                    a - b
                } else {
                    b - a
                }
            }
        } else {
            if c > a {
                c - a
            } else {
                a - c
            }
        }
    } else {
        if b < 0 {
            if c < 0 {
                0 - a - b - c
            } else {
                c
            }
        } else {
            b
        }
    }
}

fn main() -> i32 {
    complex_flow(10, 20, 30)
}
"#;

    let result = compile_program(program);
    assert!(result.is_ok(), "Complex control flow should compile");
}

#[test]
fn test_large_immediate_values() {
    // Test handling of large immediate values
    let program = r#"
fn main() -> i32 {
    let big1: i32 = 2147483647;  // i32::MAX
    let big2: i32 = -2147483648; // i32::MIN
    let result: i32 = big1 + big2;
    result
}
"#;

    let result = compile_program(program);
    assert!(result.is_ok(), "Large immediate values should compile");
}

#[test]
fn test_assignment_chains() {
    // Test chained assignments
    let program = r#"
fn main() -> i32 {
    let x: i32 = 1;
    let y: i32 = 2;
    let z: i32 = 3;
    
    x = y;
    y = z;
    z = x;
    
    x + y + z
}
"#;

    let result = compile_program(program);
    assert!(result.is_ok(), "Assignment chains should compile");
}

#[test]
fn test_function_calls_in_expressions() {
    // Test function calls within complex expressions
    let program = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn mul(a: i32, b: i32) -> i32 {
    a * b
}

fn main() -> i32 {
    add(mul(2, 3), add(4, 5))
}
"#;

    let result = compile_program(program);
    assert!(
        result.is_ok(),
        "Function calls in expressions should compile"
    );
}

#[test]
fn test_mixed_type_operations() {
    // Test operations with mixed i32 and bool types
    let program = r#"
fn mixed_types(x: i32, flag: bool) -> i32 {
    if flag {
        x + 10
    } else {
        x - 10
    }
}

fn main() -> i32 {
    let result1: i32 = mixed_types(42, true);
    let result2: i32 = mixed_types(result1, false);
    result2
}
"#;

    let result = compile_program(program);
    assert!(result.is_ok(), "Mixed type operations should compile");
}
