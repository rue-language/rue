#![recursion_limit = "256"]

//! Rue's embeddable compiler facade.
//!
//! Compilation starts with an owned [`SourceSnapshot`]. A [`CompilerSession`]
//! publishes that snapshot and exposes the syntax, program, RIR, semantic, and
//! dependency queries that all compiler consumers share. Query artifacts are
//! immutable and may be retained across later session updates.
//!
//! ```
//! use ahash::AHashMap;
//! use std::sync::Arc;
//! use rue_compiler::{
//!     CompileOptions, CompilerSession, FileId, SourceMetadata, SourceSnapshot, compile_snapshot,
//! };
//!
//! let root = FileId::new(7);
//! let paths = AHashMap::from([(root, "src/main.rue".to_owned())]);
//! let metadata = SourceMetadata::new(root, paths.clone(), paths)?;
//! let snapshot = SourceSnapshot::new(
//!     metadata,
//!     vec![(root, Arc::new("fn main() -> i32 { 0 }".to_owned()))],
//! )?;
//! let mut session = CompilerSession::new();
//! let syntax = session.update(&snapshot).into_result()?;
//! assert!(syntax.modules().next().is_some());
//! let executable = compile_snapshot(&snapshot, &CompileOptions::default())?;
//! assert!(!executable.elf.is_empty());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Callers that need a final executable use [`compile_snapshot`], the sole
//! one-shot stable adapter. Filesystem discovery remains the caller's job.
//! Stable additions are reviewed against the semantic facade inventory. Debug
//! presentation, instrumentation, and in-tree driver adapters live under
//! [`unstable`] and carry no compatibility promise.

mod artifact_views;
mod backend;
mod body_query;
mod bound_definitions;
mod canonical_lower;
mod canonical_merge;
mod canonical_semantic;
mod cfg_query;
mod codegen_query;
mod content_digest;
mod declaration_candidate;
mod definition_snapshot;
mod dependency_envelope;
mod diagnostic;
mod diagnostic_attempt_store;
mod drop_glue;
mod durable_cfg;
mod durable_comptime;
mod durable_semantics;
mod error_printer;
mod import_discovery;
mod import_graph;
mod linking;
mod local_semantic_materialization;
mod object_query;
mod parsed_modules;
mod program_image_plan;
mod queries;
mod retained_charge;
mod revisioned_query_database;
mod semantic_identity;
mod semantic_query_nucleus;
mod semantic_symbols;
mod session;
mod shared_segments;
mod source_identity;
mod source_metadata;
mod source_snapshot;
mod syntax;
mod test_candidates;
mod test_dispatcher;
mod test_inventory;
mod toolchain_module_demand;
mod type_queries;
mod typed_query_store;
pub mod unstable;
mod well_known_option;

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
mod pipeline_tests;
#[cfg(test)]
mod producer_nominal_acceptance_tests;

#[cfg(test)]
mod api_inventory;
#[cfg(test)]
mod assert_comparison_tests;
#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod scaling_harness;
#[cfg(test)]
mod supported_api_inventory;
#[cfg(test)]
mod test_body_try_tests;
#[cfg(test)]
mod test_image_tests;
mod warm_fresh_parity;

