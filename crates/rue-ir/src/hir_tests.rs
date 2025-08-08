/// Comprehensive tests for HIR SSA form and control flow
use crate::hir::*;
use crate::hir_builder::HirBuilder;
use crate::types::RueType;
use rue_lexer::Span;

/// Test 1: Block params & args round-trip
#[test]
fn test_block_params_args_roundtrip() {
    let mut builder = HirBuilder::new();

    // Create main block
    builder.start_block();

    // Create a value to pass as argument
    let init_ref = builder.emit_literal(42, RueType::I32);

    // Create then branch with two params
    let x_idx = builder.intern_string("x");
    let y_idx = builder.intern_string("y");
    builder.start_block_with_params(vec![
        BlockParam {
            name: x_idx,
            ty: RueType::I32,
        },
        BlockParam {
            name: y_idx,
            ty: RueType::I32,
        },
    ]);
    let then_block_id = builder.end_block();

    // Create else branch with one param
    let z_idx = builder.intern_string("z");
    builder.start_block_with_params(vec![BlockParam {
        name: z_idx,
        ty: RueType::I32,
    }]);
    let else_block_id = builder.end_block();

    // Create condition
    let cond_ref = builder.emit_literal(1, RueType::Bool);

    // Set if terminator with different arg counts
    builder.set_if_terminator(
        cond_ref,
        then_block_id,
        vec![init_ref, init_ref], // Two args for then
        else_block_id,
        vec![init_ref], // One arg for else
    );

    let main_block_id = builder.end_block();
    let hir = builder.finish().unwrap();

    // Verify param/arg arity matches
    let then_block = hir.block(then_block_id).unwrap();
    assert_eq!(then_block.params.len(), 2);

    let else_block = hir.block(else_block_id).unwrap();
    assert_eq!(else_block.params.len(), 1);

    let main_block = hir.block(main_block_id).unwrap();
    if let Terminator::If {
        then_args,
        else_args,
        ..
    } = &main_block.term
    {
        assert_eq!(then_args.len(), 2);
        assert_eq!(else_args.len(), 1);
    } else {
        panic!("Expected If terminator");
    }
}

/// Test 2: Loop carried variable
#[test]
fn test_loop_carried_variable() {
    let mut builder = HirBuilder::new();

    // Preheader block
    builder.start_block();
    let init_ref = builder.emit_literal(0, RueType::I32);
    let preheader_id = builder.end_block();

    // Header block with one param (loop counter)
    let i_idx = builder.intern_string("i");
    builder.start_block_with_params(vec![BlockParam {
        name: i_idx,
        ty: RueType::I32,
    }]);

    // Use Param(0) to access loop counter - use current block ID
    let header_id_temp = builder.current_block_builder().block_id;
    let counter = ValueRef::from_block_param_with_id(header_id_temp, BlockParamIndex::new(0));

    // Check condition (i < 10)
    let limit_ref = builder.emit_literal(10, RueType::I32);
    let cond_ref = builder.emit_binary(counter, limit_ref, BinOp::Lt, RueType::Bool);

    let header_id = builder.end_block();

    // Body block - needs to receive the counter as a parameter
    builder.start_block_with_params(vec![BlockParam {
        name: i_idx,
        ty: RueType::I32,
    }]);

    // Access counter from body's own parameter - use current block ID
    let body_id_temp = builder.current_block_builder().block_id;
    let body_counter = ValueRef::from_block_param_with_id(body_id_temp, BlockParamIndex::new(0));

    // Increment counter: i + 1
    let one_ref = builder.emit_literal(1, RueType::I32);
    let updated_ref = builder.emit_binary(body_counter, one_ref, BinOp::Add, RueType::I32);

    // Jump back to header with updated counter
    builder.set_goto_terminator(header_id, vec![updated_ref]);
    let body_id = builder.end_block();

    // Exit block - needs to receive the counter as a parameter
    builder.start_block_with_params(vec![BlockParam {
        name: i_idx,
        ty: RueType::I32,
    }]);
    let exit_id_temp = builder.current_block_builder().block_id;
    let exit_counter = ValueRef::from_block_param_with_id(exit_id_temp, BlockParamIndex::new(0));
    builder.set_return_terminator(Some(exit_counter));
    let exit_id = builder.end_block();

    // Set up control flow
    let mut hir = builder.finish().unwrap();

    // Preheader -> header with initial value
    hir.block_mut(preheader_id).unwrap().term = Terminator::Goto {
        target: header_id,
        args: vec![init_ref],
    };

    // Header branches to body or exit, passing counter to both
    hir.block_mut(header_id).unwrap().term = Terminator::If {
        cond: cond_ref,
        then_target: body_id,
        then_args: vec![counter], // Pass counter to body
        else_target: exit_id,
        else_args: vec![counter], // Pass counter to exit
    };

    // Verify back-edge arity
    let body_block = hir.block(body_id).unwrap();
    if let Terminator::Goto { args, .. } = &body_block.term {
        assert_eq!(args.len(), 1); // One backedge arg
    }

    let header_block = hir.block(header_id).unwrap();
    assert_eq!(header_block.params.len(), 1); // One param

    // Verify structure rather than relying on Debug output
    // The key properties are already verified above
}

