//! Deterministic aggregation of reached codegen terminals before fresh linking.
//!
//! `ProgramImagePlan` is deliberately a description, not an incremental
//! linker.  It records only compiler-owned link inputs; the fresh adapter
//! retains the existing object writer and internal/system linker behavior.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock},
};
use tracing::{info, info_span};

#[cfg(test)]
use crate::FunctionWithCfg;

use crate::{
    CompileError, CompileErrors, CompileOptions, CompileOutput, CompileWarning, ErrorKind,
    LinkerMode, MultiErrorResult, Target, backend,
    content_digest::{ContentDigest, bytes_digest},
    linking,
    object_query::CollectedObjectProjection,
};

#[cfg(test)]
use crate::codegen_query::CollectedCodegenUnit;

/// Stable, link-relevant identity for one reached codegen terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgramImageUnit {
    /// The durable callable identity retained by the terminal.  The encoded
    /// key below is only its deterministic ordering/map projection.
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) identity: String,
    pub(crate) defined_symbol: Arc<str>,
    pub(crate) content_digest: ContentDigest,
}

/// The deterministic contribution of a C-ABI export thunk.  Thunks are not
/// codegen terminals, but their object bytes are a compiler-owned link input
/// and therefore have a separate, explicit plan entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgramImageExportThunk {
    pub(crate) exported_symbol: String,
    pub(crate) native_symbol: String,
    pub(crate) content_digest: ContentDigest,
}

/// Rooted C-export metadata projected from the exact optimized CFG terminal.
/// It contains only the ABI facts needed to build the forwarding thunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootedExportThunk {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) exported_symbol: String,
    pub(crate) native_symbol: String,
    pub(crate) param_types: Vec<rue_air::Type>,
}

/// Compiler-owned inputs to a fresh program link.  It deliberately excludes
/// diagnostics, source positions, user archives, and system-linker commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgramImagePlan {
    pub(crate) units: Vec<ProgramImageUnit>,
    pub(crate) export_thunks: Vec<ProgramImageExportThunk>,
    pub(crate) target: Target,
    pub(crate) object_format: ProgramObjectFormat,
    pub(crate) entry_point: &'static str,
    pub(crate) runtime_abi_version: u32,
    pub(crate) runtime_abi_symbol: &'static str,
    pub(crate) runtime_archive: RuntimeArchiveIdentity,
    pub(crate) required_runtime_symbols: Vec<String>,
}

/// Stable local representation avoids making the linker implementation type
/// part of the plan's API surface.
pub(crate) use crate::object_query::ObjectFormat as ProgramObjectFormat;

/// Exact per-unit transition between two plans.  Linker placement/state is
/// intentionally absent: a later incremental linker can consume these facts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)] // retained as the Phase-11 handoff to a later incremental linker
pub(crate) struct ProgramImagePlanDelta {
    pub(crate) added: Vec<ProgramImageUnit>,
    pub(crate) changed: Vec<ProgramImageUnit>,
    pub(crate) removed: Vec<ProgramImageUnit>,
    pub(crate) added_export_thunks: Vec<ProgramImageExportThunk>,
    pub(crate) changed_export_thunks: Vec<ProgramImageExportThunk>,
    pub(crate) removed_export_thunks: Vec<ProgramImageExportThunk>,
    /// Target, object format, runtime, or entry changes invalidate the whole
    /// fresh link. They have no meaningful add/change/remove unit mapping.
    pub(crate) link_context_changed: bool,
}

impl ProgramImagePlan {
    #[allow(dead_code)] // consumed by the later stateful-linking phase
    pub(crate) fn delta_from(&self, previous: &Self) -> MultiErrorResult<ProgramImagePlanDelta> {
        validate_program_image_plan(previous)?;
        validate_program_image_plan(self)?;
        let before = previous
            .units
            .iter()
            .map(|unit| (unit.identity.as_str(), unit))
            .collect::<BTreeMap<_, _>>();
        let after = self
            .units
            .iter()
            .map(|unit| (unit.identity.as_str(), unit))
            .collect::<BTreeMap<_, _>>();
        let mut delta = ProgramImagePlanDelta::default();
        for (identity, unit) in &after {
            match before.get(identity) {
                None => delta.added.push((**unit).clone()),
                Some(previous) if *previous != *unit => delta.changed.push((**unit).clone()),
                Some(_) => {}
            }
        }
        for (identity, unit) in &before {
            if !after.contains_key(identity) {
                delta.removed.push((**unit).clone());
            }
        }
        let before_thunks = previous
            .export_thunks
            .iter()
            .map(|thunk| (thunk.exported_symbol.as_str(), thunk))
            .collect::<BTreeMap<_, _>>();
        let after_thunks = self
            .export_thunks
            .iter()
            .map(|thunk| (thunk.exported_symbol.as_str(), thunk))
            .collect::<BTreeMap<_, _>>();
        for (symbol, thunk) in &after_thunks {
            match before_thunks.get(symbol) {
                None => delta.added_export_thunks.push((**thunk).clone()),
                Some(previous) if *previous != *thunk => {
                    delta.changed_export_thunks.push((**thunk).clone())
                }
                Some(_) => {}
            }
        }
        for (symbol, thunk) in &before_thunks {
            if !after_thunks.contains_key(symbol) {
                delta.removed_export_thunks.push((**thunk).clone());
            }
        }
        delta.link_context_changed = self.target != previous.target
            || self.object_format != previous.object_format
            || self.entry_point != previous.entry_point
            || self.runtime_abi_version != previous.runtime_abi_version
            || self.runtime_abi_symbol != previous.runtime_abi_symbol
            || self.runtime_archive != previous.runtime_archive
            || self.required_runtime_symbols != previous.required_runtime_symbols;
        Ok(delta)
    }
}

