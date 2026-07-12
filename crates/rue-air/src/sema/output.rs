//! Output types from semantic analysis.
//!
//! This module contains the final outputs produced by semantic analysis:
//! - [`AnalyzedFunction`] - A single analyzed function with typed IR
//! - [`SemaOutput`] - Complete output from analyzing a program

use crate::inst::Air;
use crate::intern_pool::TypeInternPool;
use rue_error::CompileWarning;

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
}

/// Value-only workload counters for demand-driven body dispatch.
///
/// These counters deliberately expose no request-local RIR instruction handles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BodyAnalysisWork {
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
