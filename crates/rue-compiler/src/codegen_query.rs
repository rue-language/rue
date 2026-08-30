//! Canonical per-function code generation terminals.
//!
//! A `CodegenUnit` is keyed by its stable callable identity and target.  The
//! optimized CFG's stable semantic fingerprint and the exact callable/foreign
//! symbol projection are content inputs, rather than a whole-program backend
//! cache key.  This keeps an ordinary callee implementation edit local: its
//! callers keep their units unless a real ABI or emitted reference changes.

use std::{
    hash::{Hash, Hasher},
    sync::{Arc, OnceLock},
};

use rue_query::{QueryAbort, QueryContext, QueryFamily, QueryKey, QueryOutcome, QueryOutput};

use crate::retained_charge::RetainedCharge;

/// Internal bridge for the rooted backend's batch-slot authority. The
/// implementation lives with `OptimizedCfgBatchKey` so its source guard can
/// inspect the private authority while codegen remains the sole consumer.
pub(crate) trait OptimizedCfgBatchLookup {
    fn optimized_cfg_position(
        &self,
        source: &Arc<[crate::cfg_query::OptimizedCfgQueryKey]>,
        key: &crate::cfg_query::OptimizedCfgQueryKey,
    ) -> Option<usize>;
}

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static INJECT_CODEGEN_FAILURE: Cell<bool> = const { Cell::new(false) };
}

/// Deterministic mid-backend cancellation for tests. Codegen units evaluate
/// on query worker threads, so unlike the linker's thread-local tripwire this
/// one is a process-global slot.
#[cfg(test)]
static CODEGEN_CANCELLATION_TRIPWIRE: std::sync::Mutex<
    Option<(rue_query::CancellationToken, usize)>,
> = std::sync::Mutex::new(None);

/// Arm a deterministic mid-backend cancellation: the Nth cooperative probe
/// inside machine-code generation cancels `token`, so the stale attempt must
/// exit through the cancellation contract rather than completing the unit
/// (RUE-1827). Mirrors `linking::set_link_cancellation_tripwire`.
#[cfg(test)]
pub(crate) fn set_codegen_cancellation_tripwire(
    cancellation: Option<(rue_query::CancellationToken, usize)>,
) {
    *CODEGEN_CANCELLATION_TRIPWIRE.lock().unwrap() = cancellation;
}

/// One cooperative cancellation probe evaluation for the backend kernel.
fn generation_probe_is_canceled(context: &QueryContext) -> bool {
    #[cfg(test)]
    {
        let mut slot = CODEGEN_CANCELLATION_TRIPWIRE.lock().unwrap();
        if let Some((token, remaining)) = slot.as_mut() {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                token.cancel();
                *slot = None;
            }
        }
    }
    context.cancellation().is_canceled()
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
    /// Rooted compilations carry the exact optimized-CFG batch that performed
    /// Phase-2 general inlining. Codegen consumes that batch value so it never
    /// re-queries the uninlined per-function terminal.
    pub(crate) optimized_cfg_batch:
        Option<Arc<crate::revisioned_query_database::OptimizedCfgBatchKey>>,
    /// Request-local transport for the deterministic test failure injection.
    /// Registered batch children run on worker threads, so thread-local test
    /// state is captured into the exact key at the host boundary.
    #[cfg(test)]
    inject_failure: bool,
    memo_hash: u64,
    /// The ADR-0074 digest of this key's own fields, derived once at
    /// construction.
    ///
    /// `stable_hash` is called several times per unit — by the codegen family
    /// itself, by `CodegenUnitBatchKey`, and by every `ObjectProjectionQueryKey`
    /// that wraps this one — and each call used to re-walk the recursive
    /// `FunctionInstanceKey` beneath it, at roughly 44,000 instructions
    /// against about 500 for a flat key. Deriving the digest once and
    /// absorbing those sixteen bytes keeps equal keys equal-digested while
    /// walking the tree a single time.
    ///
    /// This is a deliberately coarse digest in the sense `QueryKey` permits:
    /// two keys sharing it are separated by typed `Eq`, exactly as the
    /// retained semantic versions of one function already share
    /// `CfgQueryKey`'s digest.
    stable_digest: rue_query::StableKeyHash,
    /// Formatted on the first diagnostic, cycle render, or abort that asks
    /// what this node is *called* (ADR-0074). Ordinary compilation never
    /// reads it: eagerly formatting `{function:?}` here walked the whole
    /// recursive function identity and allocated for every constructed key.
    display_identity: OnceLock<Arc<str>>,
}

