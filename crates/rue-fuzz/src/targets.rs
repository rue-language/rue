//! Fuzz targets for the Rue compiler.
//!
//! Each target exercises a different phase of the compiler:
//! - Lexer: tokenization
//! - Parser: AST construction
//! - Sema: semantic analysis (type checking, name resolution)
//! - Compiler: full compilation pipeline (frontend only, no codegen)
//! - Emitter: x86-64 instruction encoding
//! - Regalloc: register allocation stress testing

use super::FuzzTarget;
use rue_codegen::x86_64::{Emitter, Operand, Reg, X86Inst, X86Mir};

/// Fuzz target for the lexer.
///
/// Goal: The lexer should never panic, always produce tokens or an error.
pub struct LexerTarget;

impl FuzzTarget for LexerTarget {
    fn name(&self) -> &'static str {
        "lexer"
    }

    fn fuzz(&self, input: &[u8]) {
        // Only test valid UTF-8, since the lexer expects valid source text
        if let Ok(source) = std::str::from_utf8(input) {
            let lexer = rue_lexer::Lexer::new(source);
            // The lexer should handle all input without panicking
            let _ = lexer.tokenize();
        }
    }
}

/// Fuzz target for the parser.
///
/// Goal: The parser should never panic, always produce an AST or an error.
pub struct ParserTarget;

impl FuzzTarget for ParserTarget {
    fn name(&self) -> &'static str {
        "parser"
    }

    fn fuzz(&self, input: &[u8]) {
        // Only test valid UTF-8
        if let Ok(source) = std::str::from_utf8(input) {
            let lexer = rue_lexer::Lexer::new(source);
            if let Ok((tokens, interner)) = lexer.tokenize() {
                let parser = rue_parser::Parser::new(tokens, interner);
                // The parser should handle all tokenized input without panicking
                let _ = parser.parse();
            }
        }
    }
}

/// Fuzz target for semantic analysis specifically.
///
/// Goal: Sema should never panic on any valid or invalid input.
/// This target focuses on type checking, name resolution, and type inference.
///
/// Key assumptions that sema makes (and we want to fuzz):
/// - InstRefs point to valid instructions
/// - Extra data indices are in bounds
/// - Type IDs are valid
/// - Symbol references exist in the interner
///
/// Currently uses source-level fuzzing through compile_frontend.
/// Future enhancement: structured RIR generation with Arbitrary trait.
pub struct SemaTarget;

impl FuzzTarget for SemaTarget {
    fn name(&self) -> &'static str {
        "sema"
    }

    fn fuzz(&self, input: &[u8]) {
        // Only test valid UTF-8
        if let Ok(source) = std::str::from_utf8(input) {
            // compile_frontend runs through sema (semantic analysis)
            // without code generation. This tests:
            // - Type inference (Hindley-Milner with Algorithm W)
            // - Affine type checking (partial moves, linearity)
            // - Name resolution
            // - Multi-error collection
            let _ = rue_compiler::compile_frontend(source);
        }
    }
}

/// Fuzz target for the full frontend compilation pipeline.
///
/// Goal: Frontend compilation should never panic, always succeed or return errors.
pub struct CompilerTarget;

impl FuzzTarget for CompilerTarget {
    fn name(&self) -> &'static str {
        "compiler"
    }

    fn fuzz(&self, input: &[u8]) {
        // Only test valid UTF-8
        if let Ok(source) = std::str::from_utf8(input) {
            // Use compile_frontend to avoid code generation (which is slower)
            // and focus on the analysis phases where bugs are more likely
            let _ = rue_compiler::compile_frontend(source);
        }
    }
}

/// Fuzz target for the x86-64 instruction emitter.
///
/// Goal: The emitter should never panic on any sequence of valid instructions.
/// This tests instruction encoding for edge cases and unusual register combinations.
pub struct EmitterTarget;

