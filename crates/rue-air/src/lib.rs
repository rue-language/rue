//! Analyzed Intermediate Representation (AIR) - Typed IR.
//!
//! AIR is the second IR in the Rue compiler pipeline. It is generated from
//! RIR after semantic analysis and type checking.
//!
//! Key characteristics:
//! - Fully typed: all types are resolved
//! - Per-function: generated lazily for each function
//! - Ready for codegen: can be lowered directly to machine code
//!
//! Inspired by Zig's AIR (Analyzed Intermediate Representation).

#[cfg(test)]
mod api_inventory;
mod builtin_universe;
pub mod call_abi;
pub mod declaration_validation;
pub mod drop_glue_names;
mod exact_decimal;
pub mod ffi_predicates;
mod inference;
mod inst;
pub mod integer_semantics;
mod intern_pool;
mod intrinsic;
pub mod layout;
mod module_registry;
mod param_arena;
mod path_norm;
mod runtime_call;
mod scope;
mod sema;
mod semantic_body;
mod semantic_identity;
mod semantic_import;
mod semantic_type_resolution;
pub mod specialize;
pub mod stable_digest;
mod type_encoding;
#[cfg(test)]
mod type_properties;
mod types;

pub use call_abi::{
    AggregateArgClass, AggregateReturnClass, ArgClass, ArgConvention, CAbiScalarKind, CallAbi,
    NativeAbiTypeFacts, NativeCallAbi, ReturnClass, ScalarAbiExtension, TargetCAbiFlavor,
    TargetCCallAbi, is_slot_identical_layout, native_return_register_budget,
};
pub use exact_decimal::canonical_decimal_literal;
pub use exact_decimal::finite_float_literal_bits;
pub(crate) use exact_decimal::finite_float_literal_bits_with_sign;
pub use ffi_predicates::{
    FfiPredicate, FfiPredicateFailure, FfiRejectReason, FfiTypePool, c_ffi_safe,
    c_passable_by_value, check_c_layout, has_c_layout, repr_c_marker_eligible,
};
pub use inference::{
    Constraint, ConstraintContext, ConstraintGenerator, ExprInfo, FunctionSig, InferType,
    LocalVarInfo, MethodSig, ParamVarInfo, Substitution, TypeVarAllocator, TypeVarId,
    UnificationError, Unifier, UnifyResult,
};
pub use inst::{
    AIR_PAYLOAD_FAMILY_NAMES, Air, AirArgMode, AirArrayElements, AirBlockStatements, AirBuildError,
    AirBuildErrorKind, AirCallArg, AirCallArgs, AirConstValueWords, AirDisplay, AirEditor,
    AirEnumPayload, AirInst, AirInstData, AirIntrinsicArgs, AirMatchArms, AirParamMode, AirPattern,
    AirPayloadError, AirPayloadStorageStats, AirPlace, AirPlaceBase, AirPlaceRef, AirProjection,
    AirRef, AirSourceOrder, AirStructFields, AirTypeArgs, AirValidationContext, AirValidationError,
    AirValidationErrorKind, MAX_AIR_INSTRUCTIONS_PER_BODY, ValidatedAir,
};
pub use integer_semantics::IntegerType;
pub use intern_pool::{
    EnumData, EnumDefEntry, FrozenTypeInternPool, MAX_COMPOSITE_TYPES, StructData, StructDefEntry,
    TypeData, TypeInternPool, TypeInternPoolStats, TypeValidationError,
    composite_type_limit_message,
};
pub use intrinsic::{
    IntrinsicAirArgument, IntrinsicAirArgumentSource, IntrinsicOperation, RuntimeAirTypePool,
    intrinsic_air_argument, intrinsic_air_argument_with_place_lookup, runtime_air_result_type,
    runtime_air_type,
};
pub use layout::{Layout, LayoutKind, PaddingRange, SLOT_BYTES};
pub use module_registry::ModuleRegistry;
pub use param_arena::{ParamArena, ParamRange, ParamRangeData};
pub use path_norm::{mangle_symbol_component, normalize_module_path};
pub use runtime_call::{
    OptionVariant, RuntimeAirArgument, RuntimeAirType, RuntimeCallActivation, RuntimeCallKind,
    RuntimeOperandOrigin,
};
pub use sema::ComptimeMethodReceiverPolicy;
pub use sema::{
    AnalyzedBodyOwnerEvent, AnalyzedCallableKind, AnalyzedFunction, BodyAnalysisWork,
    BodyFactProvider, BodyNamedDependencyEvent, BodyOwnerEndpoint, BodyOwnerKind, BodyOwnerToken,
    BodyRirBundle, BodyRirIndexAttribution, BodyRirView, BuiltinTypeCallHead,
    ComptimeAnonymousKind, ComptimeArgMode, ComptimeArrayLengthBinding, ComptimeCallAdmission,
    ComptimeCallArgument, ComptimeCallKey, ComptimeCallMemoLookup, ComptimeCallPreparation,
    ComptimeCallProtocol, ComptimeCompletedCallMemo, ComptimeDomain, ComptimeEngine, ComptimeEnv,
    ComptimeExpressionIntrinsic, ComptimeExpressionIntrinsicRequest, ComptimeField, ComptimeFile,
    ComptimeFrame, ComptimeHost, ComptimeHostError, ComptimeHostResult, ComptimeIdentity,
    ComptimeIntegerBound, ComptimeInterrupts, ComptimeMemoInsertError, ComptimeMemoizedOutcome,
    ComptimeMethodDescriptor, ComptimeMethodParameter, ComptimeMethodType, ComptimeName,
    ComptimeNamedValueResolution, ComptimeOutcome, ComptimeProgram, ComptimeProgramFacts,
    ComptimeProgramKey, ComptimeProgramRegistrationError, ComptimeProgramRegistry,
    ComptimeRejections, ComptimeSite, ComptimeSiteKind, ComptimeStructuredTypeResolution,
    ComptimeStructuredTypeSuspension, ComptimeStructuredTypes, ComptimeTargetIntrinsic,
    ComptimeTrap, ComptimeType, ComptimeTypeAlgebra, ComptimeTypeIntrinsic, ComptimeValue,
    ComptimeValueAlgebra, ConstInfo, ConstValue, DeclarationBindingWork,
    DeclarationBuiltinTypeCallHeadDependencyEvent, DeclarationTypeCallHeadDependencyEvent,
    DeclarationTypeDependencyEvent, DeclarationTypeDependencyKind,
    DeclarationTypeDependencySourceKind, DeclarationTypeDependencyTargetKind, DropCopyMetadata,
    DurableAnonymousMethod, DurableAnonymousMethodType, DurableAnonymousShape,
    DurableAnonymousSource, DurableBodyLookupSource, DurableBodyModuleBinding,
    DurableBodySourceLocator, DurableCallableSource, DurableCallableTypeSyntax,
    DurableComptimeCallOutcome, DurableComptimeDiagnostic, DurableConst, DurableConstSource,
    DurableFunction, DurableMethod, DurableNominal, DurableNominalBody, DurableNominalSource,
    DurableReducedComptimeCall, DurableSignatureParameter, DurableTryProducer, FunctionInfo,
    ImplicitDropDependencySourceEvent, ImplicitNamedDestructorDependencyEvent, ImportResolution,
    MAX_COMPTIME_CALL_DEPTH, MemberCandidate, MemberKind, MethodInfo, NameCandidate,
    NameResolution, NamedConstDependencyEvent, NamedConstDependencyTargetEvent, NativeArgClass,
    NominalWellFormedness, OperatorMemberCandidate, OperatorName, ParamSlotModes,
    ProviderAggregateFacts, ProviderAnonymousBody, ProviderBodyAnalysisState, ProviderBodyWork,
    ProviderCallFacts, ProviderDefinitionKind, ProviderEndpointFacts, ProviderIdentityContext,
    ProviderModuleMember, ProviderNamespace, ProviderOrdinaryBody, ProviderSpecializedBody,
    ProviderStructHead, ProviderWellKnownOptionFacts, RirDeclarationIndexWork,
    SemanticAnonymousNominalIdentity, SemanticBindingManifestWork, SemanticDeclarationShell,
    SemanticDeclarationShellIdentity, SemanticDefinitionIdentity, SemanticExportType,
    SemanticNominalIdentity, SemanticParameterMode, SemanticProducedAnonymousMethodSignature,
    SemanticProducedAnonymousMethodType, SemanticProducedAnonymousNominal,
    SemanticProducedAnonymousNominalShape, SourceParamAbi, analyze_provider_anonymous_body,
    analyze_provider_ordinary_body, analyze_provider_specialized_body, comptime_depth_over_limit,
    next_comptime_depth,
};
pub use sema::{
    ComptimeDiagnosticSite, ComptimeIntegerOperation, ComptimeMatchPattern,
    ComptimeSemanticRejection, ComptimeUnaryOperation, decode_comptime_match_pattern,
};
pub use semantic_body::{
    SEMANTIC_BODY_INST_KINDS, SemanticAnonymousBodyExport, SemanticBody, SemanticBodyAnchor,
    SemanticBodyCallArg, SemanticBodyCandidate, SemanticBodyCandidateInstallWork,
    SemanticBodyExport, SemanticBodyExportFailure, SemanticBodyImportFailure,
    SemanticBodyImportFailureKind, SemanticBodyInst, SemanticBodyInstData,
    SemanticBodyInstDependency, SemanticBodyInstFailureContext, SemanticBodyInstKind,
    SemanticBodyMatchArm, SemanticBodyMethodReference, SemanticBodyPattern, SemanticBodyPlace,
    SemanticBodyPlaceRef, SemanticBodyProjection, SemanticBodyRef, SemanticBodyWarning,
    SemanticBodyWarningLabel, SemanticBodyWarningSuggestion, SemanticDefinitionEndpoint,
    SemanticDefinitionToken, SemanticImportedBody, SemanticModuleEndpoint, SemanticModuleToken,
    SemanticQueriedBodyCandidate, SemanticSpecializationIdentity, SemanticSpecializedBodyCandidate,
    SemanticSpecializedBodyExport, SemanticSpecializedCandidateInstallWork,
    SemanticStableResolutionFailure,
};
pub use semantic_identity::{
    AnonymousMemberKey, AnonymousMemberKind, AnonymousNominalKey, AnonymousNominalKind,
    CanonicalArgumentValue, CanonicalArguments, CanonicalDisplayParameter, CompilerCallableId,
    FunctionInstanceKey, LocalAtomId, LocalAtomKind, LocalAtomRecord, Node, NominalInstanceKey,
    STABLE_DEFINITION_KINDS, STABLE_DEFINITION_NAMESPACES, SemanticBodyLocalAtom, StableCallableId,
    StableDefinitionKind, StableDefinitionNamespace, StableProducerId, StableSymbolId,
    TypeInstanceKey, format_canonical_application,
};
pub use semantic_import::{
    SEMANTIC_IMPORT_CONST_KINDS, SEMANTIC_IMPORT_TYPE_KINDS, SemanticImportConstKind,
    SemanticImportConstValue, SemanticImportEpoch, SemanticImportFailure, SemanticImportNominal,
    SemanticImportNominalKind, SemanticImportType, SemanticImportTypeFold, SemanticImportTypeKind,
    SemanticImportedConstValue, SemanticImportedType, SemanticLocalCallable,
    SemanticLocalCompleteness, SemanticLocalMaterialization, SemanticLocalNominal,
    SemanticLocalNominalShape,
};
pub use semantic_type_resolution::{
    ComptimeStructuredTypeAuthority, ComptimeStructuredTypeJob, ComptimeStructuredTypePoll,
    ComptimeStructuredTypeRequest, ComptimeStructuredTypeSymbolAuthority,
    RegisteredComptimeStructuredTypeAuthority, SemanticComptimeCallExpectation,
    SemanticComptimeCallResult, SemanticModuleBinding, SemanticModulePathFailure,
    SemanticModulePathProvider, SemanticProviderError, SemanticProviderResult,
    SemanticResolutionError, SemanticResolvedComptimeCall, SemanticResolvedModule,
    SemanticTypeConstructorHead, SemanticTypeConstructorParameter, SemanticTypeFact,
    SemanticTypeFactKind, SemanticTypeSyntaxError, SemanticTypeSyntaxFailure,
    SemanticTypeSyntaxProvider, SemanticValueSyntax, SemanticVisibilityDomain,
    SemanticVisibilityDomainCache, resolve_semantic_module_path,
    resolve_structured_semantic_type_syntax, resolve_structured_semantic_type_syntax_with,
};
pub use types::{
    ArrayLen, ArrayTypeId, EnumDef, EnumId, LangItem, ModuleDef, ModuleId, PtrConstTypeId,
    PtrMutTypeId, StructDef, StructField, StructId, Type, TypeKind, fixed_string_capacity,
    is_slice_struct_name, is_string_view_struct_name,
};

/// Sentinel value used to encode parameter slots in AIR instructions.
///
/// When a slot value is >= this marker, it indicates a parameter slot rather than
/// a local variable slot. The actual parameter index is `slot - PARAM_SLOT_MARKER`.
///
/// This allows sema to emit Store/Load instructions for parameters without knowing
/// the total number of locals at analysis time.
pub const PARAM_SLOT_MARKER: u32 = 0x4000_0000;
