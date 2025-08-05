//! Tests for MIR aggregate type support
//!
//! These tests create synthetic MIR programs to test aggregate type handling
//! without requiring parser or HIR support.

#[cfg(test)]
mod tests {
    use crate::mir::*;
    use crate::types::{FieldId, RueType, StructId};
    use std::collections::HashMap;

    /// Helper to create a simple MIR program with one function
    fn create_test_program(name: &str, blocks: Vec<BasicBlock>) -> MirProgram {
        let func = MirFunction {
            name: name.to_string(),
            params: vec![],
            return_type: RueType::Unit,
            blocks,
            entry_block: BlockId(0),
            temp_types: HashMap::new(),
            span: rue_lexer::Span::dummy(),
        };

        MirProgram {
            functions: vec![func],
            function_signatures: HashMap::new(),
            type_context: crate::types::TypeContext::new(),
        }
    }

    #[test]
    fn test_aggregate_construction() {
        // Test: t0 = ConstructAggregate Point { t1, t2 }
        let struct_id = StructId::new(1);
        let t1 = Temp(1);
        let t2 = Temp(2);
        let t0 = Temp(0);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![
                MirStatement::Assign {
                    dest: t1,
                    value: MirValue::Const(MirConst::Int32(10)),
                    span: None,
                },
                MirStatement::Assign {
                    dest: t2,
                    value: MirValue::Const(MirConst::Int32(20)),
                    span: None,
                },
                MirStatement::Assign {
                    dest: t0,
                    value: MirValue::ConstructAggregate {
                        ty: RueType::Struct(struct_id),
                        fields: vec![t1, t2],
                    },
                    span: None,
                },
            ],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let program = create_test_program("test_construct", vec![block]);
        let output = format!("{program}");

        assert!(output.contains("ConstructAggregate"));
        assert!(output.contains("struct#1"));
    }

    #[test]
    fn test_field_access() {
        // Test: t2 = GetField t1.0
        let t1 = Temp(1);
        let t2 = Temp(2);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![
                MirStatement::Assign {
                    dest: t1,
                    value: MirValue::ConstructAggregate {
                        ty: RueType::Tuple(vec![RueType::I32, RueType::I32]),
                        fields: vec![],
                    },
                    span: None,
                },
                MirStatement::Assign {
                    dest: t2,
                    value: MirValue::GetField {
                        base: t1,
                        field: FieldId::Index(0),
                    },
                    span: None,
                },
            ],
            terminator: MirTerminator::Return {
                value: Some(t2),
                span: None,
            },
        };

        let program = create_test_program("test_field", vec![block]);
        let output = format!("{program}");

