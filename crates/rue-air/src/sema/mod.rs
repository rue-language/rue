#![allow(private_bounds)]

//! Semantic analysis - RIR to AIR conversion.
//!
//! Semantic analysis performs type checking and converts untyped RIR to typed
//! AIR. Body analysis is provider-hosted: the compiler's query graph supplies
//! durable declaration facts through [`provider::BodyFactProvider`], the
//! body-identity pool ([`body_identity`]) materializes request-local types and
//! identities from those facts, and [`provider_body_host`] drives the shared
//! [`ordinary_engine::OrdinaryBodyEngine`] over exactly one demanded body.
//!
//! # Module Organization
//!
//! - [`provider`] - the durable fact-provider boundary the compiler implements
//! - [`provider_body_host`] - the production body-analysis host and entry
//!   points ([`analyze_provider_ordinary_body`] and friends)
//! - [`body_identity`] - the provider-side identity pool and RIR views
//! - [`ordinary_engine`] - the body analysis engine, generic over its host
//! - [`context`] - per-body analysis context and helper types
//! - [`analysis`] - RIR-to-AIR lowering shared by every host
//! - [`aggregates`] - aggregate construction, fields, indexing, enums
//! - [`control_flow`] - branch, loop, match, try, return, and block analysis
//! - [`comptime_eval`] - compile-time evaluation engine
//! - [`inference_ctx`] - pre-computed type information for inference
//! - [`output`] - analyzed-body output types
//! - [`binding_manifest`] - durable declaration-transport vocabulary

mod aggregate_resolution;
mod aggregates;
pub(crate) mod analysis;
mod analyze_ops;
mod anon_structs;
mod binding_manifest;
mod body_endpoint;
mod body_identity;
mod call_resolution;
mod comptime;
mod comptime_eval;
mod context;
mod control_flow;
mod declaration_index;
mod declarations;
mod fact_mode;
mod inference_ctx;
mod info;
mod known_symbols;
mod ordinary_engine;
mod output;
mod ownership_state;
pub mod provider;
mod provider_body_host;
mod provider_module_registry;
mod semantic_body_export;
mod typeck;
mod visibility;

