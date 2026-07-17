//! Code generation for the Rue compiler.
//!
//! This crate converts CFG (Control Flow Graph) to machine code.
//! Supports x86-64 and AArch64.
//!
//! ## Pipeline
//!
//! ```text
//! CFG → MIR (virtual registers) → Register Allocation → Machine Code
//! ```
//!
//! Each backend uses a Machine IR (MIR) that closely matches the target
//! instructions but uses virtual registers. Register allocation then maps
//! virtual registers to physical registers before final emission.

/// Ends recording an instruction with lazy format string evaluation.
///
/// When `emit_asm` is false (normal compilation), this is a no-op and the
/// format string arguments are never evaluated. When `emit_asm` is true
/// (--emit asm mode), the format string is evaluated and stored.
///
/// This is more efficient than calling `end_inst(format!(...))` directly,
/// which would always evaluate the format string.
///
/// # Examples
///
/// ```ignore
/// // Instead of:
/// self.end_inst(format!("mov {}, {}", dst, src));
///
/// // Use:
/// end_inst!(self, "mov {}, {}", dst, src);
/// ```
#[macro_export]
macro_rules! end_inst {
    ($emitter:expr, $($arg:tt)*) => {
        if $emitter.emit_asm {
            let bytes = $emitter.code[$emitter.inst_start..].to_vec();
            $emitter.instructions.push($crate::EmittedInst::new(bytes, format!($($arg)*)));
        }
    };
}

mod allocation;
pub mod call_plan;
mod codegen_pipeline;
pub mod runtime_call_plan;
mod schedule_core;
mod stack_frame;
mod stack_verify;
pub mod terminator_plan;
pub mod value_plan;

pub mod aarch64;
mod agg_slots;
pub mod aggregate_eq;
#[cfg(test)]
mod api_inventory;
pub mod byref_args;
pub mod cfg_lower;
pub mod index_map;
pub mod liveness;
pub mod place_lower;
pub mod regalloc;
pub mod types;
pub mod vreg;
pub mod x86_64;

/// The kind of relocation emitted by the code generator.
///
/// This represents the semantic intent of the relocation. The linker
/// will map these to architecture-specific ELF/Mach-O relocation types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationKind {
    // ========== x86-64 relocations ==========
    /// 32-bit PC-relative relocation (x86-64).
    /// Used for string constant references and data access.
    X86Pc32,

    /// 32-bit PLT-relative relocation (x86-64).
    /// Used for function calls. For static linking, treated same as PC32.
    X86Plt32,

    // ========== AArch64 relocations ==========
    /// ADRP page-relative relocation (AArch64).
    /// Loads the page address of a symbol into a register.
    Aarch64AdrpPage21,

    /// ADD page offset relocation (AArch64).
    /// Adds the within-page offset of a symbol.
    Aarch64AddLo12,

    /// Branch with link relocation (AArch64).
    /// Used for function calls.
    Aarch64Call26,
}

/// A relocation emitted by the code generator.
///
/// Relocations are recorded for all symbol references (like function calls)
/// that need to be resolved by the linker.
#[derive(Debug, Clone)]
pub struct EmittedRelocation {
    /// Byte offset within the generated code where the relocation applies.
    pub offset: u64,
    /// The symbol name being referenced.
    pub symbol: String,
    /// The kind of relocation.
    pub kind: RelocationKind,
    /// Addend value to add to the symbol address.
    pub addend: i64,
}

impl EmittedRelocation {
    // ========== x86-64 relocation helpers ==========

    /// Create a PC-relative relocation for x86-64.
    ///
    /// Used for string constant references and RIP-relative data access.
    /// The addend is -4 because the displacement is calculated from the
    /// end of the instruction (after the 4-byte displacement field).
    pub fn x86_pc32(offset: u64, symbol: impl Into<String>) -> Self {
        Self {
            offset,
            symbol: symbol.into(),
            kind: RelocationKind::X86Pc32,
            addend: -4,
        }
    }

    /// Create a PLT-relative relocation for x86-64 function calls.
    ///
    /// For static linking, this is treated the same as PC32.
    /// The addend is -4 because the displacement is calculated from the
    /// end of the instruction.
    pub fn x86_call(offset: u64, symbol: impl Into<String>) -> Self {
        Self {
            offset,
            symbol: symbol.into(),
            kind: RelocationKind::X86Plt32,
            addend: -4,
        }
    }

    // ========== AArch64 relocation helpers ==========

