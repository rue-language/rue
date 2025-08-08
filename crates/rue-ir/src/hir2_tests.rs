//! Tests for HIR2 data-driven instruction format
//!
//! These tests verify that the new instruction-based HIR works correctly
//! and can represent the same programs as the tree-based HIR.

use crate::hir2::*;
use crate::hir2_builder::HirBuilder;
use crate::types::RueType;
use rue_lexer::Span;

#[test]
fn test_basic_instruction_creation() {
    let mut hir = Hir::new();

    // Test that we start with index 0 reserved
    assert_eq!(hir.instructions.len(), 1);

    // Add a literal instruction
    let span = Span { start: 0, end: 2 };
    let data = InstData::literal(42);
    let inst = hir.add_instruction(InstTag::Literal, data, Some(RueType::I32), span);

    assert_eq!(inst.raw(), 1);
    assert_eq!(hir.instructions.tag(inst), InstTag::Literal);
    assert_eq!(hir.instructions.data(inst).lhs, 42);
    assert_eq!(hir.get_type(inst), Some(&RueType::I32));
    assert_eq!(hir.get_span(inst), span);
}

#[test]
fn test_string_pool_integration() {
    let mut hir = Hir::new();

    let hello_idx = hir.strings.intern("hello");
    let world_idx = hir.strings.intern("world");
    let hello_idx2 = hir.strings.intern("hello");

    // String interning should work
    assert_eq!(hello_idx, hello_idx2);
    assert_ne!(hello_idx, world_idx);

    // Strings should be retrievable
    assert_eq!(hir.strings.get(hello_idx), "hello");
    assert_eq!(hir.strings.get(world_idx), "world");
}

#[test]
fn test_builder_simple_program() {
    let mut builder = HirBuilder::new();

    // Build: let x: i32 = 42; return x;
    builder.start_block();

    let forty_two = builder.emit_literal(42, RueType::I32);
    builder.emit_let("x", forty_two, RueType::I32);

    let x_load = builder.emit_load("x").unwrap();
    builder.emit_return(Some(x_load));

    let body_block = builder.end_block();

    builder
        .create_function("main", vec![], RueType::I32, body_block)
        .unwrap();

    let hir = builder.finish();

    // Verify the structure
    assert_eq!(hir.functions.len(), 1);
    assert_eq!(hir.strings.get(hir.functions[0].name), "main");
    assert!(hir.main_function.is_some());

    // Print for debugging
    println!("Generated HIR:\n{}", hir);
}

#[test]
fn test_builder_arithmetic_expression() {
    let mut builder = HirBuilder::new();

    // Build: let result: i32 = 10 + 5 * 2; return result;
    builder.start_block();

    // Create literals
    let ten = builder.emit_literal(10, RueType::I32);
    let five = builder.emit_literal(5, RueType::I32);
    let two = builder.emit_literal(2, RueType::I32);

    // Create 5 * 2
    let mul = builder.emit_binary(five, two, BinOp::Mul, RueType::I32);

    // Create 10 + (5 * 2)
    let add = builder.emit_binary(ten, mul, BinOp::Add, RueType::I32);

    // Let binding
    builder.emit_let("result", add, RueType::I32);

    // Return
    let result_load = builder.emit_load("result").unwrap();
    builder.emit_return(Some(result_load));

    let body_block = builder.end_block();

    builder
        .create_function("main", vec![], RueType::I32, body_block)
        .unwrap();

    let hir = builder.finish();

    // Verify we have the expected instructions
    let mut literal_count = 0;
    let mut binary_count = 0;
    let mut let_count = 0;
    let mut load_count = 0;
    let mut return_count = 0;

    for i in 1..hir.instructions.len() {
        if let Some(inst) = InstIndex::new(i as u32) {
            match hir.instructions.tag(inst) {
                InstTag::Literal => literal_count += 1,
                InstTag::Binary => binary_count += 1,
                InstTag::Let => let_count += 1,
                InstTag::Load => load_count += 1,
                InstTag::Return => return_count += 1,
                _ => {}
            }
        }
    }

    assert_eq!(literal_count, 3); // 10, 5, 2
    assert_eq!(binary_count, 2); // mul, add
    assert_eq!(let_count, 1); // result binding
    assert_eq!(load_count, 1); // result load
    assert_eq!(return_count, 1); // return statement

    println!("Arithmetic HIR:\n{}", hir);
}

