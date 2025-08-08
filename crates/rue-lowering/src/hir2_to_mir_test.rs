//! Tests for HIR2 to MIR lowering

use crate::hir2_to_mir::lower_hir2_to_mir;
use rue_ir::hir2::{BinOp, FunctionData, Hir, InstData, InstTag};
use rue_ir::mir::{MirBinOp, MirStatement, MirValue};
use rue_ir::types::{RueType, TypeContext};

#[test]
fn test_lower_simple_literal() {
    // Create HIR2 for: fn main() -> i32 { return 42; }
    let mut hir = Hir::new();

    let main_name = hir.strings.intern("main");

    // Add literal 42
    let literal_inst = hir.add_instruction(
        InstTag::Literal,
        InstData::literal(42),
        Some(RueType::I64),
        rue_lexer::Span { start: 0, end: 2 },
    );

    // Add return statement
    hir.add_instruction(
        InstTag::Return,
        InstData {
            lhs: literal_inst.raw(),
            rhs: 0,
        },
        None,
        rue_lexer::Span { start: 3, end: 12 },
    );

    // Create function body block (instructions 1-2)
    let body_block = hir.add_block(1, 2);

    // Create main function
    let func_data = FunctionData {
        name: main_name,
        param_count: 0,
        return_type: Some(RueType::I64),
        body_block,
    };
    hir.functions.push(func_data);

    // Lower to MIR
    let type_context = TypeContext::new();
    let mir = lower_hir2_to_mir(&hir, type_context);

    // Verify the result
    assert_eq!(mir.functions.len(), 1);

    let main_func = &mir.functions[0];
    assert_eq!(main_func.name, "main");
    assert_eq!(main_func.return_type, RueType::I64);

    // Should have at least one block
    assert!(!main_func.blocks.is_empty());

    // Entry block should have some statements
    let entry_block = &main_func.blocks[0];
    assert!(!entry_block.statements.is_empty());
}

#[test]
fn test_lower_variable_load() {
    // Create HIR2 for: fn test() -> i32 { let x = 5; return x; }
    let mut hir = Hir::new();

    let test_name = hir.strings.intern("test");
    let x_name = hir.strings.intern("x");

    // Add literal 5
    let literal_inst = hir.add_instruction(
        InstTag::Literal,
        InstData::literal(5),
        Some(RueType::I64),
        rue_lexer::Span { start: 0, end: 1 },
    );

    // Add let x = 5
    hir.add_instruction(
        InstTag::Let,
        InstData {
            lhs: x_name.raw(),
            rhs: literal_inst.raw(),
        },
        None,
        rue_lexer::Span { start: 2, end: 11 },
    );

    // Add load x
    let load_inst = hir.add_instruction(
        InstTag::Load,
        InstData {
            lhs: x_name.raw(),
            rhs: 0,
        },
        Some(RueType::I64),
        rue_lexer::Span { start: 12, end: 13 },
    );

    // Add return x
    hir.add_instruction(
        InstTag::Return,
        InstData {
            lhs: load_inst.raw(),
            rhs: 0,
        },
        None,
        rue_lexer::Span { start: 14, end: 22 },
    );

    // Create function body block (instructions 1-4)
    let body_block = hir.add_block(1, 4);

    // Create test function
    let func_data = FunctionData {
        name: test_name,
        param_count: 0,
        return_type: Some(RueType::I64),
        body_block,
    };
    hir.functions.push(func_data);

    // Lower to MIR
    let type_context = TypeContext::new();
    let mir = lower_hir2_to_mir(&hir, type_context);

    // Verify the result
    assert_eq!(mir.functions.len(), 1);

    let test_func = &mir.functions[0];
    assert_eq!(test_func.name, "test");
    assert_eq!(test_func.return_type, RueType::I64);

    // Should have blocks and statements
    assert!(!test_func.blocks.is_empty());
    assert!(!test_func.blocks[0].statements.is_empty());
}

#[test]
fn test_empty_hir2() {
    // Test lowering empty HIR2
    let hir = Hir::new();
    let type_context = TypeContext::new();
    let mir = lower_hir2_to_mir(&hir, type_context);

    assert_eq!(mir.functions.len(), 0);
    assert!(!mir.function_signatures.is_empty()); // Should have builtins
}

#[test]
fn test_builtin_signatures() {
    // Test that builtin function signatures are added
    let hir = Hir::new();
    let type_context = TypeContext::new();
    let mir = lower_hir2_to_mir(&hir, type_context);

    // Should have print function
    assert!(mir.function_signatures.contains_key("print"));

    let print_sig = &mir.function_signatures["print"];
    assert_eq!(print_sig.param_types.len(), 1);
    assert_eq!(print_sig.param_types[0], RueType::I64);
    assert_eq!(print_sig.return_type, RueType::I64);
}