// Supported source, identity, option, session, and diagnostic surface.
pub use artifact_views::{
    ImportDiscoveryStatus, ImportDiscoveryView, RirInstructionView, RirOperandView, RirView,
    SourceIdentityView, SourceLocationView, SyntaxModuleView, SyntaxNodeView, SyntaxView,
    TokenView,
};
pub use dependency_envelope::{
    DependencyEnvelope, DependencyEnvelopeStatus, DependencyResolutionOutcome, DependencyTopology,
    DependencyTopologyRecord,
};
#[cfg(test)]
pub(crate) use diagnostic_attempt_store::FrontendDiagnosticIdentity;
pub use diagnostic_attempt_store::{DiagnosticStage, FrontendDiagnosticSnapshot};
pub use import_discovery::{
    AcceptedReadManifest, AcceptedReadManifestEntry, FileMetadataFingerprint, ImportCandidateRole,
    ImportDiscoveryContext, ImportOccurrenceKey, PhysicalFileIdentity,
    trusted_logical_path_for_requested,
};
// Host discovery-protocol records are published through `unstable` only; the
// crate-local paths keep the session and its tests on one spelling.
#[cfg(test)]
pub(crate) use import_discovery::AcceptedImportSource;
pub(crate) use import_discovery::{
    ImportDemandFrontier, ImportDemandMode, ImportDemandRoots, ImportDiscoveryPlan,
    ImportDiscoveryRequest, ImportDiscoveryWave, ImportInputRevision, ImportObservation,
    ImportObservationLedger, ImportObservationStatus,
};
pub use import_graph::{
    CanonicalImportCycle, CanonicalImportGraph, CanonicalImportGraphProblem,
    CanonicalImportGraphValidation, CanonicalImportRecord, CanonicalImportResolution,
    ImportDirective, ImportDirectives,
};
pub(crate) use import_graph::{ResolvedCodegenRevision, ResolvedProgramRevision};
pub(crate) use parsed_modules::{InvalidImportShape, ParsedProgram};
pub use queries::{
    CompileOptions, CompileOutput, LinkerMode, RootSelection, SourceView, compile_snapshot,
};
#[allow(unused_imports)]
pub(crate) use semantic_identity::{
    AnonymousMemberKey, AnonymousMemberKind, AnonymousNominalKey, AnonymousNominalKind,
    CanonicalArgumentValue, CanonicalArguments, CompilerCallableId, FunctionInstanceKey,
    LocalAtomId, LocalAtomKind, NominalInstanceKey, StableCallableId, StableProducerId,
    StableSymbolEncoder, StableSymbolId, StructuralAnchor, StructuralPathSegment, TypeInstanceKey,
};
pub use session::{CanonicalImportGraphOutput, CompilerSession, CompilerSessionUpdate};
pub use source_identity::{
    ModuleId, ModuleRevision, SourceId, SourceIdVersion, SourceRevision,
    TRUSTED_STANDARD_LIBRARY_NAMESPACE,
};
pub use source_metadata::SourceMetadata;
pub use source_snapshot::{MAX_SOURCE_BYTES, MAX_SOURCE_FILES, SourceSnapshot};
// The declared test-candidate inventory is a host protocol record, published
// through `unstable` while `rue test` is still being built (ADR-0083 Phase 2).
pub(crate) use test_candidates::{
    TestCandidateInventory, TestCandidateOutcome, UnimportedTestFile,
};
pub use toolchain_module_demand::{
    OPTION_MODULE_LOGICAL_PATH, ParkedToolchainModules, STRBUF_MODULE_LOGICAL_PATH,
    TrustedToolchainModuleDemand,
};
// Internal registered-query payload — never crosses the crate boundary.
pub(crate) use toolchain_module_demand::BodyToolchainDemand;

// Query keys, invalidation records, dependency manifests, fingerprints, and
// work records are compiler implementation. Keep crate-local paths available
// while the session remains their sole owner, without publishing them through
// the supported facade.
#[allow(unused_imports)]
pub(crate) use canonical_lower::CanonicalRirWork;
#[allow(unused_imports)]
pub(crate) use canonical_merge::CanonicalMergeWork;
#[allow(unused_imports)]
pub(crate) use canonical_semantic::{CandidateBodyPlanWork, CanonicalSemanticWork};
#[allow(unused_imports)]
pub(crate) use definition_snapshot::DefinitionShardWork;
#[allow(unused_imports)]
#[cfg(test)]
pub(crate) use import_discovery::DiscoverySourceAssembler;
pub(crate) use import_discovery::IMPORT_DISCOVERY_POLICY_VERSION;
#[allow(unused_imports)]
pub(crate) use parsed_modules::{ParseInvalidationSummary, ParsedModulesWork};
pub(crate) use queries::{PipelineWork, SourceStats};
pub(crate) use session::RootedCfgOutput;
#[allow(unused_imports)]
pub(crate) use session::{
    CompilerSessionWork, FRONTEND_DIAGNOSTIC_RETENTION_LIMIT, FrontendQueryWork,
    FrontendRetentionMetrics, ImportDiscoveryRevisionArtifact, ImportDiscoveryRevisionStatus,
    ImportGraphInputDescriptor,
};
#[allow(unused_imports)]
pub(crate) use source_identity::{
    CodegenInputDescriptor, ModuleResolutionInput, ModuleResolutionInputs, SemanticInputDescriptor,
    SourceStore, StableOptLevel, StablePreviewFeatures,
};

