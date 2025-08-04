// Quick test to verify the type ID design compiles and works

use rue_ir::types::{RueType, StructId, FieldId, StructDef};

fn main() {
    // Test StructId
    let struct_id1 = StructId::new(1);
    let struct_id2 = StructId::new(2);
    
    println!("StructId display: {}", struct_id1);
    println!("StructId raw: {}", struct_id1.as_u32());
    assert_ne!(struct_id1, struct_id2);
    
    // Test FieldId
    let field1 = FieldId::from_index(0);
    let field2 = FieldId::from_name("x");
    
    println!("Indexed field: {}", field1);
    println!("Named field: {}", field2);
    
    // Test aggregate types
    let int_type = RueType::I32;
    let struct_type = RueType::Struct(struct_id1);
    let tuple_type = RueType::Tuple(vec![RueType::I32, RueType::Bool]);
    let array_type = RueType::Array(Box::new(RueType::I64), 10);
    
    println!("Struct type: {}", struct_type);
    println!("Tuple type: {}", tuple_type);
    println!("Array type: {}", array_type);
    
    // Test helper methods
    assert!(struct_type.is_aggregate());
    assert!(int_type.is_scalar());
    assert_eq!(array_type.array_len(), Some(10));
    assert_eq!(tuple_type.tuple_types().map(|t| t.len()), Some(2));
    
    // Test StructDef stub
    let mut struct_def = StructDef::stub(struct_id1, "Point");
    struct_def.fields.push(("x".to_string(), RueType::I32));
    struct_def.fields.push(("y".to_string(), RueType::I32));
    
    assert_eq!(struct_def.field_type("x"), Some(&RueType::I32));
    assert_eq!(struct_def.field_type_by_index(1), Some(&RueType::I32));
    
    println!("\nAll tests passed!");
}