#[test]
fn test_binary_operation() {
    // Create HIR2 for: fn test() -> i32 { return 5 + 3; }
    let mut hir = Hir::new();

    let test_name = hir.strings.intern("test");

    // Add literal 5
    let literal5_inst = hir.add_instruction(
        InstTag::Literal,
        InstData::literal(5),
        Some(RueType::I64),
        rue_lexer::Span { start: 0, end: 1 },
    );

    // Add literal 3
    let literal3_inst = hir.add_instruction(
        InstTag::Literal,
        InstData::literal(3),
        Some(RueType::I64),
        rue_lexer::Span { start: 4, end: 5 },
    );

    // Add binary operation: 5 + 3
    let add_inst = hir.add_instruction(
        InstTag::Binary,
        InstData::insts(literal5_inst, literal3_inst),
        Some(RueType::I64),
        rue_lexer::Span { start: 2, end: 3 },
    );

    // Store the operator in extra_data (simulate what HirBuilder does)
    if hir.extra_data.len() <= add_inst.as_usize() {
        hir.extra_data.resize(add_inst.as_usize() + 1, 0);
    }
    if add_inst.as_usize() < hir.extra_data.len() {
        hir.extra_data[add_inst.as_usize()] = BinOp::Add as u32;
    }

    // Add return statement
    hir.add_instruction(
        InstTag::Return,
        InstData {
            lhs: add_inst.raw(),
            rhs: 0,
        },
        None,
        rue_lexer::Span { start: 6, end: 14 },
    );

    // Create function body block (instructions 1-4)
    let body_block = hir.add_block(1, 4);

    // Create test function
    let func_data = FunctionData {
        name: test_name,
        param_count: 0,
        return_type: Some(RueType::I64),
        body_block,
    };
    hir.functions.push(func_data);

    // Lower to MIR
    let type_context = TypeContext::new();
    let mir = lower_hir2_to_mir(&hir, type_context);

    // Verify the result
    assert_eq!(mir.functions.len(), 1);

    let test_func = &mir.functions[0];
    assert_eq!(test_func.name, "test");
    assert_eq!(test_func.return_type, RueType::I64);

    // Should have blocks and statements including the binary operation
    assert!(!test_func.blocks.is_empty());
    assert!(!test_func.blocks[0].statements.is_empty());

    // Look for the binary addition in the statements
    let has_binary_op = test_func.blocks[0].statements.iter().any(|stmt| {
        matches!(
            stmt,
            MirStatement::Assign {
                value: MirValue::BinaryOp {
                    op: MirBinOp::Add,
                    ..
                },
                ..
            }
        )
    });
    assert!(has_binary_op, "Expected to find binary addition operation");
}

#[test]
fn test_multiple_functions() {
    // Create HIR2 with multiple functions
    let mut hir = Hir::new();

    // Function 1: fn main() -> i32 { return 1; }
    let main_name = hir.strings.intern("main");
    let literal1_inst = hir.add_instruction(
        InstTag::Literal,
        InstData::literal(1),
        Some(RueType::I64),
        rue_lexer::Span { start: 0, end: 1 },
    );
    hir.add_instruction(
        InstTag::Return,
        InstData {
            lhs: literal1_inst.raw(),
            rhs: 0,
        },
        None,
        rue_lexer::Span { start: 2, end: 10 },
    );
    let main_block = hir.add_block(1, 2);
    let main_func = FunctionData {
        name: main_name,
        param_count: 0,
        return_type: Some(RueType::I64),
        body_block: main_block,
    };
    hir.functions.push(main_func);

    // Function 2: fn test() -> i32 { return 2; }
    let test_name = hir.strings.intern("test");
    let literal2_inst = hir.add_instruction(
        InstTag::Literal,
        InstData::literal(2),
        Some(RueType::I64),
        rue_lexer::Span { start: 11, end: 12 },
    );
    hir.add_instruction(
        InstTag::Return,
        InstData {
            lhs: literal2_inst.raw(),
            rhs: 0,
        },
        None,
        rue_lexer::Span { start: 13, end: 21 },
    );
    let test_block = hir.add_block(3, 2); // Instructions 3-4
    let test_func = FunctionData {
        name: test_name,
        param_count: 0,
        return_type: Some(RueType::I64),
        body_block: test_block,
    };
    hir.functions.push(test_func);

    // Lower to MIR
    let type_context = TypeContext::new();
    let mir = lower_hir2_to_mir(&hir, type_context);

    // Verify the result
    assert_eq!(mir.functions.len(), 2);
    assert_eq!(mir.functions[0].name, "main");
    assert_eq!(mir.functions[1].name, "test");

    // Should have function signatures for both
    assert!(mir.function_signatures.contains_key("main"));
    assert!(mir.function_signatures.contains_key("test"));
}

