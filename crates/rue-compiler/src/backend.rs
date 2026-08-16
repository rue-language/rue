// ============================================================================
// Backend (code generation and linking)
// ============================================================================

/// Collect the unmangled C symbol names of every `pub extern "C" fn` export in
/// the lowered program (ADR-0064 P4). Each is the raw name under which a C-ABI
/// entry thunk exposes the exported Rue function to separately compiled C
/// callers; the name is the export's source identifier (no mangling).
#[cfg(test)]
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

/// Validate the function/symbol projection at the object-generation boundary.
/// The program-image adapter uses the same check before it serializes the
/// shared `CodegenUnit` terminals.
#[cfg(test)]
pub(crate) fn validate_backend_functions(
    functions: &[crate::session::RootedCfgUnit],
) -> MultiErrorResult<()> {
    if !functions
        .iter()
        .any(|function| function.record.codegen.defined_symbol.as_ref() == "main")
    {
        return Err(CompileError::without_span(ErrorKind::NoMainFunction).into());
    }
    for function in functions {
        let machine_name = function.record.codegen.defined_symbol.as_ref();
        if machine_name == "main" {
            continue;
        }
        let expected = crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
            crate::StableCallableId::Function(function.function.clone()),
        ));
        if expected != machine_name {
            return Err(CompileError::without_span(ErrorKind::InternalError(
                "compiler function record has inconsistent semantic/symbol projection".into(),
            ))
            .into());
        }
    }
    Ok(())
}

/// Project one canonical `CodegenUnit` into the linker-owned object builder.
/// The builder currently accepts owned strings and byte vectors, so this leaf
/// makes transient copies without retaining a second compiler-side product.
pub(crate) fn project_backend_object(
    unit: &crate::codegen_query::CodegenUnit,
    target: Target,
) -> CompileResult<Vec<u8>> {
    // Object serialization runs after code generation, so it is a sibling leaf
    // of `codegen` rather than one of its subphases (RUE-786).
    let _span = info_span!("object_serialization", phase = "object_generation").entered();
    use crate::codegen_query::{CodegenSection, SectionKind};
    let mut text: Option<&CodegenSection> = None;
    let mut rodata: Option<&CodegenSection> = None;
    let mut data: Option<&CodegenSection> = None;
    let mut bss: Option<&CodegenSection> = None;
    for section in unit.sections.iter() {
        let slot = match section.kind {
            SectionKind::Text => &mut text,
            SectionKind::Rodata => &mut rodata,
            SectionKind::Data => &mut data,
            SectionKind::Bss => &mut bss,
        };
        if slot.replace(section).is_some() {
            return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
                format!(
                    "object projection requires exactly one {:?} section, found multiple",
                    section.kind
                ),
            )));
        }
    }
    let text = text.ok_or_else(|| {
        CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection requires exactly one Text section, found 0".into(),
        ))
    })?;
    if (text.alignment, text.executable, text.writable) != (16, true, false) {
        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection found non-canonical Text metadata".into(),
        )));
    }
    if text.atoms.len() != 1 {
        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
            format!(
                "object projection requires exactly one text atom, found {}",
                text.atoms.len()
            ),
        )));
    }
    let rodata = rodata.ok_or_else(|| {
        CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection requires exactly one Rodata section, found 0".into(),
        ))
    })?;
    if (rodata.alignment, rodata.executable, rodata.writable) != (1, false, false) {
        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection found non-canonical Rodata metadata".into(),
        )));
    }
    let data = data.ok_or_else(|| {
        CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection requires exactly one Data section, found 0".into(),
        ))
    })?;
    if (data.alignment, data.executable, data.writable) != (1, false, true) {
        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection found non-canonical Data metadata".into(),
        )));
    }
    if !data.atoms.is_empty() {
        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection does not support Data atoms".into(),
        )));
    }
    let bss = bss.ok_or_else(|| {
        CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection requires exactly one Bss section, found 0".into(),
        ))
    })?;
    if (bss.alignment, bss.executable, bss.writable) != (1, false, true) {
        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection found non-canonical Bss metadata".into(),
        )));
    }
    if !bss.atoms.is_empty() {
        return Err(CompileError::without_span(ErrorKind::InternalCodegenError(
            "object projection does not support Bss atoms".into(),
        )));
    }
    let strings = rodata
        .atoms
        .iter()
        .map(|atom| {
            String::from_utf8(atom.to_vec()).map_err(|_| {
                CompileError::without_span(ErrorKind::InternalCodegenError(
                    "object projection encountered non-UTF-8 rodata atom".into(),
                ))
            })
        })
        .collect::<CompileResult<Vec<_>>>()?;
    let mut obj_builder = ObjectBuilder::new(target, unit.defined_symbol.to_string())
        .code(text.atoms[0].to_vec())
        .strings(strings);

    for reloc in unit.relocations.iter() {
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
            symbol: reloc.symbol.to_string(),
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
/// single global symbol is the unmangled C name (the export's source identifier)
/// and whose body receives arguments per the target-C convention, re-extends
/// narrow scalars, and forwards to the native body. The signature was gated to
/// register-resident scalars/pointers in semantic analysis
/// (`ExportSignatureUnsupported`), so the entry block's parameters are exactly
/// the argument-register scalars the thunk marshals.
#[cfg(test)]
pub(crate) fn generate_export_thunk_objects(
    functions: &[crate::session::RootedCfgUnit],
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
        let Some(exported_symbol) = function
            .definition_source_name()
            .filter(|name| export_set.contains(name))
        else {
            continue;
        };
        let cfg = &function.record.cfg;
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
        objects.push(generate_export_thunk_object(
            options.target,
            exported_symbol,
            &function.record.codegen.defined_symbol,
            &param_types,
        ));
    }
    objects
}

