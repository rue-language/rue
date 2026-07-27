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

/// Canonical per-function result of the production backend pipeline.
pub(crate) struct FunctionBackendProduct {
    pub(crate) machine_name: String,
    pub(crate) machine_code: rue_codegen::MachineCode,
    pub(crate) artifacts: rue_codegen::BackendArtifacts,
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
    let object_files = generate_pre_link_objects(
        functions,
        type_pool,
        strings,
        interner,
        options,
        foreign_symbols,
        export_symbols,
    )?;

    // Link to executable
    match &options.linker {
        LinkerMode::Internal => link_internal_with_warnings(options, &object_files, warnings),
        LinkerMode::System(linker_cmd) => {
            link_system_with_warnings(options, &object_files, linker_cmd, warnings)
        }
    }
}

/// Everything the backend does *before* linking: main-function validation, CFG
/// lowering, per-architecture code generation, and object-file creation with
/// relocations. This is the pre-link boundary the RUE-1086 scaling-bench runner
/// times as its `pre_link` interval; `compile_backend` calls it and then links
/// the returned objects.
#[allow(clippy::too_many_arguments)]
pub(crate) fn generate_pre_link_objects(
    functions: &[FunctionWithCfg],
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    options: &CompileOptions,
    foreign_symbols: &[String],
    export_symbols: &[String],
) -> MultiErrorResult<Vec<Vec<u8>>> {
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

    let products = generate_backend_products(
        functions,
        type_pool,
        strings,
        interner,
        options,
        foreign_symbols,
        rue_codegen::BackendArtifactRequest::default(),
    )?;
    let mut object_files = products
        .into_iter()
        .map(|product| project_backend_object(product, options.target))
        .collect::<CompileResult<Vec<_>>>()
        .map_err(CompileErrors::from)?;
    info!(
        function_count = functions.len(),
        object_bytes = object_files.iter().map(Vec::len).sum::<usize>(),
        "codegen complete"
    );

    // Emit a C-ABI entry thunk object for every `pub extern "C" fn` export
    // (ADR-0064 P4). The native body was already generated above under its
    // mangled symbol; the thunk adds the unmangled global C entry point.
    object_files.extend(generate_export_thunk_objects(
        functions,
        options,
        export_symbols,
    ));

    Ok(object_files)
}

/// Run the production per-function backend pipeline, optionally retaining
/// diagnostic projections from that exact execution.
#[allow(clippy::too_many_arguments)]
pub(crate) fn generate_backend_products(
    functions: &[FunctionWithCfg],
    type_pool: &FrozenTypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    options: &CompileOptions,
    foreign_symbols: &[String],
    request: rue_codegen::BackendArtifactRequest,
) -> MultiErrorResult<Vec<FunctionBackendProduct>> {
    // Rayon workers do not inherit the calling thread's current span, so the
    // per-function subphase spans inside rue-codegen would otherwise be
    // reported as extra timing roots. Hold the span by value and re-enter it
    // inside the closure so every worker's subphases nest under this one
    // `codegen` aggregate (RUE-786).
    let codegen_span = info_span!("codegen", arch = ?options.target.arch());
    let _entered = codegen_span.clone().entered();
    let symbol_mappings = foreign_call_symbol_mappings(functions, foreign_symbols);
    let foreign_set: std::collections::BTreeSet<String> = foreign_symbols.iter().cloned().collect();
    let symbols =
        rue_codegen::MachineSymbolResolver::new_with_foreign(&symbol_mappings, &foreign_set);

    let results: Vec<CompileResult<FunctionBackendProduct>> = functions
        .par_iter()
        .map(|func| {
            let _worker = codegen_span.enter();
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
            let mut product = match options.target.arch() {
                Arch::X86_64 => rue_codegen::x86_64::generate_product_with_symbols_and_atoms(
                    &func.cfg,
                    type_pool,
                    strings,
                    interner,
                    symbols,
                    &atom_projection,
                    request,
                )?,
                Arch::Aarch64 => rue_codegen::aarch64::generate_product_with_symbols_and_atoms(
                    &func.cfg,
                    type_pool,
                    strings,
                    interner,
                    options.target,
                    symbols,
                    &atom_projection,
                    request,
                )?,
            };
            if let Some(lowering) = &mut product.artifacts.lowering {
                lowering.fn_name.clone_from(&func.machine_name);
            }
            validate_production_call_relocations(
                &product.machine_code.relocations,
                &symbol_mappings,
            )?;
            Ok(FunctionBackendProduct {
                machine_name: func.machine_name.clone(),
                machine_code: product.machine_code,
                artifacts: product.artifacts,
            })
        })
        .collect();

    results
        .into_iter()
        .collect::<CompileResult<Vec<_>>>()
        .map_err(CompileErrors::from)
}

