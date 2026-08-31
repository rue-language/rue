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
mod backend;
pub mod call_plan;
mod codegen_pipeline;
pub mod export_thunk;
pub mod foreign_call;
mod local_storage;
mod param_storage;
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
pub mod frame_layout;
pub mod index_map;
pub mod liveness;
pub mod place_lower;
pub mod reg_class;
pub mod regalloc;
pub mod types;
pub mod vreg;
pub mod x86_64;

/// The kind of relocation emitted by the code generator.
///
/// This represents the semantic intent of the relocation. The linker
/// will map these to architecture-specific ELF/Mach-O relocation types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Borrowed compiler authority for projecting legacy live callable names to
/// canonical machine symbols. Runtime helpers never pass through this map.
///
/// It also carries the set of `extern "C"` foreign symbols (ADR-0064): a call to
/// one of these resolves to its raw (unmangled) C name and crosses under the
/// target-C ABI, so the backend must apply the boundary's narrow-integer
/// extension to a scalar return ([`is_foreign`](Self::is_foreign)).
#[derive(Clone, Copy, Default)]
pub struct MachineSymbolResolver<'a> {
    mappings: Option<&'a std::collections::BTreeMap<String, String>>,
    foreign: Option<&'a std::collections::BTreeSet<String>>,
}

impl<'a> MachineSymbolResolver<'a> {
    pub fn new(mappings: &'a std::collections::BTreeMap<String, String>) -> Self {
        Self {
            mappings: Some(mappings),
            foreign: None,
        }
    }

    /// Build a resolver that also knows which resolved symbols are `extern "C"`
    /// foreign declarations (ADR-0064 C FFI). `foreign` holds their raw C names.
    pub fn new_with_foreign(
        mappings: &'a std::collections::BTreeMap<String, String>,
        foreign: &'a std::collections::BTreeSet<String>,
    ) -> Self {
        Self {
            mappings: Some(mappings),
            foreign: Some(foreign),
        }
    }

    pub fn resolve(&self, legacy_or_canonical: &str) -> String {
        self.mappings
            .and_then(|mappings| mappings.get(legacy_or_canonical))
            .cloned()
            .unwrap_or_else(|| legacy_or_canonical.to_owned())
    }

    /// Whether `machine_symbol` (already resolved via [`resolve`](Self::resolve))
    /// names an `extern "C"` foreign function. A foreign symbol maps to itself,
    /// so the resolved name is its raw C name.
    pub fn is_foreign(&self, machine_symbol: &str) -> bool {
        self.foreign
            .is_some_and(|foreign| foreign.contains(machine_symbol))
    }
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
#[derive(Debug, Clone)]
pub struct MachineCode {
    /// The generated machine code bytes.
    pub code: Vec<u8>,
    /// Relocations that need to be resolved by the linker.
    pub relocations: Vec<EmittedRelocation>,
    /// String table referenced by the code.
    pub strings: Vec<String>,
}

/// Optional observations captured while the production backend pipeline runs.
///
/// These flags never select a different lowering, allocation, scheduling, or
/// emission implementation. They only retain diagnostic projections from the
/// exact execution which produces [`MachineCode`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackendArtifactRequest {
    pub lowering: bool,
    pub mir: bool,
    pub liveness: bool,
    pub regalloc: bool,
    pub asm: bool,
}

/// Diagnostic projections retained by one production backend execution.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackendArtifacts {
    pub lowering: Option<LoweringDebugInfo>,
    pub mir: Option<String>,
    pub liveness: Option<String>,
    pub regalloc: Option<String>,
    pub asm: Option<String>,
}

/// Canonical per-function backend product before object-file projection.
#[derive(Debug, Clone)]
pub struct BackendProduct {
    pub machine_code: MachineCode,
    pub artifacts: BackendArtifacts,
}

/// Cooperative cancellation authority for one backend generation call
/// (RUE-1827).
///
/// This crate has no query-runtime dependency, so cancellation arrives as a
/// caller-owned probe; the query layer builds one from its cancellation
/// token. [`GenerationCancellation::NONE`] makes every check a no-op branch,
/// keeping callers without an authority — tests, drivers, tools — ergonomic
/// and free of overhead. The shared pipeline checks at stage boundaries, and
/// the per-block lowering walk, the per-instruction allocation rewrite, and
/// the per-instruction emission loops check inside their unbounded work, so
/// both architectures share one cancellation contract.
#[derive(Clone, Copy, Default)]
pub struct GenerationCancellation<'a> {
    probe: Option<&'a dyn Fn() -> bool>,
}

