#![recursion_limit = "256"]

//! Rue's embeddable compiler facade.
//!
//! Compilation starts with an owned [`SourceSnapshot`]. A [`CompilerSession`]
//! publishes that snapshot and exposes the syntax, program, RIR, semantic, and
//! dependency queries that all compiler consumers share. Query artifacts are
//! immutable and may be retained across later session updates.
//!
//! ```
//! use std::{collections::HashMap, sync::Arc};
//! use rue_compiler::{
//!     CompileOptions, CompilerSession, FileId, SourceMetadata, SourceSnapshot,
//! };
//!
//! let root = FileId::new(7);
//! let paths = HashMap::from([(root, "src/main.rue".to_owned())]);
//! let metadata = SourceMetadata::new(root, paths.clone(), paths)?;
//! let snapshot = SourceSnapshot::new(
//!     metadata,
//!     vec![(root, Arc::new("fn main() -> i32 { 0 }".to_owned()))],
//! )?;
//! let mut session = CompilerSession::new();
//! session.update(&snapshot).into_result()?;
//! let executable = session.executable(&CompileOptions::default())?;
//! assert!(!executable.elf.is_empty());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Callers that only need a final executable may use [`compile_snapshot`], the
//! sole one-shot adapter. Filesystem discovery remains the caller's job.
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
mod durable_body;
mod durable_cfg;
mod durable_semantics;
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
mod integration_tests;
#[cfg(test)]
mod scaling_harness;
#[cfg(test)]
mod supported_api_inventory;

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
    AcceptedImportSource, AcceptedReadManifest, AcceptedReadManifestEntry, FileMetadataFingerprint,
    ImportCandidateRole, ImportDiscoveryContext, ImportDiscoveryPlan, ImportDiscoveryRequest,
    ImportObservation, ImportObservationLedger, ImportObservationStatus, ImportOccurrenceKey,
    PhysicalFileIdentity,
};
pub(crate) use import_discovery::{
    ImportDemandFrontier, ImportDemandMode, ImportDemandRoots, ImportInputRevision,
};
pub use import_graph::{
    CanonicalImportCycle, CanonicalImportGraph, CanonicalImportGraphProblem,
    CanonicalImportGraphValidation, CanonicalImportRecord, CanonicalImportResolution,
    ImportDirective, ImportDirectives,
};
pub(crate) use import_graph::{ResolvedCodegenRevision, ResolvedProgramRevision};
pub(crate) use parsed_modules::{InvalidImportShape, ParsedProgram};
pub use queries::{CompileOptions, CompileOutput, LinkerMode, SourceView, compile_snapshot};
#[allow(unused_imports)]
pub(crate) use semantic_identity::{
    AnonymousMemberKey, AnonymousMemberKind, AnonymousNominalKey, AnonymousNominalKind,
    CanonicalArgumentValue, CanonicalArguments, CompilerCallableId, FunctionInstanceKey,
    LocalAtomId, LocalAtomKind, NominalInstanceKey, StableCallableId, StableProducerId,
    StableSymbolEncoder, StableSymbolId, StructuralAnchor, StructuralPathSegment, TypeInstanceKey,
};
pub use session::{CanonicalImportGraphOutput, CompilerSession, CompilerSessionUpdate};
pub use source_identity::{ModuleId, ModuleRevision, SourceId, SourceIdVersion, SourceRevision};
pub use source_metadata::SourceMetadata;
pub use source_snapshot::{MAX_SOURCE_BYTES, MAX_SOURCE_FILES, SourceSnapshot};
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
pub(crate) use canonical_semantic::CanonicalSemanticWork;
#[allow(unused_imports)]
pub(crate) use definition_snapshot::DefinitionShardWork;
#[allow(unused_imports)]
pub(crate) use durable_body::DurableBodyWork;
#[cfg(test)]
pub(crate) use import_discovery::DiscoverySourceAssembler;
pub(crate) use import_discovery::IMPORT_DISCOVERY_POLICY_VERSION;
#[allow(unused_imports)]
pub(crate) use parsed_modules::{ParseInvalidationSummary, ParsedModulesWork};
pub(crate) use queries::{PipelineWork, SourceStats};
#[allow(unused_imports)]
pub(crate) use semantic_symbols::SemanticTranslationWork;
#[cfg(test)]
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
#[cfg(test)]
pub(crate) use body_query::{BodyTransaction, transaction_equal};
#[cfg(test)]
pub(crate) use bound_definitions::BoundDefinitionSet;
pub(crate) use bound_definitions::{
    StableDefinitionKey, StableDefinitionKind, StableDefinitionNamespace,
};
pub(crate) use canonical_lower::CanonicalRirOutput;
pub(crate) use canonical_merge::CanonicalMergedProgram;
pub(crate) use definition_snapshot::{DefinitionKind, DefinitionNamespace, DefinitionSnapshot};
#[cfg(test)]
pub(crate) use definition_snapshot::{DefinitionNameKey, DefinitionOccurrenceId, DefinitionRecord};
pub(crate) use durable_body::{DurableAirInstData, DurableProjection};
#[cfg(test)]
pub(crate) use durable_semantics::DurableDeclarationPayload;
pub(crate) use durable_semantics::DurableDeclarationSemantic;
pub(crate) use durable_semantics::{DurableConstValue, DurableType};
#[cfg(test)]
pub(crate) use durable_semantics::{
    DurableSemanticProjectionFailure, DurableSemanticProjectionWork,
    project_durable_declaration_semantics,
};

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
/// Per-function CFG and backend work is deliberately serialized until it is
/// represented as registered query batches, leaving one canonical concurrency
/// authority for compiler work.
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
