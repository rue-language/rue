//! Deterministic aggregation of reached codegen terminals before fresh linking.
//!
//! `ProgramImagePlan` is deliberately a description, not an incremental
//! linker.  It records only compiler-owned link inputs; the fresh adapter
//! retains the existing object writer and internal/system linker behavior.

use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};
use tracing::info;

use crate::{
    CompileError, CompileErrors, CompileOptions, CompileOutput, CompileWarning, ErrorKind,
    FunctionWithCfg, LinkerMode, MultiErrorResult, Target, backend,
    codegen_query::{CodegenSection, CollectedCodegenUnit, NormalizedRelocation, SectionKind},
    linking,
};

/// Stable, link-relevant identity for one reached codegen terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgramImageUnit {
    /// The durable callable identity retained by the terminal.  The encoded
    /// key below is only its deterministic ordering/map projection.
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) identity: String,
    pub(crate) defined_symbol: String,
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
    pub(crate) runtime_archive_digest: ContentDigest,
    pub(crate) required_runtime_symbols: Vec<String>,
}

/// Stable local representation avoids making the linker implementation type
/// part of the plan's API surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgramObjectFormat {
    Elf,
    MachO,
}

/// Collision-resistant, deterministic content identity. Every digest below is
/// SHA-256 over a domain-separated, length-framed byte encoding.
pub(crate) type ContentDigest = [u8; 32];

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
            || self.runtime_archive_digest != previous.runtime_archive_digest
            || self.required_runtime_symbols != previous.required_runtime_symbols;
        Ok(delta)
    }
}

/// Retains the plan's canonical terminals and the already-projected export
/// thunk bytes.  The adapter consumes this once; it never re-enters backend
/// lowering, allocation, or emission.
pub(crate) struct ProgramImage {
    pub(crate) plan: ProgramImagePlan,
    units: Vec<CollectedCodegenUnit>,
    export_thunk_objects: Vec<Vec<u8>>,
}

impl ProgramImage {
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
            .filter(|function| export_set.contains(function.legacy_name.as_str()))
            .count();
        if export_thunk_objects.len() != expected_thunks {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError(
                    "C-ABI export thunk projection did not produce one object per export".into(),
                ),
            )));
        }
        let plan = ProgramImagePlan::from_inputs(
            &units,
            functions,
            export_symbols,
            options,
            &export_thunk_objects,
        )?;
        Ok(Self {
            plan,
            units,
            export_thunk_objects,
        })
    }

    /// Project the existing terminal bytes to ordinary object files. This is
    /// the only production object-generation path after aggregation.
    pub(crate) fn fresh_objects(&self, options: &CompileOptions) -> MultiErrorResult<Vec<Vec<u8>>> {
        if self.plan.target != options.target {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "program image target differs from the fresh-link target".into(),
                ),
            )));
        }
        let mut objects = self
            .units
            .iter()
            .map(|collected| {
                backend::project_backend_object(collected.unit.backend_product(), options.target)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(CompileErrors::from)?;
        info!(
            function_count = self.units.len(),
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
    fn from_inputs(
        units: &[CollectedCodegenUnit],
        functions: &[FunctionWithCfg],
        export_symbols: &[String],
        options: &CompileOptions,
        export_thunk_objects: &[Vec<u8>],
    ) -> MultiErrorResult<Self> {
        let mut plan_units = units
            .iter()
            .map(|collected| ProgramImageUnit {
                function: collected.function.semantic_identity.clone(),
                identity: function_identity(&collected.function),
                defined_symbol: collected.unit.defined_symbol.to_string(),
                content_digest: unit_digest(&collected.unit),
            })
            .collect::<Vec<_>>();
        plan_units.sort_by(|left, right| left.identity.cmp(&right.identity));

        let export_set = export_symbols
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let mut export_thunks = functions
            .iter()
            .filter(|function| export_set.contains(function.legacy_name.as_str()))
            .zip(export_thunk_objects)
            .map(|(function, bytes)| ProgramImageExportThunk {
                exported_symbol: function.legacy_name.clone(),
                native_symbol: function.machine_name.clone(),
                content_digest: bytes_digest(b"rue.program-image.export-thunk\0v1\0", bytes),
            })
            .collect::<Vec<_>>();
        export_thunks.sort_by(|left, right| {
            left.exported_symbol
                .cmp(&right.exported_symbol)
                .then_with(|| left.native_symbol.cmp(&right.native_symbol))
        });

        let entry_point = if options.target.is_macho() {
            "__main"
        } else {
            "_start"
        };
        let mut required_runtime_symbols = BTreeSet::new();
        required_runtime_symbols.insert(entry_point.to_owned());
        required_runtime_symbols.insert(rue_runtime_abi::RUNTIME_ABI_VERSION_SYMBOL.to_owned());
        for unit in units {
            for symbol in unit.unit.referenced_symbols.iter() {
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
            runtime_archive_digest: runtime_archive_digest(options.target),
            required_runtime_symbols: required_runtime_symbols.into_iter().collect(),
        };
        validate_program_image_plan(&plan)?;
        Ok(plan)
    }
}

fn validate_program_image_inputs(
    units: &[CollectedCodegenUnit],
    functions: &[FunctionWithCfg],
    export_symbols: &[String],
) -> MultiErrorResult<()> {
    let mut unit_identities = BTreeSet::new();
    let mut defined_symbols = BTreeSet::new();
    for collected in units {
        let identity = function_identity(&collected.function);
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
    for function in functions
        .iter()
        .filter(|function| export_set.contains(function.legacy_name.as_str()))
    {
        if !thunk_identities.insert(function.legacy_name.as_str()) {
            return duplicate_plan_input("C-ABI export thunk identity", &function.legacy_name);
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
        let identity = function_identity(&collected.function);
        let Some(expected_symbol) = functions_by_identity.get(&identity) else {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError(format!(
                    "program image codegen unit `{identity}` has no matching semantic function"
                )),
            )));
        };
        if collected.function.machine_name != *expected_symbol
            || collected.unit.defined_symbol.as_ref() != *expected_symbol
        {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError(format!(
                    "program image codegen unit `{identity}` does not match its semantic function symbol"
                )),
            )));
        }
    }
    Ok(())
}

