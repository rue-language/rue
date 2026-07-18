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
//! let semantic = session.semantic(&CompileOptions::default())?;
//! assert_eq!(semantic.functions().len(), 1);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Callers that only need a final executable may use [`compile_snapshot`], the
//! sole one-shot adapter. Filesystem discovery remains the caller's job.

mod backend;
mod bound_definitions;
mod canonical_lower;
mod canonical_merge;
mod canonical_semantic;
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
mod parsed_modules;
mod queries;
mod query_graph;
mod semantic_symbols;
mod session;
mod source_identity;
mod source_metadata;
mod source_snapshot;
mod syntax;
mod typed_query_store;
pub mod unstable;

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
mod pipeline_tests;

#[cfg(test)]
mod api_inventory;
#[cfg(test)]
mod durable_compatibility_tests;
#[cfg(test)]
mod integration_tests;

// Supported source, identity, option, session, and diagnostic surface.
pub use dependency_envelope::{
    DependencyAcceptedRead, DependencyContext, DependencyEnvelope, DependencyEnvelopeStatus,
    DependencyObservation, DependencyObservationOutcome, DependencyRequest,
    DependencyResolutionOutcome, DependencyTopology, DependencyTopologyRecord,
};
pub use diagnostic::{
    ColorChoice, DiagnosticFormatter, JsonDiagnostic, JsonDiagnosticFormatter, JsonSpan,
    JsonSuggestion, MultiFileFormatter, MultiFileJsonFormatter, SourceInfo,
};
#[cfg(test)]
pub(crate) use diagnostic_attempt_store::FrontendDiagnosticIdentity;
pub use diagnostic_attempt_store::{DiagnosticStage, FrontendDiagnosticSnapshot};
pub use import_discovery::{
    AcceptedImportSource, AcceptedReadManifestEntry, DiscoverySourceAssembler,
    FileMetadataFingerprint, IMPORT_DISCOVERY_POLICY_VERSION, ImportCandidateRole,
    ImportDiscoveryContext, ImportDiscoveryPlan, ImportDiscoveryRequest, ImportObservation,
    ImportObservationLedger, ImportObservationStatus, ImportOccurrenceKey, PhysicalFileIdentity,
};
pub use import_graph::{
    CanonicalImportCycle, CanonicalImportGraph, CanonicalImportGraphProblem,
    CanonicalImportGraphValidation, CanonicalImportRecord, CanonicalImportResolution,
    ImportDirective, ImportDirectives,
};
pub(crate) use import_graph::{ResolvedCodegenRevision, ResolvedProgramRevision};
pub use parsed_modules::{
    InvalidImportShape, ParsedAstPresentation, ParsedInvalidImport, ParsedProgram,
    parse_source_snapshot_for_ast_presentation,
};
pub use queries::{
    CompileOptions, CompileOutput, FunctionWithCfg, LinkerMode, SourceView, compile_snapshot,
};
pub use session::{CanonicalImportGraphOutput, CompilerSession, CompilerSessionUpdate};
pub use source_identity::{ModuleId, ModuleRevision, SourceId, SourceIdVersion, SourceRevision};
pub use source_metadata::SourceMetadata;
pub use source_snapshot::{MAX_SOURCE_BYTES, SourceSnapshot};

// Query keys, invalidation records, dependency manifests, fingerprints, and
// work records are compiler implementation. Keep crate-local paths available
// while the session remains their sole owner, without publishing them through
// the supported facade.
#[allow(unused_imports)]
pub(crate) use bound_definitions::BoundDefinitionWork;
#[allow(unused_imports)]
pub(crate) use canonical_lower::CanonicalRirWork;
#[allow(unused_imports)]
pub(crate) use canonical_merge::CanonicalMergeWork;
#[allow(unused_imports)]
pub(crate) use canonical_semantic::{
    CanonicalSemanticFailurePhase, CanonicalSemanticFailureWork, CanonicalSemanticWork,
};
#[allow(unused_imports)]
pub(crate) use definition_snapshot::DefinitionShardWork;
#[allow(unused_imports)]
pub(crate) use durable_body::DurableBodyWork;
#[allow(unused_imports)]
pub(crate) use parsed_modules::{
    ParseInvalidationSummary, ParsedAstPresentationWork, ParsedModulesWork,
};
pub(crate) use queries::{PipelineWork, SourceStats};
#[allow(unused_imports)]
pub(crate) use semantic_symbols::SemanticTranslationWork;
#[allow(unused_imports)]
pub(crate) use session::{
    CompilerSessionWork, DefinitionQueryRecord, FRONTEND_DIAGNOSTIC_RETENTION_LIMIT,
    FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT, FrontendQueryWork, FrontendRetentionMetrics,
    ImportDiscoveryRevisionArtifact, ImportDiscoveryRevisionStatus, ImportGraphInputDescriptor,
    SemanticDependencyBlocker, SemanticDependencyIncompleteReason, SemanticDependencyInputManifest,
    SemanticDependencyManifestWork, SemanticDependencySurface, SemanticFullInvalidationReason,
    SemanticInvalidationPlan, SemanticInvalidationScope, SemanticInvalidationWork,
    SemanticQueryRecord, StableBodyDependencyInputRecord, StableBuiltinTypeCallHeadInput,
    StableDeclarationTypeCallHeadDependency, StableDeclarationTypeDependency,
    StableDefinitionFingerprint, StableDefinitionFingerprintPrecision,
    StableDefinitionInputFingerprint, StableFreeFunctionDependency, StableModuleImportDependency,
    StableNamedConstDependency, StableNamedConstDependencyTarget, StableNamedDestructorDependency,
    StableNamedMethodDependency, StableNamedMethodDependencyTarget,
};
#[cfg(test)]
pub(crate) use source_identity::LinkInputDescriptor;
#[allow(unused_imports)]
pub(crate) use source_identity::{
    CodegenInputDescriptor, ModuleResolutionInput, ModuleResolutionInputs, SemanticInputDescriptor,
    SourceStore, StableOptLevel, StablePreviewFeatures,
};