/// Retains the plan's canonical terminals and the already-projected export
/// thunk bytes.  The adapter consumes this once; it never re-enters backend
/// lowering, allocation, or emission.
pub(crate) struct ProgramImage {
    pub(crate) plan: ProgramImagePlan,
    objects: Vec<CollectedObjectProjection>,
    export_thunk_objects: Vec<Vec<u8>>,
}

impl ProgramImage {
    /// Construct the production image directly from rooted codegen terminals
    /// and their exact C-export projections.
    pub(crate) fn from_rooted(
        objects: Vec<CollectedObjectProjection>,
        exports: Vec<RootedExportThunk>,
        options: &CompileOptions,
    ) -> MultiErrorResult<Self> {
        // Aggregating link inputs is compiler-root work like any other pass. It
        // stayed outside every phase span until RUE-1257, so ADR-0067's
        // taxonomy could only report its cost as `unattributed`.
        let _span = info_span!("program_image_plan", phase = "object_generation").entered();
        let unit_identities = validate_rooted_program_image_inputs(&objects, &exports)?;
        let export_thunk_objects: Vec<Vec<u8>> = exports
            .iter()
            .map(|export| {
                backend::generate_export_thunk_object(
                    options.target,
                    &export.exported_symbol,
                    &export.native_symbol,
                    &export.param_types,
                )
            })
            .collect();
        let plan = ProgramImagePlan::from_rooted_inputs(
            &objects,
            unit_identities,
            &exports,
            options,
            &export_thunk_objects,
        )?;
        Ok(Self {
            plan,
            objects,
            export_thunk_objects,
        })
    }

    #[cfg(test)]
    pub(crate) fn new(
        units: Vec<CollectedCodegenUnit>,
        functions: &[FunctionWithCfg],
        options: &CompileOptions,
        export_symbols: &[String],
    ) -> MultiErrorResult<Self> {
        validate_program_image_inputs(&units, functions, export_symbols)?;
        backend::validate_backend_functions(functions)?;
        let export_thunk_objects =
            backend::generate_export_thunk_objects(functions, options, export_symbols);
        let export_set = export_symbols
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected_thunks = functions
            .iter()
            .filter(|function| {
                function
                    .definition_source_name()
                    .is_some_and(|name| export_set.contains(name))
            })
            .count();
        if export_thunk_objects.len() != expected_thunks {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError(
                    "C-ABI export thunk projection did not produce one object per export".into(),
                ),
            )));
        }
        let objects = units
            .into_iter()
            .map(|collected| {
                backend::project_backend_object(collected.unit.backend_product(), options.target)
                    .map(|bytes| CollectedObjectProjection {
                        function: collected.function,
                        unit: collected.unit,
                        object: std::sync::Arc::new(
                            crate::object_query::ObjectProjection::from_bytes(bytes),
                        ),
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(CompileErrors::from)?;
        let plan = ProgramImagePlan::from_inputs(
            &objects,
            functions,
            export_symbols,
            options,
            &export_thunk_objects,
        )?;
        Ok(Self {
            plan,
            objects,
            export_thunk_objects,
        })
    }

    /// Collect the already-projected per-unit bytes for the fresh linker.
    /// Object serialization happened in the registered query graph; this
    /// adapter only materializes the linker's existing owned byte-vector API.
    pub(crate) fn fresh_objects(&self, options: &CompileOptions) -> MultiErrorResult<Vec<Vec<u8>>> {
        let _span =
            info_span!("fresh_object_materialization", phase = "object_generation").entered();
        if self.plan.target != options.target {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "program image target differs from the fresh-link target".into(),
                ),
            )));
        }
        let mut objects = self
            .objects
            .iter()
            .map(|collected| collected.object.bytes.to_vec())
            .collect::<Vec<_>>();
        info!(
            function_count = self.objects.len(),
            object_bytes = objects.iter().map(Vec::len).sum::<usize>(),
            "codegen complete"
        );
        objects.extend(self.export_thunk_objects.iter().cloned());
        Ok(objects)
    }

    /// Invoke the existing fresh linker. Warnings are intentionally attached
    /// only here, after the diagnostic-free plan has been constructed.
    pub(crate) fn fresh_link(
        &self,
        options: &CompileOptions,
        warnings: &[CompileWarning],
    ) -> MultiErrorResult<CompileOutput> {
        let objects = self.fresh_objects(options)?;
        match &options.linker {
            LinkerMode::Internal => {
                linking::link_internal_with_warnings(options, &objects, warnings)
            }
            LinkerMode::System(command) => {
                linking::link_system_with_warnings(options, &objects, command, warnings)
            }
        }
    }
}