impl CodegenUnitQueryKey {
    #[cfg(test)]
    pub(crate) fn new(
        optimized_cfg: crate::cfg_query::OptimizedCfgQueryKey,
        target: rue_target::Target,
        request: rue_codegen::BackendArtifactRequest,
        optimization: rue_cfg::OptLevel,
    ) -> Self {
        Self::new_with_batch(optimized_cfg, target, request, optimization, None)
    }

    pub(crate) fn new_with_batch(
        optimized_cfg: crate::cfg_query::OptimizedCfgQueryKey,
        target: rue_target::Target,
        request: rue_codegen::BackendArtifactRequest,
        optimization: rue_cfg::OptLevel,
        optimized_cfg_batch: Option<Arc<crate::revisioned_query_database::OptimizedCfgBatchKey>>,
    ) -> Self {
        let function = optimized_cfg.cfg.function.clone();
        // A memo bucket selector, not an identity: `PartialEq` below compares
        // the complete fields, so this only has to be fast and deterministic.
        // `DefaultHasher` is SipHash-1-3, which dominated key construction on
        // a fresh Lattice compile.
        let mut hasher = rue_query::StableHasher::new();
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
        optimized_cfg_batch.hash(&mut hasher);
        #[cfg(test)]
        let inject_failure = INJECT_CODEGEN_FAILURE.with(Cell::get);
        #[cfg(test)]
        inject_failure.hash(&mut hasher);
        let memo_hash = hasher.finish();
        let data_model = target.data_model();
        let code_model = CodeModel::for_target(target);
        let mut digest = rue_query::StableHasher::new();
        function.hash(&mut digest);
        target.hash(&mut digest);
        data_model.hash(&mut digest);
        code_model.hash(&mut digest);
        std::mem::discriminant(&optimization).hash(&mut digest);
        BACKEND_EPOCH.hash(&mut digest);
        ABI_LAYOUT_EPOCH.hash(&mut digest);
        request.lowering.hash(&mut digest);
        request.mir.hash(&mut digest);
        request.liveness.hash(&mut digest);
        request.regalloc.hash(&mut digest);
        request.asm.hash(&mut digest);
        optimized_cfg.stable_hash(&mut digest);
        optimized_cfg_batch.hash(&mut digest);
        #[cfg(test)]
        inject_failure.hash(&mut digest);
        let stable_digest = digest.finish128();
        Self {
            function,
            target,
            data_model,
            code_model,
            optimization,
            backend_epoch: BACKEND_EPOCH,
            abi_layout_epoch: ABI_LAYOUT_EPOCH,
            request,
            optimized_cfg,
            optimized_cfg_batch,
            #[cfg(test)]
            inject_failure,
            memo_hash,
            stable_digest,
            display_identity: OnceLock::new(),
        }
    }

    fn format_identity(&self) -> String {
        let function = &self.function;
        let target = self.target;
        let data_model = self.data_model;
        let code_model = self.code_model;
        let optimization = self.optimization;
        format!(
            "{function:?};target={target:?};data-model={data_model:?};code-model={code_model:?};opt={optimization:?};backend={BACKEND_EPOCH};abi-layout={ABI_LAYOUT_EPOCH};batch={}",
            self.optimized_cfg_batch.is_some()
        )
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
            && self.optimized_cfg_batch == other.optimized_cfg_batch
            && {
                #[cfg(test)]
                {
                    self.inject_failure == other.inject_failure
                }
                #[cfg(not(test))]
                {
                    true
                }
            }
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
        self.format_identity()
    }

    fn shared_stable_identity(&self) -> Arc<str> {
        self.display_identity
            .get_or_init(|| self.format_identity().into())
            .clone()
    }

    /// The digest derived once at construction, over the field set documented
    /// on `stable_digest`.
    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        let digest = self.stable_digest.to_u128();
        hasher.write_u64(digest as u64);
        hasher.write_u64((digest >> 64) as u64);
    }
}

/// Typed terminal shared by normal object generation and `--emit` backend
/// presentations. `content_fingerprint` is deliberately over emitted linker
/// content, not current-domain identities.
#[derive(Debug, Clone)]
pub(crate) struct CodegenUnit {
    pub(crate) defined_symbol: Arc<str>,
    pub(crate) relocations: Arc<[NormalizedRelocation]>,
    pub(crate) sections: Arc<[CodegenSection]>,
    pub(crate) artifacts: rue_codegen::BackendArtifacts,
    pub(crate) content_fingerprint: u64,
}

