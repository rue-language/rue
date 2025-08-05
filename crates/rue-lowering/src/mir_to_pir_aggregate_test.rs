//! Tests for MIR to PIR lowering of aggregate types

#[cfg(test)]
mod tests {
    use super::super::*;
    use rue_ir::mir::{
        BasicBlock, BlockId, MirConst, MirFunction, MirProgram, MirStatement, MirTerminator,
        MirValue, Temp,
    };
    use rue_ir::pir::PIR;
    use rue_ir::types::{FieldId, RueType, StructId, TypeContext};

    /// Helper to create a test MIR function
    fn create_test_function(
        name: &str,
        blocks: Vec<BasicBlock>,
        temp_types: HashMap<Temp, RueType>,
    ) -> MirFunction {
        MirFunction {
            name: name.to_string(),
            params: vec![],
            return_type: RueType::Unit,
            blocks,
            entry_block: BlockId(0),
            temp_types,
            span: rue_lexer::Span::dummy(),
        }
    }

    #[test]
    fn test_struct_field_lowering_with_type_context() {
        // Create type context and define a struct
        let mut type_ctx = TypeContext::new();
        let point_id = type_ctx.define_struct(
            "Point",
            vec![
                ("x".to_string(), RueType::I64),
                ("y".to_string(), RueType::I64),
            ],
        );

        // Create MIR that constructs a struct and accesses fields
        let t0 = Temp(0); // x value
        let t1 = Temp(1); // y value
        let t2 = Temp(2); // constructed point
        let t3 = Temp(3); // extracted x

        let mut temp_types = HashMap::new();
        temp_types.insert(t0, RueType::I64);
        temp_types.insert(t1, RueType::I64);
        temp_types.insert(t2, RueType::Struct(point_id));
        temp_types.insert(t3, RueType::I64);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![
                MirStatement::Assign {
                    dest: t0,
                    value: MirValue::Const(MirConst::Int64(10)),
                    span: None,
                },
                MirStatement::Assign {
                    dest: t1,
                    value: MirValue::Const(MirConst::Int64(20)),
                    span: None,
                },
                MirStatement::Assign {
                    dest: t2,
                    value: MirValue::ConstructAggregate {
                        ty: RueType::Struct(point_id),
                        fields: vec![t0, t1],
                    },
                    span: None,
                },
                MirStatement::Assign {
                    dest: t3,
                    value: MirValue::GetField {
                        base: t2,
                        field: FieldId::Named("x".to_string()),
                    },
                    span: None,
                },
            ],
            terminator: MirTerminator::Return {
                value: Some(t3),
                span: None,
            },
        };

        let func = create_test_function("test_struct", vec![block], temp_types);
        let program = MirProgram {
            functions: vec![func],
            function_signatures: HashMap::new(),
            type_context: type_ctx.clone(),
        };

        // Lower to PIR with type context
        let mut lowerer = MirToPir::with_type_context(type_ctx);
        let pir = lowerer.lower_program(&program);

        // Verify the lowering produced expected instructions
        let allocate_found = pir
            .iter()
            .any(|inst| matches!(inst, PIR::AllocateAggregate { .. }));
        assert!(allocate_found, "Should generate AllocateAggregate");

        let store_fields_found = pir
            .iter()
            .filter(|inst| matches!(inst, PIR::StoreField { .. }))
            .count();
        assert_eq!(
            store_fields_found, 2,
            "Should generate 2 StoreField instructions for Point fields"
        );

