//! Proptest strategies for generating x86-64 MIR instructions.
//!
//! These generators produce random instruction sequences for fuzzing
//! the emitter, register allocator, and liveness analysis.

use proptest::prelude::*;
use proptest::strategy::BoxedStrategy;
use rue_codegen::x86_64::{LabelId, Operand, Reg, VReg, X86Inst, X86Mir};

/// Generate a random physical register.
pub fn arb_reg() -> BoxedStrategy<Reg> {
    prop_oneof![
        Just(Reg::Rax),
        Just(Reg::Rcx),
        Just(Reg::Rdx),
        Just(Reg::Rbx),
        // Skip Rsp and Rbp - they're special
        Just(Reg::Rsi),
        Just(Reg::Rdi),
        Just(Reg::R8),
        Just(Reg::R9),
        Just(Reg::R10),
        Just(Reg::R11),
        Just(Reg::R12),
        Just(Reg::R13),
        Just(Reg::R14),
        Just(Reg::R15),
    ]
    .boxed()
}

/// Generate a physical operand (for post-regalloc instructions).
pub fn arb_physical_operand() -> BoxedStrategy<Operand> {
    arb_reg().prop_map(Operand::Physical).boxed()
}

/// Generate a virtual operand (for pre-regalloc instructions).
pub fn arb_virtual_operand(max_vreg: u32) -> BoxedStrategy<Operand> {
    (0..max_vreg.max(1))
        .prop_map(|idx| Operand::Virtual(VReg::new(idx)))
        .boxed()
}

/// Generate an operand that can be either virtual or physical.
pub fn arb_operand(max_vreg: u32) -> BoxedStrategy<Operand> {
    prop_oneof![arb_physical_operand(), arb_virtual_operand(max_vreg),].boxed()
}

/// Generate a 32-bit immediate value.
pub fn arb_imm32() -> impl Strategy<Value = i32> {
    prop_oneof![
        // Common small values
        (-128i32..=127).boxed(),
        // Boundary values
        Just(i32::MIN).boxed(),
        Just(i32::MAX).boxed(),
        Just(0).boxed(),
        Just(-1).boxed(),
        // Any i32
        any::<i32>().boxed(),
    ]
}

/// Generate a 64-bit immediate value.
pub fn arb_imm64() -> impl Strategy<Value = i64> {
    prop_oneof![
        // Common small values
        (-128i64..=127).boxed(),
        // Boundary values
        Just(i64::MIN).boxed(),
        Just(i64::MAX).boxed(),
        Just(0).boxed(),
        Just(-1).boxed(),
        // 32-bit range
        any::<i32>().prop_map(|x| x as i64).boxed(),
        // Full 64-bit
        any::<i64>().boxed(),
    ]
}

/// Generate a shift amount (0-63 for 64-bit, 0-31 for 32-bit).
pub fn arb_shift_amount() -> impl Strategy<Value = u8> {
    prop_oneof![
        Just(0u8),
        Just(1u8),
        Just(7u8),
        Just(8u8),
        Just(31u8),
        Just(32u8),
        Just(63u8),
        (0u8..=63),
    ]
}

/// Generate a stack offset (typically negative for locals, positive for args).
pub fn arb_stack_offset() -> impl Strategy<Value = i32> {
    prop_oneof![
        // Typical local offsets
        (-256i32..0).prop_map(|x| x * 8),
        // Typical argument offsets
        (0i32..16).prop_map(|x| 16 + x * 8),
        // Zero
        Just(0i32),
        // Any aligned offset
        any::<i16>().prop_map(|x| (x as i32) * 8),
    ]
}

/// Generate a label ID.
pub fn arb_label_id(max_labels: u32) -> impl Strategy<Value = LabelId> {
    (0..max_labels.max(1)).prop_map(LabelId::new)
}