#[test]
fn test_function_call() {
    // Create HIR2 for: fn main() -> i32 { return print(42); }
    let mut hir = Hir::new();

    let main_name = hir.strings.intern("main");
    let print_name = hir.strings.intern("print");

    // Add literal 42
    let literal_inst = hir.add_instruction(
        InstTag::Literal,
        InstData::literal(42),
        Some(RueType::I64),
        rue_lexer::Span { start: 0, end: 2 },
    );

    // Add function call print(42)
    // Store arguments in extra_data: [arg_count, arg1, ...]
    let arg_data = vec![1, literal_inst.raw()]; // 1 argument: literal_inst
    let args_start = hir.add_extra_data(&arg_data);

    let call_inst = hir.add_instruction(
        InstTag::Call,
        InstData {
            lhs: print_name.raw(),
            rhs: args_start,
        },
        Some(RueType::I64),
        rue_lexer::Span { start: 3, end: 12 },
    );

    // Add return statement
    hir.add_instruction(
        InstTag::Return,
        InstData {
            lhs: call_inst.raw(),
            rhs: 0,
        },
        None,
        rue_lexer::Span { start: 14, end: 22 },
    );

    // Create function body block (instructions 1-3)
    let body_block = hir.add_block(1, 3);

    // Create main function
    let func_data = FunctionData {
        name: main_name,
        param_count: 0,
        return_type: Some(RueType::I64),
        body_block,
    };
    hir.functions.push(func_data);

    // Lower to MIR
    let type_context = TypeContext::new();
    let mir = lower_hir2_to_mir(&hir, type_context);

    // Verify the result
    assert_eq!(mir.functions.len(), 1);

    let main_func = &mir.functions[0];
    assert_eq!(main_func.name, "main");
    assert_eq!(main_func.return_type, RueType::I64);

    // Should have blocks and statements
    assert!(!main_func.blocks.is_empty());
    assert!(!main_func.blocks[0].statements.is_empty());

    // Look for the function call in the statements
    let has_call = main_func.blocks[0].statements.iter().any(|stmt| {
        matches!(stmt, MirStatement::Assign {
            value: MirValue::Call { func, args, .. }, ..
        } if func == "print" && args.len() == 1)
    });
    assert!(
        has_call,
        "Expected to find print function call with 1 argument"
    );
}

#[test]
fn test_function_call_with_hir_builder() {
    use crate::hir2_to_mir::lower_hir2_to_mir;
    use rue_ir::hir2_builder::HirBuilder;

    // Test the complete pipeline: HIR2 Builder -> HIR2 -> MIR
    let mut builder = HirBuilder::new();

    // Create function body: let result = add(5, 3); return result;
    builder.start_block();

    // Create arguments for the function call
    let arg1 = builder.emit_literal(5, RueType::I64);
    let arg2 = builder.emit_literal(3, RueType::I64);

    // Create function call: add(5, 3)
    let call_result = builder.emit_call("add", vec![arg1, arg2], RueType::I64);

    // Create let result = add(5, 3)
    let _let_result = builder.emit_let("result", call_result, RueType::I64);

    // Load result
    let load_result = builder.emit_load("result").unwrap();

    // Return result
    builder.emit_return(Some(load_result));

    let body = builder.end_block();

    // Create main function
    let _func = builder
        .create_function("main", vec![], RueType::I64, body)
        .unwrap();

    let hir = builder.finish();

    // Lower HIR2 to MIR
    let type_context = TypeContext::new();
    let mir = lower_hir2_to_mir(&hir, type_context);

    // Verify the result
    assert_eq!(mir.functions.len(), 1);

    let main_func = &mir.functions[0];
    assert_eq!(main_func.name, "main");
    assert_eq!(main_func.return_type, RueType::I64);

    // Should have blocks and statements
    assert!(!main_func.blocks.is_empty());
    assert!(!main_func.blocks[0].statements.is_empty());

    // Look for the function call in the statements
    let has_call = main_func.blocks[0].statements.iter().any(|stmt| {
        matches!(stmt, MirStatement::Assign {
            value: MirValue::Call { func, args, .. }, ..
        } if func == "add" && args.len() == 2)
    });
    assert!(
        has_call,
        "Expected to find add function call with 2 arguments"
    );
}