        let load_field_found = pir.iter().any(|inst| {
            matches!(inst, PIR::LoadField { offset: 0, .. }) // x is at offset 0
        });
        assert!(
            load_field_found,
            "Should generate LoadField for x at offset 0"
        );
    }

    #[test]
    fn test_struct_with_mixed_types() {
        let mut type_ctx = TypeContext::new();
        let mixed_id = type_ctx.define_struct(
            "Mixed",
            vec![
                ("small".to_string(), RueType::I32),
                ("big".to_string(), RueType::I64),
                ("flag".to_string(), RueType::Bool),
            ],
        );

        let t0 = Temp(0); // constructed struct
        let t1 = Temp(1); // extracted i64 field

        let mut temp_types = HashMap::new();
        temp_types.insert(t0, RueType::Struct(mixed_id));
        temp_types.insert(t1, RueType::I64);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![
                MirStatement::Assign {
                    dest: t0,
                    value: MirValue::ConstructAggregate {
                        ty: RueType::Struct(mixed_id),
                        fields: vec![],
                    },
                    span: None,
                },
                MirStatement::Assign {
                    dest: t1,
                    value: MirValue::GetField {
                        base: t0,
                        field: FieldId::Named("big".to_string()),
                    },
                    span: None,
                },
            ],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let func = create_test_function("test_mixed", vec![block], temp_types);
        let program = MirProgram {
            functions: vec![func],
            function_signatures: HashMap::new(),
            type_context: type_ctx.clone(),
        };

        let mut lowerer = MirToPir::with_type_context(type_ctx);
        let pir = lowerer.lower_program(&program);

        // Verify field access uses correct offset (8 bytes for "big" field after i32+padding)
        let load_field_found = pir.iter().any(|inst| {
            matches!(inst, PIR::LoadField { offset: 8, field_type, .. } if *field_type == RueType::I64)
        });
        assert!(
            load_field_found,
            "Should generate LoadField for 'big' field at offset 8"
        );
    }

    #[test]
    fn test_struct_field_by_index() {
        let mut type_ctx = TypeContext::new();
        let vec3_id = type_ctx.define_struct(
            "Vec3",
            vec![
                ("x".to_string(), RueType::I32),
                ("y".to_string(), RueType::I32),
                ("z".to_string(), RueType::I32),
            ],
        );

        let t0 = Temp(0);
        let t1 = Temp(1);

        let mut temp_types = HashMap::new();
        temp_types.insert(t0, RueType::Struct(vec3_id));
        temp_types.insert(t1, RueType::I32);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: t1,
                value: MirValue::GetField {
                    base: t0,
                    field: FieldId::Index(2), // Access z by index
                },
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let func = create_test_function("test_index", vec![block], temp_types);
        let program = MirProgram {
            functions: vec![func],
            function_signatures: HashMap::new(),
            type_context: type_ctx.clone(),
        };

        let mut lowerer = MirToPir::with_type_context(type_ctx);
        let pir = lowerer.lower_program(&program);

        // Verify field access by index works (z is at offset 8)
        let load_field_found = pir
            .iter()
            .any(|inst| matches!(inst, PIR::LoadField { offset: 8, .. }));
        assert!(
            load_field_found,
            "Should generate LoadField for field at index 2"
        );
    }

    #[test]
    fn test_struct_update() {
        let mut type_ctx = TypeContext::new();
        let point_id = type_ctx.define_struct(
            "Point",
            vec![
                ("x".to_string(), RueType::I64),
                ("y".to_string(), RueType::I64),
            ],
        );

        let t0 = Temp(0); // original point
        let t1 = Temp(1); // new x value
        let t2 = Temp(2); // updated point

        let mut temp_types = HashMap::new();
        temp_types.insert(t0, RueType::Struct(point_id));
        temp_types.insert(t1, RueType::I64);
        temp_types.insert(t2, RueType::Struct(point_id));

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: t2,
                value: MirValue::StructUpdate {
                    base: t0,
                    updates: vec![(FieldId::Named("x".to_string()), t1)],
                    struct_type: point_id,
                },
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let func = create_test_function("test_update", vec![block], temp_types);
        let program = MirProgram {
            functions: vec![func],
            function_signatures: HashMap::new(),
            type_context: type_ctx.clone(),
        };

        let mut lowerer = MirToPir::with_type_context(type_ctx);
        let pir = lowerer.lower_program(&program);

        // Should allocate, copy, and update the field
        let allocate_found = pir
            .iter()
            .any(|inst| matches!(inst, PIR::AllocateAggregate { .. }));
        assert!(allocate_found, "Should allocate for struct update");

        let copy_found = pir.iter().any(|inst| {
            matches!(
                inst,
                PIR::CopyAggregate { .. } | PIR::InlineCopyAggregate { .. }
            )
        });
        assert!(copy_found, "Should copy original struct");

        let store_field_found = pir.iter().any(|inst| {
            matches!(inst, PIR::StoreField { offset: 0, .. }) // x at offset 0
        });
        assert!(store_field_found, "Should store updated field");
    }

    #[test]
    fn test_unknown_struct_fallback() {
        // Test that unknown structs fall back gracefully
        let fake_id = StructId::new(999);
        let t0 = Temp(0);
        let t1 = Temp(1);

        let mut temp_types = HashMap::new();
        temp_types.insert(t0, RueType::Struct(fake_id));
        temp_types.insert(t1, RueType::I64);

        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            statements: vec![MirStatement::Assign {
                dest: t1,
                value: MirValue::GetField {
                    base: t0,
                    field: FieldId::Named("unknown".to_string()),
                },
                span: None,
            }],
            terminator: MirTerminator::Return {
                value: None,
                span: None,
            },
        };

        let func = create_test_function("test_unknown", vec![block], temp_types);
        let program = MirProgram {
            functions: vec![func],
            function_signatures: HashMap::new(),
            type_context: TypeContext::new(),
        };

        // Should not panic, but use fallback behavior
        let mut lowerer = MirToPir::new(); // No type context
        let pir = lowerer.lower_program(&program);

        // Should still generate some LoadField instruction (with fallback offset)
        let load_field_found = pir.iter().any(|inst| matches!(inst, PIR::LoadField { .. }));
        assert!(
            load_field_found,
            "Should generate LoadField even for unknown struct"
        );
    }
}
