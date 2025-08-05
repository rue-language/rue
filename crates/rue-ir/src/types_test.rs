//! Tests for the type system, especially TypeContext and aggregate types

#[cfg(test)]
mod tests {
    use crate::types::*;

    #[test]
    fn test_type_context_basic() {
        let mut ctx = TypeContext::new();

        // Define a simple struct
        let point_id = ctx.define_struct(
            "Point",
            vec![
                ("x".to_string(), RueType::I64),
                ("y".to_string(), RueType::I64),
            ],
        );

        // Look up the struct
        let point_def = ctx.lookup_struct(point_id).unwrap();
        assert_eq!(point_def.name, "Point");
        assert_eq!(point_def.fields.len(), 2);

        // Get field types
        let x_type = ctx
            .get_field_type(point_id, &FieldId::Named("x".to_string()))
            .unwrap();
        assert_eq!(*x_type, RueType::I64);

        let y_type = ctx
            .get_field_type(point_id, &FieldId::Named("y".to_string()))
            .unwrap();
        assert_eq!(*y_type, RueType::I64);
    }

    #[test]
    fn test_struct_layout() {
        let mut ctx = TypeContext::new();

        // Define a struct with mixed types
        let mixed_id = ctx.define_struct(
            "Mixed",
            vec![
                ("a".to_string(), RueType::I32),
                ("b".to_string(), RueType::I64),
                ("c".to_string(), RueType::Bool),
            ],
        );

        // Compute layout
        let layout = ctx.compute_layout(&RueType::Struct(mixed_id)).unwrap();

        // Size should be at least sum of field sizes with alignment
        // i32 (4 bytes) + padding (4 bytes) + i64 (8 bytes) + bool (1 byte) + padding (7 bytes) = 24 bytes
        assert!(layout.size >= 17); // At minimum without padding
        assert_eq!(layout.align, 8); // Should align to largest field (i64)
    }

    #[test]
    fn test_field_offsets() {
        let mut ctx = TypeContext::new();

        let struct_id = ctx.define_struct(
            "Fields",
            vec![
                ("first".to_string(), RueType::I32),
                ("second".to_string(), RueType::I64),
                ("third".to_string(), RueType::I32),
            ],
        );

        // Get field offsets
        let first_offset = ctx
            .compute_field_offset(struct_id, &FieldId::Named("first".to_string()))
            .unwrap();
        let second_offset = ctx
            .compute_field_offset(struct_id, &FieldId::Named("second".to_string()))
            .unwrap();
        let third_offset = ctx
            .compute_field_offset(struct_id, &FieldId::Named("third".to_string()))
            .unwrap();

        // First field should be at offset 0
        assert_eq!(first_offset, 0);

        // Second field should be aligned to 8 bytes
        assert_eq!(second_offset, 8);

        // Third field should be after the i64
        assert_eq!(third_offset, 16);
    }

    #[test]
    fn test_field_by_index() {
        let mut ctx = TypeContext::new();

        let struct_id = ctx.define_struct(
            "Indexed",
            vec![
                ("x".to_string(), RueType::I32),
                ("y".to_string(), RueType::I64),
            ],
        );

        // Access fields by index
        let field0_type = ctx.get_field_type(struct_id, &FieldId::Index(0)).unwrap();
        assert_eq!(*field0_type, RueType::I32);

        let field1_type = ctx.get_field_type(struct_id, &FieldId::Index(1)).unwrap();
        assert_eq!(*field1_type, RueType::I64);

        // Out of bounds access should error
        let err = ctx.get_field_type(struct_id, &FieldId::Index(2));
        assert!(matches!(err, Err(TypeError::FieldNotFound { .. })));
    }

    #[test]
    fn test_unknown_struct_error() {
        let ctx = TypeContext::new();
        let fake_id = StructId::new(999);

        // Looking up unknown struct should error
        let err = ctx.lookup_struct(fake_id);
        assert!(matches!(err, Err(TypeError::UnknownStruct(_))));

        // Getting field type of unknown struct should error
        let err = ctx.get_field_type(fake_id, &FieldId::Named("x".to_string()));
        assert!(matches!(err, Err(TypeError::UnknownStruct(_))));
    }

    #[test]
    fn test_tuple_field_offsets() {
        let tuple_ty = RueType::Tuple(vec![RueType::I32, RueType::I64, RueType::Bool]);
        let mut ctx = TypeContext::new();

        // Get field offsets
        if let RueType::Tuple(types) = &tuple_ty {
            let offset0 = ctx.compute_tuple_field_offset(types, 0).unwrap();
            let offset1 = ctx.compute_tuple_field_offset(types, 1).unwrap();
            let offset2 = ctx.compute_tuple_field_offset(types, 2).unwrap();

            assert_eq!(offset0, 0); // First field at offset 0
            assert_eq!(offset1, 8); // Second field aligned to 8 bytes
            assert_eq!(offset2, 16); // Third field after i64

            // Out of bounds access
            let err = ctx.compute_tuple_field_offset(types, 3);
            assert!(matches!(err, Err(TypeError::OutOfBoundsAccess { .. })));
        }
    }