impl ProgramImagePlan {
    fn from_rooted_inputs(
        units: &[CollectedObjectProjection],
        unit_identities: Vec<String>,
        exports: &[RootedExportThunk],
        options: &CompileOptions,
        export_thunk_objects: &[Vec<u8>],
    ) -> MultiErrorResult<Self> {
        let mut plan_units = units
            .iter()
            .zip(unit_identities)
            .map(|(collected, identity)| ProgramImageUnit {
                function: collected.function.clone(),
                identity,
                defined_symbol: collected.unit.defined_symbol.clone(),
                content_digest: collected.object.content_digest,
            })
            .collect::<Vec<_>>();
        plan_units.sort_by(|left, right| left.identity.cmp(&right.identity));

        let mut export_thunks = exports
            .iter()
            .zip(export_thunk_objects)
            .map(|(export, bytes)| ProgramImageExportThunk {
                exported_symbol: export.exported_symbol.clone(),
                native_symbol: export.native_symbol.clone(),
                content_digest: bytes_digest(b"rue.program-image.export-thunk\0v1\0", bytes),
            })
            .collect::<Vec<_>>();
        export_thunks.sort_by(|left, right| {
            left.exported_symbol
                .cmp(&right.exported_symbol)
                .then_with(|| left.native_symbol.cmp(&right.native_symbol))
        });
        Self::finish(
            plan_units,
            export_thunks,
            units.iter().map(|unit| unit.unit.as_ref()),
            options,
        )
    }

    #[cfg(test)]
    fn from_inputs(
        units: &[CollectedObjectProjection],
        functions: &[FunctionWithCfg],
        export_symbols: &[String],
        options: &CompileOptions,
        export_thunk_objects: &[Vec<u8>],
    ) -> MultiErrorResult<Self> {
        let mut plan_units = units
            .iter()
            .map(|collected| ProgramImageUnit {
                function: collected.function.clone(),
                identity: stable_function_identity(&collected.function),
                defined_symbol: collected.unit.defined_symbol.clone(),
                content_digest: collected.object.content_digest,
            })
            .collect::<Vec<_>>();
        plan_units.sort_by(|left, right| left.identity.cmp(&right.identity));

        let export_set = export_symbols
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let mut export_thunks = functions
            .iter()
            .filter_map(|function| {
                function
                    .definition_source_name()
                    .filter(|name| export_set.contains(name))
                    .map(|name| (function, name))
            })
            .zip(export_thunk_objects)
            .map(
                |((function, exported_symbol), bytes)| ProgramImageExportThunk {
                    exported_symbol: exported_symbol.to_owned(),
                    native_symbol: function.machine_name.clone(),
                    content_digest: bytes_digest(b"rue.program-image.export-thunk\0v1\0", bytes),
                },
            )
            .collect::<Vec<_>>();
        export_thunks.sort_by(|left, right| {
            left.exported_symbol
                .cmp(&right.exported_symbol)
                .then_with(|| left.native_symbol.cmp(&right.native_symbol))
        });

        Self::finish(
            plan_units,
            export_thunks,
            units.iter().map(|unit| unit.unit.as_ref()),
            options,
        )
    }

