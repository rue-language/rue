// ============================================================================
// Backend (code generation and linking)
// ============================================================================

/// Collect the raw C symbol names of every `extern "C"` foreign declaration in
/// the lowered program (ADR-0064 C FFI). These are the undefined symbols a call
/// site references and the linker resolves from a supplied static archive.
pub(crate) fn collect_foreign_symbols(rir: &rue_rir::Rir, interner: &ThreadedRodeo) -> Vec<String> {
    rir.iter()
        .filter_map(|(_, inst)| match &inst.data {
            rue_rir::InstData::FnDecl {
                is_extern: true,
                name,
                ..
            } => Some(interner.resolve(name).to_string()),
            _ => None,
        })
        .collect()
}

/// Collect the unmangled C symbol names of every `pub extern "C" fn` export in
/// the lowered program (ADR-0064 P4). Each is the raw name under which a C-ABI
/// entry thunk exposes the exported Rue function to separately compiled C
/// callers; the name is the export's source identifier (no mangling).
pub(crate) fn collect_export_symbols(rir: &rue_rir::Rir, interner: &ThreadedRodeo) -> Vec<String> {
    rir.iter()
        .filter_map(|(_, inst)| match &inst.data {
            rue_rir::InstData::FnDecl {
                is_c_export: true,
                name,
                ..
            } => Some(interner.resolve(name).to_string()),
            _ => None,
        })
        .collect()
}