impl FuzzTarget for EmitterTarget {
    fn name(&self) -> &'static str {
        "emitter"
    }

    fn fuzz(&self, input: &[u8]) {
        // Interpret the input as a seed for deterministic instruction generation
        if input.is_empty() {
            return;
        }

        let mut mir = X86Mir::new();
        let mut idx = 0;

        // Generate instructions based on input bytes
        while idx < input.len() {
            let opcode = input[idx] % 30; // ~30 instruction types
            idx += 1;

            // Get register indices from input
            let reg1_idx = input.get(idx).copied().unwrap_or(0) % 14;
            idx += 1;
            let reg2_idx = input.get(idx).copied().unwrap_or(0) % 14;
            idx += 1;

            let reg1 = reg_from_index(reg1_idx);
            let reg2 = reg_from_index(reg2_idx);
            let op1 = Operand::Physical(reg1);
            let op2 = Operand::Physical(reg2);

            // Get immediate from next bytes
            let imm32 = if idx + 4 <= input.len() {
                let bytes = [input[idx], input[idx + 1], input[idx + 2], input[idx + 3]];
                idx += 4;
                i32::from_le_bytes(bytes)
            } else {
                0
            };

            let inst = match opcode {
                0 => X86Inst::MovRI32 {
                    dst: op1,
                    imm: imm32,
                },
                1 => X86Inst::MovRR { dst: op1, src: op2 },
                2 => X86Inst::AddRR { dst: op1, src: op2 },
                3 => X86Inst::AddRR64 { dst: op1, src: op2 },
                4 => X86Inst::SubRR { dst: op1, src: op2 },
                5 => X86Inst::SubRR64 { dst: op1, src: op2 },
                6 => X86Inst::AddRI {
                    dst: op1,
                    imm: imm32,
                },
                7 => X86Inst::ImulRR { dst: op1, src: op2 },
                8 => X86Inst::Neg { dst: op1 },
                9 => X86Inst::XorRI {
                    dst: op1,
                    imm: imm32,
                },
                10 => X86Inst::AndRR { dst: op1, src: op2 },
                11 => X86Inst::OrRR { dst: op1, src: op2 },
                12 => X86Inst::XorRR { dst: op1, src: op2 },
                13 => X86Inst::NotR { dst: op1 },
                14 => X86Inst::ShlRI {
                    dst: op1,
                    imm: (imm32 as u8) % 64,
                },
                15 => X86Inst::ShrRI {
                    dst: op1,
                    imm: (imm32 as u8) % 64,
                },
                16 => X86Inst::SarRI {
                    dst: op1,
                    imm: (imm32 as u8) % 64,
                },
                17 => X86Inst::CmpRR {
                    src1: op1,
                    src2: op2,
                },
                18 => X86Inst::CmpRI {
                    src: op1,
                    imm: imm32,
                },
                19 => X86Inst::Sete { dst: op1 },
                20 => X86Inst::Setne { dst: op1 },
                21 => X86Inst::Setl { dst: op1 },
                22 => X86Inst::Setg { dst: op1 },
                23 => X86Inst::Movzx { dst: op1, src: op2 },
                24 => X86Inst::TestRR {
                    src1: op1,
                    src2: op2,
                },
                25 => X86Inst::Push { src: op1 },
                26 => X86Inst::Pop { dst: op1 },
                27 => X86Inst::Cdq,
                28 => X86Inst::Syscall,
                29 => X86Inst::Ret,
                _ => X86Inst::MovRI32 { dst: op1, imm: 0 },
            };

            mir.push(inst);
        }

        // Try to emit the instructions - should not panic
        let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]);
        let _ = emitter.emit();
    }
}

/// Convert a byte index to a register (skipping RSP and RBP).
fn reg_from_index(idx: u8) -> Reg {
    match idx % 14 {
        0 => Reg::Rax,
        1 => Reg::Rcx,
        2 => Reg::Rdx,
        3 => Reg::Rbx,
        // Skip Rsp (4) and Rbp (5)
        4 => Reg::Rsi,
        5 => Reg::Rdi,
        6 => Reg::R8,
        7 => Reg::R9,
        8 => Reg::R10,
        9 => Reg::R11,
        10 => Reg::R12,
        11 => Reg::R13,
        12 => Reg::R14,
        13 => Reg::R15,
        _ => Reg::Rax,
    }
}

/// Fuzz target for x86-64 instruction sequences with labels and jumps.
///
/// Goal: Verify that label resolution and jump encoding never panics.
pub struct EmitterSequenceTarget;

