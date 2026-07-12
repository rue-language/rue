//! Output types from semantic analysis.
//!
//! This module contains the final outputs produced by semantic analysis:
//! - [`AnalyzedFunction`] - A single analyzed function with typed IR
//! - [`SemaOutput`] - Complete output from analyzing a program

use crate::inst::Air;
use crate::intern_pool::TypeInternPool;
use rue_error::CompileWarning;
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrdinaryFreeFunctionDependencyEvent {
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
    pub caller_file: u32,
    pub caller_owner_name: String,
    pub caller_method_name: String,
    pub target: NamedMethodDependencyTargetEvent,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamedDestructorDependencyEvent {
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
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeclarationTypeDependencyKind {
    Signature,
    Field,
    Payload,
    DeclaredType,
    Owner,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeclarationTypeDependencyEvent {
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
    pub source_file: u32,
    pub source_name: String,
    pub source_owner_name: Option<String>,
    pub source_kind: DeclarationTypeDependencySourceKind,
    pub dependency_kind: DeclarationTypeDependencyKind,
    pub callable_file: u32,
    pub callable_name: String,
}

/// Per-ABI-slot parameter access metadata preserved into CFG.
///
/// `by_ref` describes the physical calling convention: both `borrow` and
/// `inout` parameters may be passed indirectly. `writable` preserves the
/// distinct source-level permission needed by consumers such as the oracle;
/// only logical `inout` slots are writable. Keeping the two facts separate
/// prevents a shared borrow from being mistaken for mutable caller storage.
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

/// Compatibility for synthetic/test CFGs that only describe the physical
/// convention. Such parameters are conservatively not writable.
impl From<Vec<bool>> for ParamSlotModes {
    fn from(by_ref: Vec<bool>) -> Self {
        let writable = vec![false; by_ref.len()];
        Self { by_ref, writable }
    }
}

/// Result of analyzing a function.
#[derive(Debug)]
pub struct AnalyzedFunction {
    pub name: String,
    pub air: Air,
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
    /// Type intern pool (contains all types including arrays).
    pub type_pool: TypeInternPool,
    /// Exact structural work performed while dispatching reachable bodies.
    pub body_analysis_work: BodyAnalysisWork,
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
}

/// Value-only workload counters for demand-driven body dispatch.
///
/// These counters deliberately expose no request-local RIR instruction handles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BodyAnalysisWork {
    pub ordinary_free_function_dependency_events: usize,
    pub specialized_origin_records: usize,
    pub specialized_free_function_dependency_events: usize,
    pub named_method_dependency_events: usize,
    pub named_destructor_dependency_events: usize,
    pub declaration_type_dependency_events: usize,
    pub declaration_type_call_head_dependency_events: usize,
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
