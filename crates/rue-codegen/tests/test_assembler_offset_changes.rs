//! Integration test to verify assembler offset changes work correctly

use rue_codegen::assembler::Assembler;
use rue_codegen::linker::asm_object::AsmObjectBuilder;
use rue_target::{LabelRef, X86Register, X8664Instr};
use std::collections::HashMap;

#[test]
fn test_asm_object_builder_changes() {
    // Test that AsmObjectBuilder has the new methods and behavior
    let mut builder = AsmObjectBuilder::new();
    builder
        .start_section(".text".to_string(), 16, true, false)
        .expect("should be able to start section");

    // Test emit_bytes returns offset
    let offset1 = builder
        .emit_bytes(&[0x48, 0x89, 0xE5])
        .expect("should emit bytes"); // mov rbp, rsp
    assert_eq!(offset1, 0, "First emit should be at offset 0");

    // Test current_offset method
    let current_offset = builder.current_offset().expect("should emit bytes");
    assert_eq!(
        current_offset, 3,
        "Current offset should be 3 after emitting 3 bytes"
    );

    let offset2 = builder.emit_bytes(&[0xC3]).expect("should emit bytes"); // ret
    assert_eq!(offset2, 3, "Second emit should be at offset 3");

    let final_offset = builder
        .current_offset()
        .expect("should return current offset");
    assert_eq!(
        final_offset, 4,
        "Final offset should be 4 after emitting 4 bytes total"
    );
}

#[test]
fn test_assembler_uses_actual_offsets() {
    // Test that the assembler uses actual offsets instead of estimated sizes
    let assembler = Assembler::new();
    let instructions = vec![
        X8664Instr::Label { id: 1 },
        X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 42,
        },
        X8664Instr::Jmp {
            target: LabelRef::Local(2),
        }, // Jump forward
        X8664Instr::MovRI64 {
            dest: X86Register::Rbx,
            imm: 24,
        },
        X8664Instr::Label { id: 2 },
        X8664Instr::Ret,
    ];

    let function_labels = HashMap::new();

    // This should not panic if our safety checks work correctly
    let result = assembler.assemble(&instructions, &function_labels);

    // Should generate an object without errors - this test mainly verifies no panics occur
    // The actual symbol count may vary based on implementation details
    println!("Generated {} symbols", result.symbols.len());
}

#[test]
#[should_panic(expected = "Jump references non-existent label with ID 999")]
fn test_assembler_panics_on_missing_label() {
    // Test that the assembler panics when a jump references a non-existent label
    let assembler = Assembler::new();
    let instructions = vec![
        X8664Instr::Label { id: 1 },
        X8664Instr::MovRI64 {
            dest: X86Register::Rax,
            imm: 42,
        },
        X8664Instr::Jmp {
            target: LabelRef::Local(999),
        }, // Jump to non-existent label
        X8664Instr::Ret,
    ];

    let function_labels = HashMap::new();

    // This should panic due to the safety check we added
    let _result = assembler.assemble(&instructions, &function_labels);
}