impl FuzzTarget for EmitterSequenceTarget {
    fn name(&self) -> &'static str {
        "emitter_sequence"
    }

    fn fuzz(&self, input: &[u8]) {
        if input.len() < 2 {
            return;
        }

        let mut mir = X86Mir::new();
        let num_labels = (input[0] % 8) as u32 + 1; // 1-8 labels
        let mut idx = 1;

        // First pass: allocate labels
        let labels: Vec<_> = (0..num_labels).map(|_| mir.alloc_label()).collect();

        // Generate instructions with jumps to labels
        while idx < input.len() {
            let opcode = input[idx] % 40;
            idx += 1;

            let reg1_idx = input.get(idx).copied().unwrap_or(0) % 14;
            idx += 1;

            let op1 = Operand::Physical(reg_from_index(reg1_idx));
            let label_idx = input.get(idx).copied().unwrap_or(0) as usize % labels.len();
            idx += 1;

            let inst = match opcode {
                // Regular instructions
                0..=19 => {
                    let reg2_idx = input.get(idx).copied().unwrap_or(0) % 14;
                    idx += 1;
                    let op2 = Operand::Physical(reg_from_index(reg2_idx));
                    match opcode {
                        0 => X86Inst::MovRR { dst: op1, src: op2 },
                        1 => X86Inst::AddRR { dst: op1, src: op2 },
                        2 => X86Inst::SubRR { dst: op1, src: op2 },
                        3 => X86Inst::CmpRR {
                            src1: op1,
                            src2: op2,
                        },
                        4 => X86Inst::XorRR { dst: op1, src: op2 },
                        _ => X86Inst::MovRI32 {
                            dst: op1,
                            imm: opcode as i32,
                        },
                    }
                }
                // Labels
                20..=24 => X86Inst::Label {
                    id: labels[label_idx],
                },
                // Conditional jumps
                25 => X86Inst::Jz {
                    label: labels[label_idx],
                },
                26 => X86Inst::Jnz {
                    label: labels[label_idx],
                },
                27 => X86Inst::Jo {
                    label: labels[label_idx],
                },
                28 => X86Inst::Jb {
                    label: labels[label_idx],
                },
                29 => X86Inst::Jae {
                    label: labels[label_idx],
                },
                30 => X86Inst::Jbe {
                    label: labels[label_idx],
                },
                31 => X86Inst::Jge {
                    label: labels[label_idx],
                },
                32 => X86Inst::Jle {
                    label: labels[label_idx],
                },
                // Unconditional jump
                33 => X86Inst::Jmp {
                    label: labels[label_idx],
                },
                // Other instructions
                _ => X86Inst::Ret,
            };

            mir.push(inst);
        }

        // Ensure all labels are defined by adding them at the end if missing
        for label in &labels {
            mir.push(X86Inst::Label { id: *label });
        }
        mir.push(X86Inst::Ret);

        // Try to emit - should handle any valid label/jump combination
        let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]);
        let _ = emitter.emit();
    }
}

/// Get all available fuzz targets.
pub fn all_targets() -> Vec<Box<dyn FuzzTarget>> {
    vec![
        Box::new(LexerTarget),
        Box::new(ParserTarget),
        Box::new(SemaTarget),
        Box::new(CompilerTarget),
        Box::new(EmitterTarget),
        Box::new(EmitterSequenceTarget),
    ]
}