/// Project live callable legacy names to their machine symbols, plus an
/// identity mapping for every `extern "C"` foreign declaration.
///
/// A foreign symbol maps to itself (no mangling, ADR-0064): a call site resolves
/// it to the raw C name, the object writer records it as an undefined external,
/// and the linker satisfies it from a static archive. The identity mapping also
/// lets `validate_production_call_relocations` recognize the raw name as a
/// declared foreign call rather than an unresolved glue symbol.
fn foreign_call_symbol_mappings(
    functions: &[FunctionWithCfg],
    foreign_symbols: &[String],
) -> std::collections::BTreeMap<String, String> {
    let mut symbol_mappings = functions
        .iter()
        .map(|function| (function.legacy_name.clone(), function.machine_name.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    for name in foreign_symbols {
        symbol_mappings
            .entry(name.clone())
            .or_insert_with(|| name.clone());
    }
    symbol_mappings
}

/// Compile analyzed functions to a binary.
///
/// This backend handles both architectures. It:
/// 1. Generates machine code for each function in parallel
/// 2. Creates object files with relocations
/// 3. Links them into an executable
///
/// This function is used by the sole one-shot compilation adapter.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_backend(
    functions: &[FunctionWithCfg],
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    options: &CompileOptions,
    warnings: &[CompileWarning],
    // Names of `extern "C"` foreign declarations (ADR-0064 C FFI). Each is an
    // undefined symbol a call site references by its raw (unmangled) name and
    // that the linker resolves from a supplied static archive.
    foreign_symbols: &[String],
    // Unmangled C names of `pub extern "C" fn` exports (ADR-0064 P4). Each gets
    // an additional C-ABI entry thunk object exposing that name globally.
    export_symbols: &[String],
) -> MultiErrorResult<CompileOutput> {
    // Check for main function
    let _main_fn = functions
        .iter()
        .find(|f| {
            matches!(
                f.symbol,
                crate::StableSymbolId::Callable(crate::StableCallableId::Compiler(
                    crate::CompilerCallableId::ProgramEntry
                ))
            ) && f.machine_name == "main"
        })
        .ok_or_else(|| {
            CompileErrors::from(CompileError::without_span(ErrorKind::NoMainFunction))
        })?;

    for function in functions {
        match &function.symbol {
            crate::StableSymbolId::Callable(crate::StableCallableId::Function(identity))
                if identity == &function.semantic_identity
                    && crate::StableSymbolEncoder::encode(&function.symbol)
                        == function.machine_name => {}
            crate::StableSymbolId::Callable(crate::StableCallableId::Compiler(
                crate::CompilerCallableId::ProgramEntry,
            )) if function.machine_name == "main" => {}
            _ => {
                return Err(CompileError::without_span(ErrorKind::InternalError(
                    "compiler function record has inconsistent semantic/symbol projection".into(),
                ))
                .into());
            }
        }
    }

    // Generate object files based on target architecture
    let mut object_files = match options.target.arch() {
        Arch::X86_64 => generate_x86_64_objects(
            functions,
            type_pool,
            strings,
            interner,
            options,
            foreign_symbols,
        )?,
        Arch::Aarch64 => generate_aarch64_objects(
            functions,
            type_pool,
            strings,
            interner,
            options,
            foreign_symbols,
        )?,
    };

    // Emit a C-ABI entry thunk object for every `pub extern "C" fn` export
    // (ADR-0064 P4). The native body was already generated above under its
    // mangled symbol; the thunk adds the unmangled global C entry point.
    object_files.extend(generate_export_thunk_objects(
        functions,
        options,
        export_symbols,
    ));

    // Link to executable
    match &options.linker {
        LinkerMode::Internal => link_internal_with_warnings(options, &object_files, warnings),
        LinkerMode::System(linker_cmd) => {
            link_system_with_warnings(options, &object_files, linker_cmd, warnings)
        }
    }
}

/// Generate x86-64 object files for all functions.
#[allow(clippy::too_many_arguments)]
fn generate_x86_64_objects(
    functions: &[FunctionWithCfg],
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    options: &CompileOptions,
    foreign_symbols: &[String],
) -> MultiErrorResult<Vec<Vec<u8>>> {
    let _span = info_span!("codegen", arch = "x86_64").entered();
    let symbol_mappings = foreign_call_symbol_mappings(functions, foreign_symbols);
    let foreign_set: std::collections::BTreeSet<String> = foreign_symbols.iter().cloned().collect();
    let symbols =
        rue_codegen::MachineSymbolResolver::new_with_foreign(&symbol_mappings, &foreign_set);

    let results: Vec<CompileResult<Vec<u8>>> = functions
        .par_iter()
        .map(|func| {
            let stable_atom_ids = func
                .local_atoms
                .iter()
                .map(|atom| {
                    crate::StableSymbolEncoder::encode(&crate::StableSymbolId::LocalAtom(
                        atom.identity.clone(),
                    ))
                })
                .collect::<Vec<_>>();
            let atom_projection = func
                .local_atoms
                .iter()
                .zip(&stable_atom_ids)
                .map(|(atom, stable_id)| rue_codegen::LocalAtomProjection {
                    stable_id,
                    dense_id: atom.dense_id,
                    content: &atom.content,
                })
                .collect::<Vec<_>>();
            let machine_code = rue_codegen::x86_64::generate_with_symbols_and_atoms(
                &func.cfg,
                type_pool,
                strings,
                interner,
                symbols,
                &atom_projection,
            )?;
            validate_production_call_relocations(&machine_code.relocations, &symbol_mappings)?;

            let mut obj_builder = ObjectBuilder::new(options.target, &func.machine_name)
                .code(machine_code.code)
                .strings(machine_code.strings);

            for reloc in machine_code.relocations {
                let rel_type = match reloc.kind {
                    RelocationKind::X86Pc32 => RelocationType::Pc32,
                    RelocationKind::X86Plt32 => RelocationType::Plt32,
                    RelocationKind::Aarch64AdrpPage21
                    | RelocationKind::Aarch64AddLo12
                    | RelocationKind::Aarch64Call26 => {
                        unreachable!("x86-64 codegen emitted AArch64 relocation {:?}", reloc.kind)
                    }
                };

                obj_builder = obj_builder.relocation(CodeRelocation {
                    offset: reloc.offset,
                    symbol: reloc.symbol,
                    rel_type,
                    addend: reloc.addend,
                });
            }

            Ok(obj_builder.build())
        })
        .collect();

    collect_codegen_results(results, functions.len())
}

/// Generate AArch64 object files for all functions.
#[allow(clippy::too_many_arguments)]
fn generate_aarch64_objects(
    functions: &[FunctionWithCfg],
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    options: &CompileOptions,
    foreign_symbols: &[String],
) -> MultiErrorResult<Vec<Vec<u8>>> {
    let _span = info_span!("codegen", arch = "aarch64").entered();
    let symbol_mappings = foreign_call_symbol_mappings(functions, foreign_symbols);
    let foreign_set: std::collections::BTreeSet<String> = foreign_symbols.iter().cloned().collect();
    let symbols =
        rue_codegen::MachineSymbolResolver::new_with_foreign(&symbol_mappings, &foreign_set);

    let results: Vec<CompileResult<Vec<u8>>> = functions
        .par_iter()
        .map(|func| {
            let stable_atom_ids = func
                .local_atoms
                .iter()
                .map(|atom| {
                    crate::StableSymbolEncoder::encode(&crate::StableSymbolId::LocalAtom(
                        atom.identity.clone(),
                    ))
                })
                .collect::<Vec<_>>();
            let atom_projection = func
                .local_atoms
                .iter()
                .zip(&stable_atom_ids)
                .map(|(atom, stable_id)| rue_codegen::LocalAtomProjection {
                    stable_id,
                    dense_id: atom.dense_id,
                    content: &atom.content,
                })
                .collect::<Vec<_>>();
            let machine_code = rue_codegen::aarch64::generate_with_symbols_and_atoms(
                &func.cfg,
                type_pool,
                strings,
                interner,
                options.target,
                symbols,
                &atom_projection,
            )?;
            validate_production_call_relocations(&machine_code.relocations, &symbol_mappings)?;

            let mut obj_builder = ObjectBuilder::new(options.target, &func.machine_name)
                .code(machine_code.code)
                .strings(machine_code.strings);

            for reloc in machine_code.relocations {
                let rel_type = match reloc.kind {
                    RelocationKind::Aarch64AdrpPage21 => RelocationType::AdrpPage21,
                    RelocationKind::Aarch64AddLo12 => RelocationType::AddLo12,
                    RelocationKind::Aarch64Call26 => RelocationType::Call26,
                    RelocationKind::X86Pc32 | RelocationKind::X86Plt32 => {
                        unreachable!("AArch64 codegen emitted x86-64 relocation {:?}", reloc.kind)
                    }
                };

                obj_builder = obj_builder.relocation(CodeRelocation {
                    offset: reloc.offset,
                    symbol: reloc.symbol,
                    rel_type,
                    addend: reloc.addend,
                });
            }

            Ok(obj_builder.build())
        })
        .collect();

    collect_codegen_results(results, functions.len())
}

/// Emit the C-ABI entry thunk objects for every `pub extern "C" fn` export
/// (ADR-0064 P4).
///
/// Each exported function was already code-generated as an ordinary native body
/// under its mangled `machine_name`; this adds one extra object per export whose
/// single global symbol is the unmangled C name (`legacy_name`, the source
/// identifier) and whose body receives arguments per the target-C convention,
/// re-extends narrow scalars, and forwards to the native body. The signature was
/// gated to register-resident scalars/pointers in semantic analysis
/// (`ExportSignatureUnsupported`), so the entry block's parameters are exactly
/// the argument-register scalars the thunk marshals.
fn generate_export_thunk_objects(
    functions: &[FunctionWithCfg],
    options: &CompileOptions,
    export_symbols: &[String],
) -> Vec<Vec<u8>> {
    if export_symbols.is_empty() {
        return Vec::new();
    }
    let export_set: std::collections::BTreeSet<&str> =
        export_symbols.iter().map(String::as_str).collect();
    let mut objects = Vec::new();
    for function in functions {
        if !export_set.contains(function.legacy_name.as_str()) {
            continue;
        }
        let cfg = &function.cfg;
        // A scalar parameter is materialized as a `Param { index }` instruction
        // carrying its type; parameter `index` arrives in the matching argument
        // register. Default every slot to a register-width scalar (no extension)
        // so an *unused* parameter — which the body never reads and therefore
        // needs no re-extension — is harmless. Semantic analysis restricted the
        // signature to register-resident scalars, so `num_params` slots map 1:1
        // to argument registers.
        let mut param_types: Vec<rue_air::Type> =
            vec![rue_air::Type::I64; cfg.num_params() as usize];
        for block in cfg.blocks() {
            for &value in &block.insts {
                let inst = cfg.get_inst(value);
                if let rue_cfg::CfgInstData::Param { index } = inst.data {
                    if let Some(slot) = param_types.get_mut(index as usize) {
                        *slot = inst.ty;
                    }
                }
            }
        }
        let machine_code = rue_codegen::export_thunk::generate_export_thunk(
            options.target,
            &function.machine_name,
            &param_types,
        );
        let mut obj_builder = ObjectBuilder::new(options.target, &function.legacy_name)
            .code(machine_code.code)
            .strings(machine_code.strings);
        for reloc in machine_code.relocations {
            let rel_type = match reloc.kind {
                RelocationKind::X86Plt32 => RelocationType::Plt32,
                RelocationKind::Aarch64Call26 => RelocationType::Call26,
                other => {
                    unreachable!("export thunk emitted unexpected relocation kind {other:?}")
                }
            };
            obj_builder = obj_builder.relocation(CodeRelocation {
                offset: reloc.offset,
                symbol: reloc.symbol,
                rel_type,
                addend: reloc.addend,
            });
        }
        objects.push(obj_builder.build());
    }
    objects
}

/// Collect codegen results, propagating errors and logging stats.
fn collect_codegen_results(
    results: Vec<CompileResult<Vec<u8>>>,
    function_count: usize,
) -> MultiErrorResult<Vec<Vec<u8>>> {
    let mut object_files = Vec::with_capacity(results.len());
    let mut total_object_bytes = 0usize;

    for result in results {
        let obj = result.map_err(CompileErrors::from)?;
        total_object_bytes += obj.len();
        object_files.push(obj);
    }

    info!(
        function_count,
        object_bytes = total_object_bytes,
        "codegen complete"
    );
    Ok(object_files)
}

/// Standalone codegen APIs retain their historical passthrough behavior, but
/// the compiler's production path must never let an unresolved source or glue
/// name reach the linker. Runtime exports and already-projected canonical
/// machine names are the only call relocations which may bypass a map lookup.
fn validate_production_call_relocations(
    relocations: &[rue_codegen::EmittedRelocation],
    symbol_mappings: &std::collections::BTreeMap<String, String>,
) -> CompileResult<()> {
    for relocation in relocations {
        let is_call = matches!(
            relocation.kind,
            RelocationKind::X86Plt32 | RelocationKind::Aarch64Call26
        );
        if !is_call
            || rue_runtime_abi::classify_export(&relocation.symbol).is_some()
            || symbol_mappings
                .values()
                .any(|machine_name| machine_name == &relocation.symbol)
        {
            continue;
        }
        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
            format!(
                "production machine-symbol resolver left source/glue call `{}` unresolved",
                relocation.symbol
            ),
        )));
    }
    Ok(())
}

