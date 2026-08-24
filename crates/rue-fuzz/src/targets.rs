//! Fuzz targets for the Rue compiler.
//!
//! Every registered target maps to a distinct pipeline boundary or concrete
//! configuration endpoint, so no two targets spend their budget re-fuzzing the
//! same endpoint (RUE-776):
//! - `lexer`: tokenization only.
//! - `parser`: tokenization + AST construction.
//! - `sema`: frontend through the canonical rooted optimized-CFG artifact,
//!   including type checking, name resolution, affine checking, and CFG work.
//! - `compiler`: the *whole* pipeline — frontend, MIR
//!   lowering, register allocation, machine emission, and internal linking to a
//!   finished executable. Strictly deeper than `sema`.
//! - `compiler_aarch64`: the same finished-image boundary with explicit
//!   AArch64 Linux/O0 options; the image is never executed on the x86-64 runner.
//! - `compiler_x86_64_o1`: the same x86-64 Linux boundary with explicit O1
//!   optimization.
//! - `payloadschemas`: the RIR/AIR/CFG payload publication path.
//! - `emitter` / `emitteraarch64`: single-instruction encoding.
//! - `emittersequence` / `emittersequenceaarch64`: instruction-sequence encoding.
//!
//! `sema` and `compiler` are kept on separate endpoints on purpose; the
//! `compiler_target_is_deeper_than_sema` test guards against them silently
//! collapsing back onto the same query.

use super::FuzzTarget;
use rue_codegen::aarch64::{
    Aarch64Inst, Aarch64Mir, Emitter as Aarch64Emitter, Operand as Aarch64Operand,
    Reg as Aarch64Reg,
};
use rue_codegen::x86_64::{Emitter as X86Emitter, Operand, Reg, X86Inst, X86Mir};

/// Panic if the frontend reported an *internal* error.
///
/// A graceful ICE (`ice_error!` -> `ErrorKind::InternalError`) is an ordinary
/// `Err` value, not a panic — so without this check the harness classifies it
/// as a clean run and the #1 thing fuzzing exists to find is invisible.
/// Panicking here (in the forked child) surfaces it with the ICE text as its
/// dedup signature. Legitimate user-facing errors pass through silently.
fn assert_no_ice<T>(result: &rue_compiler::MultiErrorResult<T>) {
    if let Err(errors) = result {
        for e in errors.iter() {
            if is_ice(&e.kind) {
                panic!("graceful ICE: {e}");
            }
        }
    }
}

fn is_ice(kind: &rue_error::ErrorKind) -> bool {
    matches!(
        kind,
        rue_error::ErrorKind::CompilerProducerInvariant(_)
            | rue_error::ErrorKind::InternalError(_)
            | rue_error::ErrorKind::InternalCodegenError(_)
    )
}

#[cfg(test)]
mod ice_classification_tests {
    use super::is_ice;
    use rue_error::ErrorKind;

    #[test]
    fn resource_failures_are_not_ices_but_producer_invariants_are() {
        assert!(!is_ice(&ErrorKind::CompilerResourceLimit("limit".into())));
        assert!(!is_ice(&ErrorKind::CompilerResourceExhaustion(
            "allocation".into()
        )));
        assert!(is_ice(&ErrorKind::CompilerProducerInvariant(
            "bad AIR".into()
        )));
    }
}

/// Run the canonical frontend root through optimized CFG construction. This is
/// the endpoint of the `sema` target, with no machine backend or linker work.
fn query_semantics(source: &str) -> rue_compiler::MultiErrorResult<()> {
    let snapshot = rue_compiler::SourceSnapshot::single("<fuzz>", source)
        .map_err(rue_compiler::CompileErrors::from)?;
    let mut session = rue_compiler::CompilerSession::new();
    session.update(&snapshot).into_result()?;
    rue_compiler::unstable::rooted_cfg(&mut session, &rue_compiler::CompileOptions::default())
        .map(drop)
}

/// Build the explicit options used by source-level compiler fuzz targets.
///
/// Keeping target and optimization selection in one constructor makes each
/// registered endpoint auditable: a target cannot accidentally fall back to
/// the host/O0 defaults while still looking distinct in the fuzzer's registry.
fn source_compile_options(
    target: rue_compiler::Target,
    opt_level: rue_compiler::OptLevel,
) -> rue_compiler::CompileOptions {
    rue_compiler::CompileOptions {
        target,
        linker: rue_compiler::LinkerMode::Internal,
        opt_level,
        preview_features: rue_compiler::PreviewFeatures::new(),
        link_archives: Vec::new(),
    }
}

fn host_source_compile_options(opt_level: rue_compiler::OptLevel) -> rue_compiler::CompileOptions {
    source_compile_options(
        rue_compiler::Target::host()
            .expect("Rue fuzz compiler requires a supported compilation host"),
        opt_level,
    )
}

/// Drive the whole compilation pipeline — frontend, backend code generation, and
/// linking — to a finished executable. This is the endpoint of the `compiler`
/// target and a strictly deeper boundary than [`query_semantics`]: it reaches
/// CFG construction, MIR lowering, register allocation, machine emission, and
/// object/link assembly, none of which the sema query touches (RUE-776). The
/// explicit host/O0 options preserve the original target's endpoint while the
/// sibling source targets exercise other concrete configurations.
fn query_full_compile_with_options(
    source: &str,
    options: &rue_compiler::CompileOptions,
) -> rue_compiler::MultiErrorResult<rue_compiler::CompileOutput> {
    let snapshot = rue_compiler::SourceSnapshot::single("<fuzz>", source)
        .map_err(rue_compiler::CompileErrors::from)?;
    rue_compiler::compile_snapshot(&snapshot, options)
}

fn query_full_compile(source: &str) -> rue_compiler::MultiErrorResult<rue_compiler::CompileOutput> {
    query_full_compile_with_options(
        source,
        &host_source_compile_options(rue_compiler::OptLevel::O0),
    )
}

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
/// Uses source-level fuzzing through `CompilerSession`.
/// Future enhancement: structured RIR generation with Arbitrary trait.
pub struct SemaTarget;

