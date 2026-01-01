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

mod analysis_state;
mod function_analyzer;
mod inference;
mod inst;
mod scope;
mod sema;
mod sema_context;
mod type_context;
mod types;

pub use analysis_state::{AnalysisStateRemapping, FunctionAnalysisState, MergedAnalysisState};
pub use function_analyzer::{
    FunctionAnalyzer, FunctionAnalyzerOutput, FunctionOutputRemapping, MergedFunctionOutput,
};
pub use inference::{
    Constraint, ConstraintContext, ConstraintGenerator, ExprInfo, FunctionSig, InferType,
    LocalVarInfo, MethodSig, ParamVarInfo, Substitution, TypeVarAllocator, TypeVarId,
    UnificationError, Unifier, UnifyResult,
};
pub use inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirParamMode, AirPattern, AirRef,
};
pub use sema::{AnalyzedFunction, FunctionInfo, GatherOutput, MethodInfo, Sema, SemaOutput};
// Note: FunctionInfo and MethodInfo are defined in sema and re-exported by sema_context.
// We export InferenceContext and SemaContext from sema_context.
pub use sema_context::{InferenceContext as SemaContextInferenceContext, SemaContext};
pub use type_context::{FunctionSignature, MethodSignature, TypeContext};
pub use types::{
    ArrayTypeDef, ArrayTypeId, EnumDef, EnumId, StructDef, StructField, StructId, Type,
    parse_array_type_syntax,
};

/// Sentinel value used to encode parameter slots in AIR instructions.
///
/// When a slot value is >= this marker, it indicates a parameter slot rather than
/// a local variable slot. The actual parameter index is `slot - PARAM_SLOT_MARKER`.
///
/// This allows sema to emit Store/Load instructions for parameters without knowing
/// the total number of locals at analysis time.
pub const PARAM_SLOT_MARKER: u32 = 0x4000_0000;
