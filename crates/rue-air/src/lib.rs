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

mod inference;
mod inst;
mod intern_pool;
mod param_arena;
mod scope;
mod sema;
mod sema_context;
pub mod specialize;
mod types;

pub use inference::{
    Constraint, ConstraintContext, ConstraintGenerator, ExprInfo, FunctionSig, InferType,
    LocalVarInfo, MethodSig, ParamVarInfo, Substitution, TypeVarAllocator, TypeVarId,
    UnificationError, Unifier, UnifyResult,
};
pub use inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirParamMode, AirPattern, AirPlace,
    AirPlaceBase, AirPlaceRef, AirProjection, AirRef,
};
pub use intern_pool::{
    EnumData, InternedType, StructData, TypeData, TypeInternPool, TypeInternPoolStats,
};
pub use param_arena::{ParamArena, ParamRange};
pub use sema::{
    AnalyzedFunction, ConstValue, FunctionInfo, GatherOutput, MethodInfo, Sema, SemaOutput,
};
pub use sema_context::ModuleRegistry;
pub use types::{
    ArrayTypeId, EnumDef, EnumId, ModuleDef, ModuleId, PtrConstTypeId, PtrMutTypeId, StructDef,
    StructField, StructId, Type, TypeKind, parse_array_type_syntax,
};

/// Sentinel value used to encode parameter slots in AIR instructions.
///
/// When a slot value is >= this marker, it indicates a parameter slot rather than
/// a local variable slot. The actual parameter index is `slot - PARAM_SLOT_MARKER`.
///
/// This allows sema to emit Store/Load instructions for parameters without knowing
/// the total number of locals at analysis time.
pub const PARAM_SLOT_MARKER: u32 = 0x4000_0000;
