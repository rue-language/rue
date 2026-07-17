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
mod canonical_imports;
mod inference;
mod inst;
mod intern_pool;
mod module_registry;
mod param_arena;
mod path_norm;
mod runtime_call;
mod scope;
mod sema;
mod semantic_body;
mod semantic_import;
pub mod specialize;
mod type_encoding;
#[cfg(test)]
mod type_properties;
mod types;

pub use canonical_imports::CanonicalImportView;
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
    ValidatedAir,
};
pub use intern_pool::{
    EnumData, FrozenTypeInternPool, StructData, TypeData, TypeInternPool, TypeInternPoolStats,
    TypeValidationError,
};
pub use module_registry::ModuleRegistry;
pub use param_arena::{ParamArena, ParamRange};
pub use path_norm::normalize_module_path;
pub use runtime_call::{
    OptionVariant, RuntimeAirArgument, RuntimeAirType, RuntimeCallActivation, RuntimeCallKind,
    RuntimeOperandOrigin,
};
pub use sema::{
    AnalyzedBodyOwnerEvent, AnalyzedFunction, BodyAnalysisFailure, BodyAnalysisWork,
    BodyNamedDependencyEvent, BodyOwnerEndpoint, BodyOwnerKind, BodyOwnerToken, BoundSema,
    BuiltinTypeCallHead, ConstValue, DeclarationBindingWork,
    DeclarationBuiltinTypeCallHeadDependencyEvent, DeclarationInstallFailure,
    DeclarationResolutionFailure, DeclarationShells, DeclarationTypeCallHeadDependencyEvent,
    DeclarationTypeDependencyEvent, DeclarationTypeDependencyKind,
    DeclarationTypeDependencySourceKind, DeclarationTypeDependencyTargetKind, FunctionInfo,
    ImplicitDropDependencySourceEvent, ImplicitNamedDestructorDependencyEvent, MethodInfo,
    NamedConstDependencyEvent, NamedConstDependencyTargetEvent, NamedDestructorDependencyEvent,
    NamedMethodDependencyEvent, NamedMethodDependencyTargetEvent,
    OrdinaryFreeFunctionDependencyEvent, ParamSlotModes, RirDeclarationIndexWork, Sema, SemaOutput,
    SemanticBinding, SemanticBindingKind, SemanticBindingManifest, SemanticBindingManifestWork,
    SemanticBindingNamespace, SemanticDeclarationExport, SemanticDeclarationExportWork,
    SemanticDeclarationPayload, SemanticDeclarationShell, SemanticDeclarationShellIdentity,
    SemanticExportConstValue, SemanticExportFailure, SemanticExportParameter, SemanticExportType,
    SemanticNominalIdentity, SemanticParameterMode, SpecializedFreeFunctionDependencyEvent,
    SpecializedFreeFunctionOrigin,
};
pub use semantic_body::{
    SemanticBody, SemanticBodyAnchor, SemanticBodyCallArg, SemanticBodyCandidate,
    SemanticBodyCandidateInstallWork, SemanticBodyDefinitionIdentity, SemanticBodyDefinitionKind,
    SemanticBodyExport, SemanticBodyExportFailure, SemanticBodyImportFailure, SemanticBodyInst,
    SemanticBodyInstData, SemanticBodyMatchArm, SemanticBodyModuleIdentity, SemanticBodyPattern,
    SemanticBodyPlace, SemanticBodyPlaceRef, SemanticBodyProjection, SemanticBodyRef,
    SemanticBodyWarning, SemanticImportedBody, SemanticSpecializationIdentity,
    SemanticSpecializedBodyCandidate, SemanticSpecializedBodyExport,
    SemanticSpecializedCandidateInstallWork,
};
pub use semantic_import::{
    SemanticImportConstValue, SemanticImportEpoch, SemanticImportFailure, SemanticImportNominal,
    SemanticImportNominalKind, SemanticImportType, SemanticImportedConstValue,
    SemanticImportedType,
};
pub use types::{
    ArrayTypeId, EnumDef, EnumId, LangItem, ModuleDef, ModuleId, PtrConstTypeId, PtrMutTypeId,
    StructDef, StructField, StructId, Type, TypeKind, parse_array_type_syntax,
};

/// Sentinel value used to encode parameter slots in AIR instructions.
///
/// When a slot value is >= this marker, it indicates a parameter slot rather than
/// a local variable slot. The actual parameter index is `slot - PARAM_SLOT_MARKER`.
///
/// This allows sema to emit Store/Load instructions for parameters without knowing
/// the total number of locals at analysis time.
pub const PARAM_SLOT_MARKER: u32 = 0x4000_0000;
