//! Canonical per-function code generation terminals.
//!
//! A `CodegenUnit` is keyed by its stable callable identity and target.  The
//! optimized CFG's stable semantic fingerprint and the exact callable/foreign
//! symbol projection are content inputs, rather than a whole-program backend
//! cache key.  This keeps an ordinary callee implementation edit local: its
//! callers keep their units unless a real ABI or emitted reference changes.

use std::{
    collections::BTreeSet,
    hash::{Hash, Hasher},
    sync::Arc,
};

use rue_query::{QueryAbort, QueryContext, QueryFamily, QueryKey, QueryOutcome, QueryOutput};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static INJECT_CODEGEN_FAILURE: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn with_test_codegen_failure_injection<T>(run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INJECT_CODEGEN_FAILURE.with(|enabled| enabled.set(false));
        }
    }

    INJECT_CODEGEN_FAILURE.with(|enabled| {
        assert!(
            !enabled.replace(true),
            "codegen failure injection is not nestable"
        );
    });
    let _reset = Reset;
    run()
}

#[derive(Debug, Clone)]
pub(crate) struct CodegenUnitQueryKey {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) target: rue_target::Target,
    pub(crate) data_model: rue_target::DataModel,
    pub(crate) code_model: CodeModel,
    pub(crate) optimization: rue_cfg::OptLevel,
    pub(crate) backend_epoch: u32,
    pub(crate) abi_layout_epoch: u32,
    /// A deliberate on-demand presentation-retention variant. It selects a
    /// memoized view of the same canonical codegen family and path, instead of
    /// forcing debug artifacts into normal builds. It participates in memo
    /// equality because the evaluator retains those projections, but remains
    /// absent from logical identity and the link-content fingerprint.
    pub(crate) request: rue_codegen::BackendArtifactRequest,
    /// The registered optimized-CFG dependency owns every current-domain value
    /// used by lowering. It is excluded from `stable_identity`, but remains in
    /// memo equality so a different function body/configuration cannot alias.
    pub(crate) optimized_cfg: crate::cfg_query::OptimizedCfgQueryKey,
    memo_hash: u64,
}

impl CodegenUnitQueryKey {
    pub(crate) fn new(
        optimized_cfg: crate::cfg_query::OptimizedCfgQueryKey,
        target: rue_target::Target,
        request: rue_codegen::BackendArtifactRequest,
        optimization: rue_cfg::OptLevel,
    ) -> Self {
        let function = optimized_cfg.cfg.function.clone();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        function.hash(&mut hasher);
        target.hash(&mut hasher);
        target.data_model().hash(&mut hasher);
        CodeModel::for_target(target).hash(&mut hasher);
        std::mem::discriminant(&optimization).hash(&mut hasher);
        BACKEND_EPOCH.hash(&mut hasher);
        ABI_LAYOUT_EPOCH.hash(&mut hasher);
        request.lowering.hash(&mut hasher);
        request.mir.hash(&mut hasher);
        request.liveness.hash(&mut hasher);
        request.regalloc.hash(&mut hasher);
        request.asm.hash(&mut hasher);
        optimized_cfg.hash(&mut hasher);
        let memo_hash = hasher.finish();
        Self {
            function,
            target,
            data_model: target.data_model(),
            code_model: CodeModel::for_target(target),
            optimization,
            backend_epoch: BACKEND_EPOCH,
            abi_layout_epoch: ABI_LAYOUT_EPOCH,
            request,
            optimized_cfg,
            memo_hash,
        }
    }
}

impl PartialEq for CodegenUnitQueryKey {
    fn eq(&self, other: &Self) -> bool {
        self.function == other.function
            && self.target == other.target
            && self.data_model == other.data_model
            && self.code_model == other.code_model
            && self.optimization == other.optimization
            && self.backend_epoch == other.backend_epoch
            && self.abi_layout_epoch == other.abi_layout_epoch
            && self.request == other.request
            && self.optimized_cfg == other.optimized_cfg
    }
}
impl Eq for CodegenUnitQueryKey {}
impl Hash for CodegenUnitQueryKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.memo_hash.hash(state);
    }
}
impl QueryKey for CodegenUnitQueryKey {
    fn stable_identity(&self) -> String {
        format!(
            "{:?};target={:?};data-model={:?};code-model={:?};opt={:?};backend={};abi-layout={}",
            self.function,
            self.target,
            self.data_model,
            self.code_model,
            self.optimization,
            self.backend_epoch,
            self.abi_layout_epoch
        )
    }
}

