//! Proptest strategies for generating machine MIR instructions.
//!
//! These generators produce random instruction sequences for fuzzing the
//! emitter (single instructions and whole-function sequences).

use proptest::prelude::*;
use proptest::strategy::BoxedStrategy;
use rue_codegen::aarch64::{
    Aarch64Inst, Aarch64Mir, Cond as Aarch64Cond, Operand as Aarch64Operand, Reg as Aarch64Reg,
};
use rue_codegen::x86_64::{LabelId, Operand, Reg, X86Inst, X86Mir};

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
            .prop_map(|(dst, src)| X86Inst::Movzx32To64 { dst, src }),
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

/// Generate a random physical AArch64 register suitable for ordinary ALU ops.
pub fn arb_aarch64_reg() -> BoxedStrategy<Aarch64Reg> {
    prop_oneof![
        Just(Aarch64Reg::X0),
        Just(Aarch64Reg::X1),
        Just(Aarch64Reg::X2),
        Just(Aarch64Reg::X3),
        Just(Aarch64Reg::X4),
        Just(Aarch64Reg::X5),
        Just(Aarch64Reg::X6),
        Just(Aarch64Reg::X7),
        Just(Aarch64Reg::X8),
        Just(Aarch64Reg::X9),
        Just(Aarch64Reg::X10),
        Just(Aarch64Reg::X11),
        Just(Aarch64Reg::X12),
        Just(Aarch64Reg::X13),
        Just(Aarch64Reg::X14),
        Just(Aarch64Reg::X15),
        Just(Aarch64Reg::X16),
        Just(Aarch64Reg::X17),
        Just(Aarch64Reg::X19),
        Just(Aarch64Reg::X20),
        Just(Aarch64Reg::X21),
        Just(Aarch64Reg::X22),
        Just(Aarch64Reg::X23),
        Just(Aarch64Reg::X24),
        Just(Aarch64Reg::X25),
        Just(Aarch64Reg::X26),
        Just(Aarch64Reg::X27),
        Just(Aarch64Reg::X28),
    ]
    .boxed()
}

/// Generate a physical AArch64 operand.
pub fn arb_aarch64_physical_operand() -> BoxedStrategy<Aarch64Operand> {
    arb_aarch64_reg().prop_map(Aarch64Operand::Physical).boxed()
}

/// Generate an AArch64 immediate accepted by ADD/SUB immediate encoding.
pub fn arb_aarch64_add_sub_imm() -> impl Strategy<Value = i32> {
    prop_oneof![
        Just(0i32),
        0i32..=4095,
        (1i32..=4095).prop_map(|n| n << 12),
        0i32..(1 << 20),
    ]
}

/// Generate an AArch64 memory offset that avoids intentional large-offset asserts.
pub fn arb_aarch64_mem_offset() -> impl Strategy<Value = i32> {
    prop_oneof![
        (-256i32..=255),
        (0i32..4096).prop_map(|n| n * 8),
        Just(0i32),
    ]
}

/// Generate a random AArch64 condition code.
pub fn arb_aarch64_cond() -> BoxedStrategy<Aarch64Cond> {
    prop_oneof![
        Just(Aarch64Cond::Eq),
        Just(Aarch64Cond::Ne),
        Just(Aarch64Cond::Lt),
        Just(Aarch64Cond::Gt),
        Just(Aarch64Cond::Le),
        Just(Aarch64Cond::Ge),
        Just(Aarch64Cond::Hi),
        Just(Aarch64Cond::Ls),
        Just(Aarch64Cond::Hs),
        Just(Aarch64Cond::Lo),
    ]
    .boxed()
}