#[test]
fn test_builder_function_with_parameters() {
    let mut builder = HirBuilder::new();

    // Build: fn add(a: i32, b: i32) -> i32 { return a + b; }
    builder.start_block();

    // For now, we'll simulate parameters by declaring them as variables
    // In a full implementation, parameters would be handled specially
    let a_literal = builder.emit_literal(0, RueType::I32); // Placeholder
    builder.emit_let("a", a_literal, RueType::I32);

    let b_literal = builder.emit_literal(0, RueType::I32); // Placeholder
    builder.emit_let("b", b_literal, RueType::I32);

    // Function body: a + b
    let a_load = builder.emit_load("a").unwrap();
    let b_load = builder.emit_load("b").unwrap();
    let sum = builder.emit_binary(a_load, b_load, BinOp::Add, RueType::I32);

    builder.emit_return(Some(sum));

    let body_block = builder.end_block();

    let params = vec![
        ("a".to_string(), RueType::I32),
        ("b".to_string(), RueType::I32),
    ];

    builder
        .create_function("add", params, RueType::I32, body_block)
        .unwrap();

    let hir = builder.finish();

    assert_eq!(hir.functions.len(), 1);
    let func = &hir.functions[0];
    assert_eq!(hir.strings.get(func.name), "add");
    assert_eq!(func.param_count, 2);
    assert_eq!(func.return_type, Some(RueType::I32));

    println!("Function HIR:\n{}", hir);
}

#[test]
fn test_builder_control_flow() {
    let mut builder = HirBuilder::new();

    // Build: fn test(x: i32) -> i32 {
    //   if x > 5 {
    //     return x * 2;
    //   } else {
    //     return x + 1;
    //   }
    // }

    builder.start_block();

    // Parameter setup
    let x_param = builder.emit_literal(0, RueType::I32); // Placeholder
    builder.emit_let("x", x_param, RueType::I32);

    // Condition: x > 5
    let x_load1 = builder.emit_load("x").unwrap();
    let five = builder.emit_literal(5, RueType::I32);
    let cond = builder.emit_binary(x_load1, five, BinOp::Gt, RueType::Bool);

    // Then block: return x * 2
    builder.start_block();
    let x_load2 = builder.emit_load("x").unwrap();
    let two = builder.emit_literal(2, RueType::I32);
    let mul_result = builder.emit_binary(x_load2, two, BinOp::Mul, RueType::I32);
    builder.emit_return(Some(mul_result));
    let then_block = builder.end_block();

    // Else block: return x + 1
    builder.start_block();
    let x_load3 = builder.emit_load("x").unwrap();
    let one = builder.emit_literal(1, RueType::I32);
    let add_result = builder.emit_binary(x_load3, one, BinOp::Add, RueType::I32);
    builder.emit_return(Some(add_result));
    let else_block = builder.end_block();

    // If expression
    builder.emit_if(cond, then_block, Some(else_block), Some(RueType::I32));

    let body_block = builder.end_block();

    let params = vec![("x".to_string(), RueType::I32)];
    builder
        .create_function("test", params, RueType::I32, body_block)
        .unwrap();

    let hir = builder.finish();

    assert_eq!(hir.functions.len(), 1);
    assert_eq!(hir.blocks.len(), 3); // Main body + then + else

    println!("Control Flow HIR:\n{}", hir);
}

