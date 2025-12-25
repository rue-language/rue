//! Hindley-Milner type inference infrastructure.
//!
//! This module provides the core types and algorithms for constraint-based
//! type inference. The design follows Algorithm W with special handling for
//! integer literals.
//!
//! # Architecture
//!
//! Type inference works in three phases:
//! 1. **Constraint Generation**: Walk RIR, assign type variables to unknowns,
//!    generate constraints
//! 2. **Unification**: Solve constraints using Algorithm W, resolve type
//!    variables to concrete types
//! 3. **AIR Emission**: Walk RIR again with resolved types to emit typed AIR
//!
//! # Module Organization
//!
//! - [`types`] - Type variable infrastructure (`TypeVarId`, `InferType`, `TypeVarAllocator`)
//! - [`constraint`] - Constraint representation (`Constraint`, `Substitution`)
//! - [`unify`] - Unification engine (`Unifier`, `UnifyResult`, `UnificationError`)
//! - [`generate`] - Constraint generation (`ConstraintContext`, `ConstraintGenerator`)
//!
//! # Type Variables
//!
//! Type variables ([`TypeVarId`]) represent unknown types to be solved.
//! The [`Substitution`] maps type variables to their resolved types.
//!
//! # Integer Literals
//!
//! Integer literals get the special [`InferType::IntLiteral`] type rather than
//! a type variable. This models the fact that a literal like `42` can become
//! any integer type. When an `IntLiteral` unifies with a concrete integer type,
//! it becomes that type. Unconstrained `IntLiteral`s default to `i32` at the end.

mod constraint;
mod generate;
mod types;
mod unify;

// Re-export all public types
pub use constraint::{Constraint, Substitution};
pub use generate::{
    ConstraintContext, ConstraintGenerator, ExprInfo, FunctionSig, LocalVarInfo, MethodSig,
    ParamVarInfo,
};
pub use types::{InferType, TypeVarAllocator, TypeVarId};
pub use unify::{UnificationError, Unifier, UnifyResult};