/// Machine IR that can hold either x86-64 or AArch64 MIR.
///
/// This enum allows the `--emit mir` and `--emit asm` commands to work
/// with any target architecture.
pub enum Mir {
    /// x86-64 machine IR.
    X86_64(X86Mir),
    /// AArch64 machine IR.
    Aarch64(Aarch64Mir),
}

impl std::fmt::Display for Mir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mir::X86_64(mir) => write!(f, "{}", mir),
            Mir::Aarch64(mir) => write!(f, "{}", mir),
        }
    }
}

impl Mir {
    /// Format MIR as assembly text.
    ///
    /// This prints the MIR instructions in assembly-like format.
    /// When called with allocated MIR (post-regalloc), physical registers
    /// are shown (rax, rbx, r12 for x86-64; x0, x1, x19 for aarch64).
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn format_assembly(&self) -> String {
        let mut output = String::new();
        match self {
            Mir::X86_64(mir) => {
                use rue_codegen::x86_64::X86Inst;
                for inst in mir.instructions() {
                    match inst {
                        X86Inst::Label { id } => {
                            output.push_str(&format!("{}:\n", id));
                        }
                        X86Inst::CallRel { symbol_id } => {
                            output.push_str(&format!("    call {}\n", mir.get_symbol(*symbol_id)));
                        }
                        _ => {
                            output.push_str(&format!("    {}\n", inst));
                        }
                    }
                }
            }
            Mir::Aarch64(mir) => {
                use rue_codegen::aarch64::Aarch64Inst;
                for inst in mir.instructions() {
                    match inst {
                        Aarch64Inst::Label { id } => {
                            output.push_str(&format!("{}:\n", id));
                        }
                        Aarch64Inst::Bl { symbol_id } => {
                            output.push_str(&format!("    bl {}\n", mir.get_symbol(*symbol_id)));
                        }
                        _ => {
                            output.push_str(&format!("    {}\n", inst));
                        }
                    }
                }
            }
        }
        output
    }
}