/// Get a fuzz target by name.
pub fn get_target(name: &str) -> Option<Box<dyn FuzzTarget>> {
    match name {
        "lexer" => Some(Box::new(LexerTarget)),
        "parser" => Some(Box::new(ParserTarget)),
        "sema" => Some(Box::new(SemaTarget)),
        "compiler" => Some(Box::new(CompilerTarget)),
        "emitter" => Some(Box::new(EmitterTarget)),
        "emitter_sequence" => Some(Box::new(EmitterSequenceTarget)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexer_target_valid() {
        let target = LexerTarget;
        target.fuzz(b"fn main() -> i32 { 42 }");
    }

    #[test]
    fn test_lexer_target_invalid_utf8() {
        let target = LexerTarget;
        // Invalid UTF-8 should be silently ignored
        target.fuzz(&[0xff, 0xfe, 0x00, 0x01]);
    }

    #[test]
    fn test_lexer_target_garbage() {
        let target = LexerTarget;
        target.fuzz(b"@#$%^&*()!~`");
    }

    #[test]
    fn test_parser_target_valid() {
        let target = ParserTarget;
        target.fuzz(b"fn main() -> i32 { 42 }");
    }

    #[test]
    fn test_parser_target_invalid_syntax() {
        let target = ParserTarget;
        target.fuzz(b"fn fn fn { { { } } }");
    }

    #[test]
    fn test_compiler_target_valid() {
        let target = CompilerTarget;
        target.fuzz(b"fn main() -> i32 { 42 }");
    }

    #[test]
    fn test_compiler_target_type_error() {
        let target = CompilerTarget;
        target.fuzz(b"fn main() -> i32 { true }");
    }

    #[test]
    fn test_sema_target_valid() {
        let target = SemaTarget;
        target.fuzz(b"fn main() -> i32 { 42 }");
    }

    #[test]
    fn test_sema_target_type_error() {
        let target = SemaTarget;
        // Type mismatch: returning bool where i32 expected
        target.fuzz(b"fn main() -> i32 { true }");
    }

    #[test]
    fn test_sema_target_undefined_variable() {
        let target = SemaTarget;
        target.fuzz(b"fn main() -> i32 { x }");
    }

    #[test]
    fn test_sema_target_complex_types() {
        let target = SemaTarget;
        // Test with structs and type inference
        target.fuzz(
            b"struct Point { x: i32, y: i32 } fn main() -> i32 { let p = Point { x: 1, y: 2 }; p.x }",
        );
    }

    #[test]
    fn test_emitter_target_valid() {
        let target = EmitterTarget;
        // Simple sequence that generates a few mov instructions
        target.fuzz(&[0, 1, 2, 0, 0, 0, 0, 1, 3, 4]);
    }

    #[test]
    fn test_emitter_target_empty() {
        let target = EmitterTarget;
        // Empty input should not panic
        target.fuzz(&[]);
    }

    #[test]
    fn test_emitter_sequence_target_valid() {
        let target = EmitterSequenceTarget;
        // Sequence with labels and jumps
        target.fuzz(&[2, 20, 1, 0, 25, 2, 0, 0, 1, 2]);
    }

    #[test]
    fn test_all_targets() {
        let targets = all_targets();
        assert_eq!(targets.len(), 6);
    }

    #[test]
    fn test_get_target() {
        assert!(get_target("lexer").is_some());
        assert!(get_target("parser").is_some());
        assert!(get_target("sema").is_some());
        assert!(get_target("compiler").is_some());
        assert!(get_target("emitter").is_some());
        assert!(get_target("emitter_sequence").is_some());
        assert!(get_target("invalid").is_none());
    }
}

/// Proptest-based fuzz tests using structured input generation.
///
/// These tests generate syntactically valid Rue programs and verify
/// that the compiler never panics.
#[cfg(test)]
mod proptest_tests {
    use super::*;
    use crate::generators;
    use proptest::prelude::*;

    proptest! {
        /// The lexer should never panic on any valid expression.
        #[test]
        fn lexer_never_panics_on_expr(expr in generators::arb_expr(3)) {
            let target = LexerTarget;
            target.fuzz(expr.as_bytes());
        }

        /// The lexer should never panic on any generated program.
        #[test]
        fn lexer_never_panics_on_program(program in generators::arb_program(2)) {
            let target = LexerTarget;
            target.fuzz(program.as_bytes());
        }

        /// The parser should never panic on any generated program.
        #[test]
        fn parser_never_panics_on_program(program in generators::arb_program(2)) {
            let target = ParserTarget;
            target.fuzz(program.as_bytes());
        }

        /// The parser should never panic on any generated expression.
        #[test]
        fn parser_never_panics_on_expr(expr in generators::arb_expr(3)) {
            let target = ParserTarget;
            // Wrap expression in a valid function to make it parseable
            let program = format!("fn main() -> i32 {{ {} }}", expr);
            target.fuzz(program.as_bytes());
        }

        /// The full compiler frontend should never panic on valid programs.
        #[test]
        fn compiler_never_panics_on_program(program in generators::arb_program(2)) {
            let target = CompilerTarget;
            target.fuzz(program.as_bytes());
        }

        /// The full compiler frontend should never panic on possibly invalid programs.
        /// This tests error handling in semantic analysis.
        #[test]
        fn compiler_never_panics_on_maybe_invalid(
            program in generators::arb_maybe_invalid_program(2)
        ) {
            let target = CompilerTarget;
            target.fuzz(program.as_bytes());
        }

        /// The lexer should handle arbitrary strings without panicking.
        #[test]
        fn lexer_handles_arbitrary_strings(s in ".*") {
            let target = LexerTarget;
            target.fuzz(s.as_bytes());
        }

        /// The parser should handle arbitrary strings without panicking.
        #[test]
        fn parser_handles_arbitrary_strings(s in ".*") {
            let target = ParserTarget;
            target.fuzz(s.as_bytes());
        }

        /// The compiler should handle arbitrary strings without panicking.
        #[test]
        fn compiler_handles_arbitrary_strings(s in ".*") {
            let target = CompilerTarget;
            target.fuzz(s.as_bytes());
        }

        /// Sema should never panic on valid programs.
        #[test]
        fn sema_never_panics_on_program(program in generators::arb_program(2)) {
            let target = SemaTarget;
            target.fuzz(program.as_bytes());
        }

        /// Sema should never panic on possibly invalid programs.
        /// This tests error handling in type inference and name resolution.
        #[test]
        fn sema_never_panics_on_maybe_invalid(
            program in generators::arb_maybe_invalid_program(2)
        ) {
            let target = SemaTarget;
            target.fuzz(program.as_bytes());
        }

        /// Sema should handle arbitrary strings without panicking.
        #[test]
        fn sema_handles_arbitrary_strings(s in ".*") {
            let target = SemaTarget;
            target.fuzz(s.as_bytes());
        }

        /// The emitter should handle arbitrary byte sequences without panicking.
        #[test]
        fn emitter_handles_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            let target = EmitterTarget;
            target.fuzz(&bytes);
        }

        /// The emitter sequence target should handle arbitrary bytes without panicking.
        #[test]
        fn emitter_sequence_handles_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 2..256)) {
            let target = EmitterSequenceTarget;
            target.fuzz(&bytes);
        }
    }
}

