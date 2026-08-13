//! Typed trusted-toolchain-module demands (RUE-1112).
//!
//! # Why this is a distinct mechanism from import discovery
//!
//! A freestanding Rue program (zero `@import`s) whose reached body contains a
//! fallible intrinsic (`@read_line`, `@parse_i32/i64/u32/u64`) must still be
//! able to obtain the trusted standard-library `Option` type: sema resolves the
//! intrinsic's `Option(payload)` result against the canonical comptime-generic
//! `Option` enum (RUE-6, ADR-0038). For an import-free program nothing ever
//! pulls `\0rue-std/option.rue` into the compilation, so the module is absent
//! and the demand cannot resolve.
//!
//! Import discovery cannot supply it: it drives parser-owned `ImportDemandRoots`
//! to a fixed point and *closes* before any semantic/body request runs, and
//! trusted-std classification happens only through real import resolution. A
//! foreign demand type threaded through the import frontier was rejected by
//! review.
//!
//! Instead the demand is a *distinct typed* [`TrustedToolchainModuleDemand`],
//! raised by the rooted body-closure attempt (not by any import occurrence) when a
//! reached body needs a trusted module absent from the current revision, and
//! satisfied by the host source-loading layer that owns filesystem access. The
//! demand:
//!
//! * is **not** an `ImportOccurrenceKey` and never enters accepted import
//!   topology or the accepted-read ledger's import plan;
//! * is raised **only** for a body the semantic worklist actually reaches, so an
//!   unreachable helper that mentions a fallible intrinsic forces no std read;
//! * is satisfied by the host, which performs one policy-checked read, classifies
//!   the module trusted through the existing classification, and publishes it on
//!   a strictly-additive successor snapshot via the assembler.
//!
//! A program whose reached bodies use no fallible intrinsic raises **zero**
//! demands and performs **zero** std reads, so unrelated programs never observe
//! the trusted module even when it is malformed on disk.

use std::sync::Arc;

use rue_error::CompileResult;

use crate::retained_charge::RetainedCharge;
use crate::well_known_option::FalliblePayload;
use crate::{ModuleId, StableDefinitionKey};

/// Canonical logical path of the trusted standard-library `Option` module.
///
/// The leading NUL cannot occur in a filesystem path, so this namespace is
/// provably disjoint from every project-relative identity.
pub const OPTION_MODULE_LOGICAL_PATH: &str = "\0rue-std/option.rue";

/// Canonical logical path of the trusted standard-library `StrBuf` module.
///
/// `@read_line`'s result is the exact trusted std `Option(StrBuf)` (spec
/// 4.13:35). Naming that payload requires the trusted `StrBuf` nominal, so a
/// reached `@read_line` in a program that has not otherwise pulled `StrBuf`
/// demands this module in addition to the `Option` module.
pub const STRBUF_MODULE_LOGICAL_PATH: &str = "\0rue-std/strbuf.rue";

/// A typed demand for a trusted toolchain-provided module.
///
/// This is deliberately **not** an `ImportOccurrenceKey`: it carries no importer
/// occurrence, participates in no import candidate ordering, and never enters
/// the `ImportObservationLedger`. It only names the trusted logical module the
/// reached bodies require.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrustedToolchainModuleDemand {
    logical_path: Arc<str>,
}

impl TrustedToolchainModuleDemand {
    /// The demand for the trusted standard-library `Option` module.
    pub fn option() -> Self {
        Self {
            logical_path: Arc::from(OPTION_MODULE_LOGICAL_PATH),
        }
    }

    /// The demand for the trusted standard-library `StrBuf` module, required to
    /// spell `@read_line`'s `Option(StrBuf)` payload.
    pub fn strbuf() -> Self {
        Self {
            logical_path: Arc::from(STRBUF_MODULE_LOGICAL_PATH),
        }
    }

    /// The trusted logical module path this demand names.
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    /// The trusted `ModuleId` this demand resolves to once the host satisfies it.
    ///
    /// The returned identity carries the standard-library origin, so a snapshot
    /// that contains it reports the module as trusted.
    pub fn trusted_module_id(&self) -> CompileResult<ModuleId> {
        ModuleId::from_trusted_standard_library_path(self.logical_path.as_ref())
    }

    /// The path fragment relative to the standard-library root (drops the
    /// `\0rue-std/` namespace prefix), used by the host to resolve the module
    /// against the toolchain's std path.
    pub fn std_relative_path(&self) -> &str {
        self.logical_path
            .strip_prefix("\0rue-std/")
            .unwrap_or(&self.logical_path)
    }
}

/// The trusted-toolchain-module demand projected from ONE reached body's exact
/// raw declaration body (RUE-1112).
///
/// This is the deterministic output of the registered `body-toolchain-demands`
/// query node: it names, sorted and deduplicated, the trusted modules the body's
/// fallible intrinsics require, together with the stable key of the demanding
/// body (its requester anchor). It is **pure**: it derives solely from the raw
/// body text, performs no filesystem I/O, and does no presence check. The rooted
/// semantic attempt separately checks these names against the satisfied
/// trusted-module catalogue and parks the absent ones before entering the body
/// transaction, so speculative evaluation of this projection is always safe.
///
/// This is internal registered-query payload — it never crosses the crate
/// boundary. Only the host-boundary park/continuation types are public; the
/// loader consumes those, not this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyToolchainDemand {
    modules: Arc<[TrustedToolchainModuleDemand]>,
    payload_kinds: Arc<[FalliblePayload]>,
    requester: Option<StableDefinitionKey>,
    raw_body_available: bool,
}

