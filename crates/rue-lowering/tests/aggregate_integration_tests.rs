//! Integration tests for aggregate lowering functionality
//!
//! These tests verify the complete HIR → MIR → PIR pipeline for aggregate types
//! including structs, tuples, and arrays.

use rue_ir::hir::{HirBlock, HirExpr, HirFunction, HirLiteral, HirProgram};
use rue_ir::mir::MirValue;
use rue_ir::types::{FieldId, RueType, StructId, TypeContext};
use rue_lexer::Span;
use rue_lowering::hir_to_mir::MirBuilder;
use rue_lowering::mir_to_pir::MirToPir;

/// Helper to create a simple HIR program with one function
fn create_test_hir_program(name: &str, body: HirBlock, return_type: RueType) -> HirProgram {
    let func = HirFunction {
        name: name.to_string(),
        params: vec![],
        return_type,
        body,
        span: Span::dummy(),
    };

    HirProgram {
        functions: vec![func],
    }
}

#[test]
fn test_struct_literal_to_pir() {
    // Test: struct Point { x: i32, y: i32 } literal
    let struct_type = RueType::Struct(StructId::new(1));

    let struct_literal = HirExpr::StructLiteral {
        struct_name: "Point".to_string(),
        fields: vec![
            (
                "x".to_string(),
                HirExpr::Literal {
                    lit: HirLiteral::Int32(10),
                    span: Span::dummy(),
                },
            ),
            (
                "y".to_string(),
                HirExpr::Literal {
                    lit: HirLiteral::Int32(20),
                    span: Span::dummy(),
                },
            ),
        ],
        ty: struct_type.clone(),
        span: Span::dummy(),
    };

    let hir_program = create_test_hir_program(
        "test_struct",
        HirBlock {
            statements: vec![],
            expr: Some(Box::new(struct_literal)),
        },
        struct_type,
    );

    // HIR → MIR
    let mir_program = MirBuilder::lower_program(&hir_program, TypeContext::new());

    // Verify MIR contains ConstructAggregate
    let mir_func = &mir_program.functions[0];
    let has_construct_aggregate = mir_func
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|stmt| match stmt {
            rue_ir::mir::MirStatement::Assign { value, .. } => {
                matches!(value, MirValue::ConstructAggregate { .. })
            }
            _ => false,
        });

    assert!(
        has_construct_aggregate,
        "MIR should contain ConstructAggregate instruction"
    );

    // MIR → PIR
    let mut lowerer = MirToPir::new();
    let pir_instructions = lowerer.lower_program(&mir_program);

    // Verify PIR contains AllocateAggregate and StoreField instructions
    let has_allocate = pir_instructions
        .iter()
        .any(|instr| matches!(instr, rue_ir::pir::PIR::AllocateAggregate { .. }));
    let has_store_field = pir_instructions
        .iter()
        .any(|instr| matches!(instr, rue_ir::pir::PIR::StoreField { .. }));

    assert!(
        has_allocate,
        "PIR should contain AllocateAggregate instruction"
    );
    assert!(
        has_store_field,
        "PIR should contain StoreField instructions"
    );
}

#[test]
fn test_tuple_literal_to_pir() {
    // Test: (i32, bool) tuple literal
    let tuple_type = RueType::Tuple(vec![RueType::I32, RueType::Bool]);

    let tuple_literal = HirExpr::TupleLiteral {
        elements: vec![
            HirExpr::Literal {
                lit: HirLiteral::Int32(42),
                span: Span::dummy(),
            },
            HirExpr::Literal {
                lit: HirLiteral::Bool(true),
                span: Span::dummy(),
            },
        ],
        ty: tuple_type.clone(),
        span: Span::dummy(),
    };

    let hir_program = create_test_hir_program(
        "test_tuple",
        HirBlock {
            statements: vec![],
            expr: Some(Box::new(tuple_literal)),
        },
        tuple_type,
    );

    // HIR → MIR
    let mir_program = MirBuilder::lower_program(&hir_program, TypeContext::new());

    // Verify MIR contains ConstructAggregate for tuple
    let mir_func = &mir_program.functions[0];
    let construct_aggregates: Vec<_> = mir_func
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .filter_map(|stmt| match stmt {
            rue_ir::mir::MirStatement::Assign {
                value: MirValue::ConstructAggregate { ty, fields },
                ..
            } => Some((ty, fields.len())),
            _ => None,
        })
        .collect();

    assert!(
        !construct_aggregates.is_empty(),
        "MIR should contain ConstructAggregate instruction"
    );
    assert_eq!(construct_aggregates[0].1, 2, "Tuple should have 2 fields");

    // MIR → PIR
    let mut lowerer = MirToPir::new();
    let pir_instructions = lowerer.lower_program(&mir_program);

    // Count StoreField instructions (should be 2 for tuple elements)
    let store_field_count = pir_instructions
        .iter()
        .filter(|instr| matches!(instr, rue_ir::pir::PIR::StoreField { .. }))
        .count();

    assert_eq!(
        store_field_count, 2,
        "PIR should contain 2 StoreField instructions for tuple elements"
    );
}

