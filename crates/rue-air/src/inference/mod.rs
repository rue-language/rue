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
//! # Integer Literals (unify-then-default)
//!
//! Un-annotated integer literals follow HM **unify-then-default** (spec
//! 3.1:14–15, ADR-0007, RUE-218), *not* an eager first-use pin. Each literal is
//! given a fresh type variable (recorded in `int_literal_vars`) that unifies
//! with **every** use across the enclosing function body's constraint set. Two
//! uses that require different integer types therefore conflict regardless of
//! textual order (E0206). A variable that is still unconstrained at the
//! defaulting point — the end of the function's constraint solve — defaults to
//! `i32` (see [`unify::Unifier::default_int_literal_vars`]).
//!
//! The order-independence is a property of solving the *whole* constraint set
//! before defaulting: binding a literal variable to a concrete integer type on
//! first contact is ordinary unification (the variable genuinely *is* that
//! type), so a later conflicting use is still rejected. Defaulting is applied
//! once, after solving, never mid-stream — do not reintroduce an eager i32
//! default at the literal's definition site.

mod constraint;
mod generate;
mod types;
mod unify;

// Re-export all public types
pub use constraint::{Constraint, Substitution};
pub(crate) use generate::LazyInferenceFacts;
pub use generate::{
    ConstraintContext, ConstraintGenerator, ExprInfo, FunctionSig, LocalVarInfo, MethodSig,
    ParamVarInfo,
};
pub use types::{InferType, TypeVarAllocator, TypeVarId};
pub use unify::{UnificationError, Unifier, UnifyResult};
