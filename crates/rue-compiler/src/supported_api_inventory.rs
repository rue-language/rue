//! Reviewed semantic inventory for Rue's compiler facade.
//!
//! One line represents one root export or public session signature:
//! `stability|owner|class|approved-consumer|symbol|canonical-signature`.
//! Intentional API changes update the relevant line instead of weakening a
//! source-size budget or adding another forbidden-name exception.

pub(crate) const APPROVED: &str = r#"stable|CompilerSession|artifact-query|embedders|committed_import_graph|pub fn committed_import_graph(&self)->Result<Arc<CanonicalImportGraphOutput>,CompileErrors>
stable|CompilerSession|artifact-query|embedders|import_diagnostics|pub fn import_diagnostics(&mut self)->Result<Arc<FrontendDiagnosticSnapshot>,CompileErrors>
stable|CompilerSession|artifact-query|embedders|published|pub fn published(&self)->Option<crate::SyntaxView>
stable|CompilerSession|artifact-query|embedders|rir|pub fn rir(&mut self)->Result<Arc<crate::RirView>,CompileErrors>
stable|CompilerSession|session-operation|embedders|new|pub fn new()->Self
stable|CompilerSession|session-operation|embedders|update|pub fn update(&mut self,snapshot:&SourceSnapshot)->CompilerSessionUpdate
stable|CompilerSessionUpdate|artifact-result|embedders|diagnostics|pub fn diagnostics(&self)->&Arc<FrontendDiagnosticSnapshot>
stable|CompilerSessionUpdate|artifact-result|embedders|into_result|pub fn into_result(self)->Result<crate::SyntaxView,CompileErrors>
stable|CompilerSessionUpdate|artifact-result|embedders|result|pub fn result(&self)->Result<crate::SyntaxView,&CompileErrors>
stable|artifact_views|artifact-view|embedders+tooling|ImportDiscoveryStatus|pub use artifact_views::ImportDiscoveryStatus
stable|artifact_views|artifact-view|embedders+tooling|ImportDiscoveryView|pub use artifact_views::ImportDiscoveryView
stable|artifact_views|artifact-view|embedders+tooling|RirInstructionView|pub use artifact_views::RirInstructionView
stable|artifact_views|artifact-view|embedders+tooling|RirOperandView|pub use artifact_views::RirOperandView
stable|artifact_views|artifact-view|embedders+tooling|RirView|pub use artifact_views::RirView
stable|artifact_views|artifact-view|embedders+tooling|SourceIdentityView|pub use artifact_views::SourceIdentityView
stable|artifact_views|artifact-view|embedders+tooling|SourceLocationView|pub use artifact_views::SourceLocationView
stable|artifact_views|artifact-view|embedders+tooling|SyntaxModuleView|pub use artifact_views::SyntaxModuleView
stable|artifact_views|artifact-view|embedders+tooling|SyntaxNodeView|pub use artifact_views::SyntaxNodeView
stable|artifact_views|artifact-view|embedders+tooling|SyntaxView|pub use artifact_views::SyntaxView
stable|artifact_views|artifact-view|embedders+tooling|TokenView|pub use artifact_views::TokenView
stable|dependency_envelope|dependency-artifact|source-loaders+embedders|DependencyEnvelope|pub use dependency_envelope::DependencyEnvelope
stable|dependency_envelope|dependency-artifact|source-loaders+embedders|DependencyEnvelopeStatus|pub use dependency_envelope::DependencyEnvelopeStatus
stable|dependency_envelope|dependency-artifact|source-loaders+embedders|DependencyResolutionOutcome|pub use dependency_envelope::DependencyResolutionOutcome
stable|dependency_envelope|dependency-artifact|source-loaders+embedders|DependencyTopology|pub use dependency_envelope::DependencyTopology
stable|dependency_envelope|dependency-artifact|source-loaders+embedders|DependencyTopologyRecord|pub use dependency_envelope::DependencyTopologyRecord
stable|diagnostic_attempt_store|diagnostic|cli+embedders|DiagnosticStage|pub use diagnostic_attempt_store::DiagnosticStage
stable|diagnostic_attempt_store|diagnostic|cli+embedders|FrontendDiagnosticSnapshot|pub use diagnostic_attempt_store::FrontendDiagnosticSnapshot
stable|import_discovery|dependency-artifact|source-loaders+embedders|AcceptedReadManifest|pub use import_discovery::AcceptedReadManifest
stable|import_discovery|dependency-artifact|source-loaders+embedders|AcceptedReadManifestEntry|pub use import_discovery::AcceptedReadManifestEntry
stable|import_discovery|dependency-artifact|source-loaders+embedders|FileMetadataFingerprint|pub use import_discovery::FileMetadataFingerprint
stable|import_discovery|dependency-artifact|source-loaders+embedders|ImportCandidateRole|pub use import_discovery::ImportCandidateRole
stable|import_discovery|dependency-artifact|source-loaders+embedders|ImportDiscoveryContext|pub use import_discovery::ImportDiscoveryContext
stable|import_discovery|dependency-artifact|source-loaders+embedders|ImportOccurrenceKey|pub use import_discovery::ImportOccurrenceKey
stable|import_discovery|dependency-artifact|source-loaders+embedders|PhysicalFileIdentity|pub use import_discovery::PhysicalFileIdentity
stable|import_graph|dependency-artifact|source-loaders+embedders|CanonicalImportCycle|pub use import_graph::CanonicalImportCycle
stable|import_graph|dependency-artifact|source-loaders+embedders|CanonicalImportGraph|pub use import_graph::CanonicalImportGraph
stable|import_graph|dependency-artifact|source-loaders+embedders|CanonicalImportGraphProblem|pub use import_graph::CanonicalImportGraphProblem
stable|import_graph|dependency-artifact|source-loaders+embedders|CanonicalImportGraphValidation|pub use import_graph::CanonicalImportGraphValidation
stable|import_graph|dependency-artifact|source-loaders+embedders|CanonicalImportRecord|pub use import_graph::CanonicalImportRecord
stable|import_graph|dependency-artifact|source-loaders+embedders|CanonicalImportResolution|pub use import_graph::CanonicalImportResolution
stable|import_graph|dependency-artifact|source-loaders+embedders|ImportDirective|pub use import_graph::ImportDirective
stable|import_graph|dependency-artifact|source-loaders+embedders|ImportDirectives|pub use import_graph::ImportDirectives
stable|lib|runtime-config|cli+embedders|configure_thread_pool|pub fn configure_thread_pool(jobs:usize)->usize
stable|queries|compilation-config|cli+embedders|CompileOptions|pub use queries::CompileOptions
stable|queries|compilation-config|cli+embedders|LinkerMode|pub use queries::LinkerMode
stable|queries|compile-artifact|cli+embedders|CompileOutput|pub use queries::CompileOutput
stable|queries|compile-artifact|cli+embedders|SourceView|pub use queries::SourceView
stable|queries|one-shot-operation|cli+embedders|compile_snapshot|pub use queries::compile_snapshot
stable|rue_cfg|compilation-config|cli+embedders|OptLevel|pub use rue_cfg::OptLevel
stable|rue_error|diagnostic|cli+embedders|CompileErrors|pub use rue_error::CompileErrors
stable|rue_error|diagnostic|cli+embedders|CompileWarning|pub use rue_error::CompileWarning
stable|rue_error|diagnostic|cli+embedders|MultiErrorResult|pub use rue_error::MultiErrorResult
stable|rue_error|diagnostic|cli+embedders|PreviewFeature|pub use rue_error::PreviewFeature
stable|rue_error|diagnostic|cli+embedders|PreviewFeatures|pub use rue_error::PreviewFeatures
stable|rue_error|diagnostic|cli+embedders|VERSION|pub use rue_error::VERSION
stable|rue_span|source-input|cli+embedders|FileId|pub use rue_span::FileId
stable|rue_target|compilation-config|cli+embedders|Arch|pub use rue_target::Arch
stable|rue_target|compilation-config|cli+embedders|Target|pub use rue_target::Target
stable|session|dependency-artifact|source-loaders+embedders|CanonicalImportGraphOutput|pub use session::CanonicalImportGraphOutput
stable|session|session-owner|embedders|CompilerSession|pub use session::CompilerSession
stable|session|session-owner|embedders|CompilerSessionUpdate|pub use session::CompilerSessionUpdate
stable|source_identity|source-input|cli+embedders|ModuleId|pub use source_identity::ModuleId
stable|source_identity|source-input|cli+embedders|ModuleRevision|pub use source_identity::ModuleRevision
stable|source_identity|source-input|cli+embedders|SourceId|pub use source_identity::SourceId
stable|source_identity|source-input|cli+embedders|SourceIdVersion|pub use source_identity::SourceIdVersion
stable|source_identity|source-input|cli+embedders|SourceRevision|pub use source_identity::SourceRevision
stable|source_metadata|source-input|cli+embedders|SourceMetadata|pub use source_metadata::SourceMetadata
stable|source_snapshot|source-input|cli+embedders|MAX_SOURCE_BYTES|pub use source_snapshot::MAX_SOURCE_BYTES
stable|source_snapshot|source-input|cli+embedders|MAX_SOURCE_FILES|pub use source_snapshot::MAX_SOURCE_FILES
stable|source_snapshot|source-input|cli+embedders|SourceSnapshot|pub use source_snapshot::SourceSnapshot
stable|toolchain_module_demand|toolchain-module-demand|source-loaders+embedders|OPTION_MODULE_LOGICAL_PATH|pub use toolchain_module_demand::OPTION_MODULE_LOGICAL_PATH
stable|toolchain_module_demand|toolchain-module-demand|source-loaders+embedders|ParkedToolchainModules|pub use toolchain_module_demand::ParkedToolchainModules
stable|toolchain_module_demand|toolchain-module-demand|source-loaders+embedders|STRBUF_MODULE_LOGICAL_PATH|pub use toolchain_module_demand::STRBUF_MODULE_LOGICAL_PATH
stable|toolchain_module_demand|toolchain-module-demand|source-loaders+embedders|TrustedToolchainModuleDemand|pub use toolchain_module_demand::TrustedToolchainModuleDemand
unstable|CompilerSession|debug-tooling|in-tree-tooling|unstable_metrics|pub fn unstable_metrics(&self)->crate::unstable::MetricsSnapshot
unstable|CompilerSession|debug-tooling|in-tree-tooling|unstable_present|pub fn unstable_present(&mut self,request:PresentationRequest<'_>,)->Result<PresentationOutput,crate::CompileErrors>
unstable|CompilerSessionUpdate|debug-tooling|in-tree-tooling|unstable_metrics|pub fn unstable_metrics(&self)->crate::unstable::ParseMetrics
unstable|unstable|debug-module|in-tree-tooling|unstable|pub mod unstable"#;