#[test]
fn test_array_literal_to_pir() {
    // Test: [i32; 3] array literal
    let array_type = RueType::Array(Box::new(RueType::I32), 3);

    let array_literal = HirExpr::ArrayLiteral {
        elements: vec![
            HirExpr::Literal {
                lit: HirLiteral::Int32(1),
                span: Span::dummy(),
            },
            HirExpr::Literal {
                lit: HirLiteral::Int32(2),
                span: Span::dummy(),
            },
            HirExpr::Literal {
                lit: HirLiteral::Int32(3),
                span: Span::dummy(),
            },
        ],
        ty: array_type.clone(),
        span: Span::dummy(),
    };

    let hir_program = create_test_hir_program(
        "test_array",
        HirBlock {
            statements: vec![],
            expr: Some(Box::new(array_literal)),
        },
        array_type,
    );

    // HIR → MIR
    let mir_program = MirBuilder::lower_program(&hir_program, TypeContext::new());

    // Verify MIR contains ConstructAggregate for array
    let mir_func = &mir_program.functions[0];
    let construct_aggregates: Vec<_> = mir_func
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .filter_map(|stmt| match stmt {
            rue_ir::mir::MirStatement::Assign {
                value: MirValue::ConstructAggregate { ty, fields },
                ..
            } => Some((ty, fields.len())),
            _ => None,
        })
        .collect();

    assert!(
        !construct_aggregates.is_empty(),
        "MIR should contain ConstructAggregate instruction"
    );
    assert_eq!(construct_aggregates[0].1, 3, "Array should have 3 elements");

    // MIR → PIR
    let mut lowerer = MirToPir::new();
    let pir_instructions = lowerer.lower_program(&mir_program);

    // Count StoreField instructions (should be 3 for array elements)
    let store_field_count = pir_instructions
        .iter()
        .filter(|instr| matches!(instr, rue_ir::pir::PIR::StoreField { .. }))
        .count();

    assert_eq!(
        store_field_count, 3,
        "PIR should contain 3 StoreField instructions for array elements"
    );
}

#[test]
fn test_field_access_to_pir() {
    // Test: struct.field access
    // We'll create a dummy struct access since we don't have full struct definitions yet
    let struct_type = RueType::Struct(StructId::new(2));

    // Create a variable reference for the base (simulating a struct parameter)
    let base_var = HirExpr::Var {
        name: "my_struct".to_string(),
        ty: struct_type.clone(),
        span: Span::dummy(),
    };

    let field_access = HirExpr::FieldAccess {
        base: Box::new(base_var),
        field: FieldId::Named("value".to_string()),
        ty: RueType::I32,
        span: Span::dummy(),
    };

    // Create a function that takes a struct parameter
    let func = HirFunction {
        name: "test_field_access".to_string(),
        params: vec![("my_struct".to_string(), struct_type)],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(field_access)),
        },
        span: Span::dummy(),
    };

    let hir_program = HirProgram {
        functions: vec![func],
    };

    // HIR → MIR
    let mir_program = MirBuilder::lower_program(&hir_program, TypeContext::new());

    // Verify MIR contains GetField
    let mir_func = &mir_program.functions[0];
    let has_get_field = mir_func
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .any(|stmt| match stmt {
            rue_ir::mir::MirStatement::Assign { value, .. } => {
                matches!(value, MirValue::GetField { .. })
            }
            _ => false,
        });

    assert!(has_get_field, "MIR should contain GetField instruction");

    // MIR → PIR
    let mut lowerer = MirToPir::new();
    let pir_instructions = lowerer.lower_program(&mir_program);

    // Verify PIR contains LoadField instruction
    let has_load_field = pir_instructions
        .iter()
        .any(|instr| matches!(instr, rue_ir::pir::PIR::LoadField { .. }));

    assert!(has_load_field, "PIR should contain LoadField instruction");
}

