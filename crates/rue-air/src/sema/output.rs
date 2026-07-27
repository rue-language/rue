//! Output types from semantic analysis.
//!
//! This module contains the final outputs produced by semantic analysis:
//! - [`AnalyzedFunction`] - A single analyzed function with typed IR
//! - [`SemaOutput`] - Complete output from analyzing a program

use std::collections::HashMap;

use crate::{SemanticBodyExport, SemanticSpecializedBodyExport, Type};
use rue_error::CompileWarning;
/// Opaque identity issued by the compiler for one supported ordinary body.
///
/// Neither component has meaning to AIR.  The issuer prevents tokens from
/// being joined across semantic epochs; the slot is interpreted only by the
/// compiler-side issuer map retained with the canonical output.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyOwnerToken {
    issuer: u64,
    slot: u32,
}
impl std::fmt::Debug for BodyOwnerToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("BodyOwnerToken")
            .field(&self.slot)
            .finish()
    }
}

impl BodyOwnerToken {
    pub fn new(issuer: u64, slot: u32) -> Self {
        Self { issuer, slot }
    }
    pub fn issuer(self) -> u64 {
        self.issuer
    }
    pub fn slot(self) -> u32 {
        self.slot
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BodyOwnerKind {
    FreeFunction,
    Method,
    AssociatedFunction,
    Destructor,
}

/// Validated installation endpoint.  Text and file identity are checked
/// provenance used only to attach the opaque token to AIR's bound declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyOwnerEndpoint {
    pub token: BodyOwnerToken,
    pub kind: BodyOwnerKind,
    pub file: u32,
    pub name: String,
    pub owner_name: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnalyzedBodyOwnerEvent {
    FreeFunction {
        token: BodyOwnerToken,
        file: u32,
        name: String,
    },
    NamedMethod {
        token: BodyOwnerToken,
        file: u32,
        owner_name: String,
        method_name: String,
        generic: bool,
    },
    NamedDestructor {
        token: BodyOwnerToken,
        file: u32,
        owner_name: String,
    },
    Anonymous,
}
impl AnalyzedBodyOwnerEvent {
    pub fn token(&self) -> Option<BodyOwnerToken> {
        match self {
            Self::FreeFunction { token, .. }
            | Self::NamedMethod { token, .. }
            | Self::NamedDestructor { token, .. } => Some(*token),
            Self::Anonymous => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrdinaryFreeFunctionDependencyEvent {
    pub caller_token: BodyOwnerToken,
    pub caller_file: u32,
    pub caller_name: String,
    pub callee_file: u32,
    pub callee_name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpecializedFreeFunctionOrigin {
    pub specialized_name: String,
    pub base_file: u32,
    pub base_name: String,
    pub type_arguments: Vec<u32>,
    pub value_arguments: Vec<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpecializedFreeFunctionDependencyEvent {
    pub specialized_name: String,
    pub base_file: u32,
    pub base_name: String,
    pub callee_file: u32,
    pub callee_name: String,
    pub type_arguments: Vec<u32>,
    pub value_arguments: Vec<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NamedMethodDependencyTargetEvent {
    FreeFunction {
        file: u32,
        name: String,
    },
    NamedMethod {
        file: u32,
        owner_name: String,
        method_name: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamedMethodDependencyEvent {
    pub caller_token: BodyOwnerToken,
    pub caller_file: u32,
    pub caller_owner_name: String,
    pub caller_method_name: String,
    pub target: NamedMethodDependencyTargetEvent,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamedDestructorDependencyEvent {
    pub caller_token: BodyOwnerToken,
    pub caller_file: u32,
    pub caller_owner_name: String,
    pub target: NamedMethodDependencyTargetEvent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeclarationTypeDependencySourceKind {
    Function,
    Struct,
    Enum,
    ValueConst,
    Method,
    AssociatedFunction,
    Destructor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeclarationTypeDependencyTargetKind {
    Struct,
    Enum,
    ValueConst,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeclarationTypeDependencyKind {
    Signature,
    Body,
    Field,
    Payload,
    DeclaredType,
    Owner,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeclarationTypeDependencyEvent {
    pub source_token: Option<BodyOwnerToken>,
    pub source_file: u32,
    pub source_name: String,
    pub source_owner_name: Option<String>,
    pub source_kind: DeclarationTypeDependencySourceKind,
    pub dependency_kind: DeclarationTypeDependencyKind,
    pub target_file: u32,
    pub target_name: String,
    pub target_kind: DeclarationTypeDependencyTargetKind,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeclarationTypeCallHeadDependencyEvent {
    pub source_token: Option<BodyOwnerToken>,
    pub source_file: u32,
    pub source_name: String,
    pub source_owner_name: Option<String>,
    pub source_kind: DeclarationTypeDependencySourceKind,
    pub dependency_kind: DeclarationTypeDependencyKind,
    pub callable_file: u32,
    pub callable_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinTypeCallHead {
    FixedCapacityString,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeclarationBuiltinTypeCallHeadDependencyEvent {
    pub source_token: Option<BodyOwnerToken>,
    pub source_file: u32,
    pub source_name: String,
    pub source_owner_name: Option<String>,
    pub source_kind: DeclarationTypeDependencySourceKind,
    pub dependency_kind: DeclarationTypeDependencyKind,
    pub builtin: BuiltinTypeCallHead,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NamedConstDependencyTargetEvent {
    ValueConst {
        file: u32,
        name: String,
    },
    FreeFunction {
        file: u32,
        name: String,
    },
    NamedType {
        file: u32,
        name: String,
        kind: DeclarationTypeDependencyTargetKind,
    },
    ModuleBinding {
        file: u32,
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamedConstDependencyEvent {
    pub source_file: u32,
    pub source_name: String,
    pub target: NamedConstDependencyTargetEvent,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyNamedDependencyEvent {
    pub source: AnalyzedBodyOwnerEvent,
    pub target: NamedConstDependencyTargetEvent,
}

/// Stable-capable owner of an analyzed body or synthesized named-type glue.
///
/// This deliberately contains no request-local symbols or type IDs. CFG
/// lowering attaches destructor obligations to this owner; the compiler later
/// joins it against the exact-revision stable-definition universe.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImplicitDropDependencySourceEvent {
    Anonymous,
    Specialization {
        identity: crate::SemanticSpecializationIdentity<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    },
    FreeFunction {
        token: BodyOwnerToken,
        file: u32,
        name: String,
    },
    NamedMethod {
        token: BodyOwnerToken,
        file: u32,
        owner_name: String,
        method_name: String,
    },
    NamedDestructor {
        token: BodyOwnerToken,
        file: u32,
        owner_name: String,
    },
    NamedStruct {
        file: u32,
        name: String,
    },
    NamedEnum {
        file: u32,
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImplicitNamedDestructorDependencyEvent {
    pub source: ImplicitDropDependencySourceEvent,
    pub target_file: u32,
    pub target_owner_name: String,
}

/// Per-source-parameter native ABI descriptor carried from semantic analysis
/// into code generation (ADR-0052 phase 5.8, RUE-1005).
///
/// The per-ABI-slot [`ParamSlotModes::by_ref`] vector tells code generation
/// which *slots* transport a pointer, but not how a *source parameter* groups
/// its slots — which the callee prologue needs to map incoming argument
/// registers onto frame parameter slots. A by-value non-slot-identical compact
/// aggregate crosses as one [`ArgClass::Indirect`] pointer (one incoming
/// register) while occupying `slot_count` frame slots, so the prologue cannot
/// assume one incoming register per parameter slot. This descriptor carries the
/// classifier's decision plus the parameter's slot span so the prologue can
/// compute per-parameter incoming-register widths from the classifier authority
/// rather than re-deriving them. With the gate off every parameter is
/// [`ArgClass::Direct`] (or a single by-reference pointer slot) and
/// `arg_class.crossing_slots() == slot_count`, so the plumbing is behaviourally
/// inert and the emitted prologue is byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceParamAbi {
    /// First parameter ABI slot this source parameter occupies.
    pub start_slot: u32,
    /// Physical value-decomposition width (frame slots reserved), equal to
    /// [`crate::NativeCallAbi::arg_slot_width`].
    pub slot_count: u32,
    /// Number of incoming argument registers this parameter consumes, equal to
    /// the classifier's [`crate::ArgClass::crossing_slots`]. For a direct
    /// parameter this equals `slot_count`; for a by-reference pointer or a
    /// by-value indirect compact aggregate it is one (the pointer), which is why
    /// `crossing_regs < slot_count` distinguishes exactly the by-value indirect
    /// aggregate the callee must unmarshal.
    pub crossing_regs: u32,
    /// The aggregate type — carried *only* for a by-value indirect parameter, so
    /// code generation can build its compact memory image; `None` for every
    /// direct parameter and every by-reference pointer. Withholding the type
    /// from a pointer-only or direct parameter keeps that CFG layout-independent
    /// and reusable when an unrelated pointee struct's layout changes; a by-value
    /// indirect aggregate already depends on its own layout (it reads its
    /// fields), so carrying the type adds no new dependency.
    pub ty: Option<crate::Type>,
}

impl SourceParamAbi {
    /// Whether this parameter is a by-value aggregate the classifier forced
    /// indirect: it reserves `slot_count` frame slots but arrives as one pointer
    /// register, so the callee unmarshals its compact image at entry (RUE-1005).
    /// A by-reference pointer (`crossing_regs == slot_count == 1`) and a direct
    /// parameter (`crossing_regs == slot_count`) are both excluded.
    pub const fn is_by_value_indirect(&self) -> bool {
        self.crossing_regs < self.slot_count
    }
}

/// Per-ABI-slot parameter access metadata preserved into CFG.
///
/// `by_ref` describes the physical calling convention: both `borrow` and
/// `inout` parameters may be passed indirectly. `writable` preserves the
/// distinct source-level permission needed by consumers such as the oracle;
/// only logical `inout` slots are writable. Keeping the two facts separate
/// prevents a shared borrow from being mistaken for mutable caller storage.
///
/// `SourceParamAbi` grouping is *not* stored here: it is derived at CFG-build
/// time from the AIR parameter types and this per-slot `by_ref` vector (see
/// `rue_cfg`), so a durable/imported body — which serializes only the per-slot
/// vectors and rebuilds its CFG from the same AIR — reconstructs an identical
/// grouping without extending this identity-bearing slot-mode structure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParamSlotModes {
    by_ref: Vec<bool>,
    writable: Vec<bool>,
}

impl ParamSlotModes {
    pub fn new(by_ref: Vec<bool>, writable: Vec<bool>) -> Self {
        assert_eq!(
            by_ref.len(),
            writable.len(),
            "parameter slot mode vectors must have equal length"
        );
        Self { by_ref, writable }
    }

    pub fn by_ref(&self) -> &[bool] {
        &self.by_ref
    }

    pub fn writable(&self) -> &[bool] {
        &self.writable
    }
}

/// Phase-local compatibility for synthetic CFG tests that model only physical
/// passing. Canonical semantic outputs always construct both vectors with
/// [`ParamSlotModes::new`]; this adapter is not a supported compiler boundary.
impl From<Vec<bool>> for ParamSlotModes {
    fn from(by_ref: Vec<bool>) -> Self {
        let writable = vec![false; by_ref.len()];
        Self::new(by_ref, writable)
    }
}

/// Result of analyzing a function.
#[derive(Debug)]
pub struct AnalyzedFunction {
    /// Exact issuer-scoped semantic identity of this concrete callable.
    ///
    /// This is independent of durable-body/CFG eligibility: every analyzed or
    /// synthesized body has an identity even when it cannot be retained.
    pub identity:
        crate::FunctionInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    pub name: String,
    pub callable_kind: AnalyzedCallableKind,
    /// Durable-body ownership of an eligible source-level ordinary body.
    /// This is retention metadata, not callable identity.
    pub ordinary_owner: Option<BodyOwnerToken>,
    /// Definition-level provenance used by CFG dependency capture.
    pub implicit_drop_source: Option<ImplicitDropDependencySourceEvent>,
    pub air: crate::ValidatedAir,
    /// Occurrence-preserving local data identities sorted at the compiler
    /// retention boundary after dense string IDs are globally remapped.
    pub local_atoms:
        Vec<crate::LocalAtomRecord<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
    /// Number of local variable slots needed
    pub num_locals: u32,
    /// Number of ABI slots used by parameters.
    /// For scalar types (i32, bool), each parameter uses 1 slot.
    /// For struct types, each field uses 1 slot (flattened ABI).
    pub num_param_slots: u32,
    /// Physical by-reference and logical writability modes for every ABI slot.
    /// Length matches `num_param_slots`; flattened parameters repeat their
    /// source parameter's mode for each occupied slot.
    pub param_modes: ParamSlotModes,
    /// Whether function-level `@allow(unreachable_code)` suppresses CFG
    /// unreachable-code warnings while lowering this function.
    pub allow_unreachable_code: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnalyzedCallableKind {
    Ordinary,
    Destructor,
    DropGlue,
}

impl AnalyzedCallableKind {
    pub fn uses_direct_slot_abi(self) -> bool {
        matches!(self, Self::Destructor | Self::DropGlue)
    }
}

/// Output from semantic analysis.
///
/// Contains all analyzed functions, struct definitions, enum definitions, and any warnings
/// generated during analysis.
#[derive(Debug)]
pub struct SemaOutput {
    /// Analyzed functions with typed IR.
    pub functions: Vec<AnalyzedFunction>,
    /// String literals indexed by their AIR string_const index.
    pub strings: Vec<String>,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
    /// Completed immutable type metadata (contains all types including arrays).
    pub type_pool: crate::FrozenTypeInternPool,
    /// Request-local anonymous nominal types mapped to identities issued before
    /// their pool allocation. Consumers must project the opaque definition and
    /// module tokens at the compiler retention boundary before persisting keys.
    pub anonymous_nominal_identities_by_type: HashMap<
        Type,
        crate::AnonymousNominalKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    >,
    /// Issuer-scoped canonical identities for every aggregate type that may
    /// own synthesized drop glue. This lets the compiler name glue without
    /// reconstructing semantic identity from display names or pool order.
    pub aggregate_type_identities_by_type: HashMap<
        Type,
        crate::TypeInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
    >,
    /// Exact structural work performed while dispatching reachable bodies.
    pub body_analysis_work: BodyAnalysisWork,
    /// Pre-specialization durable candidates for supported ordinary bodies.
    pub ordinary_body_exports: Vec<SemanticBodyExport>,
    /// Completed post-fixed-point generic instances eligible for durable reuse.
    pub specialized_body_exports: Vec<SemanticSpecializedBodyExport>,
    /// Stable-capable provenance for every successfully analyzed source body.
    pub analyzed_body_owners: Vec<AnalyzedBodyOwnerEvent>,
    pub body_named_dependencies: Vec<BodyNamedDependencyEvent>,
    pub ordinary_free_function_dependencies: Vec<OrdinaryFreeFunctionDependencyEvent>,
    pub ordinary_free_function_dependencies_complete: bool,
    pub specialized_free_function_origins: Vec<SpecializedFreeFunctionOrigin>,
    pub specialized_free_function_dependencies: Vec<SpecializedFreeFunctionDependencyEvent>,
    pub specialized_free_function_dependencies_complete: bool,
    pub named_method_dependencies: Vec<NamedMethodDependencyEvent>,
    pub non_generic_named_method_dependencies_complete: bool,
    pub generic_named_method_dependencies_complete: bool,
    pub named_destructor_dependencies: Vec<NamedDestructorDependencyEvent>,
    pub named_destructor_dependencies_complete: bool,
    pub declaration_type_dependencies: Vec<DeclarationTypeDependencyEvent>,
    pub declaration_type_dependencies_complete: bool,
    pub declaration_type_call_head_dependencies: Vec<DeclarationTypeCallHeadDependencyEvent>,
    pub declaration_type_call_head_dependencies_complete: bool,
    pub declaration_builtin_type_call_head_dependencies:
        Vec<DeclarationBuiltinTypeCallHeadDependencyEvent>,
    pub supported_type_call_heads_complete: bool,
    pub named_const_dependencies: Vec<NamedConstDependencyEvent>,
    pub named_value_const_dependencies_complete: bool,
}

/// Value-only workload counters for demand-driven body dispatch.
///
/// These counters deliberately expose no request-local RIR instruction handles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BodyAnalysisWork {
    /// Production body transactions whose analysis closure actually ran.
    pub body_analyses_computed: usize,
    /// Production body transactions satisfied by a retained terminal or
    /// compatible join without entering the analysis closure.
    pub body_analyses_reused: usize,
    /// Previously retained body transactions whose dependencies turned red and
    /// therefore entered a new computation for the current revision.
    pub body_analyses_invalidated: usize,
    pub ordinary_body_import_attempts: usize,
    pub ordinary_body_import_successes: usize,
    pub ordinary_body_import_failures: usize,
    pub last_ordinary_body_import_failure: Option<crate::SemanticBodyImportFailureKind>,
    pub ordinary_body_import_instructions_installed: usize,
    pub ordinary_body_import_places_installed: usize,
    pub ordinary_body_import_strings_installed: usize,
    pub ordinary_body_import_atomic_discards: usize,
    /// Durable ordinary bodies actually consumed from the canonical worklist.
    pub ordinary_bodies_reused: usize,
    /// Ordinary source analyses avoided by those consumed reuse hits.
    pub ordinary_body_analyses_skipped: usize,
    pub bodies_attempted: usize,
    pub bodies_succeeded: usize,
    pub bodies_failed: usize,
    /// Body-traversal coordinator restarts: `continue 'closure` iterations taken
    /// when an anonymous representative changed and the in-flight closure was
    /// discarded and re-traversed from roots. Stays zero for programs without
    /// anonymous method producers. Populated only by the session coordinator;
    /// the per-`Sema` analysis leaves it at zero.
    pub closure_restarts: usize,
    /// Times the coordinator deferred a consumer body behind an as-yet-unreached
    /// anonymous producer and rescheduled it on the priority stack. Populated
    /// only by the session coordinator.
    pub deferred_producer_retries: usize,
    /// Distinct bodies in the final published closure (coordinator `visited` set
    /// size at successful completion). Snapshot at completion, not a running
    /// total. Populated only by the session coordinator.
    pub closure_bodies_visited: usize,
    /// Deepest specialization instantiation chain reached during traversal (the
    /// maximum `instance_depth` value). Snapshot at completion, not a running
    /// total. Populated only by the session coordinator.
    pub max_specialization_depth: usize,
    /// Demand-driven comparisons between distinct concrete semantic types.
    /// This stays proportional to comparisons requested by body constraints;
    /// it never includes a scan of unrelated types in the global pool.
    pub semantic_type_equivalence_queries: usize,
    pub air_instructions_produced: usize,
    pub body_dependency_air_instructions_observed: usize,
    pub local_strings_produced: usize,
    pub ordinary_body_exports_attempted: usize,
    pub ordinary_body_exports_succeeded: usize,
    pub ordinary_body_exports_rejected: usize,
    pub last_ordinary_body_export_failure: Option<crate::SemanticBodyExportFailure>,
    pub ordinary_body_export_instructions_emitted: usize,
    pub ordinary_body_export_places_emitted: usize,
    pub ordinary_body_export_strings_emitted: usize,
    pub specialized_body_exports_attempted: usize,
    pub specialized_body_exports_succeeded: usize,
    pub specialized_body_exports_rejected: usize,
    pub last_specialized_body_export_failure: Option<crate::SemanticBodyExportFailure>,
    pub specialized_body_export_instructions_emitted: usize,
    pub specialized_body_export_places_emitted: usize,
    pub specialized_body_export_strings_emitted: usize,
    pub specialized_body_import_attempts: usize,
    pub specialized_body_import_successes: usize,
    pub specialized_body_import_failures: usize,
    pub specialized_body_import_instructions_installed: usize,
    pub specialized_body_import_places_installed: usize,
    pub specialized_body_import_strings_installed: usize,
    pub specialized_bodies_reused: usize,
    pub specialized_body_analyses_skipped: usize,
    pub string_ids_remapped: usize,
    pub specialization_air_instructions_scanned: usize,
    pub generic_calls_observed: usize,
    pub specialization_requests_unique: usize,
    pub specialization_requests_duplicate: usize,
    pub specialization_rewrites: usize,
    pub specialization_rounds: usize,
    /// Failures in specialization orchestration before a specialized body is attempted.
    pub specialization_driver_failures: usize,
    pub specialized_bodies_attempted: usize,
    pub specialized_bodies_succeeded: usize,
    pub specialized_bodies_failed: usize,
    pub ordinary_free_function_dependency_events: usize,
    pub specialized_origin_records: usize,
    pub specialized_free_function_dependency_events: usize,
    pub named_method_dependency_events: usize,
    pub named_destructor_dependency_events: usize,
    pub declaration_type_dependency_events: usize,
    pub declaration_type_call_head_dependency_events: usize,
    pub named_const_dependency_events: usize,
    /// Indexed declaration-record lookups for reachable, non-generic free functions.
    pub free_function_record_lookups: usize,
    /// Private declaration-record lookups for reachable named-struct methods.
    pub named_method_record_lookups: usize,
    /// MethodInfo-driven lookups for reachable anonymous-struct methods.
    pub anonymous_method_record_lookups: usize,
    /// Indexed named-destructor records selected as implicit roots.
    pub named_destructor_declarations_visited: usize,
    /// Raw RIR entries visited specifically to select named-destructor roots.
    /// Indexed dispatch keeps this value at zero.
    pub named_destructor_selection_rir_visits: usize,
    /// Raw RIR entries visited specifically to select ordinary reachable free
    /// functions or named methods.
    ///
    /// This excludes one-time implicit-root scans, unused-reference scans, and
    /// comptime evaluation. Indexed dispatch keeps this value at zero.
    pub reachable_declaration_rir_visits: usize,
}

/// Diagnostics and value-only structural work from a failed body-analysis
/// request. Failed AIR artifacts remain private and are never published.
#[derive(Debug, Clone)]
pub struct BodyAnalysisFailure {
    errors: rue_error::CompileErrors,
    work: BodyAnalysisWork,
}

impl BodyAnalysisFailure {
    pub(crate) fn new(errors: rue_error::CompileErrors, work: BodyAnalysisWork) -> Self {
        Self { errors, work }
    }

    pub fn work(&self) -> BodyAnalysisWork {
        self.work
    }

    pub fn into_errors(self) -> rue_error::CompileErrors {
        self.errors
    }
}