    /// Create an ADRP page relocation for AArch64.
    ///
    /// Loads the 4KB-aligned page address of the symbol.
    /// Used as the first part of a two-instruction sequence for
    /// PC-relative data access.
    pub fn aarch64_adrp(offset: u64, symbol: impl Into<String>) -> Self {
        Self {
            offset,
            symbol: symbol.into(),
            kind: RelocationKind::Aarch64AdrpPage21,
            addend: 0,
        }
    }

    /// Create an ADD page offset relocation for AArch64.
    ///
    /// Adds the within-page (12-bit) offset of the symbol.
    /// Used as the second part of a two-instruction sequence for
    /// PC-relative data access.
    pub fn aarch64_add_lo12(offset: u64, symbol: impl Into<String>) -> Self {
        Self {
            offset,
            symbol: symbol.into(),
            kind: RelocationKind::Aarch64AddLo12,
            addend: 0,
        }
    }

    /// Create a branch-with-link relocation for AArch64 function calls.
    ///
    /// Used for the BL instruction.
    pub fn aarch64_call(offset: u64, symbol: impl Into<String>) -> Self {
        Self {
            offset,
            symbol: symbol.into(),
            kind: RelocationKind::Aarch64Call26,
            addend: 0,
        }
    }
}

/// Machine code generated by a backend.
#[derive(Debug)]
pub struct MachineCode {
    /// The generated machine code bytes.
    pub code: Vec<u8>,
    /// Relocations that need to be resolved by the linker.
    pub relocations: Vec<EmittedRelocation>,
    /// String table referenced by the code.
    pub strings: Vec<String>,
}

/// Build a function-local string table and remap MIR string IDs to it.
///
/// Semantic analysis assigns IDs in the program-wide string table. Object files
/// are emitted per function, so retaining that whole table in every object is
/// both unnecessary and quadratic in programs with many functions. Ordering
/// local strings by their global ID keeps the object bytes deterministic.
fn compact_string_table(
    strings: &[String],
    referenced_ids: impl IntoIterator<Item = u32>,
) -> (Vec<String>, std::collections::BTreeMap<u32, u32>) {
    let referenced_ids: std::collections::BTreeSet<_> = referenced_ids.into_iter().collect();
    let mut remap = std::collections::BTreeMap::new();
    let mut local_strings = Vec::with_capacity(referenced_ids.len());

    for global_id in referenced_ids {
        let local_id = local_strings.len() as u32;
        let string = strings
            .get(global_id as usize)
            .unwrap_or_else(|| panic!("string ID {global_id} is absent from the global table"));
        local_strings.push(string.clone());
        remap.insert(global_id, local_id);
    }

    (local_strings, remap)
}

/// A single emitted machine instruction.
///
/// This captures both the machine code bytes and the human-readable assembly text
/// for an instruction. The emitter produces a sequence of these, which can then
/// be serialized to either raw bytes or assembly text.
#[derive(Debug, Clone)]
pub struct EmittedInst {
    /// The machine code bytes for this instruction.
    /// Empty for labels and comments.
    pub bytes: Vec<u8>,
    /// Human-readable assembly text (e.g., "mov rax, rbx").
    /// For labels, this is just the label text (e.g., "loop:").
    /// For comments, this starts with "; ".
    pub asm: String,
}

impl EmittedInst {
    /// Create a new instruction with bytes and assembly text.
    pub fn new(bytes: impl Into<Vec<u8>>, asm: impl Into<String>) -> Self {
        Self {
            bytes: bytes.into(),
            asm: asm.into(),
        }
    }

    /// Create a label (no bytes, just marks a position).
    pub fn label(name: impl Into<String>) -> Self {
        Self {
            bytes: vec![],
            asm: format!("{}:", name.into()),
        }
    }

    /// Create a comment (no bytes).
    pub fn comment(text: impl Into<String>) -> Self {
        Self {
            bytes: vec![],
            asm: format!("; {}", text.into()),
        }
    }
}