// Public re-exports
pub use binding_manifest::{
    DeclarationBindingWork, SemanticAnonymousNominalIdentity, SemanticBindingManifestWork,
    SemanticDeclarationShell, SemanticDeclarationShellIdentity, SemanticDefinitionIdentity,
    SemanticExportType, SemanticNominalIdentity, SemanticParameterMode,
};
// RUE-1831: the comptime module tree as one string, for the structural
// guards in `api_inventory` and `consistency_tests`. `comptime` itself
// stays private; only the guard source crosses this boundary.
pub use comptime::ComptimeMethodReceiverPolicy;
#[cfg(test)]
pub(crate) use comptime::{COMPTIME_PRODUCTION_SOURCE, COMPTIME_SOURCE};
pub use comptime::{
    ComptimeAnonymousKind, ComptimeArgMode, ComptimeArrayLengthBinding, ComptimeCallAdmission,
    ComptimeCallArgument, ComptimeCallKey, ComptimeCallMemoLookup, ComptimeCallPreparation,
    ComptimeCallProtocol, ComptimeCompletedCallMemo, ComptimeDomain, ComptimeEngine, ComptimeEnv,
    ComptimeExpressionIntrinsic, ComptimeExpressionIntrinsicRequest, ComptimeField, ComptimeFile,
    ComptimeFrame, ComptimeHost, ComptimeHostError, ComptimeHostResult, ComptimeIdentity,
    ComptimeIntegerBound, ComptimeIntegerOperation, ComptimeInterrupts, ComptimeMemoInsertError,
    ComptimeMemoizedOutcome, ComptimeMethodDescriptor, ComptimeMethodParameter, ComptimeMethodType,
    ComptimeName, ComptimeNamedValueResolution, ComptimeOutcome, ComptimeProgram,
    ComptimeProgramFacts, ComptimeProgramKey, ComptimeProgramRegistrationError,
    ComptimeProgramRegistry, ComptimeRejections, ComptimeSelection, ComptimeSite, ComptimeSiteKind,
    ComptimeStructuredTypeResolution, ComptimeStructuredTypeSuspension, ComptimeStructuredTypes,
    ComptimeTargetIntrinsic, ComptimeTrap, ComptimeType, ComptimeTypeAlgebra,
    ComptimeTypeIntrinsic, ComptimeValue, ComptimeValueAlgebra, MAX_COMPTIME_CALL_DEPTH,
    comptime_depth_over_limit, next_comptime_depth,
};
pub use comptime::{
    ComptimeDiagnosticSite, ComptimeMatchPattern, ComptimeSemanticRejection,
    ComptimeUnaryOperation, decode_comptime_match_pattern,
};
pub use context::ConstValue;
pub use declaration_index::RirDeclarationIndexWork;
pub(crate) use fact_mode::StructuredTypeSyntax;
pub(crate) use inference_ctx::HostInferenceFacts;
pub use inference_ctx::InferenceContext;
pub(crate) use info::FunctionCallInfo;
pub use info::{AnonMethodSig, AnonMethodType, ConstInfo, FunctionInfo, MethodInfo};
pub use known_symbols::KnownSymbols;
pub(crate) use ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
pub use output::{
    AnalyzedBodyOwnerEvent, AnalyzedCallableKind, AnalyzedFunction, BodyAnalysisWork,
    BodyNamedDependencyEvent, BodyOwnerEndpoint, BodyOwnerKind, BodyOwnerToken,
    BuiltinTypeCallHead, DeclarationBuiltinTypeCallHeadDependencyEvent,
    DeclarationTypeCallHeadDependencyEvent, DeclarationTypeDependencyEvent,
    DeclarationTypeDependencyKind, DeclarationTypeDependencySourceKind,
    DeclarationTypeDependencyTargetKind, ImplicitDropDependencySourceEvent,
    ImplicitNamedDestructorDependencyEvent, NamedConstDependencyEvent,
    NamedConstDependencyTargetEvent, ParamSlotModes, SourceParamAbi,
};
pub use provider::{
    BodyFactProvider, DropCopyMetadata, ImportResolution, MemberCandidate, MemberKind,
    NameCandidate, NameResolution, NominalWellFormedness, OperatorMemberCandidate, OperatorName,
    ProviderDefinitionKind, ProviderNamespace,
};
pub(crate) use semantic_body_export::SemanticBodyExportHost;
// RUE-1091 slice r4b-1 provider surface: the durable source vocabulary the
// rue-compiler provider adapter supplies to the body identity pool, and the
// call-resolution ProviderFacts driver that composes pool + provider answers.
pub use aggregate_resolution::{
    ProviderAggregateFacts, ProviderModuleMember, ProviderQualifiedType, ProviderStructHead,
};
pub use body_endpoint::ProviderEndpointFacts;
pub use body_identity::{
    BodyRirBundle, BodyRirIndexAttribution, BodyRirView, DurableAnonymousMethod,
    DurableAnonymousMethodType, DurableAnonymousShape, DurableAnonymousSource,
    DurableCallableSource, DurableCallableTypeSyntax, DurableConst, DurableConstSource,
    DurableFunction, DurableMethod, DurableNominal, DurableNominalBody, DurableNominalSource,
    DurableSignatureParameter, ProviderBodyAnalysisState, ProviderIdentityContext,
};
pub use call_resolution::ProviderCallFacts;
pub use provider_body_host::{
    DurableBodyLookupSource, DurableBodyModuleBinding, DurableBodySourceLocator,
    DurableComptimeCallOutcome, DurableComptimeDiagnostic, DurableReducedComptimeCall,
    DurableTryProducer, ProviderAnonymousBody, ProviderBodyWork, ProviderOrdinaryBody,
    ProviderSpecializedBody, ProviderWellKnownOptionFacts,
    SemanticProducedAnonymousMethodSignature, SemanticProducedAnonymousMethodType,
    SemanticProducedAnonymousNominal, SemanticProducedAnonymousNominalShape,
    analyze_provider_anonymous_body, analyze_provider_ordinary_body,
    analyze_provider_specialized_body,
};

use rue_span::Span;

use crate::types::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredOwnershipGateKind {
    RequireDroppable,
    RequireTriviallyDroppable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeferredOwnershipGate {
    kind: DeferredOwnershipGateKind,
    ty: Type,
    span: Span,
}

#[cfg(test)]
mod consistency_tests;
#[cfg(test)]
mod provider_accessor_tests;
#[cfg(test)]
pub(crate) mod provider_fixture;
#[cfg(test)]
mod provider_fixture_tests;
#[cfg(test)]
mod provider_semantics_tests;
#[cfg(test)]
mod provider_strings_ownership_tests;
#[cfg(test)]
mod tests;
