//! Closed-discovery continuation tokens and trusted-successor staging state.

use super::*;

/// An opaque, single-use continuation issued ONLY from a successful close of
/// import discovery (RUE-1112).
///
/// It authorizes exactly one strictly-additive trusted-toolchain successor on
/// the closed revision, in the same request generation. It is bound to the
/// issuing session (`session`) and to the outstanding close (`nonce` +
/// `revision`); a token from a different session, a stale token (superseded by a
/// newer close or a new request), or a reused token (after a successful publish)
/// is rejected. The fields are reachable only inside `crate::session`, which
/// issues and redeems the token: the host holds it and hands it back, never
/// inspecting or constructing one.
#[derive(Debug, Clone)]
pub struct ClosedDiscoveryContinuation {
    pub(super) session: Arc<()>,
    pub(super) nonce: u64,
    pub(super) revision: crate::ImportInputRevision,
}

/// Opaque, compiler-derived authority for the modules a trusted-toolchain
/// successor may stage, project, reduce, and commit (RUE-1112).
///
/// It is minted ONLY by [`CompilerSession::publish_trusted_toolchain_successor`]
/// from the verified `added == demanded` set — never from host input — and is
/// bound to the issuing session and successor revision. Its fields are private,
/// so the host cannot construct, inspect, or edit the module set: it carries the
/// value opaquely between the successor stage and close. The successor stage and
/// close derive the exact module delta from the committed predecessor and the
/// current snapshot and verify the carried `appended` roots are present, so a
/// caller can neither omit an authorized module (committing a graph that lacks
/// imports for modules actually in the snapshot) nor admit an unauthorized one.
#[derive(Debug, Clone)]
pub struct TrustedSuccessorDelta {
    pub(super) session: Arc<()>,
    pub(super) nonce: u64,
    pub(super) revision: crate::ImportInputRevision,
    pub(super) appended: Arc<[crate::ModuleId]>,
}

impl TrustedSuccessorDelta {
    /// The successor input revision this delta was minted on. Exposing the
    /// revision does not expose the authorized module set; the host needs it only
    /// to continue discovery in the same request generation.
    pub fn revision(&self) -> crate::ImportInputRevision {
        self.revision
    }
}

/// Session-held authority backing an outstanding [`ClosedDiscoveryContinuation`].
/// Retains the predecessor snapshot, context, accepted-read provenance, and the
/// carried ledger so `publish_trusted_toolchain_successor` can verify a strictly
/// additive successor entirely from records, without any filesystem access.
///
/// A close alone leaves the state NON-AUTHORIZING (`attached_demands` is `None`):
/// no token can be minted and no successor authorized. Authority is granted
/// only when a rooted body-closure park atomically attaches that park's exact sorted
/// missing-demand set to this same state. Demand authority therefore lives here,
/// bound to one closed revision and one park — never in an ambient session field
/// a later, non-parking close could inherit.
/// The CURRENT compiler-published view state a verified successor stage/close
/// consumes, with the derived module delta (RUE-1112). Everything here comes
/// from the published lineage; none of it is host-suppliable.
pub(super) struct SuccessorState {
    pub(super) snapshot: SourceSnapshot,
    pub(super) context: crate::ImportDiscoveryContext,
    pub(super) accepted_reads: crate::AcceptedReadManifest,
    pub(super) ledger: crate::ImportObservationLedger,
    /// The published lineage identity this state was read from.
    pub(super) revision: crate::ImportInputRevision,
    /// The appended module revisions (view sources minus the committed
    /// predecessor), in canonical module order.
    pub(super) delta: Arc<[crate::ModuleRevision]>,
}

/// One compiler-proven incremental staging step. The host cannot construct this
/// value: ordinary batches derive it from the immutable input view's private
/// parent/delta transition, while trusted successors derive it from their
/// existing capability protocol.
pub(super) struct IncrementalImportStage {
    pub(super) revision: crate::ImportInputRevision,
    pub(super) delta: Arc<[crate::ModuleRevision]>,
    pub(super) predecessor_plan: crate::ImportDiscoveryPlan,
    pub(super) predecessor_parse: Arc<rue_query::QueryTerminal<ParseQueryRecord>>,
    pub(super) inherited_parse_work: ParsedModulesWork,
}

#[derive(Debug, Clone)]
pub(super) struct ContinuationState {
    pub(super) nonce: u64,
    pub(super) revision: crate::ImportInputRevision,
    pub(super) snapshot: SourceSnapshot,
    pub(super) accepted_reads: crate::AcceptedReadManifest,
    pub(super) ledger: crate::ImportObservationLedger,
    /// The exact sorted missing-demand set the rooted park attached, or `None`
    /// while the closed state is non-authorizing (no park has arrived for it).
    pub(super) attached_demands: Option<Arc<[crate::TrustedToolchainModuleDemand]>>,
}

/// Convert an unsatisfied trusted-toolchain park to the error a stable
/// no-filesystem semantic entry returns at its outer boundary (RUE-1112).
///
/// This is a deterministic contract failure, never an ICE: the source is
/// otherwise valid, but a guaranteed toolchain input the reached bodies demand
/// was not supplied. The park-aware host driver acquires and retries; a stable
/// embedder that omits the input gets this distinguishable classification.
pub(crate) fn unresolved_toolchain_park_errors(
    park: &crate::ParkedToolchainModules,
) -> CompileErrors {
    let modules = park
        .demands()
        .iter()
        .map(|demand| demand.logical_path().to_owned())
        .collect::<Vec<_>>()
        .join(", ");
    CompileErrors::from(crate::CompileError::without_span(
        rue_error::ErrorKind::UnsatisfiedTrustedToolchainInput(format!(
            "reached bodies demand trusted standard-library module(s) [{modules}] that are not present in this compilation; supply them (a std root the host can acquire from) before semantic analysis"
        )),
    ))
}