/// Test 3: CFG integrity - Diamond pattern
#[test]
fn test_cfg_diamond_pattern() {
    let mut builder = HirBuilder::new();

    // Entry block
    builder.start_block();
    let cond_ref = builder.emit_literal(1, RueType::Bool);
    let entry_id = builder.end_block();

    // Then block
    builder.start_block();
    let then_ref = builder.emit_literal(1, RueType::I32);
    let then_id = builder.end_block();

    // Else block
    builder.start_block();
    let else_ref = builder.emit_literal(2, RueType::I32);
    let else_id = builder.end_block();

    // Merge block with phi
    let result_idx = builder.intern_string("result");
    builder.start_block_with_params(vec![BlockParam {
        name: result_idx,
        ty: RueType::I32,
    }]);
    let merge_id_temp = builder.current_block_builder().block_id;
    let result = ValueRef::from_block_param_with_id(merge_id_temp, BlockParamIndex::new(0));
    builder.set_return_terminator(Some(result));
    let merge_id = builder.end_block();

    // Set up terminators
    let mut hir = builder.finish().unwrap();

    hir.block_mut(entry_id).unwrap().term = Terminator::If {
        cond: cond_ref,
        then_target: then_id,
        then_args: vec![],
        else_target: else_id,
        else_args: vec![],
    };

    hir.block_mut(then_id).unwrap().term = Terminator::Goto {
        target: merge_id,
        args: vec![then_ref],
    };

    hir.block_mut(else_id).unwrap().term = Terminator::Goto {
        target: merge_id,
        args: vec![else_ref],
    };

    // Test CFG utilities
    let entry_succs = hir.successors(entry_id);
    assert_eq!(entry_succs.len(), 2);
    assert!(entry_succs.contains(&then_id));
    assert!(entry_succs.contains(&else_id));

    let merge_preds = hir.predecessors(merge_id);
    assert_eq!(merge_preds.len(), 2);
    assert!(merge_preds.contains(&then_id));
    assert!(merge_preds.contains(&else_id));

    // Test reachability
    assert!(hir.is_reachable_from(entry_id, merge_id));
    assert!(hir.is_reachable_from(entry_id, then_id));
    assert!(hir.is_reachable_from(entry_id, else_id));
    assert!(!hir.is_reachable_from(merge_id, entry_id));
}

/// Test 4: CFG integrity - Single loop
#[test]
fn test_cfg_single_loop() {
    let mut builder = HirBuilder::new();

    // Entry
    builder.start_block();
    let entry_id = builder.end_block();

    // Loop header
    let i_idx = builder.intern_string("i");
    builder.start_block_with_params(vec![BlockParam {
        name: i_idx,
        ty: RueType::I32,
    }]);
    let header_id_temp = builder.current_block_builder().block_id;
    let counter = ValueRef::from_block_param_with_id(header_id_temp, BlockParamIndex::new(0));
    let limit_ref = builder.emit_literal(10, RueType::I32);
    let cond_ref = builder.emit_binary(counter, limit_ref, BinOp::Lt, RueType::Bool);
    let header_id = builder.end_block();

    // Loop body - needs to receive counter as parameter
    builder.start_block_with_params(vec![BlockParam {
        name: i_idx,
        ty: RueType::I32,
    }]);
    let body_id_temp = builder.current_block_builder().block_id;
    let body_counter = ValueRef::from_block_param_with_id(body_id_temp, BlockParamIndex::new(0));
    let one_ref = builder.emit_literal(1, RueType::I32);
    let updated_ref = builder.emit_binary(body_counter, one_ref, BinOp::Add, RueType::I32);
    builder.set_goto_terminator(header_id, vec![updated_ref]);
    let body_id = builder.end_block();

    // Exit
    builder.start_block();
    builder.set_return_terminator(None);
    let exit_id = builder.end_block();

    // Set up control flow
    let mut hir = builder.finish().unwrap();

    let init = {
        let entry_block = hir.block_mut(entry_id).unwrap();
        let init_inst = Instruction {
            tag: InstTag::Literal,
            data: InstData::Literal(0),
            ty: Some(RueType::I32),
            span: Span::dummy(),
        };
        let idx = entry_block.push_instruction(init_inst);
        ValueRef::from_inst(entry_id, idx)
    };

    hir.block_mut(entry_id).unwrap().term = Terminator::Goto {
        target: header_id,
        args: vec![init],
    };

    hir.block_mut(header_id).unwrap().term = Terminator::If {
        cond: cond_ref,
        then_target: body_id,
        then_args: vec![counter], // Pass counter to body
        else_target: exit_id,
        else_args: vec![],
    };

    // Test loop structure
    let header_succs = hir.successors(header_id);
    assert_eq!(header_succs.len(), 2);
    assert!(header_succs.contains(&body_id));
    assert!(header_succs.contains(&exit_id));

    let header_preds = hir.predecessors(header_id);
    assert_eq!(header_preds.len(), 2);
    assert!(header_preds.contains(&entry_id)); // Entry edge
    assert!(header_preds.contains(&body_id)); // Back edge

    // Reachability including cycles
    assert!(hir.is_reachable_from(entry_id, header_id));
    assert!(hir.is_reachable_from(entry_id, body_id));
    assert!(hir.is_reachable_from(entry_id, exit_id));
    assert!(hir.is_reachable_from(header_id, body_id));
    assert!(hir.is_reachable_from(body_id, header_id)); // Back edge
}