/// Generate a single x86-64 instruction with physical registers.
///
/// This is for fuzzing the emitter which expects allocated registers.
pub fn arb_x86_inst_physical() -> BoxedStrategy<X86Inst> {
    // Use a helper macro to avoid repeating arb_physical_operand() calls
    prop_oneof![
        // Move instructions
        (arb_physical_operand(), arb_imm32()).prop_map(|(dst, imm)| X86Inst::MovRI32 { dst, imm }),
        (arb_physical_operand(), arb_imm64()).prop_map(|(dst, imm)| X86Inst::MovRI64 { dst, imm }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::MovRR { dst, src }),
        (arb_physical_operand(), arb_reg(), arb_stack_offset())
            .prop_map(|(dst, base, offset)| X86Inst::MovRM { dst, base, offset }),
        (arb_reg(), arb_stack_offset(), arb_physical_operand())
            .prop_map(|(base, offset, src)| X86Inst::MovMR { base, offset, src }),
        // Arithmetic
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::AddRR { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::AddRR64 { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::SubRR { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::SubRR64 { dst, src }),
        (arb_physical_operand(), arb_imm32()).prop_map(|(dst, imm)| X86Inst::AddRI { dst, imm }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::ImulRR { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::ImulRR64 { dst, src }),
        arb_physical_operand().prop_map(|dst| X86Inst::Neg { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Neg64 { dst }),
        // Bitwise
        (arb_physical_operand(), arb_imm32()).prop_map(|(dst, imm)| X86Inst::XorRI { dst, imm }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::AndRR { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::OrRR { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::XorRR { dst, src }),
        arb_physical_operand().prop_map(|dst| X86Inst::NotR { dst }),
        // Shifts
        arb_physical_operand().prop_map(|dst| X86Inst::ShlRCl { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Shl32RCl { dst }),
        (arb_physical_operand(), arb_shift_amount())
            .prop_map(|(dst, imm)| X86Inst::ShlRI { dst, imm }),
        (arb_physical_operand(), arb_shift_amount())
            .prop_map(|(dst, imm)| X86Inst::Shl32RI { dst, imm }),
        arb_physical_operand().prop_map(|dst| X86Inst::ShrRCl { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Shr32RCl { dst }),
        (arb_physical_operand(), arb_shift_amount())
            .prop_map(|(dst, imm)| X86Inst::ShrRI { dst, imm }),
        (arb_physical_operand(), arb_shift_amount())
            .prop_map(|(dst, imm)| X86Inst::Shr32RI { dst, imm }),
        arb_physical_operand().prop_map(|dst| X86Inst::SarRCl { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Sar32RCl { dst }),
        (arb_physical_operand(), arb_shift_amount())
            .prop_map(|(dst, imm)| X86Inst::SarRI { dst, imm }),
        (arb_physical_operand(), arb_shift_amount())
            .prop_map(|(dst, imm)| X86Inst::Sar32RI { dst, imm }),
        // Division
        Just(X86Inst::Cdq),
        arb_physical_operand().prop_map(|src| X86Inst::IdivR { src }),
        // Comparison
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(src1, src2)| X86Inst::CmpRR { src1, src2 }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(src1, src2)| X86Inst::Cmp64RR { src1, src2 }),
        (arb_physical_operand(), arb_imm32()).prop_map(|(src, imm)| X86Inst::CmpRI { src, imm }),
        (arb_physical_operand(), arb_imm32()).prop_map(|(src, imm)| X86Inst::Cmp64RI { src, imm }),
        // Set instructions
        arb_physical_operand().prop_map(|dst| X86Inst::Sete { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Setne { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Setl { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Setg { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Setle { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Setge { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Setb { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Seta { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Setbe { dst }),
        arb_physical_operand().prop_map(|dst| X86Inst::Setae { dst }),
        // Move with extension
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::Movzx { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::Movsx8To64 { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::Movsx16To64 { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::Movsx32To64 { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::Movzx8To64 { dst, src }),
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(dst, src)| X86Inst::Movzx16To64 { dst, src }),
        // Test
        (arb_physical_operand(), arb_physical_operand())
            .prop_map(|(src1, src2)| X86Inst::TestRR { src1, src2 }),
        // Stack operations
        arb_physical_operand().prop_map(|dst| X86Inst::Pop { dst }),
        arb_physical_operand().prop_map(|src| X86Inst::Push { src }),
        // Control flow (no labels for single instruction tests)
        Just(X86Inst::Syscall),
        Just(X86Inst::Ret),
    ]
    .boxed()
}

/// Generate a sequence of x86-64 instructions with labels and jumps.
///
/// This creates valid sequences where jumps target existing labels.
pub fn arb_x86_inst_sequence(
    inst_count: usize,
    num_labels: usize,
) -> impl Strategy<Value = Vec<X86Inst>> {
    // First, decide where labels go
    let label_positions = prop::collection::vec(0..inst_count.max(1), num_labels);

    label_positions.prop_flat_map(move |positions| {
        // Generate base instructions
        let base_insts = prop::collection::vec(arb_x86_inst_physical(), inst_count);

        base_insts.prop_map(move |mut insts| {
            // Insert labels at the chosen positions
            let mut labels_inserted = 0;
            let mut label_ids: Vec<LabelId> = Vec::new();

            for &pos in &positions {
                if pos < insts.len() {
                    let label = LabelId::new(labels_inserted);
                    label_ids.push(label);
                    insts.insert(pos + labels_inserted as usize, X86Inst::Label { id: label });
                    labels_inserted += 1;
                }
            }

            // Now add some jumps that target valid labels
            if !label_ids.is_empty() {
                // Add a jump near the end
                let target = label_ids[0];
                insts.push(X86Inst::Jmp { label: target });
            }

            insts
        })
    })
}

/// Generate an X86Mir with valid instruction sequences.
pub fn arb_x86_mir(inst_count: usize, num_labels: usize) -> impl Strategy<Value = X86Mir> {
    arb_x86_inst_sequence(inst_count, num_labels).prop_map(|insts| {
        let mut mir = X86Mir::new();
        for inst in insts {
            mir.push(inst);
        }
        mir
    })
}

/// Generate an X86Mir with virtual registers for regalloc testing.
pub fn arb_x86_mir_with_vregs(inst_count: usize, num_vregs: u32) -> BoxedStrategy<X86Mir> {
    prop::collection::vec(
        prop_oneof![
            // Simple register-to-register operations that regalloc handles
            (arb_operand(num_vregs), arb_imm32())
                .prop_map(|(dst, imm)| X86Inst::MovRI32 { dst, imm }),
            (arb_operand(num_vregs), arb_operand(num_vregs))
                .prop_map(|(dst, src)| X86Inst::MovRR { dst, src }),
            (arb_operand(num_vregs), arb_operand(num_vregs))
                .prop_map(|(dst, src)| X86Inst::AddRR { dst, src }),
            (arb_operand(num_vregs), arb_operand(num_vregs))
                .prop_map(|(dst, src)| X86Inst::SubRR { dst, src }),
            arb_operand(num_vregs).prop_map(|dst| X86Inst::Neg { dst }),
        ],
        inst_count,
    )
    .prop_map(move |insts| {
        let mut mir = X86Mir::new();
        // Allocate the vregs
        for _ in 0..num_vregs {
            mir.alloc_vreg();
        }
        for inst in insts {
            mir.push(inst);
        }
        mir
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    #[test]
    fn test_arb_reg_generates_valid_regs() {
        let mut runner = TestRunner::default();
        for _ in 0..20 {
            let reg = arb_reg().new_tree(&mut runner).unwrap().current();
            // Just verify it's a valid register
            assert!(reg.encoding() <= 15);
        }
    }

    #[test]
    fn test_arb_x86_inst_physical_generates_valid_insts() {
        let mut runner = TestRunner::default();
        for _ in 0..50 {
            let inst = arb_x86_inst_physical()
                .new_tree(&mut runner)
                .unwrap()
                .current();
            // Just verify it can be displayed (exercises Display impl)
            let _ = format!("{}", inst);
        }
    }

    #[test]
    fn test_arb_x86_mir_generates_valid_mir() {
        let mut runner = TestRunner::default();
        for _ in 0..10 {
            let mir = arb_x86_mir(10, 2).new_tree(&mut runner).unwrap().current();
            // Verify it has the expected structure
            assert!(mir.instructions().len() >= 10);
        }
    }
}