#[test]
fn test_zero_sized_aggregate() {
    // Test: Unit tuple () - zero-sized aggregate
    let unit_tuple_type = RueType::Tuple(vec![]);

    let unit_tuple = HirExpr::TupleLiteral {
        elements: vec![],
        ty: unit_tuple_type.clone(),
        span: Span::dummy(),
    };

    let hir_program = create_test_hir_program(
        "test_unit_tuple",
        HirBlock {
            statements: vec![],
            expr: Some(Box::new(unit_tuple)),
        },
        unit_tuple_type,
    );

    // HIR → MIR → PIR
    let mir_program = MirBuilder::lower_program(&hir_program, TypeContext::new());
    let mut lowerer = MirToPir::new();
    let pir_instructions = lowerer.lower_program(&mir_program);

    // For zero-sized aggregates, we should get a Copy instruction with dummy pointer
    let has_copy_with_dummy = pir_instructions.iter().any(|instr| {
        matches!(
            instr,
            rue_ir::pir::PIR::Copy {
                src: rue_ir::pir::Value::UnsignedImm(1),
                ..
            }
        )
    });

    assert!(
        has_copy_with_dummy,
        "Zero-sized aggregate should result in Copy with dummy pointer"
    );
}

#[test]
fn test_aggregate_type_layout_usage() {
    // Test that our implementation uses proper type layout for offset computation
    let tuple_type = RueType::Tuple(vec![RueType::I32, RueType::I64, RueType::Bool]);

    // Manually verify the expected layout
    let mut type_ctx = TypeContext::new();
    let layout = type_ctx.compute_layout(&tuple_type).unwrap();
    assert_eq!(layout.size, 24); // i32(4) + padding(4) + i64(8) + bool(1) + padding(7) = 24

    if let RueType::Tuple(types) = &tuple_type {
        assert_eq!(type_ctx.compute_tuple_field_offset(types, 0).unwrap(), 0); // First field at offset 0
        assert_eq!(type_ctx.compute_tuple_field_offset(types, 1).unwrap(), 8); // Second field aligned to 8 bytes
        assert_eq!(type_ctx.compute_tuple_field_offset(types, 2).unwrap(), 16); // Third field after i64
    }

    // Create a tuple literal to test the layout usage
    let tuple_literal = HirExpr::TupleLiteral {
        elements: vec![
            HirExpr::Literal {
                lit: HirLiteral::Int32(1),
                span: Span::dummy(),
            },
            HirExpr::Literal {
                lit: HirLiteral::Int64(2),
                span: Span::dummy(),
            },
            HirExpr::Literal {
                lit: HirLiteral::Bool(true),
                span: Span::dummy(),
            },
        ],
        ty: tuple_type.clone(),
        span: Span::dummy(),
    };

    let hir_program = create_test_hir_program(
        "test_layout",
        HirBlock {
            statements: vec![],
            expr: Some(Box::new(tuple_literal)),
        },
        tuple_type,
    );

    // HIR → MIR → PIR
    let mir_program = MirBuilder::lower_program(&hir_program, TypeContext::new());
    let mut lowerer = MirToPir::new();
    let pir_instructions = lowerer.lower_program(&mir_program);

    // Extract StoreField instructions and verify their offsets
    let store_field_offsets: Vec<i64> = pir_instructions
        .iter()
        .filter_map(|instr| match instr {
            rue_ir::pir::PIR::StoreField { offset, .. } => Some(*offset),
            _ => None,
        })
        .collect();

    // Should have 3 StoreField instructions with proper offsets
    assert_eq!(
        store_field_offsets.len(),
        3,
        "Should have 3 store field instructions"
    );
    assert!(
        store_field_offsets.contains(&0),
        "Should store at offset 0 (i32 field)"
    );
    assert!(
        store_field_offsets.contains(&8),
        "Should store at offset 8 (i64 field)"
    );
    assert!(
        store_field_offsets.contains(&16),
        "Should store at offset 16 (bool field)"
    );
}