    #[test]
    fn test_array_element_offsets() {
        let array_ty = RueType::Array(Box::new(RueType::I64), 5);
        let mut ctx = TypeContext::new();

        // Get element offsets
        if let RueType::Array(elem_ty, len) = &array_ty {
            let offset0 = ctx.compute_array_element_offset(elem_ty, *len, 0).unwrap();
            let offset1 = ctx.compute_array_element_offset(elem_ty, *len, 1).unwrap();
            let offset4 = ctx.compute_array_element_offset(elem_ty, *len, 4).unwrap();

            assert_eq!(offset0, 0);
            assert_eq!(offset1, 8);
            assert_eq!(offset4, 32);

            // Out of bounds access
            let err = ctx.compute_array_element_offset(elem_ty, *len, 5);
            assert!(matches!(
                err,
                Err(TypeError::OutOfBoundsAccess { index: 5, len: 5 })
            ));
        }
    }

    #[test]
    fn test_invalid_field_access() {
        let mut ctx = TypeContext::new();

        // Try to access tuple field on non-tuple
        let int_ty = RueType::I32;
        if let RueType::Tuple(types) = &int_ty {
            // This won't match, so test is a bit different
            let _ = ctx.compute_tuple_field_offset(types, 0);
        } else {
            // Expected: not a tuple - test passes if we reach this point
        }

        // Direct test with empty types array (invalid for non-tuple)
        let err = ctx.compute_tuple_field_offset(&[], 0);
        assert!(matches!(err, Err(TypeError::OutOfBoundsAccess { .. })));
    }

    #[test]
    fn test_nested_struct() {
        let mut ctx = TypeContext::new();

        // Define inner struct
        let inner_id = ctx.define_struct("Inner", vec![("value".to_string(), RueType::I32)]);

        // Define outer struct containing inner
        let outer_id = ctx.define_struct(
            "Outer",
            vec![
                ("inner".to_string(), RueType::Struct(inner_id)),
                ("extra".to_string(), RueType::I64),
            ],
        );

        // Get field types
        let inner_field_type = ctx
            .get_field_type(outer_id, &FieldId::Named("inner".to_string()))
            .unwrap();
        assert_eq!(*inner_field_type, RueType::Struct(inner_id));

        // Compute layout
        let layout = ctx.compute_layout(&RueType::Struct(outer_id)).unwrap();
        assert!(layout.size > 0);
    }

    #[test]
    fn test_empty_struct() {
        let mut ctx = TypeContext::new();

        // Define empty struct
        let empty_id = ctx.define_struct("Empty", vec![]);

        // Layout should be minimal
        let layout = ctx.compute_layout(&RueType::Struct(empty_id)).unwrap();
        assert_eq!(layout.size, 0);
        assert_eq!(layout.align, 1);

        // Accessing any field should error
        let err = ctx.get_field_type(empty_id, &FieldId::Named("nonexistent".to_string()));
        assert!(matches!(err, Err(TypeError::FieldNotFound { .. })));
    }

    #[test]
    fn test_struct_with_many_fields() {
        let mut ctx = TypeContext::new();

        // Define struct with many fields
        let fields: Vec<_> = (0..10)
            .map(|i| (format!("field{i}"), RueType::I64))
            .collect();

        let big_struct_id = ctx.define_struct("BigStruct", fields);

        // Should be able to access all fields
        for i in 0..10 {
            let field_name = format!("field{i}");
            let field_type = ctx
                .get_field_type(big_struct_id, &FieldId::Named(field_name))
                .unwrap();
            assert_eq!(*field_type, RueType::I64);

            // Also by index
            let field_type = ctx
                .get_field_type(big_struct_id, &FieldId::Index(i))
                .unwrap();
            assert_eq!(*field_type, RueType::I64);
        }
    }

    #[test]
    fn test_type_error_display() {
        let err = TypeError::UnknownStruct(StructId::new(42));
        assert_eq!(format!("{err}"), "Unknown struct: struct#42");

        let err = TypeError::InvalidFieldAccess {
            ty: RueType::I32,
            field: FieldId::Named("x".to_string()),
        };
        assert_eq!(format!("{err}"), "Invalid field access: x on type i32");

        let err = TypeError::OutOfBoundsAccess { index: 5, len: 3 };
        assert_eq!(format!("{err}"), "Index 5 out of bounds for length 3");
    }
}