#[test]
fn test_builder_while_loop() {
    let mut builder = HirBuilder::new();

    // Build: fn countdown(n: i32) -> i32 {
    //   while n > 0 {
    //     n = n - 1;
    //   }
    //   return n;
    // }

    builder.start_block();

    // Parameter setup
    let n_param = builder.emit_literal(10, RueType::I32); // Example value
    builder.emit_let("n", n_param, RueType::I32);

    // Loop condition: n > 0
    let n_load1 = builder.emit_load("n").unwrap();
    let zero = builder.emit_literal(0, RueType::I32);
    let cond = builder.emit_binary(n_load1, zero, BinOp::Gt, RueType::Bool);

    // Loop body: n = n - 1
    builder.start_block();
    let n_load2 = builder.emit_load("n").unwrap();
    let one = builder.emit_literal(1, RueType::I32);
    let sub_result = builder.emit_binary(n_load2, one, BinOp::Sub, RueType::I32);
    builder.emit_assign("n", sub_result).unwrap();
    let loop_body = builder.end_block();

    // While loop
    builder.emit_while(cond, loop_body);

    // Return n
    let n_load3 = builder.emit_load("n").unwrap();
    builder.emit_return(Some(n_load3));

    let body_block = builder.end_block();

    let params = vec![("n".to_string(), RueType::I32)];
    builder
        .create_function("countdown", params, RueType::I32, body_block)
        .unwrap();

    let hir = builder.finish();

    assert_eq!(hir.functions.len(), 1);

    // Look for while instruction
    let mut while_count = 0;
    let mut assign_count = 0;
    for i in 1..hir.instructions.len() {
        if let Some(inst) = InstIndex::new(i as u32) {
            match hir.instructions.tag(inst) {
                InstTag::While => while_count += 1,
                InstTag::Assign => assign_count += 1,
                _ => {}
            }
        }
    }

    assert_eq!(while_count, 1);
    assert_eq!(assign_count, 1);

    println!("While Loop HIR:\n{}", hir);
}

#[test]
fn test_binary_op_conversions() {
    // Test that binary operators can be converted to/from u32
    let ops = [
        BinOp::Add,
        BinOp::Sub,
        BinOp::Mul,
        BinOp::Div,
        BinOp::Mod,
        BinOp::Eq,
        BinOp::Ne,
        BinOp::Lt,
        BinOp::Le,
        BinOp::Gt,
        BinOp::Ge,
    ];

    for (i, op) in ops.iter().enumerate() {
        let as_u32 = *op as u32;
        let converted_back = BinOp::from_u32(as_u32);
        assert_eq!(*op, converted_back, "Failed for operator at index {}", i);
    }
}

#[test]
fn test_unary_op_conversions() {
    let neg = UnaryOp::Neg;
    let as_u32 = neg as u32;
    let converted_back = UnaryOp::from_u32(as_u32);
    assert_eq!(neg, converted_back);
}

#[test]
fn test_instruction_memory_layout() {
    // Verify that our structs have the expected sizes for cache efficiency
    use std::mem;

    assert_eq!(mem::size_of::<InstTag>(), 1);
    assert_eq!(mem::size_of::<InstData>(), 8);
    assert_eq!(mem::size_of::<StringIndex>(), 4);

    // Total per instruction should be manageable
    let total_per_inst = mem::size_of::<InstTag>() + mem::size_of::<InstData>();
    assert_eq!(total_per_inst, 9); // 1 + 8 bytes

    println!("Memory layout:");
    println!("  InstTag: {} bytes", mem::size_of::<InstTag>());
    println!("  InstData: {} bytes", mem::size_of::<InstData>());
    println!("  Total per instruction: {} bytes", total_per_inst);
}

#[test]
fn test_block_walking() {
    let mut builder = HirBuilder::new();

    // Build a simple block with a few instructions
    builder.start_block();
    let lit1 = builder.emit_literal(42, RueType::I32);
    let lit2 = builder.emit_literal(99, RueType::I32);
    let add = builder.emit_binary(lit1, lit2, BinOp::Add, RueType::I32);
    builder.emit_return(Some(add));
    let block = builder.end_block();

    let hir = builder.finish();

    // Walk the block and collect instructions
    let mut instructions = Vec::new();
    hir.walk_block(block, |inst_idx, tag, _data| {
        instructions.push((inst_idx, tag));
    });

    // We should have collected all the instructions in the block
    assert!(!instructions.is_empty());

    println!("Block contains {} instructions", instructions.len());
    for (idx, tag) in instructions {
        println!("  Inst({}): {:?}", idx.raw(), tag);
    }
}
