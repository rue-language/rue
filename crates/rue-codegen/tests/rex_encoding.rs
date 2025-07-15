use rue_codegen::{BinOp, Instruction, Lowering, RegisterAllocator, VReg, Value, X86Emitter};

#[test]
fn test_rex_encoding_mov_imm64() {
    // Test mov r8, imm64 encoding
    let instructions = vec![Instruction::Copy {
        dest: VReg(0),
        src: Value::Immediate(0x1234567890ABCDEF),
    }];

    // Create a simple register allocator that assigns VReg(0) to R8
    let mut regalloc = RegisterAllocator::new();
    regalloc.allocate(VReg(0)).unwrap();

    // Force R8 allocation by filling other registers
    for i in 0..8 {
        regalloc.allocate(VReg(i + 10)).unwrap();
    }

    // Lower to machine instructions
    let mut lowering = Lowering::new(&mut regalloc, 0);
    let machine_instrs = lowering.lower(&instructions).unwrap();

    // Emit machine code
    let mut x86_emitter = X86Emitter::new();
    let machine_code = x86_emitter.emit_all(&machine_instrs).unwrap();

    // Check for proper REX.WB prefix (0x49) followed by mov r8, imm64
    // The machine code should contain the mov instruction somewhere
    println!(
        "Machine code: {:?}",
        machine_code
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
    );

    // Find REX.WB (0x49) followed by 0xb8-0xbf (mov r8-r15, imm64)
    let has_rex_mov = machine_code
        .windows(2)
        .any(|w| w[0] == 0x49 && (w[1] >= 0xb8 && w[1] <= 0xbf));
    assert!(
        has_rex_mov,
        "Expected REX.WB prefix (0x49) followed by mov r8-r15, imm64"
    );
}

#[test]
fn test_rex_encoding_mov_reg_to_reg() {
    // Test mov between extended registers
    let mut regalloc = RegisterAllocator::new();

    // This tests register-to-register move with R8-R15
    // The test verifies REX encoding is properly generated

    // Allocate many vregs to force use of extended registers
    for i in 0..10 {
        regalloc.allocate(VReg(i)).unwrap();
    }

    let instructions = vec![Instruction::Copy {
        dest: VReg(8),
        src: Value::VReg(VReg(9)),
    }];

    // Lower to machine instructions
    let mut lowering = Lowering::new(&mut regalloc, 0);
    let machine_instrs = lowering.lower(&instructions).unwrap();

    // Emit machine code
    let mut x86_emitter = X86Emitter::new();
    let machine_code = x86_emitter.emit_all(&machine_instrs).unwrap();

    // Should have REX prefix for extended registers
    assert!(
        machine_code.contains(&0x4d)
            || machine_code.contains(&0x49)
            || machine_code.contains(&0x4c)
            || machine_code.contains(&0x45)
    );
}

#[test]
fn test_rex_encoding_push_pop() {
    // Test push/pop with extended registers

    // Force allocation to R8
    let mut regalloc = RegisterAllocator::new();
    for i in 0..8 {
        regalloc.allocate(VReg(i)).unwrap();
    }
    regalloc.allocate(VReg(8)).unwrap(); // Should get R8

    let instructions = vec![
        Instruction::Push { src: VReg(8) },
        Instruction::Pop { dest: VReg(8) },
    ];

    // Lower to machine instructions
    let mut lowering = Lowering::new(&mut regalloc, 0);
    let machine_instrs = lowering.lower(&instructions).unwrap();

    // Emit machine code
    let mut x86_emitter = X86Emitter::new();
    let machine_code = x86_emitter.emit_all(&machine_instrs).unwrap();

    // Check for REX.B prefix (0x41) for push/pop r8
    assert!(machine_code.contains(&0x41));
}

#[test]
fn test_rex_encoding_arithmetic() {
    // Test arithmetic operations with extended registers
    let mut regalloc = RegisterAllocator::new();

    // Allocate many vregs to force use of extended registers
    for i in 0..10 {
        regalloc.allocate(VReg(i)).unwrap();
    }

    let instructions = vec![Instruction::BinaryOp {
        dest: VReg(8),
        lhs: Value::VReg(VReg(8)),
        rhs: Value::VReg(VReg(9)),
        op: BinOp::Add,
    }];

    // Lower to machine instructions
    let mut lowering = Lowering::new(&mut regalloc, 0);
    let machine_instrs = lowering.lower(&instructions).unwrap();

    // Emit machine code
    let mut x86_emitter = X86Emitter::new();
    let machine_code = x86_emitter.emit_all(&machine_instrs).unwrap();

    // Should have REX prefix for arithmetic with extended registers
    assert!(machine_code.iter().any(|&b| (b & 0xf0) == 0x40));
}