/// Replace assembly-mode instruction snapshots with the final bytes after
/// label fixups have patched the emitter's contiguous code buffer.
///
/// Instruction sizes never change during fixup: x86-64 uses fixed-width rel32
/// jumps and AArch64 branches are always four bytes. Labels and comments have
/// zero-length snapshots, so walking the recorded lengths reconstructs the
/// exact byte range belonging to every instruction without target knowledge.
pub(crate) fn synchronize_emitted_bytes(
    instructions: &mut [EmittedInst],
    code: &[u8],
) -> rue_error::CompileResult<()> {
    let recorded_len: usize = instructions.iter().map(|inst| inst.bytes.len()).sum();
    if recorded_len != code.len() {
        return Err(rue_error::ice_error!(
            "assembly instruction byte coverage mismatch",
            phase: "codegen/emit",
            details: {
                "recorded_bytes" => recorded_len.to_string(),
                "emitted_bytes" => code.len().to_string()
            }
        ));
    }

    let mut offset = 0;
    for inst in instructions {
        let end = offset + inst.bytes.len();
        inst.bytes.copy_from_slice(&code[offset..end]);
        offset = end;
    }
    Ok(())
}

/// Result of emitting a function's machine code.
///
/// This holds all emitted instructions along with metadata needed for
/// linking (relocations, labels for fixups).
#[derive(Debug)]
pub struct EmittedCode {
    /// All emitted instructions in order.
    pub instructions: Vec<EmittedInst>,
    /// Relocations that need to be resolved by the linker.
    pub relocations: Vec<EmittedRelocation>,
}

impl EmittedCode {
    /// Create a new empty EmittedCode.
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            relocations: Vec::new(),
        }
    }

    /// Get the raw machine code bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.instructions
            .iter()
            .flat_map(|inst| inst.bytes.iter().copied())
            .collect()
    }

    /// Get the assembly text representation with byte offsets.
    pub fn to_asm(&self) -> String {
        let mut result = String::new();
        let mut offset = 0usize;

        for inst in &self.instructions {
            if inst.bytes.is_empty() {
                // Label or comment - no offset prefix
                result.push_str(&inst.asm);
            } else {
                // Instruction with bytes - show offset
                result.push_str(&format!("{:4x}:   {}", offset, inst.asm));
            }
            result.push('\n');
            offset += inst.bytes.len();
        }

        result
    }

    /// Get the total size in bytes.
    pub fn len(&self) -> usize {
        self.instructions.iter().map(|i| i.bytes.len()).sum()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }
}

impl Default for EmittedCode {
    fn default() -> Self {
        Self::new()
    }
}

/// Format an offset for assembly output.
///
/// This produces consistent assembly syntax for memory operand offsets:
/// - `0` → `""` (empty string, no offset shown)
/// - Positive values → `"+N"` (e.g., `+16`)
/// - Negative values → `"-N"` (e.g., `-8`)
///
/// Used by both x86-64 and aarch64 emitters for generating readable assembly.
pub fn format_offset(offset: i32) -> String {
    if offset == 0 {
        String::new()
    } else if offset > 0 {
        format!("+{}", offset)
    } else {
        format!("{}", offset)
    }
}

// Re-export the generate function for x86_64 (default)
pub use x86_64::generate;

// Re-export shared types
pub use cfg_lower::{
    BlockLoweringInfo, LoweringDebugInfo, LoweringDecision, TerminatorLoweringDecision,
};
pub use index_map::{Handle, IndexMap};
pub use regalloc::{
    Allocation, InstructionLiveness, LivenessDebugInfo, RegAllocDebugInfo, RematerializeOp,
    VRegInfo, linear_scan_with_debug, linear_scan_with_remat,
};
pub use stack_frame::{
    ArgumentLocation, ReturnLocation, StackFrameInfo, StackSlot, generate_stack_frame_info,
};
pub use vreg::{LabelId, VReg};