fn project_backend_object(
    product: FunctionBackendProduct,
    target: Target,
) -> CompileResult<Vec<u8>> {
    // Object serialization runs after the parallel codegen fan-out, so it is a
    // sibling leaf of `codegen` rather than one of its subphases (RUE-786).
    let _span = info_span!("object_serialization").entered();
    let mut obj_builder = ObjectBuilder::new(target, &product.machine_name)
        .code(product.machine_code.code)
        .strings(product.machine_code.strings);

    for reloc in product.machine_code.relocations {
        let rel_type = match (target.arch(), reloc.kind) {
            (Arch::X86_64, RelocationKind::X86Pc32) => RelocationType::Pc32,
            (Arch::X86_64, RelocationKind::X86Plt32) => RelocationType::Plt32,
            (Arch::Aarch64, RelocationKind::Aarch64AdrpPage21) => RelocationType::AdrpPage21,
            (Arch::Aarch64, RelocationKind::Aarch64AddLo12) => RelocationType::AddLo12,
            (Arch::Aarch64, RelocationKind::Aarch64Call26) => RelocationType::Call26,
            (arch, kind) => {
                return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
                    format!("{arch:?} codegen emitted incompatible relocation {kind:?}"),
                )));
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
        let options = CompileOptions {
            target,
            ..Default::default()
        };
        let interner = rir.semantic_symbols().interner();
        let foreign_symbols = collect_foreign_symbols(rir.rir(), interner);
        let products = generate_backend_products(
            semantic.functions(),
            semantic.type_pool(),
            semantic.strings(),
            interner,
            &options,
            &foreign_symbols,
            rue_codegen::BackendArtifactRequest {
                asm: true,
                ..Default::default()
            },
        )
        .unwrap();
        let assembly = products
            .into_iter()
            .find(|product| product.machine_name == "main")
            .and_then(|product| product.artifacts.asm)
            .expect("main assembly projection");
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

        let concat_symbols = semantic
            .functions()
            .iter()
            .filter(|function| function.analyzed.name.contains("concat_borrowed"))
            .map(|function| function.machine_name.as_str())
            .collect::<HashSet<_>>();
        let cleanup_symbols = semantic
            .functions()
            .iter()
            .filter(|function| {
                function.analyzed.name.contains(".__drop")
                    || function.analyzed.name.starts_with("__rue_drop_")
            })
            .map(|function| function.machine_name.as_str())
            .collect::<HashSet<_>>();
        let concats: Vec<_> = calls
            .iter()
            .filter(|(_, symbol)| concat_symbols.contains(symbol.as_str()))
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
                        && cleanup_symbols.contains(symbol.as_str())
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
            let objects = generate_pre_link_objects(
                semantic.functions(),
                semantic.type_pool(),
                semantic.strings(),
                interner,
                &options,
                &[],
                &[],
            )
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
            let objects = generate_pre_link_objects(
                semantic.functions(),
                semantic.type_pool(),
                semantic.strings(),
                interner,
                &options,
                &[],
                &[],
            )
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
