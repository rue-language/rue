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

    println!("Struct ID: {record_id}");

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

    println!("Field offsets:");
    println!("  id (i64): {id_offset}");
    println!("  active (bool): {active_offset}");
    println!("  score (i32): {score_offset}");

    // Compute struct layout
    let layout = type_ctx
        .compute_layout(&RueType::Struct(record_id))
        .unwrap();
    println!("Struct layout:");
    println!("  size: {}", layout.size);
    println!("  align: {}", layout.align);

    // Verify individual type layouts
    println!("Individual type layouts:");
    let i64_layout = type_ctx.compute_layout(&RueType::I64).unwrap();
    println!(
        "  i64: size={}, align={}",
        i64_layout.size, i64_layout.align
    );
    let bool_layout = type_ctx.compute_layout(&RueType::Bool).unwrap();
    println!(
        "  bool: size={}, align={}",
        bool_layout.size, bool_layout.align
    );
    let i32_layout = type_ctx.compute_layout(&RueType::I32).unwrap();
    println!(
        "  i32: size={}, align={}",
        i32_layout.size, i32_layout.align
    );

    // Expected offsets:
    // id (i64): offset 0, size 8, align 8
    // active (bool): offset 8, size 1, align 1
    // score (i32): offset 12 (8 + 1 + 3 padding), size 4, align 4
    // Total struct size: 16 (aligned to 8 bytes for i64)

    assert_eq!(id_offset, 0, "id field should be at offset 0");
    assert_eq!(active_offset, 8, "active field should be at offset 8");
    assert_eq!(
        score_offset, 12,
        "score field should be at offset 12 (with padding)"
    );
    assert_eq!(layout.size, 16, "struct should be 16 bytes total");
    assert_eq!(layout.align, 8, "struct should be aligned to 8 bytes");
}