        assert!(output.contains("t1.0"));
        assert!(output.contains("GetField") || output.contains("."));
    }

    #[test]
    fn test_struct_update() {
        // Test: t3 = StructUpdate t1 { x: t2 }
        let struct_id = StructId::new(2);
        let t1 = Temp(1);
        let t2 = Temp(2);
        let t3 = Temp(3);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: t3,
                value: MirValue::StructUpdate {
                    base: t1,
                    updates: vec![(FieldId::Named("x".to_string()), t2)],
                    struct_type: struct_id,
                },
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let program = create_test_program("test_update", vec![block]);
        let output = format!("{program}");

        assert!(output.contains("StructUpdate"));
        assert!(output.contains("..t1"));
    }

    #[test]
    fn test_aggregate_constant() {
        // Test: const Tuple(i32, i32) { 1, 2 }
        let const_val = MirConst::aggregate(
            RueType::Tuple(vec![RueType::I32, RueType::I32]),
            vec![MirConst::Int32(1), MirConst::Int32(2)],
        );

        let t0 = Temp(0);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: t0,
                value: MirValue::Const(const_val),
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let program = create_test_program("test_const", vec![block]);
        let output = format!("{program}");

        assert!(output.contains("const"));
        // Check for the actual tuple format in the output
        assert!(output.contains("(i32, i32)"));
    }

    #[test]
    fn test_array_type() {
        // Test array construction and access
        let array_ty = RueType::Array(Box::new(RueType::I32), 3);
        let t0 = Temp(0);
        let t1 = Temp(1);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![
                MirStatement::Assign {
                    dest: t0,
                    value: MirValue::ConstructAggregate {
                        ty: array_ty.clone(),
                        fields: vec![],
                    },
                    span: None,
                },
                MirStatement::Assign {
                    dest: t1,
                    value: MirValue::GetField {
                        base: t0,
                        field: FieldId::Index(1),
                    },
                    span: None,
                },
            ],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let program = create_test_program("test_array", vec![block]);
        let output = format!("{program}");

        assert!(output.contains("[i32; 3]"));
        assert!(output.contains("t0.1"));
    }

    #[test]
    fn test_nested_aggregates() {
        // Test: Tuple containing a struct
        let struct_id = StructId::new(3);
        let nested_ty = RueType::Tuple(vec![RueType::Struct(struct_id), RueType::I32]);

        let t0 = Temp(0);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: t0,
                value: MirValue::ConstructAggregate {
                    ty: nested_ty,
                    fields: vec![],
                },
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let program = create_test_program("test_nested", vec![block]);
        let output = format!("{program}");

        // Check for tuple containing struct
        assert!(output.contains("(struct#3, i32)"));
    }

    #[test]
    fn test_set_field() {
        // Test: t2 = SetField t0.field = t1
        let t0 = Temp(0);
        let t1 = Temp(1);
        let t2 = Temp(2);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: t2,
                value: MirValue::SetField {
                    base: t0,
                    field: FieldId::Named("value".to_string()),
                    value: t1,
                },
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let program = create_test_program("test_set_field", vec![block]);
        let output = format!("{program}");

        assert!(output.contains("SetField"));
        assert!(output.contains("t0.value"));
    }

    #[test]
    fn test_nested_field_access() {
        // Test: p.sub_point.x - accessing field of nested aggregate
        // This demonstrates accessing a field of a struct that is itself a field

        let t0 = Temp(0); // outer struct (assumed to have a sub_point field)
        let t1 = Temp(1); // inner struct (result of first GetField)
        let t2 = Temp(2); // final value (result of second GetField)

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![(t0, RueType::Struct(StructId::new(11)))], // t0 is passed as param
            statements: vec![
                // t1 = t0.sub_point (get inner struct)
                MirStatement::Assign {
                    dest: t1,
                    value: MirValue::GetField {
                        base: t0,
                        field: FieldId::Named("sub_point".to_string()),
                    },
                    span: None,
                },
                // t2 = t1.x (get field from inner struct)
                MirStatement::Assign {
                    dest: t2,
                    value: MirValue::GetField {
                        base: t1,
                        field: FieldId::Named("x".to_string()),
                    },
                    span: None,
                },
            ],
            terminator: MirTerminator::Return {
                value: Some(t2),
                span: None,
            },
        };

        let program = create_test_program("test_nested_access", vec![block]);
        let output = format!("{program}");

        // Should show chained field access
        assert!(output.contains("t0.sub_point"));
        assert!(output.contains("t1.x"));
    }

    #[test]
    fn test_zero_field_structs() {
        // Test: Empty struct and unit tuple
        let empty_struct = StructId::new(20);
        let unit_tuple = RueType::Tuple(vec![]);

        let t0 = Temp(0);
        let t1 = Temp(1);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![
                // Construct empty struct
                MirStatement::Assign {
                    dest: t0,
                    value: MirValue::ConstructAggregate {
                        ty: RueType::Struct(empty_struct),
                        fields: vec![],
                    },
                    span: None,
                },
                // Construct unit tuple
                MirStatement::Assign {
                    dest: t1,
                    value: MirValue::ConstructAggregate {
                        ty: unit_tuple,
                        fields: vec![],
                    },
                    span: None,
                },
            ],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let program = create_test_program("test_zero_fields", vec![block]);
        let output = format!("{program}");

        // Should handle empty aggregates without panic
        assert!(output.contains("struct#20 {}"));
        assert!(output.contains("() {}"));
    }

    #[test]
    fn test_array_constant() {
        // Test: const [i32; 3] { 1, 2, 3 }
        let array_const = MirConst::aggregate(
            RueType::Array(Box::new(RueType::I32), 3),
            vec![MirConst::Int32(1), MirConst::Int32(2), MirConst::Int32(3)],
        );

        let t0 = Temp(0);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: t0,
                value: MirValue::Const(array_const),
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: Some(t0),
                span: None,
            },
        };

        let program = create_test_program("test_array_const", vec![block]);
        let output = format!("{program}");

        // Should show array constant
        assert!(output.contains("const [i32; 3]"));
        assert!(output.contains("1_i32") && output.contains("2_i32") && output.contains("3_i32"));
    }

    #[test]
    fn test_struct_update_consumption() {
        // Test: StructUpdate consumes base (important for MVS)
        // After StructUpdate, base should not be usable
        let struct_id = StructId::new(30);
        let t0 = Temp(0); // base struct
        let t1 = Temp(1); // new value
        let t2 = Temp(2); // result of StructUpdate

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![
                // t2 = StructUpdate t0 { field: t1 }
                // This CONSUMES t0 - it's moved into t2
                MirStatement::Assign {
                    dest: t2,
                    value: MirValue::StructUpdate {
                        base: t0,
                        updates: vec![(FieldId::Named("field".to_string()), t1)],
                        struct_type: struct_id,
                    },
                    span: None,
                },
                // NOTE: Using t0 after this point would be invalid in MVS
                // This is a semantic test - the MIR represents the consumption
            ],
            terminator: MirTerminator::Return {
                value: Some(t2),
                span: None,
            },
        };

        let program = create_test_program("test_update_consumes", vec![block]);
        let output = format!("{program}");

        // The output shows the consumption pattern
        assert!(output.contains("StructUpdate"));
        assert!(output.contains("..t0"));
        // Comment documents that t0 is consumed
    }

    #[test]
    fn test_pretty_printer_determinism() {
        // Test: Pretty-printer produces stable output
        let ty = RueType::Tuple(vec![RueType::I32, RueType::Bool]);
        let t0 = Temp(0);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: t0,
                value: MirValue::ConstructAggregate {
                    ty: ty.clone(),
                    fields: vec![],
                },
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let program = create_test_program("test_determinism", vec![block]);

        // Generate output multiple times
        let output1 = format!("{program}");
        let output2 = format!("{program}");
        let output3 = format!("{program}");

        // All outputs should be identical
        assert_eq!(output1, output2);
        assert_eq!(output2, output3);
    }
}