impl FuzzTarget for SemaTarget {
    fn name(&self) -> &'static str {
        "sema"
    }

    fn fuzz(&self, input: &[u8]) {
        // Only test valid UTF-8
        if let Ok(source) = std::str::from_utf8(input) {
            // The session query runs through sema (semantic analysis)
            // without code generation. This tests:
            // - Type inference (Hindley-Milner with Algorithm W)
            // - Affine type checking (partial moves, linearity)
            // - Name resolution
            // - Multi-error collection
            let result = query_semantics(source);
            assert_no_ice(&result);
        }
    }
}

/// Fuzz target for the whole compilation pipeline, through code generation and
/// linking.
///
/// Goal: end-to-end compilation should never panic — it must succeed with a
/// finished executable or return ordinary errors.
///
/// This is deliberately a deeper boundary than [`SemaTarget`], which stops at
/// the canonical optimized-CFG root. Where sema fuzzes type inference, name
/// resolution, affine checking, and CFG construction, this target additionally exercises MIR
/// lowering, register allocation, machine emission, and internal linking — the
/// backend phases where a distinct family of ICEs lives. Keeping the two on
/// separate endpoints stops their fuzzing budget from being spent twice on the
/// same query (RUE-776).
pub struct CompilerTarget;

impl FuzzTarget for CompilerTarget {
    fn name(&self) -> &'static str {
        "compiler"
    }

    fn fuzz(&self, input: &[u8]) {
        // Only test valid UTF-8
        if let Ok(source) = std::str::from_utf8(input) {
            // Take valid programs all the way through the backend and linker so
            // codegen-stage invariants are fuzzed, not just semantic ones.
            let result = query_full_compile(source);
            assert_no_ice(&result);
        }
    }
}

/// Source-level whole-pipeline fuzzing for the AArch64 Linux backend.
///
/// The generated image is inspected only as bytes; this target deliberately
/// never executes it on the x86-64 nightly runner. That still reaches AArch64
/// lowering, register allocation, emission, and internal ELF linking.
pub struct CompilerAarch64Target;

impl FuzzTarget for CompilerAarch64Target {
    fn name(&self) -> &'static str {
        "compiler_aarch64"
    }

    fn fuzz(&self, input: &[u8]) {
        if let Ok(source) = std::str::from_utf8(input) {
            let result = Self::compile(source);
            assert_no_ice(&result);
        }
    }
}

impl CompilerAarch64Target {
    fn options() -> rue_compiler::CompileOptions {
        source_compile_options(
            rue_compiler::Target::Aarch64Linux,
            rue_compiler::OptLevel::O0,
        )
    }

    fn compile(source: &str) -> rue_compiler::MultiErrorResult<rue_compiler::CompileOutput> {
        query_full_compile_with_options(source, &Self::options())
    }
}

/// Source-level x86-64 Linux whole-pipeline fuzzing with basic optimization.
///
/// Naming the concrete target keeps a crash replay identical on every supported
/// developer host instead of resolving `Target::host()` differently from the
/// Linux x86-64 nightly runner.
pub struct CompilerX86_64O1Target;

impl FuzzTarget for CompilerX86_64O1Target {
    fn name(&self) -> &'static str {
        "compiler_x86_64_o1"
    }

    fn fuzz(&self, input: &[u8]) {
        if let Ok(source) = std::str::from_utf8(input) {
            let result = Self::compile(source);
            assert_no_ice(&result);
        }
    }
}

impl CompilerX86_64O1Target {
    fn options() -> rue_compiler::CompileOptions {
        source_compile_options(
            rue_compiler::Target::X86_64Linux,
            rue_compiler::OptLevel::O1,
        )
    }

    fn compile(source: &str) -> rue_compiler::MultiErrorResult<rue_compiler::CompileOutput> {
        query_full_compile_with_options(source, &Self::options())
    }
}

/// Fuzz the production payload publication path shared by RIR, AIR, and CFG.
///
/// This intentionally enters through `CompilerSession`: RIR and AIR are
/// consumed into their validated owner types and semantic publication builds
/// validated CFGs. Keeping the target on that path prevents a fuzz-only raw
/// decoder from drifting from the schemas used by real compilation.
pub struct PayloadSchemasTarget;

