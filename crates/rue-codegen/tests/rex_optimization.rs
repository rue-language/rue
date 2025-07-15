use rue_codegen::{BinOp, Instruction, Lowering, RegisterAllocator, VReg, Value, X86Emitter};

#[test]
fn test_rex_optimization_skips_base_prefix() {
    // Test that REX prefix 0x40 is omitted for operations using low registers
    let mut regalloc = RegisterAllocator::new();

    // Allocate low registers (RAX-RDI) which don't need REX extension bits
    let v0 = VReg(0);
    let v1 = VReg(1);
    regalloc.allocate(v0).unwrap();
    regalloc.allocate(v1).unwrap();

    let instructions = vec![
        // mov between low registers
        Instruction::Copy {
            dest: v0,
            src: Value::VReg(v1),
        },
        // add with low registers
        Instruction::BinaryOp {
            dest: v0,
            lhs: Value::VReg(v0),
            rhs: Value::VReg(v1),
            op: BinOp::Add,
        },
    ];

    // Lower to machine instructions
    let mut lowering = Lowering::new(&mut regalloc, 0);
    let machine_instrs = lowering.lower(&instructions).unwrap();

    // Emit machine code
    let mut x86_emitter = X86Emitter::new();
    let machine_code = x86_emitter.emit_all(&machine_instrs).unwrap();

    // The machine code should NOT contain the base REX prefix (0x40)
    // since we're only using low registers
    assert!(
        !machine_code.contains(&0x40),
        "Machine code should not contain base REX prefix 0x40 when using only low registers"
    );

    // But it should still contain the mov (0x89) and add (0x01) opcodes
    assert!(
        machine_code.contains(&0x89),
        "Machine code should contain mov opcode"
    );
    assert!(
        machine_code.contains(&0x01),
        "Machine code should contain add opcode"
    );
}

#[test]
fn test_rex_optimization_keeps_extended_prefix() {
    // Test that REX prefixes with extension bits are still emitted
    let mut regalloc = RegisterAllocator::new();

    // Force allocation to extended registers by filling low registers first
    for i in 0..8 {
        regalloc.allocate(VReg(i + 100)).unwrap();
    }

    // These should get R8 and R9
    let v8 = VReg(8);
    let v9 = VReg(9);
    regalloc.allocate(v8).unwrap();
    regalloc.allocate(v9).unwrap();

    let instructions = vec![
        // mov between extended registers
        Instruction::Copy {
            dest: v8,
            src: Value::VReg(v9),
        },
    ];

    // Lower to machine instructions
    let mut lowering = Lowering::new(&mut regalloc, 0);
    let machine_instrs = lowering.lower(&instructions).unwrap();

    // Emit machine code
    let mut x86_emitter = X86Emitter::new();
    let machine_code = x86_emitter.emit_all(&machine_instrs).unwrap();

    // Should have REX prefix with extension bits (not 0x40)
    // REX.RB (0x45) or REX.R (0x44) or REX.B (0x41) for extended registers
    let has_extended_rex = machine_code
        .iter()
        .any(|&b| b == 0x41 || b == 0x44 || b == 0x45 || b == 0x4c || b == 0x4d);
    assert!(
        has_extended_rex,
        "Machine code should contain REX prefix with extension bits for extended registers"
    );
}

#[test]
fn test_rex_optimization_immediate_operations() {
    // Test immediate operations with low registers don't emit base REX
    let mut regalloc = RegisterAllocator::new();

    let v0 = VReg(0);
    regalloc.allocate(v0).unwrap();

    let instructions = vec![
        // add immediate to low register
        Instruction::BinaryOp {
            dest: v0,
            lhs: Value::VReg(v0),
            rhs: Value::Immediate(42),
            op: BinOp::Add,
        },
    ];

    // Lower to machine instructions
    let mut lowering = Lowering::new(&mut regalloc, 0);
    let machine_instrs = lowering.lower(&instructions).unwrap();

    // Emit machine code
    let mut x86_emitter = X86Emitter::new();
    let machine_code = x86_emitter.emit_all(&machine_instrs).unwrap();

    // Should not contain base REX prefix (0x40)
    assert!(
        !machine_code.contains(&0x40),
        "Machine code should not contain base REX prefix 0x40 for immediate operations with low registers"
    );

    // Should contain add immediate opcode (0x81 for 32-bit or 0x83 for 8-bit immediates)
    assert!(
        machine_code.contains(&0x81) || machine_code.contains(&0x83),
        "Machine code should contain add immediate opcode"
    );
}