    fn finish<'a>(
        plan_units: Vec<ProgramImageUnit>,
        export_thunks: Vec<ProgramImageExportThunk>,
        units: impl IntoIterator<Item = &'a crate::codegen_query::CodegenUnit>,
        options: &CompileOptions,
    ) -> MultiErrorResult<Self> {
        let entry_point = if options.target.is_macho() {
            "__main"
        } else {
            "_start"
        };
        let mut required_runtime_symbols = BTreeSet::new();
        required_runtime_symbols.insert(entry_point.to_owned());
        required_runtime_symbols.insert(rue_runtime_abi::RUNTIME_ABI_VERSION_SYMBOL.to_owned());
        for unit in units {
            for symbol in unit.referenced_symbols.iter() {
                if rue_runtime_abi::classify_export(symbol).is_some() {
                    required_runtime_symbols.insert(symbol.to_string());
                }
            }
        }

        let plan = Self {
            units: plan_units,
            export_thunks,
            target: options.target,
            object_format: if options.target.is_macho() {
                ProgramObjectFormat::MachO
            } else {
                ProgramObjectFormat::Elf
            },
            entry_point,
            runtime_abi_version: rue_runtime_abi::RUNTIME_ABI_VERSION,
            runtime_abi_symbol: rue_runtime_abi::RUNTIME_ABI_VERSION_SYMBOL,
            runtime_archive: RuntimeArchiveIdentity::for_target(options.target),
            required_runtime_symbols: required_runtime_symbols.into_iter().collect(),
        };
        // Both private construction paths validate their raw unit and export
        // identities before translating them into this canonical plan.
        // Retained-plan deltas independently validate both compared plans.
        Ok(plan)
    }
}

#[cfg(test)]
fn validate_program_image_inputs(
    units: &[CollectedCodegenUnit],
    functions: &[FunctionWithCfg],
    export_symbols: &[String],
) -> MultiErrorResult<()> {
    let mut unit_identities = BTreeSet::new();
    let mut defined_symbols = BTreeSet::new();
    for collected in units {
        let identity = stable_function_identity(&collected.function);
        if !unit_identities.insert(identity.clone()) {
            return duplicate_plan_input("codegen unit identity", &identity);
        }
        if !defined_symbols.insert(collected.unit.defined_symbol.to_string()) {
            return duplicate_plan_input("defined symbol", &collected.unit.defined_symbol);
        }
    }

    let export_set = export_symbols
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if export_set.len() != export_symbols.len() {
        return duplicate_plan_input("C-ABI export symbol", "duplicate export declaration");
    }
    let mut thunk_identities = BTreeSet::new();
    for exported_symbol in functions
        .iter()
        .filter_map(FunctionWithCfg::definition_source_name)
        .filter(|name| export_set.contains(name))
    {
        if !thunk_identities.insert(exported_symbol) {
            return duplicate_plan_input("C-ABI export thunk identity", exported_symbol);
        }
    }

    if units.len() != functions.len() {
        return Err(CompileErrors::from(CompileError::without_span(
            ErrorKind::InternalError(format!(
                "program image has {} codegen units for {} semantic functions",
                units.len(),
                functions.len()
            )),
        )));
    }
    let mut functions_by_identity = BTreeMap::new();
    for function in functions {
        let identity = function_identity(function);
        if functions_by_identity
            .insert(identity.clone(), function.machine_name.as_str())
            .is_some()
        {
            return duplicate_plan_input("semantic function identity", &identity);
        }
    }
    for collected in units {
        let identity = stable_function_identity(&collected.function);
        let Some(expected_symbol) = functions_by_identity.get(&identity) else {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError(format!(
                    "program image codegen unit `{identity}` has no matching semantic function"
                )),
            )));
        };
        if collected.unit.defined_symbol.as_ref() != *expected_symbol {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError(format!(
                    "program image codegen unit `{identity}` does not match its semantic function symbol"
                )),
            )));
        }
    }
    Ok(())
}

fn validate_rooted_program_image_inputs(
    units: &[CollectedObjectProjection],
    exports: &[RootedExportThunk],
) -> MultiErrorResult<Vec<String>> {
    let mut units_by_identity = BTreeMap::new();
    let mut defined_symbols = BTreeSet::new();
    let mut unit_identities = Vec::with_capacity(units.len());
    for collected in units {
        let identity = stable_function_identity(&collected.function);
        if units_by_identity
            .insert(identity.clone(), collected.unit.defined_symbol.as_ref())
            .is_some()
        {
            return duplicate_plan_input("codegen unit identity", &identity);
        }
        unit_identities.push(identity);
        if !defined_symbols.insert(collected.unit.defined_symbol.as_ref()) {
            return duplicate_plan_input("defined symbol", &collected.unit.defined_symbol);
        }
    }
    if !units
        .iter()
        .any(|unit| unit.unit.defined_symbol.as_ref() == "main")
    {
        return Err(CompileError::without_span(ErrorKind::NoMainFunction).into());
    }
    let mut exported_symbols = BTreeSet::new();
    for export in exports {
        if !exported_symbols.insert(export.exported_symbol.as_str()) {
            return duplicate_plan_input("C-ABI export symbol", &export.exported_symbol);
        }
        let identity = stable_function_identity(&export.function);
        let Some(defined) = units_by_identity.get(&identity) else {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError(format!(
                    "C-ABI export `{}` has no rooted codegen unit",
                    export.exported_symbol
                )),
            )));
        };
        if **defined != export.native_symbol {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError(format!(
                    "C-ABI export `{}` names a foreign native symbol",
                    export.exported_symbol
                )),
            )));
        }
    }
    Ok(unit_identities)
}