#[test]
fn test_nested_aggregate_access() {
    // Test: tuple.0 field access on tuple
    let tuple_type = RueType::Tuple(vec![RueType::I32, RueType::Bool]);

    // Create a variable reference for the base tuple
    let base_tuple = HirExpr::Var {
        name: "my_tuple".to_string(),
        ty: tuple_type.clone(),
        span: Span::dummy(),
    };

    let field_access = HirExpr::FieldAccess {
        base: Box::new(base_tuple),
        field: FieldId::Index(0), // Access first element
        ty: RueType::I32,
        span: Span::dummy(),
    };

    // Create a function that takes a tuple parameter
    let func = HirFunction {
        name: "test_tuple_access".to_string(),
        params: vec![("my_tuple".to_string(), tuple_type)],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(field_access)),
        },
        span: Span::dummy(),
    };

    let hir_program = HirProgram {
        functions: vec![func],
    };

    // HIR → MIR → PIR
    let mir_program = MirBuilder::lower_program(&hir_program, TypeContext::new());
    let mut lowerer = MirToPir::new();
    let pir_instructions = lowerer.lower_program(&mir_program);

    // Verify PIR contains LoadField instruction with correct offset
    let load_field_info: Vec<i64> = pir_instructions
        .iter()
        .filter_map(|instr| match instr {
            rue_ir::pir::PIR::LoadField { offset, .. } => Some(*offset),
            _ => None,
        })
        .collect();

    assert!(
        !load_field_info.is_empty(),
        "PIR should contain LoadField instruction"
    );
    assert!(
        load_field_info.contains(&0),
        "Should load from offset 0 (first tuple element)"
    );
}

#[test]
fn test_allocation_strategy_integration() {
    // Test that different sized aggregates use appropriate allocation strategies

    // Small tuple (should use stack allocation strategy in future)
    let small_tuple = RueType::Tuple(vec![RueType::I32, RueType::I32]); // 8 bytes
    let mut type_ctx = TypeContext::new();
    let small_layout = type_ctx.compute_layout(&small_tuple).unwrap();
    assert_eq!(small_layout.size, 8);

    // Large array (should use heap allocation strategy)
    let large_array = RueType::Array(Box::new(RueType::I64), 20); // 160 bytes
    let large_layout = type_ctx.compute_layout(&large_array).unwrap();
    assert_eq!(large_layout.size, 160);

    // Test that both can be lowered without panic
    let small_literal = HirExpr::TupleLiteral {
        elements: vec![
            HirExpr::Literal {
                lit: HirLiteral::Int32(1),
                span: Span::dummy(),
            },
            HirExpr::Literal {
                lit: HirLiteral::Int32(2),
                span: Span::dummy(),
            },
        ],
        ty: small_tuple.clone(),
        span: Span::dummy(),
    };

    let large_literal = HirExpr::ArrayLiteral {
        elements: (0..20)
            .map(|i| HirExpr::Literal {
                lit: HirLiteral::Int64(i),
                span: Span::dummy(),
            })
            .collect(),
        ty: large_array.clone(),
        span: Span::dummy(),
    };

    // Test small aggregate
    let small_hir = create_test_hir_program(
        "test_small",
        HirBlock {
            statements: vec![],
            expr: Some(Box::new(small_literal)),
        },
        small_tuple,
    );

    let small_mir = MirBuilder::lower_program(&small_hir, TypeContext::new());
    let mut small_lowerer = MirToPir::new();
    let small_pir = small_lowerer.lower_program(&small_mir);

    // Test large aggregate
    let large_hir = create_test_hir_program(
        "test_large",
        HirBlock {
            statements: vec![],
            expr: Some(Box::new(large_literal)),
        },
        large_array,
    );

    let large_mir = MirBuilder::lower_program(&large_hir, TypeContext::new());
    let mut large_lowerer = MirToPir::new();
    let large_pir = large_lowerer.lower_program(&large_mir);

    // Both should complete without error and contain allocation instructions
    assert!(
        small_pir
            .iter()
            .any(|i| matches!(i, rue_ir::pir::PIR::AllocateAggregate { .. }))
    );
    assert!(
        large_pir
            .iter()
            .any(|i| matches!(i, rue_ir::pir::PIR::AllocateAggregate { .. }))
    );
}