/// Generate a single AArch64 instruction with physical registers.
pub fn arb_aarch64_inst_physical() -> BoxedStrategy<Aarch64Inst> {
    prop_oneof![
        (arb_aarch64_physical_operand(), arb_imm64())
            .prop_map(|(dst, imm)| Aarch64Inst::MovImm { dst, imm }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(dst, src)| Aarch64Inst::MovRR { dst, src }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_reg(),
            arb_aarch64_mem_offset(),
        )
            .prop_map(|(dst, base, offset)| Aarch64Inst::Ldr { dst, base, offset }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_reg(),
            arb_aarch64_mem_offset(),
        )
            .prop_map(|(src, base, offset)| Aarch64Inst::Str { src, base, offset }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            0u8..=63,
        )
            .prop_map(|(dst, src, imm)| Aarch64Inst::LslImm { dst, src, imm }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            0u8..=31,
        )
            .prop_map(|(dst, src, imm)| Aarch64Inst::Lsl32Imm { dst, src, imm }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            0u8..=31,
        )
            .prop_map(|(dst, src, imm)| Aarch64Inst::Lsr32Imm { dst, src, imm }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            0u8..=31,
        )
            .prop_map(|(dst, src, imm)| Aarch64Inst::Asr32Imm { dst, src, imm }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(dst, src1, src2)| Aarch64Inst::AddRR { dst, src1, src2 }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(dst, src1, src2)| Aarch64Inst::SubRR { dst, src1, src2 }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            arb_aarch64_add_sub_imm(),
        )
            .prop_map(|(dst, src, imm)| Aarch64Inst::AddImm { dst, src, imm }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            arb_aarch64_add_sub_imm(),
        )
            .prop_map(|(dst, src, imm)| Aarch64Inst::SubImm { dst, src, imm }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(dst, src1, src2)| Aarch64Inst::MulRR { dst, src1, src2 }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(dst, src1, src2)| Aarch64Inst::AndRR { dst, src1, src2 }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(dst, src1, src2)| Aarch64Inst::OrrRR { dst, src1, src2 }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(dst, src1, src2)| Aarch64Inst::EorRR { dst, src1, src2 }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(dst, src)| Aarch64Inst::MvnRR { dst, src }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(dst, src)| Aarch64Inst::Uxtw { dst, src }),
        (
            arb_aarch64_physical_operand(),
            arb_aarch64_physical_operand(),
        )
            .prop_map(|(src1, src2)| Aarch64Inst::CmpRR { src1, src2 }),
        (arb_aarch64_physical_operand(), arb_aarch64_cond())
            .prop_map(|(dst, cond)| Aarch64Inst::Cset { dst, cond }),
        Just(Aarch64Inst::Ret),
        Just(Aarch64Inst::Brk),
    ]
    .boxed()
}

/// Generate an Aarch64Mir with physical-register instruction sequences.
pub fn arb_aarch64_mir(inst_count: usize) -> impl Strategy<Value = Aarch64Mir> {
    prop::collection::vec(arb_aarch64_inst_physical(), inst_count).prop_map(|insts| {
        let mut mir = Aarch64Mir::new();
        for inst in insts {
            mir.push(inst);
        }
        mir
    })
}

/// One entry in a generated AArch64 branch sequence: either an ordinary filler
/// instruction or a branch of a specific form targeting a label (by index).
#[derive(Clone, Debug)]
enum Aarch64SeqEntry {
    Filler(Aarch64Inst),
    B(usize),
    BCond(Aarch64Cond, usize),
    Bvs(usize),
    Bvc(usize),
    Cbz(Aarch64Reg, usize),
    Cbnz(Aarch64Reg, usize),
}

impl Aarch64SeqEntry {
    fn into_inst(self, labels: &[LabelId]) -> Aarch64Inst {
        let pick = |t: usize| labels[t % labels.len()];
        match self {
            Aarch64SeqEntry::Filler(inst) => inst,
            Aarch64SeqEntry::B(t) => Aarch64Inst::B { label: pick(t) },
            Aarch64SeqEntry::BCond(cond, t) => Aarch64Inst::BCond {
                cond,
                label: pick(t),
            },
            Aarch64SeqEntry::Bvs(t) => Aarch64Inst::Bvs { label: pick(t) },
            Aarch64SeqEntry::Bvc(t) => Aarch64Inst::Bvc { label: pick(t) },
            Aarch64SeqEntry::Cbz(rt, t) => Aarch64Inst::Cbz {
                src: Aarch64Operand::Physical(rt),
                label: pick(t),
            },
            Aarch64SeqEntry::Cbnz(rt, t) => Aarch64Inst::Cbnz {
                src: Aarch64Operand::Physical(rt),
                label: pick(t),
            },
        }
    }
}