/// Typed terminal shared by normal object generation and `--emit` backend
/// presentations. `content_fingerprint` is deliberately over emitted linker
/// content, not current-domain identities.
#[derive(Debug, Clone)]
pub(crate) struct CodegenUnit {
    pub(crate) defined_symbol: Arc<str>,
    pub(crate) referenced_symbols: Arc<[Arc<str>]>,
    pub(crate) relocations: Arc<[NormalizedRelocation]>,
    pub(crate) sections: Arc<[CodegenSection]>,
    pub(crate) artifacts: rue_codegen::BackendArtifacts,
    pub(crate) content_fingerprint: u64,
    pub(crate) presentation_fingerprint: u64,
    pub(crate) presentation: Arc<str>,
}

/// A reached canonical terminal paired only with its stable function identity.
/// RUE-1217 owns replacing the semantic root enumerator that assembles this
/// list; neither the terminal nor this collection record retains live frontend
/// state.
#[derive(Clone)]
pub(crate) struct CollectedCodegenUnit {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) unit: Arc<CodegenUnit>,
}

/// Typed codegen terminal. Backend and relocation validation failures remain
/// ordinary compiler diagnostics rather than escaping the query runtime.
#[derive(Debug, Clone)]
pub(crate) enum CodegenUnitValue {
    Available(Arc<CodegenUnit>),
    Failure(crate::CompileErrors),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CodeModel {
    StaticPic,
}
impl CodeModel {
    fn for_target(_: rue_target::Target) -> Self {
        Self::StaticPic
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SectionContents {
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) alignment: u32,
    pub(crate) executable: bool,
    pub(crate) writable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SectionKind {
    Text,
    Rodata,
    Data,
    Bss,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CodegenSection {
    pub(crate) kind: SectionKind,
    pub(crate) contents: SectionContents,
    /// Atoms preserve the deterministic local ownership boundary. The current
    /// backend has anonymous text and string atoms; data/BSS are explicit empty
    /// sections until a producer supplies them.
    pub(crate) atoms: Arc<[Arc<[u8]>]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct NormalizedRelocation {
    pub(crate) offset: u64,
    pub(crate) symbol: Arc<str>,
    pub(crate) kind: rue_codegen::RelocationKind,
    pub(crate) addend: i64,
}

const BACKEND_EPOCH: u32 = 1;
const ABI_LAYOUT_EPOCH: u32 = 1;

pub(crate) fn codegen_unit_value_equal(left: &CodegenUnitValue, right: &CodegenUnitValue) -> bool {
    match (left, right) {
        (CodegenUnitValue::Available(left), CodegenUnitValue::Available(right)) => {
            left.defined_symbol == right.defined_symbol
                && left.referenced_symbols == right.referenced_symbols
                && left.relocations == right.relocations
                && left.sections == right.sections
                && left.content_fingerprint == right.content_fingerprint
                && left.presentation_fingerprint == right.presentation_fingerprint
                && left.presentation == right.presentation
        }
        (CodegenUnitValue::Failure(left), CodegenUnitValue::Failure(right)) => left == right,
        _ => false,
    }
}

impl CodegenUnit {
    /// Thin legacy projection for the object writer.  No lowering, allocation,
    /// or emission occurs here: all link-relevant bytes come from this terminal.
    pub(crate) fn backend_product(&self) -> crate::backend::FunctionBackendProduct {
        let text = self
            .sections
            .iter()
            .find(|section| section.kind == SectionKind::Text)
            .expect("CodegenUnit always has text");
        let rodata = self
            .sections
            .iter()
            .find(|section| section.kind == SectionKind::Rodata)
            .expect("CodegenUnit always has rodata");
        crate::backend::FunctionBackendProduct {
            machine_name: self.defined_symbol.to_string(),
            machine_code: rue_codegen::MachineCode {
                code: text.contents.bytes.to_vec(),
                relocations: self
                    .relocations
                    .iter()
                    .map(|relocation| rue_codegen::EmittedRelocation {
                        offset: relocation.offset,
                        symbol: relocation.symbol.to_string(),
                        kind: relocation.kind,
                        addend: relocation.addend,
                    })
                    .collect(),
                strings: rodata
                    .atoms
                    .iter()
                    .map(|atom| {
                        String::from_utf8(atom.to_vec())
                            .expect("codegen rodata atoms are UTF-8 strings")
                    })
                    .collect(),
            },
            artifacts: self.artifacts.clone(),
        }
    }
}

fn product_fingerprint(product: &crate::backend::FunctionBackendProduct) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    product.machine_name.hash(&mut hasher);
    product.machine_code.code.hash(&mut hasher);
    product.machine_code.strings.hash(&mut hasher);
    for relocation in &product.machine_code.relocations {
        relocation.offset.hash(&mut hasher);
        relocation.symbol.hash(&mut hasher);
        std::mem::discriminant(&relocation.kind).hash(&mut hasher);
        relocation.addend.hash(&mut hasher);
    }
    hasher.finish()
}

pub(crate) fn evaluate_codegen_unit(
    context: &QueryContext,
    optimized_cfgs: &QueryFamily<
        crate::cfg_query::OptimizedCfgQueryKey,
        crate::cfg_query::CfgValue,
    >,
    key: &CodegenUnitQueryKey,
) -> Result<QueryOutput<CodegenUnitValue>, QueryAbort> {
    context.check_canceled()?;
    context.record_work(rue_query::WorkItem::new("codegen.unit.attempts", 1));
    let _nested = context.retain_nested_attempts_for(&["compiler.optimized-cfg"]);
    let optimized = context.query_registered(optimized_cfgs, key.optimized_cfg.clone())?;
    context.record_work(rue_query::WorkItem::new(
        "codegen.dependencies.optimized-cfg",
        1,
    ));
    let QueryOutcome::Success(optimized) = optimized.outcome() else {
        unreachable!("OptimizedCfg publishes typed values");
    };
    if let crate::cfg_query::CfgValue::Failure { errors, .. } = optimized {
        return Ok(codegen_failure(errors.clone()));
    }
    let crate::cfg_query::CfgValue::Available(record) = optimized else {
        unreachable!("failure handled above")
    };
    let _span = tracing::info_span!("codegen_unit", phase = "backend").entered();
    context.record_work(rue_query::WorkItem::new(
        "codegen.domain.symbol-aliases",
        record.codegen.symbol_mappings.len() as u64,
    ));
    context.record_work(rue_query::WorkItem::new(
        "codegen.domain.foreign-aliases",
        record.codegen.foreign_symbols.len() as u64,
    ));
    context.record_work(rue_query::WorkItem::new(
        "codegen.dependencies.local-atoms",
        record.local_atoms.len() as u64,
    ));
    let stable_atoms = record
        .local_atoms
        .iter()
        .map(|atom| {
            crate::StableSymbolEncoder::encode(&crate::StableSymbolId::LocalAtom(
                atom.identity.clone(),
            ))
        })
        .collect::<Vec<_>>();
    let atoms = record
        .local_atoms
        .iter()
        .zip(&stable_atoms)
        .map(|(atom, stable_id)| rue_codegen::LocalAtomProjection {
            stable_id,
            dense_id: atom.dense_id,
            content: &atom.content,
        })
        .collect::<Vec<_>>();
    let symbols = rue_codegen::MachineSymbolResolver::new_with_foreign(
        &record.codegen.symbol_mappings,
        &record.codegen.foreign_symbols,
    );
    context.record_work(rue_query::WorkItem::new("codegen.lowering.local", 1));
    let generated = match key.target.arch() {
        rue_target::Arch::X86_64 => rue_codegen::x86_64::generate_product_with_symbols_and_atoms(
            &record.cfg,
            &record.type_pool,
            &record.strings,
            &record.interner,
            symbols,
            &atoms,
            key.request,
        ),
        rue_target::Arch::Aarch64 => rue_codegen::aarch64::generate_product_with_symbols_and_atoms(
            &record.cfg,
            &record.type_pool,
            &record.strings,
            &record.interner,
            key.target,
            symbols,
            &atoms,
            key.request,
        ),
    };
    let mut product = match generated {
        Ok(product) => product,
        Err(error) => return Ok(codegen_failure(error.into())),
    };
    if let Some(lowering) = &mut product.artifacts.lowering {
        lowering.fn_name = record.codegen.defined_symbol.to_string();
    }
    #[cfg(test)]
    if INJECT_CODEGEN_FAILURE.with(Cell::get) {
        product
            .machine_code
            .relocations
            .push(rue_codegen::EmittedRelocation {
                offset: 0,
                symbol: "injected_unresolved_codegen_symbol".to_owned(),
                kind: match key.target.arch() {
                    rue_target::Arch::X86_64 => rue_codegen::RelocationKind::X86Plt32,
                    rue_target::Arch::Aarch64 => rue_codegen::RelocationKind::Aarch64Call26,
                },
                addend: 0,
            });
    }
    if let Err(error) = crate::backend::validate_production_call_relocations(
        &product.machine_code.relocations,
        &record.codegen.symbol_mappings,
    ) {
        return Ok(codegen_failure(error.into()));
    }
    context.check_canceled()?;
    context.record_work(rue_query::WorkItem::new("codegen.unit.successes", 1));
    let product = crate::backend::FunctionBackendProduct {
        machine_name: record.codegen.defined_symbol.to_string(),
        machine_code: product.machine_code,
        artifacts: product.artifacts,
    };
    let relocations = product
        .machine_code
        .relocations
        .iter()
        .map(|relocation| NormalizedRelocation {
            offset: relocation.offset,
            symbol: Arc::from(relocation.symbol.as_str()),
            kind: relocation.kind,
            addend: relocation.addend,
        })
        .collect::<Vec<_>>()
        .into();
    let referenced_symbols = product
        .machine_code
        .relocations
        .iter()
        .map(|relocation| Arc::from(relocation.symbol.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .into();
    let text: Arc<[u8]> = product.machine_code.code.clone().into();
    let rodata_atoms = product
        .machine_code
        .strings
        .iter()
        .map(|value| Arc::<[u8]>::from(value.as_bytes()))
        .collect::<Vec<_>>();
    let rodata_bytes = rodata_atoms
        .iter()
        .flat_map(|atom| atom.iter().copied())
        .collect::<Vec<_>>();
    let empty = SectionContents {
        bytes: Arc::from([]),
        alignment: 1,
        executable: false,
        writable: true,
    };
    let sections: Arc<[CodegenSection]> = vec![
        CodegenSection {
            kind: SectionKind::Text,
            contents: SectionContents {
                bytes: text.clone(),
                alignment: 16,
                executable: true,
                writable: false,
            },
            atoms: Arc::from([text.clone()]),
        },
        CodegenSection {
            kind: SectionKind::Rodata,
            contents: SectionContents {
                bytes: rodata_bytes.into(),
                alignment: 1,
                executable: false,
                writable: false,
            },
            atoms: rodata_atoms.into(),
        },
        CodegenSection {
            kind: SectionKind::Data,
            contents: empty.clone(),
            atoms: Arc::from([]),
        },
        CodegenSection {
            kind: SectionKind::Bss,
            contents: empty,
            atoms: Arc::from([]),
        },
    ]
    .into();
    let content_fingerprint = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        product_fingerprint(&product).hash(&mut hasher);
        sections.hash(&mut hasher);
        hasher.finish()
    };
    let presentation: Arc<str> = Arc::from(format!("{:?}", product.artifacts));
    let presentation_fingerprint = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        presentation.hash(&mut hasher);
        hasher.finish()
    };
    Ok(QueryOutput::success(CodegenUnitValue::Available(Arc::new(
        CodegenUnit {
            defined_symbol: Arc::from(product.machine_name.as_str()),
            referenced_symbols,
            relocations,
            sections,
            artifacts: product.artifacts,
            content_fingerprint,
            presentation_fingerprint,
            presentation,
        },
    ))))
}

fn codegen_failure(errors: crate::CompileErrors) -> QueryOutput<CodegenUnitValue> {
    QueryOutput::success(CodegenUnitValue::Failure(errors))
        .with_terminal_kind(rue_query::QueryTerminalKind::Failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_fingerprint_prevents_stale_projection_reuse() {
        let empty: Arc<[CodegenSection]> = Arc::from([]);
        let unit = |presentation_fingerprint| CodegenUnit {
            defined_symbol: Arc::from("main"),
            referenced_symbols: Arc::from([]),
            relocations: Arc::from([]),
            sections: empty.clone(),
            artifacts: rue_codegen::BackendArtifacts::default(),
            content_fingerprint: 1,
            presentation_fingerprint,
            presentation: Arc::from("presentation"),
        };
        assert!(!codegen_unit_value_equal(
            &CodegenUnitValue::Available(Arc::new(unit(1))),
            &CodegenUnitValue::Available(Arc::new(unit(2))),
        ));
    }
}