/// Proptest-based fuzz tests for codegen using structured instruction generation.
#[cfg(test)]
mod codegen_proptest_tests {
    use crate::codegen_generators;
    use proptest::prelude::*;
    use rue_codegen::x86_64::{Emitter, Operand, Reg, X86Inst, X86Mir};

    proptest! {
        /// The emitter should never panic on any valid instruction.
        #[test]
        fn emitter_never_panics_on_instruction(inst in codegen_generators::arb_x86_inst_physical()) {
            let mut mir = X86Mir::new();
            mir.push(inst);
            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]);
            let _ = emitter.emit();
        }

        /// The emitter should never panic on any valid MIR.
        #[test]
        fn emitter_never_panics_on_mir(mir in codegen_generators::arb_x86_mir(20, 3)) {
            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]);
            let _ = emitter.emit();
        }

        /// The emitter should handle various register combinations.
        #[test]
        fn emitter_handles_register_combos(
            reg1 in codegen_generators::arb_reg(),
            reg2 in codegen_generators::arb_reg(),
            imm in codegen_generators::arb_imm32()
        ) {
            let mut mir = X86Mir::new();
            let op1 = Operand::Physical(reg1);
            let op2 = Operand::Physical(reg2);

            // Test various instructions with the register combo
            mir.push(X86Inst::MovRR { dst: op1, src: op2 });
            mir.push(X86Inst::AddRR { dst: op1, src: op2 });
            mir.push(X86Inst::MovRI32 { dst: op1, imm });
            mir.push(X86Inst::CmpRR { src1: op1, src2: op2 });

            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]);
            let _ = emitter.emit();
        }

        /// The emitter should handle extreme immediate values.
        #[test]
        fn emitter_handles_extreme_immediates(imm64 in codegen_generators::arb_imm64()) {
            let mut mir = X86Mir::new();
            let dst = Operand::Physical(Reg::Rax);

            mir.push(X86Inst::MovRI64 { dst, imm: imm64 });

            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]);
            let _ = emitter.emit();
        }

        /// The emitter should handle various shift amounts.
        #[test]
        fn emitter_handles_shifts(
            reg in codegen_generators::arb_reg(),
            shift in codegen_generators::arb_shift_amount()
        ) {
            let mut mir = X86Mir::new();
            let dst = Operand::Physical(reg);

            mir.push(X86Inst::ShlRI { dst, imm: shift % 64 });
            mir.push(X86Inst::ShrRI { dst, imm: shift % 64 });
            mir.push(X86Inst::SarRI { dst, imm: shift % 64 });
            mir.push(X86Inst::Shl32RI { dst, imm: shift % 32 });
            mir.push(X86Inst::Shr32RI { dst, imm: shift % 32 });
            mir.push(X86Inst::Sar32RI { dst, imm: shift % 32 });

            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]);
            let _ = emitter.emit();
        }
    }
}