impl BodyToolchainDemand {
    /// Build a demand projection for `requester` from the body's fallible-intrinsic
    /// payload kinds, deriving the trusted-module demands and carrying the payload
    /// kinds themselves so the body transaction observes ONE canonical scan rather
    /// than rescanning the raw text (RUE-1112 C1). `raw_body_available` records
    /// whether that scan had an available raw body, allowing the projection to own
    /// the availability edge as well. Sorts/deduplicates both sets so the node's
    /// output is canonical. `requester` is `None` only for a reached instance with
    /// no source declaration key, which always projects the empty demand set;
    /// whenever a module is demanded the requester anchor is present.
    pub(crate) fn from_payload_kinds(
        payload_kinds: impl IntoIterator<Item = FalliblePayload>,
        requester: Option<StableDefinitionKey>,
        raw_body_available: bool,
    ) -> Self {
        let mut payload_kinds: Vec<_> = payload_kinds.into_iter().collect();
        payload_kinds.sort();
        payload_kinds.dedup();
        let mut modules = Vec::new();
        if !payload_kinds.is_empty() {
            modules.push(TrustedToolchainModuleDemand::option());
        }
        if payload_kinds.contains(&FalliblePayload::StrBuf) {
            modules.push(TrustedToolchainModuleDemand::strbuf());
        }
        modules.sort();
        modules.dedup();
        Self {
            modules: Arc::from(modules),
            payload_kinds: Arc::from(payload_kinds),
            requester,
            raw_body_available,
        }
    }

    /// The trusted modules this body demands (sorted, deduplicated).
    pub(crate) fn modules(&self) -> &[TrustedToolchainModuleDemand] {
        &self.modules
    }

    /// The fallible-intrinsic payload kinds this body uses (sorted, deduplicated).
    /// The single canonical per-body scan; the body transaction observes this
    /// instead of rescanning the raw body text.
    pub(crate) fn payload_kinds(&self) -> &[FalliblePayload] {
        &self.payload_kinds
    }

    /// The stable key of the body that demands these modules (its anchor),
    /// present whenever any module is demanded.
    pub(crate) fn requester(&self) -> Option<&StableDefinitionKey> {
        self.requester.as_ref()
    }

    /// Whether the projection observed an available raw declaration body.
    ///
    /// This bit is part of the registered value so consumers can use this
    /// terminal as the one raw-body availability authority without adding a
    /// duplicate direct edge to the raw-body query.
    pub(crate) fn raw_body_available(&self) -> bool {
        self.raw_body_available
    }
}

impl RetainedCharge for BodyToolchainDemand {
    fn retained_charge(&self) -> u64 {
        self.modules
            .retained_charge()
            .saturating_add(
                (self.payload_kinds.len() * std::mem::size_of::<FalliblePayload>()) as u64,
            )
            .saturating_add(self.requester.retained_charge())
    }
}

/// The park raised by the rooted body-closure attempt when a reached body demands a
/// trusted toolchain module absent from the current revision (RUE-1112).
///
/// It carries the sorted, deduplicated absent modules and the stable requester
/// anchors of the bodies that demanded them. It is carried out of band from the
/// rooted attempt to its outer host boundary — the transport-failure pattern
/// (`producer_transport_failure`) — and is never originated inside a body
/// transaction: only the rooted attempt, having checked the satisfied catalogue,
/// records it, and only the host driver acts on it by acquiring the modules and
/// retrying on a successor. Stable no-filesystem APIs convert an unsatisfied
/// park to their own error/absence result at their outer boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedToolchainModules {
    demands: Arc<[TrustedToolchainModuleDemand]>,
    requesters: Arc<[StableDefinitionKey]>,
}

impl ParkedToolchainModules {
    /// Build a park from the absent demands and their requester anchors, sorting
    /// and deduplicating both so the surfaced state is canonical.
    pub fn new(
        demands: impl IntoIterator<Item = TrustedToolchainModuleDemand>,
        requesters: impl IntoIterator<Item = StableDefinitionKey>,
    ) -> Self {
        let mut demands: Vec<_> = demands.into_iter().collect();
        demands.sort();
        demands.dedup();
        let mut requesters: Vec<_> = requesters.into_iter().collect();
        requesters.sort();
        requesters.dedup();
        Self {
            demands: Arc::from(demands),
            requesters: Arc::from(requesters),
        }
    }

    /// The absent trusted modules the reached bodies demand (sorted, deduped).
    pub fn demands(&self) -> &[TrustedToolchainModuleDemand] {
        &self.demands
    }

    /// The stable keys of the bodies that demanded the absent modules.
    pub fn requesters(&self) -> &[StableDefinitionKey] {
        &self.requesters
    }
}