/// Exact marker carried by a cancellation rejection. The message never
/// reaches users: the query layer converts it to its own abort before the
/// error can surface, via [`is_generation_canceled`].
const GENERATION_CANCELED: &str = "backend generation canceled";

impl<'a> GenerationCancellation<'a> {
    /// No cancellation authority: every check is a no-op.
    pub const NONE: GenerationCancellation<'static> = GenerationCancellation { probe: None };

    /// A cancellation authority backed by `probe`; generation stops at the
    /// next check once the probe returns true.
    pub fn from_probe(probe: &'a dyn Fn() -> bool) -> Self {
        Self { probe: Some(probe) }
    }

    /// Fail cooperatively when the caller's authority reports cancellation.
    pub fn check(self) -> rue_error::CompileResult<()> {
        match self.probe {
            Some(probe) if probe() => Err(rue_error::CompileError::without_span(
                rue_error::ErrorKind::InternalError(GENERATION_CANCELED.to_owned()),
            )),
            _ => Ok(()),
        }
    }
}

/// Whether `error` is this crate's cooperative-cancellation rejection.
/// Callers with a cancellation authority use this to convert the rejection
/// into their own abort channel instead of recording a codegen failure.
pub fn is_generation_canceled(error: &rue_error::CompileError) -> bool {
    matches!(&error.kind, rue_error::ErrorKind::InternalError(message) if message == GENERATION_CANCELED)
}

/// Stable local-data identity projected onto the current program string table.
///
/// Multiple occurrence identities may intentionally share one dense ID when
/// equal-content literals are coalesced. Codegen validates this projection
/// before compacting the table for a function-local object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalAtomProjection<'a> {
    pub stable_id: &'a str,
    pub dense_id: u32,
    pub content: &'a str,
}

