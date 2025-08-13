use crate::types::{FieldId, RueType, TypeContext};

#[test]
fn debug_mixed_type_struct_offsets() {
    let mut type_ctx = TypeContext::new();

    // Define the Record struct with mixed types: id: i64, active: bool, score: i32
    let record_id = type_ctx.define_struct(
        "Record",
        vec![
            ("id".to_string(), RueType::I64),
            ("active".to_string(), RueType::Bool),
            ("score".to_string(), RueType::I32),
        ],
    );

    // Compute field offsets
    let id_offset = type_ctx
        .compute_field_offset(record_id, &FieldId::Named("id".to_string()))
        .unwrap();
    let active_offset = type_ctx
        .compute_field_offset(record_id, &FieldId::Named("active".to_string()))
        .unwrap();
    let score_offset = type_ctx
        .compute_field_offset(record_id, &FieldId::Named("score".to_string()))
        .unwrap();

    // Compute struct layout
    let layout = type_ctx
        .compute_layout(&RueType::Struct(record_id))
        .unwrap();

    // Verify individual type layouts
    let i64_layout = type_ctx.compute_layout(&RueType::I64).unwrap();
    let bool_layout = type_ctx.compute_layout(&RueType::Bool).unwrap();
    let i32_layout = type_ctx.compute_layout(&RueType::I32).unwrap();

    // Verify individual type layouts match expectations
    assert_eq!(i64_layout.size, 8, "i64 should be 8 bytes");
    assert_eq!(i64_layout.align, 8, "i64 should be aligned to 8 bytes");
    assert_eq!(bool_layout.size, 1, "bool should be 1 byte");
    assert_eq!(bool_layout.align, 1, "bool should be aligned to 1 byte");
    assert_eq!(i32_layout.size, 4, "i32 should be 4 bytes");
    assert_eq!(i32_layout.align, 4, "i32 should be aligned to 4 bytes");

    assert_eq!(id_offset, 0, "id field should be at offset 0");
    assert_eq!(active_offset, 8, "active field should be at offset 8");
    assert_eq!(
        score_offset, 12,
        "score field should be at offset 12 (with padding)"
    );
    assert_eq!(layout.size, 16, "struct should be 16 bytes total");
    assert_eq!(layout.align, 8, "struct should be aligned to 8 bytes");
}