fn arb_aarch64_seq_entry(num_labels: usize) -> BoxedStrategy<Aarch64SeqEntry> {
    let n = num_labels.max(1);
    prop_oneof![
        3 => arb_aarch64_inst_physical().prop_map(Aarch64SeqEntry::Filler),
        1 => (0..n).prop_map(Aarch64SeqEntry::B),
        1 => (arb_aarch64_cond(), 0..n).prop_map(|(c, t)| Aarch64SeqEntry::BCond(c, t)),
        1 => (0..n).prop_map(Aarch64SeqEntry::Bvs),
        1 => (0..n).prop_map(Aarch64SeqEntry::Bvc),
        1 => (arb_aarch64_reg(), 0..n).prop_map(|(r, t)| Aarch64SeqEntry::Cbz(r, t)),
        1 => (arb_aarch64_reg(), 0..n).prop_map(|(r, t)| Aarch64SeqEntry::Cbnz(r, t)),
    ]
    .boxed()
}

/// Generate an `Aarch64Mir` of branch/label control flow: `num_labels` labels
/// each defined exactly once, interleaved with forward and backward branches of
/// every form (B, B.cond, B.vs, B.vc, CBZ, CBNZ). Because every label is defined
/// exactly once, all branch targets resolve and the sequence emits without
/// undefined-label errors; a branch placed before its label's definition is a
/// forward edge, one placed after is a backward edge.
pub fn arb_aarch64_branch_mir(
    inst_count: usize,
    num_labels: usize,
) -> impl Strategy<Value = Aarch64Mir> {
    let num_labels = num_labels.clamp(1, 16);
    let entries = prop::collection::vec(arb_aarch64_seq_entry(num_labels), inst_count);
    let label_positions = prop::collection::vec(0..inst_count.max(1), num_labels);
    (entries, label_positions).prop_map(move |(entries, label_positions)| {
        let labels: Vec<LabelId> = (0..num_labels as u32).map(LabelId::new).collect();
        let mut insts: Vec<Aarch64Inst> =
            entries.into_iter().map(|e| e.into_inst(&labels)).collect();
        // Define each label exactly once at its chosen position. Adding `i`
        // accounts for the labels already inserted ahead of this one.
        for (i, &pos) in label_positions.iter().enumerate() {
            let at = (pos + i).min(insts.len());
            insts.insert(at, Aarch64Inst::Label { id: labels[i] });
        }
        insts.push(Aarch64Inst::Ret);
        let mut mir = Aarch64Mir::new();
        for inst in insts {
            mir.push(inst);
        }
        mir
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use rue_codegen::aarch64::{Aarch64Inst as AInst, Emitter as Aarch64Emitter};
    use std::collections::HashSet;

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

    #[test]
    fn test_arb_aarch64_inst_physical_generates_valid_insts() {
        let mut runner = TestRunner::default();
        for _ in 0..50 {
            let inst = arb_aarch64_inst_physical()
                .new_tree(&mut runner)
                .unwrap()
                .current();
            let _ = format!("{}", inst);
        }
    }

    #[test]
    fn test_arb_aarch64_mir_generates_valid_mir() {
        let mut runner = TestRunner::default();
        for _ in 0..10 {
            let mir = arb_aarch64_mir(10).new_tree(&mut runner).unwrap().current();
            assert_eq!(mir.instructions().len(), 10);
        }
    }

    #[test]
    fn test_arb_aarch64_branch_mir_defines_each_label_once_and_emits() {
        let mut runner = TestRunner::default();
        for _ in 0..25 {
            let mir = arb_aarch64_branch_mir(20, 3)
                .new_tree(&mut runner)
                .unwrap()
                .current();

            // Every label is defined exactly once.
            let mut seen = HashSet::new();
            for inst in mir.instructions() {
                if let AInst::Label { id } = inst {
                    assert!(
                        seen.insert(id.index()),
                        "label {} defined more than once",
                        id.index()
                    );
                }
            }

            // Emission must not panic (a graceful ICE Err is acceptable).
            let emitter = Aarch64Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
            let _ = emitter.emit();
        }
    }
}