impl FuzzTarget for PayloadSchemasTarget {
    fn name(&self) -> &'static str {
        "payload_schemas"
    }

    fn fuzz(&self, input: &[u8]) {
        // The first bytes select a family and bounded corruption operation in
        // each owner. Results are deliberately accepted: the fuzz invariant is
        // that production checked decoders return structured errors, not panic.
        let _ = std::hint::black_box(rue_rir_fuzz_support::Rir::fuzz_payload_corruption(input));
        let _ = std::hint::black_box(rue_air_fuzz_support::Air::fuzz_payload_corruption(input));
        let _ = std::hint::black_box(rue_cfg_fuzz_support::fuzz_payload_corruption(input));
        if let Ok(source) = std::str::from_utf8(input) {
            assert_no_ice(&query_semantics(source));
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
        let emitter = X86Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
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

/// Fuzz target for the AArch64 instruction emitter.
///
/// Goal: The emitter should never panic on any sequence of valid post-regalloc
/// physical-register AArch64 instructions. Encoding-level fuzzing does not need
/// to execute the generated bytes, so this target can run on x86 hosts.
pub struct EmitterAarch64Target;

impl FuzzTarget for EmitterAarch64Target {
    fn name(&self) -> &'static str {
        "emitter_aarch64"
    }

    fn fuzz(&self, input: &[u8]) {
        if input.is_empty() {
            return;
        }

        let mut mir = Aarch64Mir::new();
        let mut idx = 0;

        while idx < input.len() {
            let opcode = input[idx] % 24;
            idx += 1;

            let reg1 = aarch64_reg_from_index(input.get(idx).copied().unwrap_or(0));
            idx += 1;
            let reg2 = aarch64_reg_from_index(input.get(idx).copied().unwrap_or(1));
            idx += 1;
            let reg3 = aarch64_reg_from_index(input.get(idx).copied().unwrap_or(2));
            idx += 1;

            let op1 = Aarch64Operand::Physical(reg1);
            let op2 = Aarch64Operand::Physical(reg2);
            let op3 = Aarch64Operand::Physical(reg3);

            let imm32 = if idx + 4 <= input.len() {
                let bytes = [input[idx], input[idx + 1], input[idx + 2], input[idx + 3]];
                idx += 4;
                i32::from_le_bytes(bytes)
            } else {
                0
            };
            let imm64 = if idx + 8 <= input.len() {
                let bytes = [
                    input[idx],
                    input[idx + 1],
                    input[idx + 2],
                    input[idx + 3],
                    input[idx + 4],
                    input[idx + 5],
                    input[idx + 6],
                    input[idx + 7],
                ];
                idx += 8;
                i64::from_le_bytes(bytes)
            } else {
                imm32 as i64
            };

            let add_sub_imm = imm32.unsigned_abs() as i32 % (1 << 20);
            let mem_offset = match input.get(idx).copied().unwrap_or(0) % 3 {
                0 => (imm32 % 256).clamp(-256, 255),
                1 => ((imm32.unsigned_abs() % 4096) as i32) * 8,
                _ => 0,
            };
            idx += 1;

            let inst = match opcode {
                0 => Aarch64Inst::MovImm {
                    dst: op1,
                    imm: imm64,
                },
                1 => Aarch64Inst::MovRR { dst: op1, src: op2 },
                2 => Aarch64Inst::Ldr {
                    dst: op1,
                    base: reg2,
                    offset: mem_offset,
                },
                3 => Aarch64Inst::Str {
                    src: op1,
                    base: reg2,
                    offset: mem_offset,
                },
                4 => Aarch64Inst::AddRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                5 => Aarch64Inst::SubRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                6 => Aarch64Inst::AddImm {
                    dst: op1,
                    src: op2,
                    imm: add_sub_imm,
                },
                7 => Aarch64Inst::SubImm {
                    dst: op1,
                    src: op2,
                    imm: add_sub_imm,
                },
                8 => Aarch64Inst::MulRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                9 => Aarch64Inst::AndRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                10 => Aarch64Inst::OrrRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                11 => Aarch64Inst::EorRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                12 => Aarch64Inst::MvnRR { dst: op1, src: op2 },
                13 => Aarch64Inst::LslImm {
                    dst: op1,
                    src: op2,
                    imm: (imm32 as u8) % 64,
                },
                14 => Aarch64Inst::Lsr64Imm {
                    dst: op1,
                    src: op2,
                    imm: (imm32 as u8) % 64,
                },
                15 => Aarch64Inst::Asr64Imm {
                    dst: op1,
                    src: op2,
                    imm: (imm32 as u8) % 64,
                },
                16 => Aarch64Inst::CmpRR {
                    src1: op1,
                    src2: op2,
                },
                17 => Aarch64Inst::CmpImm {
                    src: op1,
                    imm: (imm32.unsigned_abs() % 4096) as i32,
                },
                18 => Aarch64Inst::Cset {
                    dst: op1,
                    cond: aarch64_cond_from_index(input.get(idx).copied().unwrap_or(0)),
                },
                19 => Aarch64Inst::TstRR {
                    src1: op1,
                    src2: op2,
                },
                20 => Aarch64Inst::Sxtw { dst: op1, src: op2 },
                21 => Aarch64Inst::Uxtb { dst: op1, src: op2 },
                22 => Aarch64Inst::Brk,
                23 => Aarch64Inst::Ret,
                _ => Aarch64Inst::Ret,
            };

            mir.push(inst);
        }

        let emitter = Aarch64Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
        let _ = emitter.emit();
    }
}

/// Convert a byte index to an ordinary AArch64 general-purpose register.
fn aarch64_reg_from_index(idx: u8) -> Aarch64Reg {
    match idx % 28 {
        0 => Aarch64Reg::X0,
        1 => Aarch64Reg::X1,
        2 => Aarch64Reg::X2,
        3 => Aarch64Reg::X3,
        4 => Aarch64Reg::X4,
        5 => Aarch64Reg::X5,
        6 => Aarch64Reg::X6,
        7 => Aarch64Reg::X7,
        8 => Aarch64Reg::X8,
        9 => Aarch64Reg::X9,
        10 => Aarch64Reg::X10,
        11 => Aarch64Reg::X11,
        12 => Aarch64Reg::X12,
        13 => Aarch64Reg::X13,
        14 => Aarch64Reg::X14,
        15 => Aarch64Reg::X16,
        16 => Aarch64Reg::X17,
        17 => Aarch64Reg::X19,
        18 => Aarch64Reg::X20,
        19 => Aarch64Reg::X21,
        20 => Aarch64Reg::X22,
        21 => Aarch64Reg::X23,
        22 => Aarch64Reg::X24,
        23 => Aarch64Reg::X25,
        24 => Aarch64Reg::X26,
        25 => Aarch64Reg::X27,
        26 => Aarch64Reg::X28,
        27 => Aarch64Reg::Lr,
        _ => Aarch64Reg::X0,
    }
}

fn aarch64_cond_from_index(idx: u8) -> rue_codegen::aarch64::Cond {
    match idx % 10 {
        0 => rue_codegen::aarch64::Cond::Eq,
        1 => rue_codegen::aarch64::Cond::Ne,
        2 => rue_codegen::aarch64::Cond::Lt,
        3 => rue_codegen::aarch64::Cond::Gt,
        4 => rue_codegen::aarch64::Cond::Le,
        5 => rue_codegen::aarch64::Cond::Ge,
        6 => rue_codegen::aarch64::Cond::Hi,
        7 => rue_codegen::aarch64::Cond::Ls,
        8 => rue_codegen::aarch64::Cond::Hs,
        9 => rue_codegen::aarch64::Cond::Lo,
        _ => rue_codegen::aarch64::Cond::Eq,
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
        let emitter = X86Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
        let _ = emitter.emit();
    }
}

/// AArch64 branch/label sequence generation plus a decode oracle.
///
/// The fuzz target builds a valid MIR sequence with labels defined exactly once
/// and branches (forward and backward) of every AArch64 branch form, emits it,
/// then decodes each branch word straight out of the machine code and checks
/// that the resolved target matches the label's known position — a structural
/// oracle rather than just "did not panic".
///
/// Filler instructions are restricted to guaranteed single-word encodings, and
/// the emitter is constructed with `num_locals`/`num_params`/callee-saved all
/// zero, so there is no prologue and `Ret` is a bare `RET`. Every non-label
/// instruction therefore occupies exactly four bytes, and the instruction at
/// sequence index `n` lands at byte offset `4 * n`. `Label` markers emit no
/// bytes, so a label preceded by `k` non-label instructions sits at byte `4 * k`.
mod aarch64_seq {
    use super::{aarch64_cond_from_index, aarch64_reg_from_index};
    use rue_codegen::aarch64::{Aarch64Inst, Aarch64Mir, Cond, Operand, Reg};
    use std::collections::HashMap;

    /// The ten condition codes exercised by `BCond` (used by the deterministic
    /// per-form regression tests).
    #[cfg(test)]
    pub(crate) const ALL_CONDS: [Cond; 10] = [
        Cond::Eq,
        Cond::Ne,
        Cond::Lt,
        Cond::Gt,
        Cond::Le,
        Cond::Ge,
        Cond::Hi,
        Cond::Ls,
        Cond::Hs,
        Cond::Lo,
    ];

    /// A branch form together with any operand needed to decode/verify it.
    #[derive(Clone, Copy)]
    pub(crate) enum BranchKind {
        B,
        BCond(Cond),
        Bvs,
        Bvc,
        Cbz(Reg),
        Cbnz(Reg),
    }

    /// A recorded branch site for the oracle.
    pub(crate) struct BranchSite {
        /// Index of this branch among non-label instructions (byte offset / 4).
        pub inst_index: usize,
        pub kind: BranchKind,
        /// `LabelId::index()` of the target label.
        pub target: u32,
    }

    /// A built sequence plus the metadata the oracle needs to verify it.
    pub(crate) struct SequenceBuild {
        pub mir: Aarch64Mir,
        pub branches: Vec<BranchSite>,
        /// Target label id -> its non-label instruction index.
        pub label_index: HashMap<u32, usize>,
    }

    /// Build a valid AArch64 branch/label sequence from fuzzer bytes.
    ///
    /// Each label is defined exactly once: either at the point the byte stream
    /// selects a label-definition opcode for a not-yet-defined label, or (for
    /// any label never selected) appended once at the end. Branches may target
    /// labels defined earlier (backward) or later (forward).
    pub(crate) fn build_sequence(input: &[u8]) -> SequenceBuild {
        let num_labels = (input[0] % 8) as usize + 1;
        let mut mir = Aarch64Mir::new();
        let labels: Vec<_> = (0..num_labels).map(|_| mir.alloc_label()).collect();
        let mut defined = vec![false; num_labels];
        let mut label_index: HashMap<u32, usize> = HashMap::new();
        let mut branches: Vec<BranchSite> = Vec::new();
        // Count of non-label instructions emitted so far == byte offset / 4.
        let mut emitted = 0usize;
        let mut idx = 1usize;

        while idx < input.len() {
            let opcode = input[idx] % 30;
            idx += 1;
            let r1 = aarch64_reg_from_index(input.get(idx).copied().unwrap_or(0));
            idx += 1;
            let r2 = aarch64_reg_from_index(input.get(idx).copied().unwrap_or(1));
            idx += 1;
            let r3 = aarch64_reg_from_index(input.get(idx).copied().unwrap_or(2));
            idx += 1;
            let label_idx = input.get(idx).copied().unwrap_or(0) as usize % num_labels;
            idx += 1;
            let cond = aarch64_cond_from_index(input.get(idx).copied().unwrap_or(0));
            idx += 1;

            let op1 = Operand::Physical(r1);
            let op2 = Operand::Physical(r2);
            let op3 = Operand::Physical(r3);

            // Label definition: no bytes emitted, and only the first time for a
            // given label (a repeat selection falls through to a filler below).
            if opcode < 5 && !defined[label_idx] {
                defined[label_idx] = true;
                label_index.insert(labels[label_idx].index(), emitted);
                mir.push(Aarch64Inst::Label {
                    id: labels[label_idx],
                });
                continue;
            }

            let target = labels[label_idx];
            let inst = match opcode {
                5 | 6 => {
                    branches.push(BranchSite {
                        inst_index: emitted,
                        kind: BranchKind::B,
                        target: target.index(),
                    });
                    Aarch64Inst::B { label: target }
                }
                7..=9 => {
                    branches.push(BranchSite {
                        inst_index: emitted,
                        kind: BranchKind::BCond(cond),
                        target: target.index(),
                    });
                    Aarch64Inst::BCond {
                        cond,
                        label: target,
                    }
                }
                10 => {
                    branches.push(BranchSite {
                        inst_index: emitted,
                        kind: BranchKind::Bvs,
                        target: target.index(),
                    });
                    Aarch64Inst::Bvs { label: target }
                }
                11 => {
                    branches.push(BranchSite {
                        inst_index: emitted,
                        kind: BranchKind::Bvc,
                        target: target.index(),
                    });
                    Aarch64Inst::Bvc { label: target }
                }
                12 | 13 => {
                    branches.push(BranchSite {
                        inst_index: emitted,
                        kind: BranchKind::Cbz(r1),
                        target: target.index(),
                    });
                    Aarch64Inst::Cbz {
                        src: op1,
                        label: target,
                    }
                }
                14 | 15 => {
                    branches.push(BranchSite {
                        inst_index: emitted,
                        kind: BranchKind::Cbnz(r1),
                        target: target.index(),
                    });
                    Aarch64Inst::Cbnz {
                        src: op1,
                        label: target,
                    }
                }
                // Guaranteed single-word filler instructions.
                16 => Aarch64Inst::MovRR { dst: op1, src: op2 },
                17 => Aarch64Inst::AddRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                18 => Aarch64Inst::SubRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                19 => Aarch64Inst::MulRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                20 => Aarch64Inst::AndRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                21 => Aarch64Inst::OrrRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                22 => Aarch64Inst::EorRR {
                    dst: op1,
                    src1: op2,
                    src2: op3,
                },
                23 => Aarch64Inst::CmpRR {
                    src1: op1,
                    src2: op2,
                },
                24 => Aarch64Inst::TstRR {
                    src1: op1,
                    src2: op2,
                },
                25 => Aarch64Inst::MvnRR { dst: op1, src: op2 },
                26 => Aarch64Inst::Cset { dst: op1, cond },
                27 => Aarch64Inst::Sxtw { dst: op1, src: op2 },
                28 => Aarch64Inst::Uxtb { dst: op1, src: op2 },
                // opcode 29, plus repeat label-definition selections, land here.
                _ => Aarch64Inst::Brk,
            };
            mir.push(inst);
            emitted += 1;
        }

        // Define any never-selected labels exactly once at the end. They all
        // share the current `emitted` offset (labels emit no bytes).
        for (i, placed) in defined.iter().enumerate() {
            if !placed {
                label_index.insert(labels[i].index(), emitted);
                mir.push(Aarch64Inst::Label { id: labels[i] });
            }
        }
        // Single-word return (no frame), so it does not disturb byte offsets.
        mir.push(Aarch64Inst::Ret);

        SequenceBuild {
            mir,
            branches,
            label_index,
        }
    }

    fn word_at(code: &[u8], byte: usize) -> u32 {
        u32::from_le_bytes(code[byte..byte + 4].try_into().unwrap())
    }

    /// Sign-extend the low `bits` bits of `value` to `i64`.
    fn sign_extend(value: u32, bits: u32) -> i64 {
        let shift = 64 - bits;
        ((value as i64) << shift) >> shift
    }

    /// Resolve the byte target of a `B` word (imm26) emitted at `site_byte`.
    pub(crate) fn decode_b_target(word: u32, site_byte: usize) -> i64 {
        let imm = sign_extend(word & 0x03FF_FFFF, 26);
        site_byte as i64 + imm * 4
    }

    /// Resolve the byte target of a conditional-form word — B.cond / B.vs /
    /// B.vc / CBZ / CBNZ (imm19) — emitted at `site_byte`.
    pub(crate) fn decode_cond_target(word: u32, site_byte: usize) -> i64 {
        let imm = sign_extend((word >> 5) & 0x7_FFFF, 19);
        site_byte as i64 + imm * 4
    }

    /// Verify every recorded branch resolves to its label's byte offset and
    /// that the opcode / condition / register fields round-trip. Panics with a
    /// descriptive message on any mismatch (the harness treats panics as
    /// findings).
    pub(crate) fn validate(build: &SequenceBuild, code: &[u8]) {
        for site in &build.branches {
            let site_byte = site.inst_index * 4;
            let target_index = build.label_index[&site.target];
            let target_byte = (target_index * 4) as i64;
            assert!(
                site_byte + 4 <= code.len(),
                "branch site byte {site_byte} beyond {}-byte code",
                code.len()
            );
            let word = word_at(code, site_byte);
            match site.kind {
                BranchKind::B => {
                    assert_eq!(
                        word & 0xFC00_0000,
                        0x1400_0000,
                        "expected B opcode at byte {site_byte}, got {word:#010x}"
                    );
                    let decoded = decode_b_target(word, site_byte);
                    assert_eq!(
                        decoded, target_byte,
                        "B at {site_byte}: decoded target {decoded} != label offset {target_byte}"
                    );
                }
                BranchKind::BCond(_) | BranchKind::Bvs | BranchKind::Bvc => {
                    assert_eq!(
                        (word >> 24) & 0xFF,
                        0x54,
                        "expected B.cond opcode at byte {site_byte}, got {word:#010x}"
                    );
                    assert_eq!(word & 0x10, 0, "B.cond bit 4 must be 0 at byte {site_byte}");
                    let decoded = decode_cond_target(word, site_byte);
                    assert_eq!(
                        decoded, target_byte,
                        "B.cond at {site_byte}: decoded target {decoded} != label offset {target_byte}"
                    );
                    let expected_cond: u32 = match site.kind {
                        BranchKind::BCond(c) => c.encoding() as u32,
                        BranchKind::Bvs => 6,
                        BranchKind::Bvc => 7,
                        _ => unreachable!(),
                    };
                    assert_eq!(
                        word & 0xF,
                        expected_cond,
                        "condition nibble mismatch at byte {site_byte}"
                    );
                }
                BranchKind::Cbz(rt) | BranchKind::Cbnz(rt) => {
                    let is_nz = matches!(site.kind, BranchKind::Cbnz(_));
                    let expected_top: u32 = if is_nz { 0xB5 } else { 0xB4 };
                    assert_eq!(
                        (word >> 24) & 0xFF,
                        expected_top,
                        "expected CBZ/CBNZ opcode at byte {site_byte}, got {word:#010x}"
                    );
                    let decoded = decode_cond_target(word, site_byte);
                    assert_eq!(
                        decoded, target_byte,
                        "CBZ/CBNZ at {site_byte}: decoded target {decoded} != label offset {target_byte}"
                    );
                    assert_eq!(
                        word & 0x1F,
                        rt.encoding() as u32,
                        "CBZ/CBNZ register mismatch at byte {site_byte}"
                    );
                }
            }
        }
    }
}

/// Fuzz target for AArch64 instruction sequences with labels and branches.
///
/// Unlike `emitter_aarch64` (independent ALU/memory/return instructions), this
/// exercises label definitions, every branch form (`B`, `BCond` with all
/// conditions, `Bvs`, `Bvc`, `Cbz`, `Cbnz`), and the fixup machinery — with a
/// decode oracle that checks each resolved branch target. Both the ordinary
/// `emit()` path and the assembly-recording `emit_all()` path (which runs
/// `synchronize_emitted_bytes`) are exercised and required to agree.
pub struct EmitterSequenceAarch64Target;

impl FuzzTarget for EmitterSequenceAarch64Target {
    fn name(&self) -> &'static str {
        "emitter_sequence_aarch64"
    }

    fn fuzz(&self, input: &[u8]) {
        if input.len() < 2 {
            return;
        }
        let build = aarch64_seq::build_sequence(input);

        let emitter = Aarch64Emitter::new(&build.mir, 0, 0, 0, &[], &[]).without_frame();
        let (code, _relocations) = match emitter.emit() {
            Ok(result) => result,
            // A graceful ICE (e.g. an out-of-range displacement) is a normal
            // Err, not a finding; these bounded sequences stay within range.
            Err(_) => return,
        };

        aarch64_seq::validate(&build, &code);

        // The assembly-recording path must produce identical final bytes.
        let recording = Aarch64Emitter::new(&build.mir, 0, 0, 0, &[], &[]).without_frame();
        if let Ok(emitted) = recording.emit_all() {
            assert_eq!(
                emitted.to_bytes(),
                code,
                "emit_all() final bytes must equal emit() bytes"
            );
        }
    }
}

