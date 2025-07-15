use rue_codegen::{BinOp, Instruction, Lowering, RegisterAllocator, VReg, Value, X86Emitter};

#[test]
fn test_rex_optimization_in_generated_code() {
    // Create a simple program that uses only low registers
    let mut regalloc = RegisterAllocator::new();

    // This will use RAX, RCX, RDX (low registers)
    let a = VReg(0);
    let b = VReg(1);
    let c = VReg(2);

    // Allocate registers
    regalloc.allocate(a).unwrap();
    regalloc.allocate(b).unwrap();
    regalloc.allocate(c).unwrap();

    let instructions = vec![
        // mov a, 10
        Instruction::Copy {
            dest: a,
            src: Value::Immediate(10),
        },
        // mov b, 20
        Instruction::Copy {
            dest: b,
            src: Value::Immediate(20),
        },
        // add c, a, b
        Instruction::BinaryOp {
            dest: c,
            lhs: Value::VReg(a),
            rhs: Value::VReg(b),
            op: BinOp::Add,
        },
        // Additional operations to generate more code
        Instruction::BinaryOp {
            dest: c,
            lhs: Value::VReg(c),
            rhs: Value::Immediate(5),
            op: BinOp::Sub,
        },
        Instruction::BinaryOp {
            dest: c,
            lhs: Value::VReg(c),
            rhs: Value::Immediate(2),
            op: BinOp::Mul,
        },
    ];

    // Lower to machine instructions
    let mut lowering = Lowering::new(&mut regalloc, 0);
    let machine_instrs = lowering.lower(&instructions).unwrap();

    // Emit machine code
    let mut x86_emitter = X86Emitter::new();
    let machine_code = x86_emitter.emit_all(&machine_instrs).unwrap();

    // Count occurrences of 0x40 (base REX prefix)
    let rex_40_count = machine_code.windows(1).filter(|&w| w[0] == 0x40).count();

    // Should be 0, since we're using only low registers and the optimization removes 0x40
    assert_eq!(
        rex_40_count, 0,
        "Found {rex_40_count} occurrences of base REX prefix 0x40, expected 0 due to optimization"
    );

    // Print machine code for debugging first
    println!("Generated {} bytes of machine code", machine_code.len());
    println!("Machine code: {machine_code:02x?}");

    // Check that we still have some instructions (not empty)
    assert!(
        machine_code.len() > 20,
        "Machine code too small, likely missing instructions"
    );

    // Check for presence of expected opcodes (movabs uses 0xb8-0xbf for 64-bit immediates)
    let has_mov_opcode = machine_code
        .iter()
        .any(|&b| (0xb8..=0xbf).contains(&b) || b == 0xc7);
    assert!(has_mov_opcode, "Should contain mov immediate opcode");
    assert!(
        machine_code.contains(&0x01) || machine_code.contains(&0x4d),
        "Should contain add opcode"
    );
    // Check for arithmetic immediate opcodes (0x81 for 32-bit or 0x83 for 8-bit immediates)
    assert!(
        machine_code.contains(&0x81) || machine_code.contains(&0x83),
        "Should contain arithmetic immediate opcode"
    );
}