/// Test 5: Typing invariant - value-producing instructions must have types
#[test]
fn test_typing_invariant() {
    let mut builder = HirBuilder::new();

    builder.start_block();

    // Value-producing instructions should have types
    let lit_ref = builder.emit_literal(42, RueType::I32);

    let unary_ref = builder.emit_unary(lit_ref, UnaryOp::Neg, RueType::I32);

    let _binary = builder.emit_binary(lit_ref, unary_ref, BinOp::Add, RueType::I32);

    let block_id = builder.end_block();
    let hir = builder.finish().unwrap();
    let block = hir.block(block_id).unwrap();

    // All value-producing instructions should have types
    assert!(block.body[0].ty.is_some()); // literal
    assert!(block.body[1].ty.is_some()); // unary
    assert!(block.body[2].ty.is_some()); // binary

    // Verify specific types
    assert_eq!(block.body[0].ty, Some(RueType::I32));
    assert_eq!(block.body[1].ty, Some(RueType::I32));
    assert_eq!(block.body[2].ty, Some(RueType::I32));
}

/// Test 6: Function call resolution
#[test]
fn test_function_call_resolution() {
    let mut builder = HirBuilder::new();

    // Define a function first
    builder.start_block();
    builder.set_return_terminator(None);
    let func_body_id = builder.end_block();

    // We need to get the block first before finishing
    let func_body = {
        let hir_temp = builder.finish().unwrap();
        hir_temp.block(func_body_id).unwrap().clone()
    };

    // Create a new builder to continue
    let mut builder = HirBuilder::new();

    // Add the function directly to the HIR
    let name_idx = builder.intern_string("test_func");
    let param_name_idx = builder.intern_string("x");
    let func = Function {
        name: name_idx,
        params: vec![BlockParam {
            name: param_name_idx,
            ty: RueType::I32,
        }],
        return_type: Some(RueType::I32),
        body: func_body,
        span: Span::dummy(),
    };
    let func_id = builder.add_function(func);

    // Now create a call to it
    builder.start_block();
    let arg_ref = builder.emit_literal(42, RueType::I32);
    let _call = builder.emit_call("test_func", vec![arg_ref], RueType::I32);

    let block_id = builder.end_block();
    let hir = builder.finish().unwrap();

    // Verify function was added
    assert_eq!(hir.functions.len(), 1);
    assert_eq!(hir.function_arena.len(), 1);

    // Verify function can be found
    let found_id = hir.find_function("test_func").unwrap();
    assert_eq!(found_id, func_id);

    // Verify function data
    let func = hir.get_function(func_id).unwrap();
    assert_eq!(hir.strings.get(func.name), "test_func");
    assert_eq!(func.params.len(), 1);
    assert_eq!(func.return_type, Some(RueType::I32));

    // Note: Call instruction still has callee: None because we don't
    // do resolution during building. A separate resolution pass would
    // fill in the callee field.
    let block = hir.block(block_id).unwrap();
    if let InstData::Call { func, callee, args } = &block.body[1].data {
        assert_eq!(hir.strings.get(*func), "test_func");
        assert_eq!(callee, &None); // Not resolved yet
        assert_eq!(args.len(), 1);
    } else {
        panic!("Expected Call instruction");
    }
}

/// Test 7: Break and Continue terminators
#[test]
fn test_break_continue_terminators() {
    let mut builder = HirBuilder::new();

    // Loop header with param
    let i_idx = builder.intern_string("i");
    builder.start_block_with_params(vec![BlockParam {
        name: i_idx,
        ty: RueType::I32,
    }]);
    let header_id = builder.end_block();

    // Body with break
    builder.start_block();
    builder.set_break_terminator(header_id, vec![]);
    let break_block_id = builder.end_block();

    // Body with continue
    builder.start_block();
    let value_ref = builder.emit_literal(5, RueType::I32);
    builder.set_continue_terminator(header_id, vec![value_ref]);
    let continue_block_id = builder.end_block();

    let hir = builder.finish().unwrap();

    // Verify break terminator
    let break_block = hir.block(break_block_id).unwrap();
    if let Terminator::Break { loop_header, args } = &break_block.term {
        assert_eq!(*loop_header, header_id);
        assert_eq!(args.len(), 0);
    } else {
        panic!("Expected Break terminator");
    }

    // Verify continue terminator
    let continue_block = hir.block(continue_block_id).unwrap();
    if let Terminator::Continue { loop_header, args } = &continue_block.term {
        assert_eq!(*loop_header, header_id);
        assert_eq!(args.len(), 1);
    } else {
        panic!("Expected Continue terminator");
    }
}