/// A reached canonical terminal paired only with its stable function identity.
/// RUE-1217 owns replacing the semantic root enumerator that assembles this
/// list; neither the terminal nor this collection record retains live frontend
/// state.
#[derive(Debug, Clone)]
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
    pub(crate) alignment: u32,
    pub(crate) executable: bool,
    pub(crate) writable: bool,
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

impl RetainedCharge for rue_codegen::LoweringDecision {
    fn retained_charge(&self) -> u64 {
        self.cfg_inst_desc
            .retained_charge()
            .saturating_add(self.cfg_type.retained_charge())
            .saturating_add(self.mir_insts.retained_charge())
            .saturating_add(self.rationale.retained_charge())
    }
}

impl RetainedCharge for rue_codegen::terminator_plan::TerminatorTrace {
    fn retained_charge(&self) -> u64 {
        let edge_work = self.edge_work.iter().fold(
            (self.edge_work.len() * std::mem::size_of::<Vec<(u32, u32)>>()) as u64,
            |charge, edge| {
                charge.saturating_add((edge.len() * std::mem::size_of::<(u32, u32)>()) as u64)
            },
        );
        ((self.successors.len() * std::mem::size_of::<rue_cfg::BlockId>()) as u64)
            .saturating_add((self.fallthrough.len() * std::mem::size_of::<bool>()) as u64)
            .saturating_add((self.edge_move_counts.len() * std::mem::size_of::<usize>()) as u64)
            .saturating_add(edge_work)
            .saturating_add(
                (self.switch_cases.len() * std::mem::size_of::<(i64, rue_cfg::BlockId)>()) as u64,
            )
    }
}

impl RetainedCharge for rue_codegen::TerminatorLoweringDecision {
    fn retained_charge(&self) -> u64 {
        self.terminator_desc
            .retained_charge()
            .saturating_add(self.mir_insts.retained_charge())
            .saturating_add(self.rationale.retained_charge())
            .saturating_add(self.policy_trace.retained_charge())
    }
}

impl RetainedCharge for rue_codegen::BlockLoweringInfo {
    fn retained_charge(&self) -> u64 {
        self.instructions
            .retained_charge()
            .saturating_add(self.terminator.retained_charge())
    }
}

impl RetainedCharge for rue_codegen::LoweringDebugInfo {
    fn retained_charge(&self) -> u64 {
        self.fn_name
            .retained_charge()
            .saturating_add(self.target_arch.retained_charge())
            .saturating_add(self.blocks.retained_charge())
    }
}

impl RetainedCharge for rue_codegen::BackendArtifacts {
    fn retained_charge(&self) -> u64 {
        self.lowering
            .retained_charge()
            .saturating_add(self.mir.retained_charge())
            .saturating_add(self.liveness.retained_charge())
            .saturating_add(self.regalloc.retained_charge())
            .saturating_add(self.asm.retained_charge())
    }
}

impl RetainedCharge for CodegenSection {
    fn retained_charge(&self) -> u64 {
        self.atoms.retained_charge()
    }
}

impl RetainedCharge for NormalizedRelocation {
    fn retained_charge(&self) -> u64 {
        self.symbol.retained_charge()
    }
}

impl RetainedCharge for CodegenUnit {
    fn retained_charge(&self) -> u64 {
        self.defined_symbol
            .retained_charge()
            .saturating_add(self.relocations.retained_charge())
            .saturating_add(self.sections.retained_charge())
            .saturating_add(self.artifacts.retained_charge())
    }
}

impl RetainedCharge for CodegenUnitValue {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Available(unit) => unit.retained_charge(),
            Self::Failure(errors) => errors.retained_charge(),
        }
    }
}

impl CodegenUnit {
    #[cfg(test)]
    pub(crate) fn text_atom(&self) -> Option<&[u8]> {
        self.sections
            .iter()
            .find(|section| section.kind == SectionKind::Text)
            .and_then(|section| (section.atoms.len() == 1).then(|| section.atoms[0].as_ref()))
    }
}

const BACKEND_EPOCH: u32 = 1;
const ABI_LAYOUT_EPOCH: u32 = 1;

pub(crate) fn codegen_unit_value_equal(left: &CodegenUnitValue, right: &CodegenUnitValue) -> bool {
    match (left, right) {
        (CodegenUnitValue::Available(left), CodegenUnitValue::Available(right)) => {
            left.defined_symbol == right.defined_symbol
                && left.relocations == right.relocations
                && left.sections == right.sections
                && left.artifacts == right.artifacts
                && left.content_fingerprint == right.content_fingerprint
        }
        (CodegenUnitValue::Failure(left), CodegenUnitValue::Failure(right)) => left == right,
        _ => false,
    }
}