#[cfg(test)]
fn function_identity(function: &FunctionWithCfg) -> String {
    stable_function_identity(&function.semantic_identity)
}

fn stable_function_identity(function: &crate::FunctionInstanceKey) -> String {
    crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
        crate::StableCallableId::Function(function.clone()),
    ))
}

fn duplicate_plan_input<T>(kind: &str, identity: &str) -> MultiErrorResult<T> {
    Err(CompileErrors::from(CompileError::without_span(
        ErrorKind::InternalError(format!("program image has duplicate {kind} `{identity}`")),
    )))
}

fn validate_program_image_plan(plan: &ProgramImagePlan) -> MultiErrorResult<()> {
    let mut unit_identities = BTreeSet::new();
    let mut defined_symbols = BTreeSet::new();
    for unit in &plan.units {
        if !unit_identities.insert(unit.identity.as_str()) {
            return duplicate_plan_input("codegen unit identity", &unit.identity);
        }
        if !defined_symbols.insert(unit.defined_symbol.as_ref()) {
            return duplicate_plan_input("defined symbol", &unit.defined_symbol);
        }
    }
    let mut thunk_identities = BTreeSet::new();
    for thunk in &plan.export_thunks {
        if !thunk_identities.insert(thunk.exported_symbol.as_str()) {
            return duplicate_plan_input("C-ABI export thunk identity", &thunk.exported_symbol);
        }
    }
    Ok(())
}

static X86_64_LINUX_RUNTIME_DIGEST: OnceLock<ContentDigest> = OnceLock::new();
static AARCH64_LINUX_RUNTIME_DIGEST: OnceLock<ContentDigest> = OnceLock::new();
static AARCH64_MACOS_RUNTIME_DIGEST: OnceLock<ContentDigest> = OnceLock::new();

/// Counts every request for archive content, cached or not, so a test can
/// assert that an ordinary compilation never reaches for the archive bytes.
#[cfg(test)]
static RUNTIME_ARCHIVE_CONTENT_REQUESTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Which runtime archive a plan links against.
///
/// The archive is an `include_bytes!` constant of the compiler binary, so
/// within one process the target names it exactly: two plans built by the same
/// compiler link the same runtime whenever they agree on the target, and
/// comparing them needs no bytes at all. [`Self::content_digest`] is the
/// stronger identity a plan needs only once it outlives the process that built
/// it, and is therefore materialized on demand.
///
/// The distinction is worth the type. Digesting several megabytes of archive
/// costs multiples of an entire fresh compilation of a small program, so
/// computing it during plan construction made every fresh link pay for a fact
/// no fresh link reads (RUE-1257).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeArchiveIdentity {
    target: Target,
}

impl RuntimeArchiveIdentity {
    pub(crate) fn for_target(target: Target) -> Self {
        Self { target }
    }