pub(crate) fn generate_export_thunk_object(
    target: Target,
    exported_symbol: &str,
    native_symbol: &str,
    param_types: &[rue_air::Type],
) -> Vec<u8> {
    let machine_code =
        rue_codegen::export_thunk::generate_export_thunk(target, native_symbol, param_types);
    let mut obj_builder = ObjectBuilder::new(target, exported_symbol)
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
    obj_builder.build()
}

/// Standalone codegen APIs retain their historical passthrough behavior, but
/// the compiler's production path must never let an unresolved source or glue
/// name reach the linker. Runtime exports and already-projected canonical
/// machine names are the only call relocations which may bypass a map lookup.
pub(crate) fn validate_production_call_relocations(
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
use tracing::info_span;

use crate::*;

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use rue_linker::{CodeRelocation, ObjectBuilder, ObjectFile, RelocationType};

    use crate::codegen_query::{CodegenSection, CodegenUnit, NormalizedRelocation, SectionKind};

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

    fn projection_section(kind: SectionKind, atoms: &[&[u8]]) -> CodegenSection {
        let (alignment, executable, writable) = match kind {
            SectionKind::Text => (16, true, false),
            SectionKind::Rodata => (1, false, false),
            SectionKind::Data | SectionKind::Bss => (1, false, true),
        };
        CodegenSection {
            kind,
            alignment,
            executable,
            writable,
            atoms: atoms
                .iter()
                .map(|atom| Arc::<[u8]>::from(*atom))
                .collect::<Vec<_>>()
                .into(),
        }
    }

    fn projection_unit(sections: Vec<CodegenSection>) -> CodegenUnit {
        CodegenUnit {
            defined_symbol: Arc::from("main"),
            relocations: Arc::from([]),
            sections: sections.into(),
            artifacts: Default::default(),
            content_fingerprint: 0,
        }
    }

    fn canonical_projection_unit(text: &[u8], strings: &[&[u8]]) -> CodegenUnit {
        projection_unit(vec![
            projection_section(SectionKind::Text, &[text]),
            projection_section(SectionKind::Rodata, strings),
            projection_section(SectionKind::Data, &[]),
            projection_section(SectionKind::Bss, &[]),
        ])
    }

    fn assert_projection_rejects(unit: CodegenUnit, expected: &str) {
        let error = project_backend_object(&unit, Target::X86_64Linux).unwrap_err();
        assert!(
            matches!(&error.kind, ErrorKind::InternalCodegenError(message) if message.contains(expected)),
            "unexpected projection error: {error}"
        );
    }

    #[test]
    fn object_projection_rejects_malformed_section_shapes_without_panicking() {
        let all_sections = vec![
            projection_section(SectionKind::Text, &[b"text"]),
            projection_section(SectionKind::Rodata, &[]),
            projection_section(SectionKind::Data, &[]),
            projection_section(SectionKind::Bss, &[]),
        ];
        for kind in [
            SectionKind::Text,
            SectionKind::Rodata,
            SectionKind::Data,
            SectionKind::Bss,
        ] {
            let missing = all_sections
                .iter()
                .filter(|section| section.kind != kind)
                .cloned()
                .collect();
            assert_projection_rejects(
                projection_unit(missing),
                &format!("exactly one {kind:?} section"),
            );
            let mut duplicate = all_sections.clone();
            duplicate.push(projection_section(kind, &[]));
            assert_projection_rejects(
                projection_unit(duplicate),
                &format!("exactly one {kind:?} section"),
            );
        }
        assert_projection_rejects(
            canonical_projection_unit(b"text", &[&[0xff]]),
            "non-UTF-8 rodata",
        );
        for kind in [SectionKind::Data, SectionKind::Bss] {
            let mut malformed = canonical_projection_unit(b"text", &[]);
            Arc::make_mut(&mut malformed.sections)
                .iter_mut()
                .find(|section| section.kind == kind)
                .unwrap()
                .atoms = Arc::from([Arc::<[u8]>::from(*b"unsupported")]);
            assert_projection_rejects(malformed, &format!("does not support {kind:?} atoms"));
        }
        assert_projection_rejects(
            {
                let mut malformed = canonical_projection_unit(b"text", &[]);
                Arc::make_mut(&mut malformed.sections)
                    .iter_mut()
                    .find(|section| section.kind == SectionKind::Data)
                    .unwrap()
                    .atoms = Arc::from([Arc::<[u8]>::from(&b""[..])]);
                malformed
            },
            "does not support Data atoms",
        );
        for kind in [
            SectionKind::Text,
            SectionKind::Rodata,
            SectionKind::Data,
            SectionKind::Bss,
        ] {
            let mut malformed = canonical_projection_unit(b"text", &[]);
            let section = Arc::make_mut(&mut malformed.sections)
                .iter_mut()
                .find(|section| section.kind == kind)
                .unwrap();
            section.alignment = section.alignment.saturating_add(1);
            assert_projection_rejects(malformed, &format!("non-canonical {kind:?}"));
        }
        let mut wrong_arch = canonical_projection_unit(b"text", &[]);
        wrong_arch.relocations = Arc::from([NormalizedRelocation {
            offset: 0,
            symbol: Arc::from("callee"),
            kind: RelocationKind::Aarch64Call26,
            addend: 0,
        }]);
        assert_projection_rejects(wrong_arch, "incompatible relocation");
    }

    #[test]
    fn object_projection_preserves_rodata_atoms_and_relocations_on_every_target() {
        let non_ascii = "é".as_bytes();
        let atom_bytes: [&[u8]; 4] = [b"", b"duplicate", non_ascii, b"duplicate"];
        let strings = vec![
            String::new(),
            "duplicate".to_owned(),
            "é".to_owned(),
            "duplicate".to_owned(),
        ];
        for target in [
            Target::X86_64Linux,
            Target::Aarch64Linux,
            Target::Aarch64Macos,
        ] {
            let (relocations, expected_relocations): (Vec<_>, Vec<_>) = match target.arch() {
                Arch::X86_64 => (
                    vec![
                        NormalizedRelocation {
                            offset: 1,
                            symbol: Arc::from("first"),
                            kind: RelocationKind::X86Pc32,
                            addend: -4,
                        },
                        NormalizedRelocation {
                            offset: 3,
                            symbol: Arc::from("duplicate"),
                            kind: RelocationKind::X86Plt32,
                            addend: 0,
                        },
                    ],
                    vec![
                        CodeRelocation {
                            offset: 1,
                            symbol: "first".to_owned(),
                            rel_type: RelocationType::Pc32,
                            addend: -4,
                        },
                        CodeRelocation {
                            offset: 3,
                            symbol: "duplicate".to_owned(),
                            rel_type: RelocationType::Plt32,
                            addend: 0,
                        },
                    ],
                ),
                Arch::Aarch64 => (
                    vec![
                        NormalizedRelocation {
                            offset: 0,
                            symbol: Arc::from("first"),
                            kind: RelocationKind::Aarch64AdrpPage21,
                            addend: 0,
                        },
                        NormalizedRelocation {
                            offset: 4,
                            symbol: Arc::from("duplicate"),
                            kind: RelocationKind::Aarch64AddLo12,
                            addend: 8,
                        },
                        NormalizedRelocation {
                            offset: 8,
                            symbol: Arc::from("first"),
                            kind: RelocationKind::Aarch64Call26,
                            addend: 0,
                        },
                    ],
                    vec![
                        CodeRelocation {
                            offset: 0,
                            symbol: "first".to_owned(),
                            rel_type: RelocationType::AdrpPage21,
                            addend: 0,
                        },
                        CodeRelocation {
                            offset: 4,
                            symbol: "duplicate".to_owned(),
                            rel_type: RelocationType::AddLo12,
                            addend: 8,
                        },
                        CodeRelocation {
                            offset: 8,
                            symbol: "first".to_owned(),
                            rel_type: RelocationType::Call26,
                            addend: 0,
                        },
                    ],
                ),
            };
            let mut unit = canonical_projection_unit(b"\x90\xc3", &atom_bytes);
            unit.relocations = relocations.into();
            let actual = project_backend_object(&unit, target).unwrap();
            let mut expected = ObjectBuilder::new(target, "main")
                .code(vec![0x90, 0xc3])
                .strings(strings.clone());
            for relocation in expected_relocations {
                expected = expected.relocation(relocation);
            }
            assert_eq!(actual, expected.build(), "{target:?}");
        }
    }

    fn frontend() -> SourceSnapshot {
        let snapshot = SourceSnapshot::single("<rue-784>", SOURCE).unwrap();
        snapshot
    }

    fn strbuf_concat_frontend() -> (
        SourceSnapshot,
        std::sync::Arc<CanonicalRirOutput>,
        RootedCfgOutput,
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
        (snapshot, rir, semantic)
    }

    fn assert_three_result_slots_cross_cleanup(target: Target) {
        let (snapshot, _rir, semantic) = strbuf_concat_frontend();
        let options = CompileOptions {
            target,
            ..Default::default()
        };
        let mut session = CompilerSession::new();
        crate::test_support::publish_test_snapshot(&mut session, &snapshot).unwrap();
        let rooted = session
            .rooted_codegen(
                &options,
                rue_codegen::BackendArtifactRequest {
                    asm: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let assembly = rooted
            .units
            .into_iter()
            .find(|unit| unit.unit.defined_symbol.as_ref() == "main")
            .and_then(|unit| unit.unit.artifacts.asm.clone())
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
            .filter(|function| function.record.source_name.contains("concat_borrowed"))
            .map(|function| function.record.codegen.defined_symbol.as_ref())
            .collect::<HashSet<_>>();
        let cleanup_symbols = semantic
            .functions()
            .iter()
            .filter(|function| {
                function.record.source_name.contains(".__drop")
                    || function.record.source_name.starts_with("__rue_drop_")
            })
            .map(|function| function.record.codegen.defined_symbol.as_ref())
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
        let snapshot = frontend();

        for target in [
            Target::X86_64Linux,
            Target::Aarch64Linux,
            Target::Aarch64Macos,
        ] {
            let options = CompileOptions {
                target,
                ..CompileOptions::default()
            };
            let mut session = CompilerSession::new();
            crate::test_support::publish_test_snapshot(&mut session, &snapshot).unwrap();
            let rooted = session
                .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                .unwrap();
            let all_object_bytes: Vec<_> = rooted
                .objects
                .iter()
                .flat_map(|object| object.object.bytes.iter().copied())
                .collect();
            assert_eq!(count(&all_object_bytes, FIRST_LITERAL), 1, "{target}");
            assert_eq!(count(&all_object_bytes, SECOND_LITERAL), 1, "{target}");
        }
    }

    #[test]
    fn text_objects_and_runtime_archive_have_no_obsolete_string_symbols() {
        let (snapshot, _rir, _semantic) = strbuf_concat_frontend();
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
            let mut session = CompilerSession::new();
            crate::test_support::publish_test_snapshot(&mut session, &snapshot).unwrap();
            let rooted = session
                .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                .unwrap();
            let objects = rooted
                .objects
                .iter()
                .map(|object| object.object.bytes.as_ref())
                .collect::<Vec<_>>();

            let mut undefined = HashSet::new();
            for bytes in objects {
                let object = ObjectFile::parse(bytes).unwrap();
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

        crate::test_support::test_compile_snapshot(&snapshot, &options)
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