/// Get all available fuzz targets.
pub fn all_targets() -> Vec<Box<dyn FuzzTarget>> {
    vec![
        Box::new(LexerTarget),
        Box::new(ParserTarget),
        Box::new(SemaTarget),
        Box::new(CompilerTarget),
        Box::new(CompilerAarch64Target),
        Box::new(CompilerX86_64O1Target),
        Box::new(PayloadSchemasTarget),
        Box::new(EmitterTarget),
        Box::new(EmitterAarch64Target),
        Box::new(EmitterSequenceTarget),
        Box::new(EmitterSequenceAarch64Target),
    ]
}

/// Get a fuzz target by name.
pub fn get_target(name: &str) -> Option<Box<dyn FuzzTarget>> {
    match name {
        "lexer" => Some(Box::new(LexerTarget)),
        "parser" => Some(Box::new(ParserTarget)),
        "sema" => Some(Box::new(SemaTarget)),
        "compiler" => Some(Box::new(CompilerTarget)),
        "compiler_aarch64" => Some(Box::new(CompilerAarch64Target)),
        "compiler_x86_64_o1" => Some(Box::new(CompilerX86_64O1Target)),
        "payload_schemas" => Some(Box::new(PayloadSchemasTarget)),
        "emitter" => Some(Box::new(EmitterTarget)),
        "emitter_aarch64" => Some(Box::new(EmitterAarch64Target)),
        "emitter_sequence" => Some(Box::new(EmitterSequenceTarget)),
        "emitter_sequence_aarch64" => Some(Box::new(EmitterSequenceAarch64Target)),
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
    fn payload_schema_target_reaches_every_family_and_corruption_operation() {
        let target = PayloadSchemasTarget;
        // The phase-local selectors have 17, 10, and 10 families. Walking the
        // least common bounded selector range exercises every family in every
        // owner under each of the four supported corruption operations.
        for selector in 0_u8..170 {
            for operation in 0_u8..4 {
                target.fuzz(&[selector, operation, 0, 0, 0, 0]);
            }
        }
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
    fn compiler_aarch64_target_selects_aarch64_and_links_without_running() {
        let options = CompilerAarch64Target::options();
        assert_eq!(options.target, rue_compiler::Target::Aarch64Linux);
        assert_eq!(options.opt_level, rue_compiler::OptLevel::O0);

        let output = CompilerAarch64Target::compile("fn main() -> i32 { 42 }")
            .expect("AArch64 source compile reaches the linker");
        assert!(output.elf.starts_with(b"\x7fELF"));
        assert_eq!(
            u16::from_le_bytes([output.elf[18], output.elf[19]]),
            0xB7,
            "finished image must be an AArch64 ELF executable"
        );
    }

    #[test]
    fn compiler_x86_64_o1_target_selects_and_applies_basic_optimization() {
        let options = CompilerX86_64O1Target::options();
        assert_eq!(options.target, rue_compiler::Target::X86_64Linux);
        assert_eq!(options.opt_level, rue_compiler::OptLevel::O1);

        // A dynamic identity cannot be folded by the frontend: O1 removes the
        // addition in `identity`, so its finished image differs from x86-64/O0.
        // Calling the target's exact compile method makes this fail if nightly
        // wiring silently returns to host/O0 defaults.
        let source = "fn identity(value: i32) -> i32 { value + 0 } \
                      fn main() -> i32 { identity(42) }";
        let output = CompilerX86_64O1Target::compile(source)
            .expect("x86-64 O1 source compile reaches the linker");
        assert!(output.elf.starts_with(b"\x7fELF"));
        assert_eq!(
            u16::from_le_bytes([output.elf[18], output.elf[19]]),
            0x3E,
            "finished image must be an x86-64 ELF executable"
        );

        let o0 = query_full_compile_with_options(
            source,
            &source_compile_options(
                rue_compiler::Target::X86_64Linux,
                rue_compiler::OptLevel::O0,
            ),
        )
        .expect("x86-64 O0 comparison compile succeeds");
        assert_ne!(
            output.elf, o0.elf,
            "the registered O1 path must apply an optimization observable in the finished image"
        );
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

    /// RUE-776: the `compiler` target must reach a strictly deeper boundary than
    /// `sema`. The sema query stops at the rooted CFG artifact; the compiler query
    /// must drive codegen and linking to a finished executable. If the compiler
    /// target is ever pointed back at that frontend root, `query_full_compile`
    /// stops yielding a binary and this test fails — so the two contracts cannot
    /// silently collapse onto the same endpoint again.
    #[test]
    fn compiler_target_is_deeper_than_sema() {
        let source = "fn main() -> i32 { 42 }";

        // sema endpoint: the rooted optimized-CFG query, no backend artifact.
        query_semantics(source).expect("sema query succeeds on a valid program");

        // compiler endpoint: a fully linked executable image. The concrete
        // object format is host-dependent (ELF or Mach-O), so only its presence
        // is asserted here.
        let output = query_full_compile(source).expect("full compile succeeds on a valid program");
        assert!(
            !output.elf.is_empty(),
            "compiler target must produce a linked executable — a deeper boundary than sema"
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
    fn test_emitter_aarch64_target_valid() {
        let target = EmitterAarch64Target;
        target.fuzz(&[0, 1, 2, 3, 0, 0, 0, 0, 1, 4, 5, 6]);
    }

    #[test]
    fn test_emitter_aarch64_target_empty() {
        let target = EmitterAarch64Target;
        target.fuzz(&[]);
    }

    #[test]
    fn test_emitter_sequence_target_valid() {
        let target = EmitterSequenceTarget;
        // Sequence with labels and jumps
        target.fuzz(&[2, 20, 1, 0, 25, 2, 0, 0, 1, 2]);
    }

    // ===== AArch64 branch/label sequence target =====

    use super::aarch64_seq::{self, BranchKind, BranchSite, SequenceBuild};
    use rue_codegen::aarch64::Cond as A64Cond;
    use std::collections::HashMap;

    /// Build a forward branch of `kind` whose target label is `disp`
    /// instructions ahead: the branch at index 0, `disp - 1` `Brk` fillers, then
    /// the label at index `disp`.
    fn build_forward_branch(kind: BranchKind, disp: usize) -> SequenceBuild {
        let mut mir = Aarch64Mir::new();
        let label = mir.alloc_label();
        let branch = match kind {
            BranchKind::B => Aarch64Inst::B { label },
            BranchKind::BCond(c) => Aarch64Inst::BCond { cond: c, label },
            BranchKind::Bvs => Aarch64Inst::Bvs { label },
            BranchKind::Bvc => Aarch64Inst::Bvc { label },
            BranchKind::Cbz(rt) => Aarch64Inst::Cbz {
                src: Aarch64Operand::Physical(rt),
                label,
            },
            BranchKind::Cbnz(rt) => Aarch64Inst::Cbnz {
                src: Aarch64Operand::Physical(rt),
                label,
            },
        };
        let branches = vec![BranchSite {
            inst_index: 0,
            kind,
            target: label.index(),
        }];
        mir.push(branch);
        for _ in 0..disp.saturating_sub(1) {
            mir.push(Aarch64Inst::Brk);
        }
        let mut label_index = HashMap::new();
        label_index.insert(label.index(), disp);
        mir.push(Aarch64Inst::Label { id: label });
        mir.push(Aarch64Inst::Ret);
        SequenceBuild {
            mir,
            branches,
            label_index,
        }
    }

    /// Build a backward branch of `kind` whose target label is `disp`
    /// instructions behind: the label at index 0, `disp` `Brk` fillers, then the
    /// branch at index `disp`.
    fn build_backward_branch(kind: BranchKind, disp: usize) -> SequenceBuild {
        let mut mir = Aarch64Mir::new();
        let label = mir.alloc_label();
        let mut label_index = HashMap::new();
        label_index.insert(label.index(), 0);
        mir.push(Aarch64Inst::Label { id: label });
        for _ in 0..disp {
            mir.push(Aarch64Inst::Brk);
        }
        let branch = match kind {
            BranchKind::B => Aarch64Inst::B { label },
            BranchKind::BCond(c) => Aarch64Inst::BCond { cond: c, label },
            BranchKind::Bvs => Aarch64Inst::Bvs { label },
            BranchKind::Bvc => Aarch64Inst::Bvc { label },
            BranchKind::Cbz(rt) => Aarch64Inst::Cbz {
                src: Aarch64Operand::Physical(rt),
                label,
            },
            BranchKind::Cbnz(rt) => Aarch64Inst::Cbnz {
                src: Aarch64Operand::Physical(rt),
                label,
            },
        };
        let branches = vec![BranchSite {
            inst_index: disp,
            kind,
            target: label.index(),
        }];
        mir.push(branch);
        mir.push(Aarch64Inst::Ret);
        SequenceBuild {
            mir,
            branches,
            label_index,
        }
    }

    /// Emit via `emit()`, run the decode oracle, and require `emit_all()` to
    /// produce identical bytes. Returns the emitted code.
    fn emit_and_validate(build: &SequenceBuild) -> Vec<u8> {
        let (code, _) = Aarch64Emitter::new(&build.mir, 0, 0, 0, &[], &[])
            .without_frame()
            .emit()
            .expect("in-range sequence should emit");
        aarch64_seq::validate(build, &code);
        let recorded = Aarch64Emitter::new(&build.mir, 0, 0, 0, &[], &[])
            .without_frame()
            .emit_all()
            .expect("in-range sequence should emit_all");
        assert_eq!(
            recorded.to_bytes(),
            code,
            "emit_all() bytes must equal emit() bytes"
        );
        code
    }

    fn emit_is_err(build: &SequenceBuild) -> bool {
        Aarch64Emitter::new(&build.mir, 0, 0, 0, &[], &[])
            .without_frame()
            .emit()
            .is_err()
    }

    #[test]
    fn emitter_sequence_aarch64_forward_and_backward_each_form() {
        let mut kinds = vec![
            BranchKind::B,
            BranchKind::Bvs,
            BranchKind::Bvc,
            BranchKind::Cbz(Aarch64Reg::X3),
            BranchKind::Cbnz(Aarch64Reg::X7),
        ];
        // BCond with all ten condition codes.
        for c in aarch64_seq::ALL_CONDS {
            kinds.push(BranchKind::BCond(c));
        }
        for kind in kinds {
            for disp in [1usize, 4, 9] {
                emit_and_validate(&build_forward_branch(kind, disp));
                emit_and_validate(&build_backward_branch(kind, disp));
            }
        }
    }

    #[test]
    fn emitter_sequence_aarch64_cond_branch_imm19_boundaries() {
        // imm19 is a signed instruction count: -2^18 ..= 2^18 - 1.
        let max_forward = (1usize << 18) - 1; // +262143
        let max_backward = 1usize << 18; // displacement -262144

        // Exact legal boundaries encode and decode correctly.
        emit_and_validate(&build_forward_branch(
            BranchKind::Cbz(Aarch64Reg::X0),
            max_forward,
        ));
        emit_and_validate(&build_forward_branch(
            BranchKind::BCond(A64Cond::Eq),
            max_forward,
        ));
        emit_and_validate(&build_backward_branch(
            BranchKind::Cbz(Aarch64Reg::X0),
            max_backward,
        ));
        emit_and_validate(&build_backward_branch(
            BranchKind::BCond(A64Cond::Ne),
            max_backward,
        ));

        // One instruction past each boundary is a graceful ICE Err, not a panic.
        assert!(emit_is_err(&build_forward_branch(
            BranchKind::Cbz(Aarch64Reg::X0),
            max_forward + 1
        )));
        assert!(emit_is_err(&build_backward_branch(
            BranchKind::BCond(A64Cond::Ne),
            max_backward + 1
        )));
    }

    #[test]
    fn emitter_sequence_aarch64_b_imm26_beyond_imm19() {
        // A displacement far past the imm19 range (a conditional branch here
        // would ICE) still fits B's imm26 and decodes to the right target. We
        // cover the largest practical displacement rather than the full +-2^25
        // imm26 boundary, which would need ~134 MB of code and slow the suite.
        let disp = 300_000usize;
        emit_and_validate(&build_forward_branch(BranchKind::B, disp));
        emit_and_validate(&build_backward_branch(BranchKind::B, disp));
    }

    #[test]
    fn test_emitter_sequence_aarch64_target_valid() {
        let target = EmitterSequenceAarch64Target;
        // A deterministic seed driving many opcodes (labels, every branch form,
        // and fillers) through the full build + oracle + emit_all path.
        let seed: Vec<u8> = (0u8..120).collect();
        target.fuzz(&seed);
    }

    #[test]
    fn test_emitter_sequence_aarch64_target_tiny_and_empty() {
        let target = EmitterSequenceAarch64Target;
        target.fuzz(&[]);
        target.fuzz(&[0]);
        target.fuzz(&[1, 2]);
        target.fuzz(&[7, 5, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn test_all_targets() {
        let targets = all_targets();
        assert_eq!(targets.len(), 11);
    }

    #[test]
    fn test_get_target() {
        assert!(get_target("lexer").is_some());
        assert!(get_target("parser").is_some());
        assert!(get_target("sema").is_some());
        assert!(get_target("compiler").is_some());
        assert!(get_target("compiler_aarch64").is_some());
        assert!(get_target("compiler_x86_64_o1").is_some());
        assert!(get_target("payload_schemas").is_some());
        assert!(get_target("emitter").is_some());
        assert!(get_target("emitter_aarch64").is_some());
        assert!(get_target("emitter_sequence").is_some());
        assert!(get_target("emitter_sequence_aarch64").is_some());
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

        /// The AArch64 emitter should handle arbitrary byte sequences without panicking.
        #[test]
        fn emitter_aarch64_handles_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            let target = EmitterAarch64Target;
            target.fuzz(&bytes);
        }

        /// The emitter sequence target should handle arbitrary bytes without panicking.
        #[test]
        fn emitter_sequence_handles_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 2..256)) {
            let target = EmitterSequenceTarget;
            target.fuzz(&bytes);
        }

        /// The AArch64 emitter sequence target should handle arbitrary bytes
        /// without panicking and with its decode oracle satisfied.
        #[test]
        fn emitter_sequence_aarch64_handles_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 2..256)) {
            let target = EmitterSequenceAarch64Target;
            target.fuzz(&bytes);
        }
    }
}

/// Proptest-based fuzz tests for codegen using structured instruction generation.
#[cfg(test)]
mod codegen_proptest_tests {
    use crate::codegen_generators;
    use proptest::prelude::*;
    use rue_codegen::aarch64::{Aarch64Inst, Aarch64Mir, Emitter as Aarch64Emitter};
    use rue_codegen::x86_64::{Emitter, Operand, Reg, X86Inst, X86Mir};

    proptest! {
        /// The emitter should never panic on any valid instruction.
        #[test]
        fn emitter_never_panics_on_instruction(inst in codegen_generators::arb_x86_inst_physical()) {
            let mut mir = X86Mir::new();
            mir.push(inst);
            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
            let _ = emitter.emit();
        }

        /// The emitter should never panic on any valid MIR.
        #[test]
        fn emitter_never_panics_on_mir(mir in codegen_generators::arb_x86_mir(20, 3)) {
            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
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

            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
            let _ = emitter.emit();
        }

        /// The emitter should handle extreme immediate values.
        #[test]
        fn emitter_handles_extreme_immediates(imm64 in codegen_generators::arb_imm64()) {
            let mut mir = X86Mir::new();
            let dst = Operand::Physical(Reg::Rax);

            mir.push(X86Inst::MovRI64 { dst, imm: imm64 });

            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
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

            let emitter = Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
            let _ = emitter.emit();
        }

        /// The AArch64 emitter should never panic on any valid physical instruction.
        #[test]
        fn emitter_aarch64_never_panics_on_instruction(inst in codegen_generators::arb_aarch64_inst_physical()) {
            let mut mir = Aarch64Mir::new();
            mir.push(inst);
            let emitter = Aarch64Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
            let _ = emitter.emit();
        }

        /// The AArch64 emitter should never panic on generated physical MIR.
        #[test]
        fn emitter_aarch64_never_panics_on_mir(mir in codegen_generators::arb_aarch64_mir(20)) {
            let emitter = Aarch64Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
            let _ = emitter.emit();
        }

        /// The AArch64 emitter should never panic on generated branch/label
        /// control-flow MIR (labels defined once, forward and backward edges).
        #[test]
        fn emitter_aarch64_sequence_never_panics_on_mir(
            mir in codegen_generators::arb_aarch64_branch_mir(20, 3)
        ) {
            let emitter = Aarch64Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
            let _ = emitter.emit();
        }

        /// The AArch64 emitter should handle register and immediate combinations.
        #[test]
        fn emitter_aarch64_handles_register_combos(
            reg1 in codegen_generators::arb_aarch64_reg(),
            reg2 in codegen_generators::arb_aarch64_reg(),
            reg3 in codegen_generators::arb_aarch64_reg(),
            imm in codegen_generators::arb_aarch64_add_sub_imm()
        ) {
            let mut mir = Aarch64Mir::new();
            let op1 = rue_codegen::aarch64::Operand::Physical(reg1);
            let op2 = rue_codegen::aarch64::Operand::Physical(reg2);
            let op3 = rue_codegen::aarch64::Operand::Physical(reg3);

            mir.push(Aarch64Inst::MovRR { dst: op1, src: op2 });
            mir.push(Aarch64Inst::AddRR { dst: op1, src1: op2, src2: op3 });
            mir.push(Aarch64Inst::SubImm { dst: op1, src: op2, imm });
            mir.push(Aarch64Inst::CmpRR { src1: op1, src2: op2 });

            let emitter = Aarch64Emitter::new(&mir, 0, 0, 0, &[], &[]).without_frame();
            let _ = emitter.emit();
        }
    }
}