// Immutable query artifacts and stable identities returned by CompilerSession.
pub(crate) use body_query::{BodyTransaction, transaction_equal};
pub(crate) use bound_definitions::{
    StableDefinitionKey, StableDefinitionKind, StableDefinitionNamespace,
};
pub(crate) use canonical_lower::CanonicalRirOutput;
pub(crate) use canonical_merge::CanonicalMergedProgram;
pub(crate) use definition_snapshot::{DefinitionKind, DefinitionNamespace, DefinitionSnapshot};
#[cfg(test)]
pub(crate) use durable_semantics::DurableDeclarationPayload;
pub(crate) use durable_semantics::DurableDeclarationSemantic;
pub(crate) use durable_semantics::{DurableConstValue, DurableType};

// Small foundational types callers need to configure or inspect the facade.
pub use rue_cfg::OptLevel;
pub(crate) use rue_error::{CompileError, CompileResult, Diagnostic, ErrorCode, ErrorKind};
pub use rue_error::{
    CompileErrors, CompileWarning, MultiErrorResult, PreviewFeature, PreviewFeatures, VERSION,
};
#[cfg(test)]
pub(crate) use rue_error::{Suggestion, WarningKind};
pub(crate) use rue_lexer::Lexer;
pub use rue_span::FileId;
pub(crate) use rue_span::Span;
pub use rue_target::{Arch, Target};

// Internal phase vocabulary. These are intentionally not part of the facade.
pub(crate) use import_graph::{validate_additive_successor, validate_canonical_import_graph};
#[cfg(test)]
pub(crate) use linking::{parse_runtime_archive, validate_runtime};
#[cfg(test)]
pub(crate) use rue_parser::Item;
pub(crate) use semantic_symbols::SemanticSymbolUniverse;
pub(crate) use syntax::SyntaxWork;

pub(crate) use lasso::ThreadedRodeo;
#[cfg(test)]
pub(crate) use rue_air::Type;
pub(crate) use rue_codegen::RelocationKind;
pub(crate) use rue_linker::{
    Archive, CodeRelocation, Linker, ObjectBuilder, ObjectFile, RelocationType,
};
pub(crate) use rue_parser::Parser;

// Zero means no embedder/driver override has been installed yet. The first
// session lazily snapshots host parallelism so constructing CompilerSession
// directly retains the CLI's default behavior without a peer executor.
static QUERY_CONCURRENCY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Configure the compiler's shared structured-query concurrency budget.
///
/// Registered batch schedulers consume this one shared budget, keeping
/// structured-query concurrency under a single authority.
pub fn configure_thread_pool(jobs: usize) -> usize {
    let jobs = if jobs == 0 {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1)
    } else {
        jobs
    };
    QUERY_CONCURRENCY.store(jobs, std::sync::atomic::Ordering::Release);
    jobs
}

pub(crate) fn query_concurrency() -> usize {
    let configured = QUERY_CONCURRENCY.load(std::sync::atomic::Ordering::Acquire);
    if configured != 0 {
        return configured;
    }
    let detected = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    let _ = QUERY_CONCURRENCY.compare_exchange(
        0,
        detected,
        std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Acquire,
    );
    QUERY_CONCURRENCY.load(std::sync::atomic::Ordering::Acquire)
}