    /// SHA-256 over the exact embedded archive bytes, memoized for the process
    /// because those bytes cannot change within one.
    #[allow(dead_code)] // the durable identity a later incremental linker persists
    pub(crate) fn content_digest(self) -> ContentDigest {
        #[cfg(test)]
        RUNTIME_ARCHIVE_CONTENT_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cache = match self.target {
            Target::X86_64Linux => &X86_64_LINUX_RUNTIME_DIGEST,
            Target::Aarch64Linux => &AARCH64_LINUX_RUNTIME_DIGEST,
            Target::Aarch64Macos => &AARCH64_MACOS_RUNTIME_DIGEST,
        };
        *cache.get_or_init(|| {
            bytes_digest(
                b"rue.program-image.runtime-archive\0v1\0",
                linking::runtime_for_target(self.target),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_for(
        source: &str,
        target: Target,
        workers: usize,
    ) -> (ProgramImagePlan, Vec<Vec<u8>>, Vec<String>) {
        let snapshot = crate::SourceSnapshot::single("main.rue", source).unwrap();
        let options = CompileOptions {
            target,
            ..CompileOptions::default()
        };
        let mut session = crate::CompilerSession::with_query_concurrency(workers);
        crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
        let rir = session.canonical_rir().unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let exports = backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
        let units = session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let image = ProgramImage::new(units, semantic.functions(), &options, &exports).unwrap();
        (
            image.plan.clone(),
            image.fresh_objects(&options).unwrap(),
            semantic
                .warnings()
                .iter()
                .map(ToString::to_string)
                .collect(),
        )
    }

    fn old_objects_for(source: &str, target: Target) -> Vec<Vec<u8>> {
        let snapshot = crate::SourceSnapshot::single("main.rue", source).unwrap();
        let options = CompileOptions {
            target,
            ..CompileOptions::default()
        };
        let mut session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
        let rir = session.canonical_rir().unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let exports = backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
        let products = session
            .codegen_products(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        backend::generate_pre_link_objects_from_products(
            semantic.functions(),
            products,
            &options,
            &exports,
        )
        .unwrap()
    }

    fn unit(identity: &str, digest_byte: u8) -> ProgramImageUnit {
        ProgramImageUnit {
            function: crate::FunctionInstanceKey::DropGlue(Box::new(crate::TypeInstanceKey::I64)),
            identity: identity.to_owned(),
            defined_symbol: Arc::from(identity),
            content_digest: [digest_byte; 32],
        }
    }

    fn plan(units: Vec<ProgramImageUnit>) -> ProgramImagePlan {
        ProgramImagePlan {
            units,
            export_thunks: Vec::new(),
            target: Target::X86_64Linux,
            object_format: ProgramObjectFormat::Elf,
            entry_point: "_start",
            runtime_abi_version: 1,
            runtime_abi_symbol: "__rue_runtime_abi_v1",
            runtime_archive: RuntimeArchiveIdentity::for_target(Target::X86_64Linux),
            required_runtime_symbols: vec!["_start".to_owned()],
        }
    }

    #[test]
    fn delta_is_exact_and_sorted_for_add_change_remove() {
        let before = plan(vec![unit("a", 1), unit("removed", 1), unit("changed", 1)]);
        let after = plan(vec![unit("added", 1), unit("a", 1), unit("changed", 2)]);
        let delta = after.delta_from(&before).unwrap();
        assert_eq!(delta.added, vec![unit("added", 1)]);
        assert_eq!(delta.changed, vec![unit("changed", 2)]);
        assert_eq!(delta.removed, vec![unit("removed", 1)]);

        let reordered = plan(vec![unit("changed", 2), unit("added", 1), unit("a", 1)]);
        assert_eq!(
            reordered.delta_from(&after).unwrap(),
            ProgramImagePlanDelta::default(),
            "input enumeration order is not a program-image change"
        );
    }

    #[test]
    fn delta_tracks_serialized_object_bytes_not_only_codegen_metadata() {
        let mut before = unit("same", 1);
        before.content_digest =
            bytes_digest(b"rue.program-image.object\0v1\0", b"first object encoding");
        let mut after = before.clone();
        after.content_digest =
            bytes_digest(b"rue.program-image.object\0v1\0", b"second object encoding");
        let delta = plan(vec![after.clone()])
            .delta_from(&plan(vec![before]))
            .unwrap();
        assert_eq!(delta.changed, vec![after]);
    }

    #[test]
    fn delta_covers_export_thunks_and_plan_wide_link_inputs() {
        let thunk = |symbol: &str, digest_byte| ProgramImageExportThunk {
            exported_symbol: symbol.to_owned(),
            native_symbol: format!("native_{symbol}"),
            content_digest: [digest_byte; 32],
        };
        let mut before = plan(vec![]);
        before.export_thunks = vec![thunk("removed", 1), thunk("changed", 1)];
        let mut after = plan(vec![]);
        after.export_thunks = vec![thunk("added", 1), thunk("changed", 2)];
        after
            .required_runtime_symbols
            .push("__rue_memcpy".to_owned());
        let delta = after.delta_from(&before).unwrap();
        assert_eq!(delta.added_export_thunks, vec![thunk("added", 1)]);
        assert_eq!(delta.changed_export_thunks, vec![thunk("changed", 2)]);
        assert_eq!(delta.removed_export_thunks, vec![thunk("removed", 1)]);
        assert!(delta.link_context_changed);
    }

    #[test]
    fn digest_is_stable_and_content_sensitive() {
        assert_eq!(
            bytes_digest(b"test-domain", b"runtime"),
            bytes_digest(b"test-domain", b"runtime")
        );
        assert_ne!(
            bytes_digest(b"test-domain", b"runtime"),
            bytes_digest(b"test-domain", b"Runtime")
        );
    }

    /// The runtime archive is a build-time constant of the compiler binary, so
    /// a plan identifies it by target and digests it only when something asks
    /// for the durable identity. Digesting it during plan construction made a
    /// fresh compilation of `fn main() -> i32 { return 0; }` several times
    /// slower than the whole rest of the pipeline (RUE-1257).
    ///
    /// This is the only test that materializes the digest, so the counter it
    /// reads is untouched by anything but production plan construction.
    #[test]
    fn a_fresh_program_image_never_digests_the_runtime_archive() {
        use std::sync::atomic::Ordering;

        let source = "fn callee() -> i32 { 7 } fn main() -> i32 { callee() }";
        let snapshot = crate::SourceSnapshot::single("main.rue", source).unwrap();
        let options = CompileOptions {
            target: Target::X86_64Linux,
            ..CompileOptions::default()
        };
        let mut session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
        let rooted = session
            .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
            .unwrap();
        let image = ProgramImage::from_rooted(rooted.objects, rooted.exports, &options).unwrap();
        image.fresh_objects(&options).unwrap();
        assert_eq!(
            RUNTIME_ARCHIVE_CONTENT_REQUESTS.load(Ordering::Relaxed),
            0,
            "building and materializing a program image must not read the runtime archive"
        );

        // The durable identity is still exact when a caller does want it.
        let identity = image.plan.runtime_archive;
        assert_eq!(
            identity,
            RuntimeArchiveIdentity::for_target(Target::X86_64Linux)
        );
        assert_eq!(identity.content_digest(), identity.content_digest());
        assert_ne!(
            identity.content_digest(),
            RuntimeArchiveIdentity::for_target(Target::Aarch64Linux).content_digest(),
            "each target embeds its own archive"
        );
    }

    fn session_inputs(
        source: &str,
    ) -> (
        Vec<CollectedCodegenUnit>,
        Vec<FunctionWithCfg>,
        CompileOptions,
    ) {
        let snapshot = crate::SourceSnapshot::single("main.rue", source).unwrap();
        let options = CompileOptions {
            target: Target::X86_64Linux,
            ..CompileOptions::default()
        };
        let mut session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let units = session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        (units, semantic.functions().to_vec(), options)
    }

    #[test]
    fn duplicate_unit_identities_and_defined_symbols_are_rejected() {
        let (units, functions, options) = session_inputs("fn main() -> i32 { 0 }");
        let mut duplicate_identity = units.clone();
        duplicate_identity.push(duplicate_identity[0].clone());
        let error = ProgramImage::new(duplicate_identity, &functions, &options, &[])
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("duplicate codegen unit identity")
        );

        let mut duplicate_symbol = units;
        let mut second = duplicate_symbol[0].clone();
        second.function =
            crate::FunctionInstanceKey::DropGlue(Box::new(crate::TypeInstanceKey::I64));
        duplicate_symbol.push(second);
        let error = ProgramImage::new(duplicate_symbol, &functions, &options, &[])
            .err()
            .unwrap();
        assert!(error.to_string().contains("duplicate defined symbol"));
    }

    /// A C export's linker-visible name is the identifier its declaration
    /// spells, independently of the module-qualified internal symbol its native
    /// body carries (ADR-0064 P4, RUE-1125).
    #[test]
    fn export_thunks_carry_the_declared_c_name_over_a_module_qualified_body() {
        let source = "pub extern \"C\" fn rue_answer() -> i32 { 42 }\n\
                      fn main() -> i32 { 0 }";
        let snapshot = crate::SourceSnapshot::single("main.rue", source).unwrap();
        let options = CompileOptions {
            preview_features: crate::PreviewFeatures::from([crate::PreviewFeature::CFfi]),
            ..CompileOptions::default()
        };
        let mut session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
        let rir = session.canonical_rir().unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let exports = backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
        assert_eq!(exports, ["rue_answer"]);
        let units = session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let image = ProgramImage::new(units, semantic.functions(), &options, &exports).unwrap();

        let exported = semantic
            .functions()
            .iter()
            .find(|function| function.definition_source_name() == Some("rue_answer"))
            .expect("the export is code-generated as an ordinary body");
        assert_eq!(
            exported.legacy_name, "__rue_fn_main_2erue__rue_answer",
            "the export's native body is an ordinary module-qualified callable"
        );
        assert_eq!(
            image.plan.export_thunks.len(),
            1,
            "{:?}",
            image.plan.export_thunks
        );
        assert_eq!(image.plan.export_thunks[0].exported_symbol, "rue_answer");
        assert_eq!(
            image.plan.export_thunks[0].native_symbol,
            exported.machine_name
        );
    }

    #[test]
    fn duplicate_export_thunk_identities_are_rejected() {
        let (units, mut functions, options) = session_inputs("fn main() -> i32 { 0 }");
        let exported = functions[0]
            .definition_source_name()
            .expect("main is an ordinary definition")
            .to_owned();
        functions.push(functions[0].clone());
        let error = ProgramImage::new(units, &functions, &options, &[exported])
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("duplicate C-ABI export thunk identity")
        );
    }

    #[test]
    fn units_must_match_the_supplied_semantic_function_records() {
        let (units, mut functions, options) = session_inputs("fn main() -> i32 { 0 }");
        functions[0].machine_name = "mismatched_main".to_owned();
        let error = ProgramImage::new(units, &functions, &options, &[])
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("does not match its semantic function symbol")
        );
    }

    #[test]
    fn delta_rejects_a_malformed_duplicate_plan_instead_of_overwriting_it() {
        let malformed = plan(vec![unit("same", 1), unit("same", 2)]);
        let error = malformed.delta_from(&plan(vec![])).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("duplicate codegen unit identity")
        );
    }

    #[test]
    fn warning_text_and_source_position_do_not_change_the_plan_or_objects() {
        let first = "fn main() -> i32 { let unused = 1; 0 }";
        let second = "\n\nfn main() -> i32 { let renamed = 1; 0 }";
        let (first_plan, first_objects, first_warnings) = image_for(first, Target::X86_64Linux, 1);
        let (second_plan, second_objects, second_warnings) =
            image_for(second, Target::X86_64Linux, 1);
        assert_eq!(first_plan, second_plan);
        assert_eq!(first_objects, second_objects);
        assert_ne!(first_warnings, second_warnings);
    }

    #[test]
    fn plan_and_object_bytes_are_schedule_independent() {
        let source = "fn callee() -> i32 { 7 } fn main() -> i32 { callee() }";
        let one = image_for(source, Target::X86_64Linux, 1);
        let four = image_for(source, Target::X86_64Linux, 4);
        assert_eq!(one.0, four.0);
        assert_eq!(one.1, four.1);
    }

    #[test]
    fn cold_and_reused_sessions_produce_the_same_plan_and_objects() {
        let source = "fn callee() -> i32 { 7 } fn main() -> i32 { callee() }";
        let snapshot = crate::SourceSnapshot::single("main.rue", source).unwrap();
        let options = CompileOptions {
            target: Target::X86_64Linux,
            ..CompileOptions::default()
        };
        let mut session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
        let rir = session.canonical_rir().unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let exports = backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
        let first = ProgramImage::new(
            session
                .codegen_units(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap(),
            semantic.functions(),
            &options,
            &exports,
        )
        .unwrap();
        let first_plan = first.plan.clone();
        let first_objects = first.fresh_objects(&options).unwrap();

        let rir = session.canonical_rir().unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let exports = backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
        let second = ProgramImage::new(
            session
                .codegen_units(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap(),
            semantic.functions(),
            &options,
            &exports,
        )
        .unwrap();
        assert_eq!(first_plan, second.plan);
        assert_eq!(first_objects, second.fresh_objects(&options).unwrap());
        assert!(
            session
                .codegen_executions()
                .iter()
                .all(|(_, execution)| *execution == rue_query::RequestExecution::Reused)
        );
    }

    #[test]
    fn fresh_adapter_matches_the_prior_object_projection_on_every_target() {
        let source = "fn callee() -> i32 { 7 } fn main() -> i32 { callee() }";
        let snapshot = crate::SourceSnapshot::single("main.rue", source).unwrap();
        for target in [
            Target::X86_64Linux,
            Target::Aarch64Linux,
            Target::Aarch64Macos,
        ] {
            let options = CompileOptions {
                target,
                ..CompileOptions::default()
            };
            let mut session = crate::CompilerSession::new();
            crate::publish_test_snapshot(&mut session, &snapshot).unwrap();
            let rooted = session
                .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
                .unwrap();
            let image =
                ProgramImage::from_rooted(rooted.objects, rooted.exports, &options).unwrap();
            assert_eq!(
                image.fresh_objects(&options).unwrap(),
                old_objects_for(source, target),
                "{target:?}"
            );
        }
    }

    #[test]
    fn fresh_adapter_matches_the_prior_host_executable() {
        let source = "fn callee() -> i32 { 7 } fn main() -> i32 { callee() }";
        let target = Target::host().unwrap();
        let snapshot = crate::SourceSnapshot::single("main.rue", source).unwrap();
        let options = CompileOptions {
            target,
            ..CompileOptions::default()
        };

        let mut old_session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut old_session, &snapshot).unwrap();
        let old_rir = old_session.canonical_rir().unwrap();
        let old_semantic = old_session.canonical_semantic(&options).unwrap();
        let old_exports =
            backend::collect_export_symbols(old_rir.rir(), old_rir.semantic_symbols().interner());
        let old = backend::compile_backend_products(
            old_semantic.functions(),
            old_session
                .codegen_products(
                    &old_semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap(),
            &options,
            old_semantic.warnings(),
            &old_exports,
        )
        .unwrap();

        let mut fresh_session = crate::CompilerSession::new();
        crate::publish_test_snapshot(&mut fresh_session, &snapshot).unwrap();
        let rooted = fresh_session
            .rooted_codegen(&options, rue_codegen::BackendArtifactRequest::default())
            .unwrap();
        let image = ProgramImage::from_rooted(rooted.objects, rooted.exports, &options).unwrap();
        let fresh = image.fresh_link(&options, &rooted.warnings).unwrap();
        assert_eq!(fresh.elf, old.elf);
        assert_eq!(fresh.warnings, old.warnings);
    }
}