/// Generate MIR from CFG for the given target (for debugging/inspection).
///
/// This returns the MIR before register allocation, with virtual registers.
pub fn generate_mir(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<Mir> {
    match target.arch() {
        Arch::X86_64 => {
            let mir = rue_codegen::x86_64::CfgLower::new(cfg, type_pool, interner).lower()?;
            Ok(Mir::X86_64(mir))
        }
        Arch::Aarch64 => {
            let mir =
                rue_codegen::aarch64::CfgLower::new(cfg, type_pool, interner, target).lower()?;
            Ok(Mir::Aarch64(mir))
        }
    }
}

/// Generate liveness debug information for a CFG.
///
/// This performs liveness analysis on the MIR (before register allocation)
/// and returns detailed per-instruction liveness information.
///
/// Used by `--emit liveness` to visualize which values are live at each program point.
pub fn generate_liveness_info(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<rue_codegen::LivenessDebugInfo> {
    match target.arch() {
        Arch::X86_64 => {
            let mir = rue_codegen::x86_64::CfgLower::new(cfg, type_pool, interner).lower()?;
            Ok(rue_codegen::x86_64::liveness::analyze_debug(&mir))
        }
        Arch::Aarch64 => {
            let mir =
                rue_codegen::aarch64::CfgLower::new(cfg, type_pool, interner, target).lower()?;
            Ok(rue_codegen::aarch64::liveness::analyze_debug(&mir))
        }
    }
}

/// Generate lowering debug information for a CFG.
///
/// This performs CFG-to-MIR lowering (instruction selection) and returns
/// detailed information about how each CFG instruction maps to MIR instructions.
///
/// Used by `--emit lowering` to visualize the instruction selection process.
pub fn generate_lowering_info(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<rue_codegen::LoweringDebugInfo> {
    match target.arch() {
        Arch::X86_64 => {
            let (_mir, debug_info) =
                rue_codegen::x86_64::CfgLower::new(cfg, type_pool, interner).lower_with_debug()?;
            Ok(debug_info)
        }
        Arch::Aarch64 => {
            let (_mir, debug_info) =
                rue_codegen::aarch64::CfgLower::new(cfg, type_pool, interner, target)
                    .lower_with_debug()?;
            Ok(debug_info)
        }
    }
}

/// Generate the actual emitted assembly text for a CFG.
///
/// Unlike `format_assembly()` on Mir which shows MIR instructions,
/// this returns the actual assembly that will be emitted, including
/// prologue/epilogue code that the emitter adds.
///
/// This is useful for debugging and for --emit asm output that accurately
/// reflects what's in the binary.
pub fn generate_emitted_asm(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<String> {
    match target.arch() {
        Arch::X86_64 => {
            let (_machine_code, asm) =
                rue_codegen::x86_64::generate_with_asm(cfg, type_pool, strings, interner)?;
            Ok(asm)
        }
        Arch::Aarch64 => {
            let (_machine_code, asm) =
                rue_codegen::aarch64::generate_with_asm(cfg, type_pool, strings, interner, target)?;
            Ok(asm)
        }
    }
}

/// Generate register allocation debug information for a CFG.
///
/// This returns information about the register allocation process,
/// including live ranges, interference edges, and allocation decisions.
/// The output is formatted as a human-readable string.
pub fn generate_regalloc_info(
    cfg: &Cfg,
    type_pool: &FrozenTypeInternPool,
    interner: &ThreadedRodeo,
    target: Target,
) -> CompileResult<String> {
    match target.arch() {
        Arch::X86_64 => {
            let debug_info = rue_codegen::x86_64::generate_regalloc_info(cfg, type_pool, interner)?;
            Ok(debug_info.to_string())
        }
        Arch::Aarch64 => {
            let debug_info =
                rue_codegen::aarch64::generate_regalloc_info(cfg, type_pool, interner, target)?;
            Ok(debug_info.to_string())
        }
    }
}

// ============================================================================
use rayon::prelude::*;
use tracing::{info, info_span};

use crate::linking::{link_internal_with_warnings, link_system_with_warnings};
use crate::*;

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use rue_linker::ObjectFile;

    use super::*;

    const FIRST_LITERAL: &[u8] = b"RUE784_FIRST_LITERAL";
    const SECOND_LITERAL: &[u8] = b"RUE784_SECOND_LITERAL";
    const SOURCE: &str = r#"
fn first() -> i32 {
    println("RUE784_FIRST_LITERAL");
    0
}

fn second() -> i32 {
    println("RUE784_SECOND_LITERAL");
    0
}

fn main() -> i32 {
    first() + second()
}
"#;

    fn count(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }

    #[test]
    fn production_call_symbol_validation_fails_closed_for_source_and_glue_names() {
        let mappings = std::collections::BTreeMap::from([(
            "legacy".to_owned(),
            "__rue_sem_v1_projected".to_owned(),
        )]);
        let relocation = |symbol: &str| rue_codegen::EmittedRelocation {
            offset: 0,
            symbol: symbol.to_owned(),
            kind: RelocationKind::X86Plt32,
            addend: -4,
        };

        assert!(
            validate_production_call_relocations(
                &[relocation("__rue_sem_v1_projected")],
                &mappings,
            )
            .is_ok()
        );
        assert!(
            validate_production_call_relocations(
                &[relocation(
                    rue_runtime_abi::RuntimeHelperId::DebugBool.symbol(),
                )],
                &mappings,
            )
            .is_ok()
        );
        assert!(validate_production_call_relocations(&[relocation("legacy")], &mappings).is_err());
        assert!(
            validate_production_call_relocations(
                &[relocation("__rue_drop_unprojected")],
                &mappings,
            )
            .is_err()
        );
    }

    fn frontend() -> (
        std::sync::Arc<CanonicalRirOutput>,
        std::sync::Arc<CanonicalSemanticOutput>,
    ) {
        let snapshot = SourceSnapshot::single("<rue-784>", SOURCE).unwrap();
        let (rir, semantic, _) =
            crate::test_support::test_frontend_snapshot(&snapshot, &CompileOptions::default())
                .unwrap();
        (rir, semantic)
    }

    fn strbuf_concat_frontend() -> (
        std::sync::Arc<CanonicalRirOutput>,
        std::sync::Arc<CanonicalSemanticOutput>,
    ) {
        let root = FileId::new(1);
        let strbuf = FileId::new(2);
        let metadata = SourceMetadata::new_with_trusted_standard_library(
            root,
            HashMap::from([
                (root, "/project/main.rue".to_owned()),
                (strbuf, "/project/std/strbuf.rue".to_owned()),
            ]),
            HashMap::from([
                (root, "main.rue".to_owned()),
                (strbuf, "\0rue-std/strbuf.rue".to_owned()),
            ]),
            HashSet::from([strbuf]),
        )
        .unwrap();
        let source = r#"
const strbuf = @import("std/strbuf.rue");

fn main() -> i32 {
    println("count: " + @to_string(3));
    println("sum: " + @to_string(13));
    0
}
"#;
        let snapshot = SourceSnapshot::new(
            metadata,
            vec![
                (root, Arc::new(source.to_owned())),
                (
                    strbuf,
                    Arc::new(
                        r#"
pub struct StrBuf {
    buf: ptr mut u8,
    len: u64,
    cap: u64,

    fn concat_borrowed(borrow first: Self, borrow second: Self) -> Self {
        Self { buf: first.buf, len: first.len + second.len, cap: 0 }
    }

    fn len(borrow self) -> u64 { self.len }
    fn as_ptr(borrow self) -> ptr mut u8 { self.buf }
}

drop fn StrBuf(self) { }
"#
                        .to_owned(),
                    ),
                ),
            ],
        )
        .unwrap();
        let (rir, semantic, _) =
            crate::test_support::test_frontend_snapshot(&snapshot, &CompileOptions::default())
                .unwrap();
        (rir, semantic)
    }

    fn assert_three_result_slots_cross_cleanup(target: Target) {
        let (rir, semantic) = strbuf_concat_frontend();
        let main = semantic
            .functions()
            .iter()
            .find(|function| function.analyzed.name == "main")
            .unwrap();
        let assembly = generate_emitted_asm(
            &main.cfg,
            semantic.type_pool(),
            semantic.strings(),
            rir.semantic_symbols().interner(),
            target,
        )
        .unwrap();
        let instructions = assembly
            .lines()
            .map(|line| line.split_once(":   ").map_or(line, |(_, inst)| inst))
            .collect::<Vec<_>>();
        let call_prefix = match target.arch() {
            Arch::X86_64 => "call ",
            Arch::Aarch64 => "bl ",
        };
        let calls = instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                instruction
                    .strip_prefix(call_prefix)
                    .map(|symbol| (index, symbol.to_owned()))
            })
            .collect::<Vec<_>>();
        let frame_offset = |instruction: &str, store: bool| match target.arch() {
            Arch::X86_64 => {
                let offset = if store {
                    instruction.strip_prefix("mov [rbp")?.split_once("],")?.0
                } else {
                    instruction.split_once(", [rbp")?.1.strip_suffix(']')?
                };
                offset.parse::<i32>().ok()
            }
            Arch::Aarch64 => {
                let operation = if store { "str " } else { "ldr " };
                instruction
                    .strip_prefix(operation)?
                    .split_once(", [fp, #")?
                    .1
                    .strip_suffix(']')?
                    .parse::<i32>()
                    .ok()
            }
        };
        let frame_accesses = |store| {
            instructions
                .iter()
                .enumerate()
                .filter_map(|(index, instruction)| {
                    frame_offset(instruction, store).map(|offset| (index, offset))
                })
                .collect::<Vec<_>>()
        };
        let stores = frame_accesses(true);
        let loads = frame_accesses(false);

        let concats: Vec<_> = calls
            .iter()
            .filter(|(_, symbol)| symbol.contains("concat_borrowed"))
            .collect();
        assert_eq!(concats.len(), 2, "{target}: expected both concatenations");

        for (concat_index, _) in concats {
            let println_index = calls
                .iter()
                .find(|(index, symbol)| {
                    index > concat_index && symbol.as_str() == "__rue_str_println"
                })
                .map(|(index, _)| *index)
                .expect("println after concatenation");
            let cleanups: Vec<_> = calls
                .iter()
                .filter(|(index, symbol)| {
                    index > concat_index
                        && *index < println_index
                        && (symbol.contains(".__drop") || symbol.starts_with("__rue_drop_"))
                })
                .map(|(index, _)| *index)
                .collect();
            let first_cleanup = *cleanups.first().expect("temporary cleanup before println");
            let last_cleanup = *cleanups.last().unwrap();
            let saved: HashSet<_> = stores
                .iter()
                .filter(|(index, _)| *index > *concat_index && *index < first_cleanup)
                .map(|(_, offset)| *offset)
                .collect();
            let restored: HashSet<_> = loads
                .iter()
                .filter(|(index, _)| *index > last_cleanup && *index < println_index)
                .map(|(_, offset)| *offset)
                .collect();
            let preserved: HashSet<_> = saved.intersection(&restored).copied().collect();
            assert_eq!(
                preserved.len(),
                3,
                "{target}: StrBuf pointer, length, and capacity must all cross cleanup in frame storage"
            );
        }
    }

    #[test]
    fn function_objects_only_contain_their_referenced_strings() {
        let (rir, semantic) = frontend();
        let interner = rir.semantic_symbols().interner();

        for target in [
            Target::X86_64Linux,
            Target::Aarch64Linux,
            Target::Aarch64Macos,
        ] {
            let options = CompileOptions {
                target,
                ..CompileOptions::default()
            };
            let objects = match target.arch() {
                Arch::X86_64 => generate_x86_64_objects(
                    semantic.functions(),
                    semantic.type_pool(),
                    semantic.strings(),
                    interner,
                    &options,
                    &[],
                ),
                Arch::Aarch64 => generate_aarch64_objects(
                    semantic.functions(),
                    semantic.type_pool(),
                    semantic.strings(),
                    interner,
                    &options,
                    &[],
                ),
            }
            .unwrap();

            let all_object_bytes: Vec<_> = objects.into_iter().flatten().collect();
            assert_eq!(count(&all_object_bytes, FIRST_LITERAL), 1, "{target}");
            assert_eq!(count(&all_object_bytes, SECOND_LITERAL), 1, "{target}");
        }
    }

    #[test]
    fn text_objects_and_runtime_archive_have_no_obsolete_string_symbols() {
        let (rir, semantic) = strbuf_concat_frontend();
        let interner = rir.semantic_symbols().interner();
        let obsolete =
            |name: &str| name.starts_with("__rue_String_") || name == "__rue_drop_String";

        for target in [
            Target::X86_64Linux,
            Target::Aarch64Linux,
            Target::Aarch64Macos,
        ] {
            let options = CompileOptions {
                target,
                ..CompileOptions::default()
            };
            let objects = match target.arch() {
                Arch::X86_64 => generate_x86_64_objects(
                    semantic.functions(),
                    semantic.type_pool(),
                    semantic.strings(),
                    interner,
                    &options,
                    &[],
                ),
                Arch::Aarch64 => generate_aarch64_objects(
                    semantic.functions(),
                    semantic.type_pool(),
                    semantic.strings(),
                    interner,
                    &options,
                    &[],
                ),
            }
            .unwrap();

            let mut undefined = HashSet::new();
            for bytes in objects {
                let object = ObjectFile::parse(&bytes).unwrap();
                undefined.extend(
                    object
                        .symbols
                        .iter()
                        .filter(|symbol| symbol.section_index.is_none())
                        .map(|symbol| symbol.name.clone()),
                );
            }
            assert!(undefined.contains("__rue_to_string"), "{target}");
            assert!(undefined.contains("__rue_str_println"), "{target}");
            assert!(
                undefined.iter().all(|name| !obsolete(name)),
                "{target}: obsolete undefined symbols: {undefined:?}"
            );
        }

        let options = CompileOptions::default();
        let runtime_bytes = crate::linking::runtime_for_target(options.target);
        let runtime = crate::linking::parse_runtime_archive(runtime_bytes).unwrap();
        let obsolete_exports: Vec<_> = runtime
            .objects
            .iter()
            .flat_map(|object| &object.symbols)
            .filter(|symbol| symbol.section_index.is_some() && obsolete(&symbol.name))
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert!(
            obsolete_exports.is_empty(),
            "runtime archive still contains obsolete exports: {obsolete_exports:?}"
        );

        compile_backend(
            semantic.functions(),
            semantic.type_pool(),
            semantic.strings(),
            interner,
            &options,
            &[],
            &[],
            &[],
        )
        .expect("ordinary source-defined StrBuf program must link without obsolete members");
    }

    #[test]
    fn final_binary_contains_each_function_literal_once() {
        let snapshot = SourceSnapshot::single("<rue-784>", SOURCE).unwrap();
        let output = compile_snapshot(&snapshot, &CompileOptions::default()).unwrap();
        assert_eq!(count(&output.elf, FIRST_LITERAL), 1);
        assert_eq!(count(&output.elf, SECOND_LITERAL), 1);

        let repeated = compile_snapshot(&snapshot, &CompileOptions::default()).unwrap();
        assert_eq!(output.elf, repeated.elf);
    }

    #[test]
    fn strbuf_concat_result_survives_cleanup_after_register_allocation() {
        assert_three_result_slots_cross_cleanup(Target::X86_64Linux);
        assert_three_result_slots_cross_cleanup(Target::Aarch64Linux);
    }
}