fn content_fingerprint(
    defined_symbol: &str,
    sections: &[CodegenSection],
    relocations: &[NormalizedRelocation],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    defined_symbol.hash(&mut hasher);
    sections.hash(&mut hasher);
    for relocation in relocations {
        relocation.offset.hash(&mut hasher);
        relocation.symbol.hash(&mut hasher);
        relocation.kind.hash(&mut hasher);
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
    optimized_cfg_batches: &QueryFamily<
        crate::revisioned_query_database::OptimizedCfgBatchKey,
        crate::revisioned_query_database::OptimizedCfgBatchOutput,
    >,
    key: &CodegenUnitQueryKey,
) -> Result<QueryOutput<CodegenUnitValue>, QueryAbort> {
    context.check_canceled()?;
    context.record_work(rue_query::WorkItem::new("codegen.unit.attempts", 1));
    let _nested = context
        .retain_nested_attempts_for(&["compiler.optimized-cfg", "compiler.optimized-cfg-batch"]);
    let optimized = if let Some(batch) = &key.optimized_cfg_batch {
        let batch = context.query_registered(optimized_cfg_batches, (**batch).clone())?;
        let rue_query::QueryOutcome::Success(batch) = batch.outcome() else {
            unreachable!("optimized CFG batch publishes typed values")
        };
        let index = key
            .optimized_cfg_batch
            .as_ref()
            .and_then(|batch_key| {
                batch_key.optimized_cfg_position(&batch_key.keys, &key.optimized_cfg)
            })
            .expect("codegen optimized-CFG key belongs to its batch");
        let value = batch.values[index].clone();
        value
    } else {
        let terminal = context.query_registered(optimized_cfgs, key.optimized_cfg.clone())?;
        let QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("OptimizedCfg publishes typed values")
        };
        value.clone()
    };
    context.record_work(rue_query::WorkItem::new(
        "codegen.dependencies.optimized-cfg",
        1,
    ));
    if let crate::cfg_query::CfgValue::Failure { errors, .. } = optimized {
        return Ok(codegen_failure(errors.clone()));
    }
    if let crate::cfg_query::CfgValue::AccessorFailure { errors, origin } = optimized {
        return Ok(codegen_failure(crate::cfg_query::import_accessor_failure(
            &errors,
            &origin,
            &key.optimized_cfg,
        )));
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
    // The backend kernel has no query-runtime dependency; hand it this
    // task's cancellation authority as a probe so a stale watch revision
    // stops lowering/allocation/emission promptly instead of completing the
    // unit (RUE-1827).
    let probe = || generation_probe_is_canceled(context);
    let cancellation = rue_codegen::GenerationCancellation::from_probe(&probe);
    let generated = match key.target.arch() {
        rue_target::Arch::X86_64 => {
            rue_codegen::x86_64::generate_product_with_symbols_atoms_and_cancellation(
                &record.cfg,
                &record.type_pool,
                &record.strings,
                &record.interner,
                key.target,
                symbols,
                &atoms,
                key.request,
                cancellation,
            )
        }
        rue_target::Arch::Aarch64 => {
            rue_codegen::aarch64::generate_product_with_symbols_atoms_and_cancellation(
                &record.cfg,
                &record.type_pool,
                &record.strings,
                &record.interner,
                key.target,
                symbols,
                &atoms,
                key.request,
                cancellation,
            )
        }
    };
    let mut product = match generated {
        Ok(product) => product,
        // A cooperative cancellation rejection is this task's own abort, not
        // a codegen failure: it must not record a failure terminal.
        Err(error) if rue_codegen::is_generation_canceled(&error) => {
            return Err(QueryAbort::Canceled);
        }
        Err(error) => return Ok(codegen_failure(error.into())),
    };
    if let Some(lowering) = &mut product.artifacts.lowering {
        lowering.fn_name = record.codegen.defined_symbol.to_string();
    }
    #[cfg(test)]
    if key.inject_failure {
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
    let rue_codegen::BackendProduct {
        machine_code:
            rue_codegen::MachineCode {
                code,
                relocations: emitted_relocations,
                strings,
            },
        artifacts,
    } = product;
    if let Err(error) = crate::backend::validate_production_call_relocations(
        &emitted_relocations,
        &record.codegen.symbol_mappings,
    ) {
        return Ok(codegen_failure(error.into()));
    }
    context.check_canceled()?;
    context.record_work(rue_query::WorkItem::new("codegen.unit.successes", 1));
    let relocations: Arc<[NormalizedRelocation]> = emitted_relocations
        .into_iter()
        .map(|relocation| NormalizedRelocation {
            offset: relocation.offset,
            symbol: Arc::from(relocation.symbol),
            kind: relocation.kind,
            addend: relocation.addend,
        })
        .collect::<Vec<_>>()
        .into();
    let text: Arc<[u8]> = code.into();
    let rodata_atoms = strings
        .into_iter()
        .map(|value| Arc::<[u8]>::from(value.into_bytes()))
        .collect::<Vec<_>>();
    let sections: Arc<[CodegenSection]> = vec![
        CodegenSection {
            kind: SectionKind::Text,
            alignment: 16,
            executable: true,
            writable: false,
            atoms: Arc::from([text.clone()]),
        },
        CodegenSection {
            kind: SectionKind::Rodata,
            alignment: 1,
            executable: false,
            writable: false,
            atoms: rodata_atoms.into(),
        },
        CodegenSection {
            kind: SectionKind::Data,
            alignment: 1,
            executable: false,
            writable: true,
            atoms: Arc::from([]),
        },
        CodegenSection {
            kind: SectionKind::Bss,
            alignment: 1,
            executable: false,
            writable: true,
            atoms: Arc::from([]),
        },
    ]
    .into();
    let content_fingerprint =
        content_fingerprint(&record.codegen.defined_symbol, &sections, &relocations);
    Ok(QueryOutput::success(CodegenUnitValue::Available(Arc::new(
        CodegenUnit {
            defined_symbol: record.codegen.defined_symbol.clone(),
            relocations,
            sections,
            artifacts,
            content_fingerprint,
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
    fn backend_artifacts_participate_in_exact_unit_equality() {
        let empty: Arc<[CodegenSection]> = Arc::from([]);
        let unit = |artifacts| CodegenUnit {
            defined_symbol: Arc::from("main"),
            relocations: Arc::from([]),
            sections: empty.clone(),
            artifacts,
            content_fingerprint: 1,
        };
        assert!(!codegen_unit_value_equal(
            &CodegenUnitValue::Available(Arc::new(unit(rue_codegen::BackendArtifacts {
                mir: Some("one".into()),
                ..Default::default()
            },))),
            &CodegenUnitValue::Available(Arc::new(unit(rue_codegen::BackendArtifacts {
                mir: Some("two".into()),
                ..Default::default()
            },))),
        ));
    }

    #[test]
    fn codegen_unit_retained_charge_counts_canonical_bytes_once() {
        let text = Arc::<[u8]>::from(*b"abc");
        let rodata =
            Arc::<[Arc<[u8]>]>::from([Arc::<[u8]>::from(*b"xy"), Arc::<[u8]>::from(*b"xyz")]);
        let sections: Arc<[CodegenSection]> = vec![
            CodegenSection {
                kind: SectionKind::Text,
                alignment: 16,
                executable: true,
                writable: false,
                atoms: Arc::from([text]),
            },
            CodegenSection {
                kind: SectionKind::Rodata,
                alignment: 1,
                executable: false,
                writable: false,
                atoms: rodata,
            },
            CodegenSection {
                kind: SectionKind::Data,
                alignment: 1,
                executable: false,
                writable: true,
                atoms: Arc::from([]),
            },
            CodegenSection {
                kind: SectionKind::Bss,
                alignment: 1,
                executable: false,
                writable: true,
                atoms: Arc::from([]),
            },
        ]
        .into();
        let unit = CodegenUnit {
            defined_symbol: Arc::from("main"),
            relocations: Arc::from([
                NormalizedRelocation {
                    offset: 1,
                    symbol: Arc::from("a"),
                    kind: rue_codegen::RelocationKind::X86Pc32,
                    addend: 0,
                },
                NormalizedRelocation {
                    offset: 2,
                    symbol: Arc::from("bb"),
                    kind: rue_codegen::RelocationKind::X86Plt32,
                    addend: -4,
                },
            ]),
            sections,
            artifacts: rue_codegen::BackendArtifacts {
                mir: Some("mir".to_owned()),
                ..Default::default()
            },
            content_fingerprint: 0,
        };
        let expected = 4 * std::mem::size_of::<CodegenSection>()
            + std::mem::size_of::<Arc<[u8]>>()
            + 3
            + 2 * std::mem::size_of::<Arc<[u8]>>()
            + 2
            + 3
            + 2 * std::mem::size_of::<NormalizedRelocation>()
            + "main".len()
            + "a".len()
            + "bb".len()
            + 3;
        assert_eq!(unit.retained_charge(), expected as u64);
    }
}