// Re-export commonly used x86_64 types for convenience
pub use x86_64::{Operand, Reg, X86Inst, X86Mir};

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;
    use rue_air::{AirEditor, AirValidationContext, FrozenTypeInternPool, Type, TypeInternPool};
    use rue_cfg::{CfgBuilder, ValidatedCfg};
    use rue_span::Span;

    fn test_cfg() -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let mut air = AirEditor::new(Type::I32);

        let const_ref = air.add_const(42, Type::I32, Span::new(0, 2));
        air.add_ret(Some(const_ref), Type::I32, Span::new(0, 2));

        let interner = ThreadedRodeo::new();
        let type_pool = FrozenTypeInternPool::new();
        let air = air
            .finish(AirValidationContext::Canonical(&type_pool))
            .expect("test AIR must validate");
        let cfg_output =
            CfgBuilder::build(&air, 0, 0, "main", &type_pool, vec![], &interner, false);
        (cfg_output.cfg.unwrap(), type_pool, interner)
    }

    fn aggregate_cfg(len: u64) -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let type_pool = TypeInternPool::new();
        let array_id = type_pool.intern_array_from_type(Type::I64, len);
        let type_pool = type_pool.freeze();
        let array_ty = Type::new_array(array_id);
        let mut air = AirEditor::new(array_ty);
        let span = Span::new(0, 2);

        let seed = air.add_const(1, Type::I64, span);
        let mut elements = Vec::with_capacity(len as usize);
        for value in 0..len {
            let rhs = air.add_const(value, Type::I64, span);
            elements.push(air.add_add(seed, rhs, Type::I64, span));
        }
        let array = air.add_array_init(&elements, array_ty, span).unwrap();
        air.add_ret(Some(array), array_ty, span);

        let interner = ThreadedRodeo::new();
        let air = air
            .finish(AirValidationContext::Canonical(&type_pool))
            .expect("test AIR must validate");
        let cfg_output = CfgBuilder::build(
            &air,
            3,
            2,
            "pipeline_parity",
            &type_pool,
            vec![false, false],
            &interner,
            false,
        );
        (cfg_output.cfg.unwrap(), type_pool, interner)
    }

    fn syscall_cfg() -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let type_pool = FrozenTypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let mut air = AirEditor::new(Type::I64);
        let span = Span::new(0, 2);
        let number = air.add_const(1, Type::I64, span);
        let result = air
            .add_intrinsic(
                None,
                interner.get_or_intern("syscall"),
                &[number],
                Type::I64,
                span,
            )
            .unwrap();
        air.add_ret(Some(result), Type::I64, span);

        let air = air
            .finish(AirValidationContext::Canonical(&type_pool))
            .expect("test AIR must validate");

        let cfg_output = CfgBuilder::build(
            &air,
            0,
            0,
            "syscall_parity",
            &type_pool,
            vec![],
            &interner,
            false,
        );
        (cfg_output.cfg.unwrap(), type_pool, interner)
    }

    fn assert_same_machine_code(normal: &MachineCode, with_asm: &MachineCode) {
        assert_eq!(normal.code, with_asm.code);
        assert_eq!(normal.strings, with_asm.strings);
        assert_eq!(normal.relocations.len(), with_asm.relocations.len());
        for (normal, with_asm) in normal.relocations.iter().zip(&with_asm.relocations) {
            assert_eq!(normal.offset, with_asm.offset);
            assert_eq!(normal.symbol, with_asm.symbol);
            assert_eq!(normal.kind, with_asm.kind);
            assert_eq!(normal.addend, with_asm.addend);
        }
    }

    #[test]
    fn emitted_byte_synchronization_handles_labels_and_rejects_gaps() {
        let mut instructions = vec![
            EmittedInst::comment("before"),
            EmittedInst::new([0, 0], "first"),
            EmittedInst::label("target"),
            EmittedInst::new([0], "second"),
        ];
        synchronize_emitted_bytes(&mut instructions, &[1, 2, 3])
            .expect("recorded instruction lengths cover the final code");
        assert_eq!(instructions[0].bytes, []);
        assert_eq!(instructions[1].bytes, [1, 2]);
        assert_eq!(instructions[2].bytes, []);
        assert_eq!(instructions[3].bytes, [3]);

        let error = synchronize_emitted_bytes(&mut instructions, &[1, 2, 3, 4])
            .expect_err("a byte-coverage gap must be rejected");
        assert!(
            error
                .to_string()
                .contains("assembly instruction byte coverage mismatch")
        );
    }

    #[test]
    fn test_generate_x86_64() {
        let (cfg, type_pool, interner) = test_cfg();

        // Test the generate function
        let machine_code = generate(&cfg, &type_pool, &[], &interner).unwrap();

        // Should generate working code
        assert!(!machine_code.code.is_empty());

        // Code should end with call rel32 (E8 xx xx xx xx)
        // The last 5 bytes should be the call instruction
        let len = machine_code.code.len();
        assert!(len >= 5);
        assert_eq!(machine_code.code[len - 5], 0xE8); // call opcode

        // Should have one relocation for __rue_exit
        assert_eq!(machine_code.relocations.len(), 1);
        assert_eq!(machine_code.relocations[0].symbol, "__rue_exit");

        // Should have no strings
        assert!(machine_code.strings.is_empty());
    }

    #[test]
    fn normal_and_assembly_entry_points_match_with_sret_and_spills() {
        let (threshold_cfg, threshold_types, _) = aggregate_cfg(7);
        assert!(cfg_lower::fn_uses_sret_return(
            &threshold_cfg,
            &threshold_types,
            6
        ));
        assert!(!cfg_lower::fn_uses_sret_return(
            &threshold_cfg,
            &threshold_types,
            8
        ));

        let (cfg, type_pool, interner) = aggregate_cfg(40);
        assert!(cfg_lower::fn_uses_sret_return(&cfg, &type_pool, 6));
        assert!(cfg_lower::fn_uses_sret_return(&cfg, &type_pool, 8));
        assert!(
            !x86_64::generate_regalloc_info(&cfg, &type_pool, &interner)
                .expect("x86 register allocation should succeed")
                .spills
                .is_empty(),
            "fixture must exercise x86 spills"
        );
        assert!(
            !aarch64::generate_regalloc_info(
                &cfg,
                &type_pool,
                &interner,
                rue_target::Target::Aarch64Linux,
            )
            .expect("AArch64 register allocation should succeed")
            .spills
            .is_empty(),
            "fixture must exercise AArch64 spills"
        );
        let strings = vec!["pipeline parity sentinel".to_owned()];

        let x86 = x86_64::generate(&cfg, &type_pool, &strings, &interner)
            .expect("x86 normal generation should succeed");
        let (x86_asm_code, x86_asm) =
            x86_64::generate_with_asm(&cfg, &type_pool, &strings, &interner)
                .expect("x86 assembly generation should succeed");
        assert_same_machine_code(&x86, &x86_asm_code);
        assert!(!x86_asm.is_empty());
        assert!(x86_asm.contains("jno "), "fixture must retain rel32 fixups");

        for target in [
            rue_target::Target::Aarch64Linux,
            rue_target::Target::Aarch64Macos,
        ] {
            let arm = aarch64::generate(&cfg, &type_pool, &strings, &interner, target)
                .expect("AArch64 normal generation should succeed");
            let (arm_asm_code, arm_asm) =
                aarch64::generate_with_asm(&cfg, &type_pool, &strings, &interner, target)
                    .expect("AArch64 assembly generation should succeed");
            assert_same_machine_code(&arm, &arm_asm_code);
            assert!(!arm_asm.is_empty());
            assert!(
                arm_asm.contains("b.vc "),
                "fixture must retain AArch64 branch fixups"
            );
        }
    }

    #[test]
    fn aarch64_entry_points_preserve_target_specific_syscall_lowering() {
        let (cfg, type_pool, interner) = syscall_cfg();
        let linux = aarch64::generate(
            &cfg,
            &type_pool,
            &[],
            &interner,
            rue_target::Target::Aarch64Linux,
        )
        .expect("AArch64 Linux normal generation should succeed");
        let (linux_asm_code, linux_asm) = aarch64::generate_with_asm(
            &cfg,
            &type_pool,
            &[],
            &interner,
            rue_target::Target::Aarch64Linux,
        )
        .expect("AArch64 Linux assembly generation should succeed");
        assert_same_machine_code(&linux, &linux_asm_code);
        assert!(linux_asm.contains("svc #0x0"));
        assert!(!linux_asm.contains("b.lo "));
        assert!(!linux_asm.contains("neg x0, x0"));

        let macos = aarch64::generate(
            &cfg,
            &type_pool,
            &[],
            &interner,
            rue_target::Target::Aarch64Macos,
        )
        .expect("AArch64 macOS normal generation should succeed");
        let (macos_asm_code, macos_asm) = aarch64::generate_with_asm(
            &cfg,
            &type_pool,
            &[],
            &interner,
            rue_target::Target::Aarch64Macos,
        )
        .expect("AArch64 macOS assembly generation should succeed");
        assert_same_machine_code(&macos, &macos_asm_code);
        assert!(macos_asm.contains("svc #0x80"));
        let svc = macos_asm
            .find("svc #0x80")
            .expect("macOS syscall must use Darwin's SVC immediate");
        let carry_test = macos_asm[svc..]
            .find("b.lo ")
            .map(|offset| svc + offset)
            .expect("macOS syscall must test for carry clear");
        let negation = macos_asm[carry_test..]
            .find("neg x0, x0")
            .map(|offset| carry_test + offset)
            .expect("macOS syscall must negate carry-set errno");
        assert!(svc < carry_test && carry_test < negation);
        assert_ne!(linux.code, macos.code);
    }
}
