//! Compiler-side composition for the canonical AIR comptime engine.
//!
//! This facade deliberately owns no evaluation, query, lifecycle, projection,
//! or diagnostic policy. Each responsibility has one module owner below, and
//! [`host::DurableComptimeHost`] composes those services for AIR without
//! becoming another evaluator.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ahash::AHashMap;
use lasso::Key;
use rue_air::{
    ComptimeFile, ComptimeIdentity, ComptimeMatchPattern, ComptimeName, ComptimeSemanticRejection,
    ComptimeTargetIntrinsic, ComptimeType, ComptimeUnaryOperation, ComptimeValue,
};
#[cfg(test)]
use rue_query::CancellationToken;
use rue_query::QueryAbort;

use crate::ModuleId;
use crate::body_query::ForeignComptimeCallLookup;
use crate::declaration_candidate::{DeclarationCandidateKey, DeclarationImportFailure};
use crate::durable_semantics::{DurableConstValue, DurableType};
use crate::semantic_query_nucleus::SemanticNucleusFailure;

type DurableAnonymousNominal = crate::durable_semantics::DurableAnonymousNominal;
type SemanticDeclarationDependency = crate::semantic_query_nucleus::SemanticDeclarationDependency;
type DeferredOwnershipGate = crate::semantic_query_nucleus::DeferredOwnershipGate;
type DeferredOwnershipApplication = crate::semantic_query_nucleus::DeferredOwnershipApplication;

mod diagnostics;
mod effects;
mod host;
mod lifecycle;
mod projection;
mod services;
mod structured;
mod target;

pub(crate) use diagnostics::*;
pub(crate) use effects::*;
pub(crate) use host::*;
pub(crate) use lifecycle::*;
pub(crate) use projection::*;
pub(crate) use services::*;
pub(crate) use structured::*;
pub(crate) use target::*;