// Immutable query artifacts and stable identities returned by CompilerSession.
pub use bound_definitions::{
    BoundDefinitionId, BoundDefinitionRecord, BoundDefinitionSet, SnapshotBoundDefinitionId,
    StableDefinitionKey, StableDefinitionKind, StableDefinitionNamespace, StableNamedTypeKey,
};
pub use canonical_lower::CanonicalRirOutput;
pub use canonical_merge::{CanonicalMergedAst, CanonicalMergedProgram};
pub use canonical_semantic::CanonicalSemanticOutput;
pub use definition_snapshot::{
    DefinitionId, DefinitionKind, DefinitionNameKey, DefinitionNamespace, DefinitionOccurrenceId,
    DefinitionRecord, DefinitionShard, DefinitionSnapshot, ModuleDefinition,
};
#[cfg(test)]
pub(crate) use durable_body::DurableBodyProjectionFailure;
pub(crate) use durable_body::{
    DURABLE_ORDINARY_BODY_SCHEMA_VERSION, DURABLE_SPECIALIZED_BODY_SCHEMA_VERSION,
    DurableAirInstData, DurableBodyAnchor, DurableOrdinaryBody, DurableOrdinaryBodyPayload,
    DurableProjection, DurableSpecializedBody, DurableSpecializedBodyPayload,
    convert_semantic_specialized_body_exports,
};
pub(crate) use durable_semantics::{
    DURABLE_SEMANTIC_SCHEMA_VERSION, DurableDeclarationSemantic, DurableSemanticExportFailure,
    DurableSemanticProjectionFailure, DurableSemanticSchemaVersion,
};
#[cfg(test)]
pub(crate) use durable_semantics::{
    DurableConstValue, DurableDeclarationPayload, DurableParameterMode, DurableSemanticParameter,
    DurableSemanticProjectionWork, DurableType,
};

// Backend presentation queries used by the command-line emit modes.
pub use backend::{
    Mir, generate_emitted_asm, generate_liveness_info, generate_lowering_info, generate_mir,
    generate_regalloc_info,
};

// Small foundational types callers need to configure or inspect the facade.
pub use rue_air::{FrozenTypeInternPool, TypeInternPool};
pub use rue_cfg::OptLevel;
pub use rue_codegen::{
    LoweringDebugInfo, RegAllocDebugInfo, StackFrameInfo, generate_stack_frame_info,
};
pub use rue_error::{
    Applicability, CompileError, CompileErrors, CompileResult, CompileWarning, Diagnostic,
    ErrorCode, ErrorKind, MultiErrorResult, PreviewFeature, PreviewFeatures, Suggestion, VERSION,
    WarningKind,
};
pub use rue_lexer::{Lexer, Token, TokenKind};
pub use rue_parser::Ast;
pub use rue_span::{FileId, Span};
pub use rue_target::{Arch, Target};

// Internal phase vocabulary. These are intentionally not part of the facade.
#[cfg(test)]
pub(crate) use canonical_lower::lower_canonical_rir;
#[cfg(test)]
pub(crate) use canonical_merge::merge_parsed_modules;
pub(crate) use durable_body::{convert_semantic_body_exports, finalize_durable_ordinary_bodies};
pub(crate) use durable_semantics::{
    import_durable_declaration_semantics, project_durable_declaration_semantics,
};
pub(crate) use import_graph::validate_canonical_import_graph;
#[cfg(test)]
pub(crate) use linking::{parse_runtime_archive, validate_runtime};
pub(crate) use parsed_modules::CanonicalParseUpdate;
pub(crate) use queries::build_functions_and_cfgs;
#[cfg(test)]
pub(crate) use rue_parser::Item;
pub use semantic_symbols::{SemanticSymbol, SemanticSymbolUniverse};
pub(crate) use syntax::SyntaxWork;

pub use lasso::ThreadedRodeo;
pub(crate) use rue_air::{AnalyzedFunction, SemaOutput, Type};
pub(crate) use rue_cfg::{CfgBuilder, ValidatedCfg as Cfg};
pub(crate) use rue_codegen::{RelocationKind, X86Mir, aarch64::Aarch64Mir};
pub(crate) use rue_linker::{
    Archive, CodeRelocation, Linker, ObjectBuilder, ObjectFile, RelocationType,
};
pub(crate) use rue_parser::Parser;
pub use rue_rir::Rir;

/// Configure Rayon's global worker pool once, before issuing compiler queries.
pub fn configure_thread_pool(jobs: usize) {
    if jobs > 0 {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build_global();
    }
}