fn function_identity(function: &FunctionWithCfg) -> String {
    crate::StableSymbolEncoder::encode(&crate::StableSymbolId::Callable(
        crate::StableCallableId::Function(function.semantic_identity.clone()),
    ))
}

fn duplicate_plan_input(kind: &str, identity: &str) -> MultiErrorResult<()> {
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
        if !defined_symbols.insert(unit.defined_symbol.as_str()) {
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

fn unit_digest(unit: &crate::codegen_query::CodegenUnit) -> ContentDigest {
    let mut digest = StableDigest::new(b"rue.program-image.unit\0v1\0");
    digest.bytes(unit.defined_symbol.as_bytes());
    digest.usize(unit.referenced_symbols.len());
    for symbol in unit.referenced_symbols.iter() {
        digest.bytes(symbol.as_bytes());
    }
    digest.usize(unit.relocations.len());
    for relocation in unit.relocations.iter() {
        digest.relocation(relocation);
    }
    digest.usize(unit.sections.len());
    for section in unit.sections.iter() {
        digest.section(section);
    }
    digest.finish()
}

fn bytes_digest(domain: &[u8], bytes: &[u8]) -> ContentDigest {
    let mut digest = StableDigest::new(domain);
    digest.bytes(bytes);
    digest.finish()
}

static X86_64_LINUX_RUNTIME_DIGEST: OnceLock<ContentDigest> = OnceLock::new();
static AARCH64_LINUX_RUNTIME_DIGEST: OnceLock<ContentDigest> = OnceLock::new();
static AARCH64_MACOS_RUNTIME_DIGEST: OnceLock<ContentDigest> = OnceLock::new();

fn runtime_archive_digest(target: Target) -> ContentDigest {
    let cache = match target {
        Target::X86_64Linux => &X86_64_LINUX_RUNTIME_DIGEST,
        Target::Aarch64Linux => &AARCH64_LINUX_RUNTIME_DIGEST,
        Target::Aarch64Macos => &AARCH64_MACOS_RUNTIME_DIGEST,
    };
    *cache.get_or_init(|| {
        bytes_digest(
            b"rue.program-image.runtime-archive\0v1\0",
            linking::runtime_for_target(target),
        )
    })
}

/// SHA-256 writer with a fixed domain and explicit length framing. This avoids
/// boundary ambiguity while retaining deterministic, collision-resistant plan
/// identities across processes and platforms.
struct StableDigest(Sha256);

impl StableDigest {
    fn new(domain: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(domain);
        Self(digest)
    }

    fn byte(&mut self, byte: u8) {
        self.0.update([byte]);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.u64(bytes.len() as u64);
        self.0.update(bytes);
    }

    fn u64(&mut self, value: u64) {
        self.0.update(value.to_le_bytes());
    }

    fn usize(&mut self, value: usize) {
        self.u64(value as u64);
    }

    fn relocation(&mut self, relocation: &NormalizedRelocation) {
        self.u64(relocation.offset);
        self.bytes(relocation.symbol.as_bytes());
        self.byte(match relocation.kind {
            rue_codegen::RelocationKind::X86Pc32 => 0,
            rue_codegen::RelocationKind::X86Plt32 => 1,
            rue_codegen::RelocationKind::Aarch64AdrpPage21 => 2,
            rue_codegen::RelocationKind::Aarch64AddLo12 => 3,
            rue_codegen::RelocationKind::Aarch64Call26 => 4,
        });
        self.u64(relocation.addend as u64);
    }

    fn section(&mut self, section: &CodegenSection) {
        self.byte(match section.kind {
            SectionKind::Text => 0,
            SectionKind::Rodata => 1,
            SectionKind::Data => 2,
            SectionKind::Bss => 3,
        });
        self.bytes(&section.contents.bytes);
        self.u64(u64::from(section.contents.alignment));
        self.byte(u8::from(section.contents.executable));
        self.byte(u8::from(section.contents.writable));
        self.usize(section.atoms.len());
        for atom in section.atoms.iter() {
            self.bytes(atom);
        }
    }

    fn finish(self) -> ContentDigest {
        self.0.finalize().into()
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
        let foreign =
            backend::collect_foreign_symbols(rir.rir(), rir.semantic_symbols().interner());
        let exports = backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
        let units = session
            .codegen_units(
                &semantic,
                &foreign,
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
        let foreign =
            backend::collect_foreign_symbols(rir.rir(), rir.semantic_symbols().interner());
        let exports = backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
        let products = session
            .codegen_products(
                &semantic,
                &foreign,
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
            defined_symbol: identity.to_owned(),
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
            runtime_archive_digest: [1; 32],
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
        let rir = session.canonical_rir().unwrap();
        let semantic = session.canonical_semantic(&options).unwrap();
        let foreign =
            backend::collect_foreign_symbols(rir.rir(), rir.semantic_symbols().interner());
        let units = session
            .codegen_units(
                &semantic,
                &foreign,
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
        second.function.semantic_identity =
            crate::FunctionInstanceKey::DropGlue(Box::new(crate::TypeInstanceKey::I64));
        duplicate_symbol.push(second);
        let error = ProgramImage::new(duplicate_symbol, &functions, &options, &[])
            .err()
            .unwrap();
        assert!(error.to_string().contains("duplicate defined symbol"));
    }

    #[test]
    fn duplicate_export_thunk_identities_are_rejected() {
        let (units, mut functions, options) = session_inputs("fn main() -> i32 { 0 }");
        let exported = functions[0].legacy_name.clone();
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
        let foreign =
            backend::collect_foreign_symbols(rir.rir(), rir.semantic_symbols().interner());
        let exports = backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
        let first = ProgramImage::new(
            session
                .codegen_units(
                    &semantic,
                    &foreign,
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
        let foreign =
            backend::collect_foreign_symbols(rir.rir(), rir.semantic_symbols().interner());
        let exports = backend::collect_export_symbols(rir.rir(), rir.semantic_symbols().interner());
        let second = ProgramImage::new(
            session
                .codegen_units(
                    &semantic,
                    &foreign,
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
        for target in [
            Target::X86_64Linux,
            Target::Aarch64Linux,
            Target::Aarch64Macos,
        ] {
            let (_, objects, _) = image_for(source, target, 1);
            assert_eq!(objects, old_objects_for(source, target), "{target:?}");
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
        let old_foreign =
            backend::collect_foreign_symbols(old_rir.rir(), old_rir.semantic_symbols().interner());
        let old_exports =
            backend::collect_export_symbols(old_rir.rir(), old_rir.semantic_symbols().interner());
        let old = backend::compile_backend_products(
            old_semantic.functions(),
            old_session
                .codegen_products(
                    &old_semantic,
                    &old_foreign,
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
        let fresh_rir = fresh_session.canonical_rir().unwrap();
        let fresh_semantic = fresh_session.canonical_semantic(&options).unwrap();
        let foreign = backend::collect_foreign_symbols(
            fresh_rir.rir(),
            fresh_rir.semantic_symbols().interner(),
        );
        let exports = backend::collect_export_symbols(
            fresh_rir.rir(),
            fresh_rir.semantic_symbols().interner(),
        );
        let image = ProgramImage::new(
            fresh_session
                .codegen_units(
                    &fresh_semantic,
                    &foreign,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap(),
            fresh_semantic.functions(),
            &options,
            &exports,
        )
        .unwrap();
        let fresh = image
            .fresh_link(&options, fresh_semantic.warnings())
            .unwrap();
        assert_eq!(fresh.elf, old.elf);
        assert_eq!(fresh.warnings, old.warnings);
    }
}