/// Build a function-local string table and remap MIR string IDs to it.
///
/// Semantic analysis assigns IDs in the program-wide string table. Object files
/// are emitted per function, so retaining that whole table in every object is
/// both unnecessary and quadratic in programs with many functions. Ordering
/// local strings by their global ID keeps the object bytes deterministic.
fn compact_string_table(
    strings: &[String],
    atoms: &[LocalAtomProjection<'_>],
    referenced_ids: impl IntoIterator<Item = u32>,
    require_complete_atoms: bool,
) -> rue_error::CompileResult<(Vec<String>, std::collections::BTreeMap<u32, u32>)> {
    let mut stable_ids = std::collections::BTreeSet::new();
    let mut projected_dense_ids = std::collections::BTreeSet::new();
    for atom in atoms {
        if atom.stable_id.is_empty()
            || !stable_ids.insert(atom.stable_id)
            || strings.get(atom.dense_id as usize).map(String::as_str) != Some(atom.content)
        {
            return Err(rue_error::CompileError::without_span(
                rue_error::ErrorKind::InternalCodegenError(
                    "invalid stable local-atom string projection".to_owned(),
                ),
            ));
        }
        projected_dense_ids.insert(atom.dense_id);
    }
    let referenced_ids: std::collections::BTreeSet<_> = referenced_ids.into_iter().collect();
    if require_complete_atoms
        && referenced_ids
            .iter()
            .any(|dense_id| !projected_dense_ids.contains(dense_id))
    {
        return Err(rue_error::CompileError::without_span(
            rue_error::ErrorKind::InternalCodegenError(
                "referenced local string has no stable local-atom projection".to_owned(),
            ),
        ));
    }
    let mut remap = std::collections::BTreeMap::new();
    let mut local_strings = Vec::with_capacity(referenced_ids.len());

    for global_id in referenced_ids {
        let local_id = local_strings.len() as u32;
        let Some(string) = strings.get(global_id as usize) else {
            return Err(rue_error::CompileError::without_span(
                rue_error::ErrorKind::InternalCodegenError(format!(
                    "string ID {global_id} is absent from the global table"
                )),
            ));
        };
        local_strings.push(string.clone());
        remap.insert(global_id, local_id);
    }

    Ok((local_strings, remap))
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

// Re-export shared types
pub use cfg_lower::{
    BlockLoweringInfo, LoweringDebugInfo, LoweringDecision, TerminatorLoweringDecision,
};
pub use index_map::{Handle, IndexMap};
pub use reg_class::{RegClass, VRegClasses};
pub use regalloc::{
    Allocation, InstructionLiveness, LivenessDebugInfo, RegAllocDebugInfo, RegisterFile,
    RematerializeOp, SaveClasses, VRegInfo, linear_scan_with_debug, linear_scan_with_remat,
};
pub use stack_frame::{
    ArgumentLocation, ReturnLocation, StackFrameInfo, StackSlot, generate_stack_frame_info,
};
pub use vreg::{LabelId, VReg};

#[cfg(test)]
mod tests {
    use super::*;
    use aarch64::MAX_ADD_SUB_IMMEDIATE;
    use lasso::ThreadedRodeo;
    use rue_air::{
        AirEditor, AirValidationContext, FrozenTypeInternPool, IntrinsicOperation, StructDef,
        StructField, Type, TypeInternPool, layout::SLOT_BYTES,
    };
    use rue_cfg::{Cfg, CfgBuilder, CfgInst, CfgInstData, ValidatedCfg};
    use rue_span::{FileId, Span};

    #[test]
    fn stable_atom_aliases_compact_to_one_dense_string_deterministically() {
        let strings = vec!["other".to_owned(), "same".to_owned()];
        let atoms = [
            LocalAtomProjection {
                stable_id: "atom-b",
                dense_id: 1,
                content: "same",
            },
            LocalAtomProjection {
                stable_id: "atom-a",
                dense_id: 1,
                content: "same",
            },
        ];
        let (compacted, remap) = compact_string_table(&strings, &atoms, [1, 1], true).unwrap();
        assert_eq!(compacted, ["same"]);
        assert_eq!(remap.into_iter().collect::<Vec<_>>(), [(1, 0)]);

        let mut shuffled = atoms;
        shuffled.reverse();
        assert_eq!(
            compact_string_table(&strings, &shuffled, [1], true)
                .unwrap()
                .0,
            compacted
        );
    }

    #[test]
    fn stable_atom_projection_rejects_duplicate_identity_and_content_mismatch() {
        let strings = vec!["actual".to_owned()];
        let duplicate = [
            LocalAtomProjection {
                stable_id: "same-id",
                dense_id: 0,
                content: "actual",
            },
            LocalAtomProjection {
                stable_id: "same-id",
                dense_id: 0,
                content: "actual",
            },
        ];
        assert!(compact_string_table(&strings, &duplicate, [0], true).is_err());
        assert!(
            compact_string_table(
                &strings,
                &[LocalAtomProjection {
                    stable_id: "atom",
                    dense_id: 0,
                    content: "wrong",
                }],
                [0],
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn strict_atom_projection_rejects_unidentified_referenced_strings() {
        let strings = vec!["identified".to_owned(), "unidentified".to_owned()];
        let atoms = [LocalAtomProjection {
            stable_id: "stable-identified",
            dense_id: 0,
            content: "identified",
        }];
        assert!(compact_string_table(&strings, &atoms, [1], true).is_err());
        assert_eq!(
            compact_string_table(&strings, &atoms, [1], false)
                .unwrap()
                .0,
            ["unidentified"]
        );
    }

    fn test_cfg() -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        test_cfg_with_locals(0)
    }

    fn test_cfg_with_locals(
        num_locals: u32,
    ) -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        test_cfg_with_locals_named(num_locals, "main")
    }

    fn test_cfg_with_locals_named(
        num_locals: u32,
        fn_name: &str,
    ) -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let mut air = AirEditor::new(Type::I32);

        let const_ref = air.add_const(42, Type::I32, Span::new(0, 2));
        air.add_ret(Some(const_ref), Type::I32, Span::new(0, 2));

        let interner = ThreadedRodeo::new();
        let type_pool = FrozenTypeInternPool::new();
        let air = air
            .finish(AirValidationContext::Canonical(&type_pool))
            .expect("test AIR must validate");
        let cfg_output = CfgBuilder::build(
            &air,
            num_locals,
            0,
            fn_name,
            &type_pool,
            vec![],
            &interner,
            false,
            rue_air::AnalyzedCallableKind::Ordinary,
        );
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
            rue_air::AnalyzedCallableKind::Ordinary,
        );
        (cfg_output.cfg.unwrap(), type_pool, interner)
    }

    fn syscall_cfg() -> (ValidatedCfg, FrozenTypeInternPool, ThreadedRodeo) {
        let type_pool = FrozenTypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let mut air = AirEditor::new(Type::I64);
        let span = Span::new(0, 2);
        let number = air.add_const(1, Type::U64, span);
        let result = air
            .add_intrinsic(
                rue_air::IntrinsicOperation::Syscall,
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
            rue_air::AnalyzedCallableKind::Ordinary,
        );
        (cfg_output.cfg.unwrap(), type_pool, interner)
    }

    fn diagnostic_intrinsic_cfg(
        operation: IntrinsicOperation,
        diagnostic_name: &str,
    ) -> (
        ValidatedCfg,
        FrozenTypeInternPool,
        ThreadedRodeo,
        Vec<String>,
    ) {
        let type_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let ptr_u8 = Type::new_ptr_const(type_pool.intern_ptr_const_from_type(Type::U8));
        let (str_id, _) = type_pool.register_struct(
            interner.get_or_intern("str"),
            StructDef {
                name: "str".into(),
                fields: vec![
                    StructField {
                        name: "ptr".into(),
                        ty: ptr_u8,
                    },
                    StructField {
                        name: "len".into(),
                        ty: Type::U64,
                    },
                ],
                is_copy: true,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: true,
                is_pub: true,
                file_id: FileId::DEFAULT,
            },
        );
        let str_ty = Type::new_struct(str_id);
        let result_ty = if matches!(
            operation,
            IntrinsicOperation::PanicNoMessage | IntrinsicOperation::Panic
        ) {
            Type::NEVER
        } else {
            Type::UNIT
        };
        let type_pool = type_pool.freeze();
        let mut cfg = Cfg::new(
            result_ty,
            0,
            0,
            "typed_intrinsic_dispatch".to_owned(),
            Vec::<bool>::new(),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let span = Span::new(0, 1);
        let mut append = |data, ty| cfg.append_inst(entry, CfgInst { data, ty, span });
        let args = match operation {
            IntrinsicOperation::PanicNoMessage => vec![],
            IntrinsicOperation::Panic | IntrinsicOperation::DebugStr => {
                vec![append(CfgInstData::StringConst(0), str_ty)]
            }
            IntrinsicOperation::AssertFailed | IntrinsicOperation::BoundsCheck => {
                vec![append(CfgInstData::BoolConst(false), Type::BOOL)]
            }
            IntrinsicOperation::AssertWithMessage => vec![
                append(CfgInstData::BoolConst(false), Type::BOOL),
                append(CfgInstData::StringConst(0), str_ty),
            ],
            IntrinsicOperation::DebugI64 => vec![append(CfgInstData::Const(7), Type::I64)],
            IntrinsicOperation::DebugU64 => vec![append(CfgInstData::Const(7), Type::U64)],
            IntrinsicOperation::DebugBool => {
                vec![append(CfgInstData::BoolConst(true), Type::BOOL)]
            }
            IntrinsicOperation::ReadLine
            | IntrinsicOperation::ParseI32
            | IntrinsicOperation::ParseI64
            | IntrinsicOperation::ParseU32
            | IntrinsicOperation::ParseU64
            | IntrinsicOperation::RandomU32
            | IntrinsicOperation::RandomU64
            | IntrinsicOperation::PtrToInt
            | IntrinsicOperation::IntToPtr
            | IntrinsicOperation::PtrRead
            | IntrinsicOperation::PtrReadUnaligned
            | IntrinsicOperation::PtrWrite
            | IntrinsicOperation::PtrWriteUnaligned
            | IntrinsicOperation::PtrOffset
            | IntrinsicOperation::Alloc
            | IntrinsicOperation::AllocZeroed
            | IntrinsicOperation::Free
            | IntrinsicOperation::Realloc
            | IntrinsicOperation::Resize
            | IntrinsicOperation::ByteCopy
            | IntrinsicOperation::ByteMove
            | IntrinsicOperation::ByteSet
            | IntrinsicOperation::ArgCount
            | IntrinsicOperation::ArgPtr
            | IntrinsicOperation::ArgLen
            | IntrinsicOperation::EnvCount
            | IntrinsicOperation::EnvPtr
            | IntrinsicOperation::EnvLen
            | IntrinsicOperation::Raw
            | IntrinsicOperation::RawMut
            | IntrinsicOperation::FieldPtr
            | IntrinsicOperation::Syscall
            | IntrinsicOperation::BitCast => {
                panic!("fixture only accepts trap and debug operations")
            }
        };
        let _intrinsic = cfg
            .append_intrinsic_operation(
                entry,
                operation,
                interner.get_or_intern(diagnostic_name),
                args,
                result_ty,
                span,
            )
            .unwrap();
        if result_ty == Type::NEVER {
            cfg.set_unreachable(entry);
        } else {
            cfg.set_return(entry, None);
        }
        (
            cfg.finish(&type_pool)
                .expect("typed intrinsic fixture must validate"),
            type_pool,
            interner,
            vec!["typed diagnostic payload".to_owned()],
        )
    }

    fn pointer_zero_cfgs() -> (
        ValidatedCfg,
        ValidatedCfg,
        FrozenTypeInternPool,
        ThreadedRodeo,
    ) {
        let type_pool = TypeInternPool::new();
        let ptr_const = Type::new_ptr_const(type_pool.intern_ptr_const_from_type(Type::I32));
        let ptr_mut = Type::new_ptr_mut(type_pool.intern_ptr_mut_from_type(Type::I32));
        let type_pool = type_pool.freeze();
        let interner = ThreadedRodeo::new();
        let span = Span::new(0, 1);

        let build = |synthesized_const: bool| {
            let mut cfg = Cfg::new(
                Type::U64,
                0,
                0,
                "empty_slice_pointer_bytes".to_owned(),
                Vec::<bool>::new(),
            );
            let entry = cfg.new_block();
            cfg.entry = entry;
            let pointer = if synthesized_const {
                cfg.append_inst(
                    entry,
                    CfgInst {
                        data: CfgInstData::Const(0),
                        ty: ptr_const,
                        span,
                    },
                )
            } else {
                let zero = cfg.append_inst(
                    entry,
                    CfgInst {
                        data: CfgInstData::Const(0),
                        ty: Type::U64,
                        span,
                    },
                );
                cfg.append_intrinsic_operation(
                    entry,
                    IntrinsicOperation::IntToPtr,
                    interner.get_or_intern("int_to_ptr"),
                    [zero],
                    ptr_mut,
                    span,
                )
                .unwrap()
            };
            let address = cfg
                .append_intrinsic_operation(
                    entry,
                    IntrinsicOperation::PtrToInt,
                    interner.get_or_intern("ptr_to_int"),
                    [pointer],
                    Type::U64,
                    span,
                )
                .unwrap();
            cfg.set_return(entry, Some(address));
            cfg.finish(&type_pool)
                .expect("pointer fixture must validate")
        };
        (build(true), build(false), type_pool, interner)
    }

    /// Assert that a `--emit regalloc` projection exercises the widened
    /// caller-saved class (RUE-1146) and never reports one of its registers as
    /// callee-saved.
    ///
    /// The projection covers every function in the fixture, so this pins both
    /// halves of the policy on the real pipeline: `caller_saved` names the
    /// first caller-saved candidate on the target, which allocation offers
    /// before any callee-saved register, and no such register may reach frame
    /// planning's save list.
    fn assert_widened_allocation(regalloc: &str, caller_saved: &str) {
        assert!(
            regalloc.contains(&format!("-> {caller_saved}")),
            "fixture must allocate the caller-saved register {caller_saved}"
        );
        for section in regalloc.split("Callee-saved registers used:").skip(1) {
            let reported = section.lines().nth(1).unwrap_or_default();
            assert!(
                !reported.contains(caller_saved),
                "{caller_saved} is caller-saved and must not reach the prologue \
                 save list, found: {reported}"
            );
        }
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

    /// Exercise the same production artifact path for every supported target.
    /// The target-specific MIR and emitter remain behind their qualified
    /// modules; this harness owns only the parity-level entry-point contract.
    fn generate_product_for_target(
        cfg: &ValidatedCfg,
        type_pool: &FrozenTypeInternPool,
        strings: &[String],
        interner: &ThreadedRodeo,
        target: rue_target::Target,
        request: BackendArtifactRequest,
    ) -> BackendProduct {
        match target.arch() {
            rue_target::Arch::X86_64 => x86_64::generate_product_with_symbols_and_atoms(
                cfg,
                type_pool,
                strings,
                interner,
                target,
                MachineSymbolResolver::default(),
                &[],
                request,
            ),
            rue_target::Arch::Aarch64 => aarch64::generate_product_with_symbols_and_atoms(
                cfg,
                type_pool,
                strings,
                interner,
                target,
                MachineSymbolResolver::default(),
                &[],
                request,
            ),
        }
        .expect("production backend generation should succeed")
    }

    /// Both backends share one cooperative cancellation contract (RUE-1827):
    /// a probe that trips mid-pipeline stops generation with the crate's
    /// cancellation rejection, a counting probe proves the pipeline actually
    /// consults the authority repeatedly (block/instruction-level checks),
    /// and a live-but-quiet authority leaves the product identical to an
    /// uncancellable run.
    #[test]
    fn generation_cancels_promptly_and_quiet_probes_change_nothing() {
        use std::cell::Cell;

        let (cfg, type_pool, interner) = test_cfg();
        for target in [
            rue_target::Target::X86_64Linux,
            rue_target::Target::Aarch64Linux,
        ] {
            let generate = |cancellation: GenerationCancellation<'_>| match target.arch() {
                rue_target::Arch::X86_64 => {
                    x86_64::generate_product_with_symbols_atoms_and_cancellation(
                        &cfg,
                        &type_pool,
                        &[],
                        &interner,
                        target,
                        MachineSymbolResolver::default(),
                        &[],
                        BackendArtifactRequest::default(),
                        cancellation,
                    )
                }
                rue_target::Arch::Aarch64 => {
                    aarch64::generate_product_with_symbols_atoms_and_cancellation(
                        &cfg,
                        &type_pool,
                        &[],
                        &interner,
                        target,
                        MachineSymbolResolver::default(),
                        &[],
                        BackendArtifactRequest::default(),
                        cancellation,
                    )
                }
            };

            // An authority canceled from the first probe stops at entry.
            let tripped = || true;
            let error = generate(GenerationCancellation::from_probe(&tripped))
                .expect_err("a tripped authority must stop generation");
            assert!(is_generation_canceled(&error), "{error:?}");

            // A counting authority that trips after a few checks stops
            // mid-pipeline; the check count proves the pipeline consulted
            // the authority more than once before tripping.
            let remaining = Cell::new(3_u32);
            let counting = || {
                if remaining.get() == 0 {
                    true
                } else {
                    remaining.set(remaining.get() - 1);
                    false
                }
            };
            let error = generate(GenerationCancellation::from_probe(&counting))
                .expect_err("a counting authority must stop generation mid-pipeline");
            assert!(is_generation_canceled(&error), "{error:?}");
            assert_eq!(remaining.get(), 0);

            // A quiet authority changes nothing relative to NONE.
            let quiet = || false;
            let with_authority = generate(GenerationCancellation::from_probe(&quiet)).unwrap();
            let without_authority = generate(GenerationCancellation::NONE).unwrap();
            assert_eq!(
                with_authority.machine_code.code,
                without_authority.machine_code.code
            );
            assert_eq!(
                with_authority.machine_code.relocations.len(),
                without_authority.machine_code.relocations.len()
            );
        }
    }

    fn generate_machine_code_for_target(
        cfg: &ValidatedCfg,
        type_pool: &FrozenTypeInternPool,
        strings: &[String],
        interner: &ThreadedRodeo,
        target: rue_target::Target,
    ) -> MachineCode {
        match target.arch() {
            rue_target::Arch::X86_64 => x86_64::generate(cfg, type_pool, strings, interner, target),
            rue_target::Arch::Aarch64 => {
                aarch64::generate(cfg, type_pool, strings, interner, target)
            }
        }
        .expect("production backend generation should succeed")
    }

    #[test]
    fn typed_trap_and_debug_dispatch_ignores_counterfeit_diagnostic_names_on_every_target() {
        let operations = [
            IntrinsicOperation::PanicNoMessage,
            IntrinsicOperation::Panic,
            IntrinsicOperation::AssertFailed,
            IntrinsicOperation::AssertWithMessage,
            IntrinsicOperation::BoundsCheck,
            IntrinsicOperation::DebugI64,
            IntrinsicOperation::DebugU64,
            IntrinsicOperation::DebugBool,
            IntrinsicOperation::DebugStr,
        ];
        for operation in operations {
            let (canonical, canonical_types, canonical_interner, canonical_strings) =
                diagnostic_intrinsic_cfg(operation, operation.expected_spelling());
            let (counterfeit, counterfeit_types, counterfeit_interner, counterfeit_strings) =
                diagnostic_intrinsic_cfg(operation, "counterfeit_diagnostic_name");
            for target in [
                rue_target::Target::X86_64Linux,
                rue_target::Target::Aarch64Linux,
                rue_target::Target::Aarch64Macos,
            ] {
                let canonical_code = generate_machine_code_for_target(
                    &canonical,
                    &canonical_types,
                    &canonical_strings,
                    &canonical_interner,
                    target,
                );
                let counterfeit_code = generate_machine_code_for_target(
                    &counterfeit,
                    &counterfeit_types,
                    &counterfeit_strings,
                    &counterfeit_interner,
                    target,
                );
                assert_same_machine_code(&canonical_code, &counterfeit_code);

                let helper = operation
                    .runtime_call_kind()
                    .expect("trap/debug operation is runtime backed")
                    .helper()
                    .symbol();
                assert!(
                    canonical_code
                        .relocations
                        .iter()
                        .any(|relocation| relocation.symbol == helper),
                    "{operation:?} on {target:?} must select {helper}"
                );
            }
        }
    }

    #[test]
    fn synthesized_empty_slice_pointer_const_preserves_previous_native_bytes() {
        let (pointer_const, previous_int_to_ptr, type_pool, interner) = pointer_zero_cfgs();
        for target in [
            rue_target::Target::X86_64Linux,
            rue_target::Target::Aarch64Linux,
            rue_target::Target::Aarch64Macos,
        ] {
            let current = generate_product_for_target(
                &pointer_const,
                &type_pool,
                &[],
                &interner,
                target,
                BackendArtifactRequest::default(),
            )
            .machine_code;
            let previous = generate_product_for_target(
                &previous_int_to_ptr,
                &type_pool,
                &[],
                &interner,
                target,
                BackendArtifactRequest::default(),
            )
            .machine_code;
            assert_same_machine_code(&current, &previous);
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
        let machine_code = x86_64::generate(
            &cfg,
            &type_pool,
            &[],
            &interner,
            rue_target::Target::X86_64Linux,
        )
        .unwrap();

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
    fn large_frame_codegen_emits_on_both_architectures_without_large_fixture_data() {
        // One slot above the immediate sequence's byte capacity forces the
        // real frame prologue/epilogue through the large-SP materialization
        // path. The CFG itself remains a constant return, so this allocates
        // only compact slot metadata rather than an aggregate-sized AIR body.
        let num_locals = (MAX_ADD_SUB_IMMEDIATE as u64 / SLOT_BYTES + 1) as u32;
        let (cfg, type_pool, interner) = test_cfg_with_locals_named(num_locals, "large_frame");

        let x86 = x86_64::generate(
            &cfg,
            &type_pool,
            &[],
            &interner,
            rue_target::Target::X86_64Linux,
        )
        .expect("x86-64 large frame generation should succeed");
        assert!(!x86.code.is_empty());

        let arm = aarch64::generate(
            &cfg,
            &type_pool,
            &[],
            &interner,
            rue_target::Target::Aarch64Linux,
        )
        .expect("AArch64 large frame generation should succeed");
        assert!(!arm.code.is_empty());

        let words = arm
            .code
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()));
        let words = words.collect::<Vec<_>>();
        let has_large_sp_adjust = words.iter().copied().any(|word| {
            (word & 0xFF200000) == 0xCB200000
                && ((word >> 16) & 0x1F) == 15
                && ((word >> 5) & 0x1F) == 31
                && (word & 0x1F) == 31
                && ((word >> 13) & 0x7) == 3
        }) && words.iter().copied().any(|word| {
            (word & 0xFF200000) == 0x8B200000
                && ((word >> 16) & 0x1F) == 15
                && ((word >> 5) & 0x1F) == 31
                && (word & 0x1F) == 31
                && ((word >> 13) & 0x7) == 3
        });
        assert!(
            has_large_sp_adjust,
            "AArch64 frame must use UXTX register ADD/SUB through x15: {words:x?}"
        );
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
        let strings = vec!["pipeline parity sentinel".to_owned()];
        let request = BackendArtifactRequest {
            lowering: true,
            mir: true,
            liveness: true,
            regalloc: true,
            asm: true,
        };

        let x86 = x86_64::generate(
            &cfg,
            &type_pool,
            &strings,
            &interner,
            rue_target::Target::X86_64Linux,
        )
        .expect("x86 normal generation should succeed");
        let x86_product = generate_product_for_target(
            &cfg,
            &type_pool,
            &strings,
            &interner,
            rue_target::Target::X86_64Linux,
            request,
        );
        assert_same_machine_code(&x86, &x86_product.machine_code);
        assert!(x86_product.artifacts.lowering.is_some());
        assert!(x86_product.artifacts.mir.is_some());
        assert!(x86_product.artifacts.liveness.is_some());
        let x86_regalloc = x86_product
            .artifacts
            .regalloc
            .expect("x86 register allocation projection");
        assert!(
            !x86_regalloc.contains("Spills:\n  none"),
            "fixture must exercise x86 spills"
        );
        assert_widened_allocation(&x86_regalloc, "r11");
        let x86_asm = x86_product.artifacts.asm.expect("x86 assembly projection");
        assert!(!x86_asm.is_empty());
        assert!(x86_asm.contains("jno "), "fixture must retain rel32 fixups");

        for target in [
            rue_target::Target::Aarch64Linux,
            rue_target::Target::Aarch64Macos,
        ] {
            let arm = aarch64::generate(&cfg, &type_pool, &strings, &interner, target)
                .expect("AArch64 normal generation should succeed");
            let arm_product =
                generate_product_for_target(&cfg, &type_pool, &strings, &interner, target, request);
            assert_same_machine_code(&arm, &arm_product.machine_code);
            assert!(arm_product.artifacts.lowering.is_some());
            assert!(arm_product.artifacts.mir.is_some());
            assert!(arm_product.artifacts.liveness.is_some());
            let arm_regalloc = arm_product
                .artifacts
                .regalloc
                .expect("AArch64 register allocation projection");
            assert!(
                !arm_regalloc.contains("Spills:\n  none"),
                "fixture must exercise AArch64 spills"
            );
            assert_widened_allocation(&arm_regalloc, "x13");
            let arm_asm = arm_product
                .artifacts
                .asm
                .expect("AArch64 assembly projection");
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
        let linux_product = aarch64::generate_product_with_symbols_and_atoms(
            &cfg,
            &type_pool,
            &[],
            &interner,
            rue_target::Target::Aarch64Linux,
            MachineSymbolResolver::default(),
            &[],
            BackendArtifactRequest {
                asm: true,
                ..Default::default()
            },
        )
        .expect("AArch64 Linux assembly generation should succeed");
        assert_same_machine_code(&linux, &linux_product.machine_code);
        let linux_asm = linux_product
            .artifacts
            .asm
            .expect("Linux assembly projection");
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
        let macos_product = aarch64::generate_product_with_symbols_and_atoms(
            &cfg,
            &type_pool,
            &[],
            &interner,
            rue_target::Target::Aarch64Macos,
            MachineSymbolResolver::default(),
            &[],
            BackendArtifactRequest {
                asm: true,
                ..Default::default()
            },
        )
        .expect("AArch64 macOS assembly generation should succeed");
        assert_same_machine_code(&macos, &macos_product.machine_code);
        let macos_asm = macos_product
            .artifacts
            .asm
            .expect("macOS assembly projection");
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
