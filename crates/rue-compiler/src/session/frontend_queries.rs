//! The frontend query database and its parse/merge/RIR family registrations.

use super::*;

/// Canonical frontend runtime state owned by `CompilerSession`.
///
/// Import staging and closure artifacts are thin projections of the revisioned
/// source/import frontier. They retain presentation-facing immutable results,
/// never a second selected-query authority.
#[derive(Debug)]
pub(super) struct FrontendQueryDatabase {
    pub(super) revisioned: crate::revisioned_query_database::RevisionedQueryDatabase,
    pub(super) discovery_attempt: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    pub(super) last_good_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    pub(super) prior_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    pub(super) oracle_import_fault: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    pub(super) direct_import_diagnostic: Option<Arc<FrontendDiagnosticSnapshot>>,
}

impl Default for FrontendQueryDatabase {
    fn default() -> Self {
        Self {
            revisioned: crate::revisioned_query_database::RevisionedQueryDatabase::new(
                RevisionedQueryDatabaseConstructionToken::new(),
            ),
            discovery_attempt: None,
            last_good_discovery: None,
            prior_discovery: None,
            oracle_import_fault: None,
            direct_import_diagnostic: None,
        }
    }
}

impl FrontendQueryDatabase {
    pub(super) fn record_discovery_attempt(
        &mut self,
        artifact: Arc<ImportDiscoveryRevisionArtifact>,
    ) {
        if let Some(previous) = self.discovery_attempt.replace(artifact.clone()) {
            if previous.source_revision() != artifact.source_revision() {
                self.prior_discovery = Some(previous);
            }
        }
        if artifact.status == ImportDiscoveryRevisionStatus::ClosedValid {
            self.last_good_discovery = Some(artifact);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ParseQueryKey {
    /// Ordinary content-addressed parsing: keyed on the exact source content,
    /// table order, and presentation, so any equal re-request reuses the
    /// terminal.
    Ordinary(Box<OrdinaryParseKey>),
    /// A trusted-toolchain successor parse projection (RUE-1112): keyed on the
    /// published predecessor lineage identity plus the exact appended source
    /// segment. Content is pinned by the published revision, so key hashing and
    /// equality never touch a predecessor entry.
    Successor {
        revision: crate::ImportInputRevision,
        segment: Arc<[crate::ModuleRevision]>,
        /// The exact retained predecessor parse terminal this successor
        /// extends, verified by structural source ancestry at preparation.
        predecessor: rue_query::Revision,
    },
}

impl ParseQueryKey {
    /// The exact source identity an Ordinary key pins (and whose stamp it
    /// retains); a Successor key pins its sources through the published
    /// lineage identity instead and allocates no stamp.
    pub(crate) fn pinned_source(&self) -> Option<&ExactSourceInput> {
        match self {
            Self::Ordinary(key) => Some(&key.source),
            Self::Successor { .. } => None,
        }
    }

    /// Compact display identity for engine diagnostics (RUE-1142): cycle,
    /// wait-graph, and contention reports name the query through this string.
    /// Display only — exact key equality owns memo identity, so a collision
    /// here is harmless — and deliberately bounded: it must not render source
    /// content, which the ordinary key embeds in full.
    pub(crate) fn compatibility_identity(&self) -> String {
        match self {
            Self::Ordinary(key) => {
                let provenance = match &key.presentation {
                    DiagnosticAttemptProvenance::Canonical => "canonical".to_owned(),
                    DiagnosticAttemptProvenance::Presentation(order) => {
                        format!("presentation[{}]", order.iter().count())
                    }
                };
                format!(
                    "parse:ordinary:{}[{} modules]:{provenance}",
                    key.source.revision.root(),
                    key.source.revision.modules().len(),
                )
            }
            Self::Successor {
                revision,
                segment,
                predecessor,
            } => format!(
                "parse:successor:r{}+{}<-{:?}",
                revision.revision_id,
                segment.len(),
                predecessor,
            ),
        }
    }
}

/// The reconciled inputs of one successor parse extension (RUE-1112), prepared
/// without side effects so both the staging and adoption paths verify the
/// predecessor binding before starting a metrics attempt. The caller has
/// already proven the exact compiler-owned input transition; this routine
/// verifies the retained parse and its appended suffix without rescanning
/// the accumulated source prefix.
pub(super) struct PreparedSuccessorParse {
    pub(super) predecessor_program: Arc<ParsedProgram>,
    pub(super) predecessor_order: crate::shared_segments::SharedList<crate::ModuleId>,
    /// The retained predecessor parse terminal's exact runtime identity; the
    /// successor key embeds it, so the successor terminal is bound to THIS
    /// predecessor artifact, never an ambient "latest".
    pub(super) predecessor_revision: rue_query::Revision,
    /// The predecessor parse terminal ITSELF, minted into the exact-terminal
    /// adoption capability by the parse family's content-addressed
    /// registration. The successor computation records it as a runtime
    /// dependency, so the graph carries a real successor-after-predecessor
    /// edge with the captured terminal's exact node, incarnation, and stamp.
    pub(super) predecessor_terminal: rue_query::AdoptableTerminal<ParseQueryRecord>,
    pub(super) appended: Vec<(crate::ModuleId, crate::FileId)>,
    /// The exact source segment appended by this parse stage. The opaque
    /// successor capability carries the cumulative additions since the
    /// committed close, but parse extends the retained predecessor by only this
    /// suffix, so its key carries only these module revisions.
    pub(super) segment: Arc<[crate::ModuleRevision]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct OrdinaryParseKey {
    pub(super) source: ExactSourceInput,
    /// Caller-owned source table order. This is presentation state rather than
    /// module identity, but a selected parse record retains the exact snapshot
    /// and diagnostic source table, so order changes must reproject the outer
    /// record while granular module terminals remain reusable.
    pub(super) file_order: Arc<[crate::FileId]>,
    pub(super) presentation: DiagnosticAttemptProvenance,
}

#[derive(Debug, Clone)]
pub(crate) struct ParseQueryRecord {
    pub(super) key: ParseQueryKey,
    pub(super) runtime_revision: rue_query::Revision,
    pub(super) snapshot: SourceSnapshot,
    pub(super) result: Result<Arc<ParsedProgram>, CompileErrors>,
    pub(super) diagnostics: Arc<FrontendDiagnosticSnapshot>,
    pub(super) work: ParsedModulesWork,
    pub(super) invalidation: ParseInvalidationSummary,
}

impl RetainedCharge for ExactSourceInput {
    fn retained_charge(&self) -> u64 {
        self.revision
            .retained_charge()
            .saturating_add(self.metadata.retained_charge())
    }
}

impl RetainedCharge for OrdinaryParseKey {
    fn retained_charge(&self) -> u64 {
        self.source
            .retained_charge()
            .saturating_add(self.file_order.retained_charge())
            .saturating_add(self.presentation.retained_charge())
    }
}

impl RetainedCharge for ParseQueryKey {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Ordinary(key) => key.retained_charge(),
            Self::Successor { segment, .. } => segment.retained_charge(),
        }
    }
}

impl RetainedCharge for ParseQueryRecord {
    fn retained_charge(&self) -> u64 {
        self.key
            .retained_charge()
            .saturating_add(self.snapshot.retained_charge())
            .saturating_add(self.result.retained_charge())
            .saturating_add(self.diagnostics.retained_charge())
            .saturating_add(self.invalidation.retained_charge())
    }
}

impl ParseQueryRecord {
    pub(crate) fn runtime_revision(&self) -> rue_query::Revision {
        self.runtime_revision
    }
}

#[derive(Debug)]
pub(crate) struct ParseQuery;

impl TypedQueryFamily for ParseQuery {
    type Key = ParseQueryKey;
    type Record = ParseQueryRecord;
    const MAX_TERMINALS: usize = QUERY_TERMINAL_RETENTION_LIMIT;

    fn key(record: &Self::Record) -> &Self::Key {
        &record.key
    }

    fn terminal_kind(record: &Self::Record) -> TerminalKind {
        if record.result.is_ok() {
            TerminalKind::Success
        } else {
            TerminalKind::Failure
        }
    }

    fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool {
        match (&left.result, &right.result) {
            // The complete key contains exact source bytes, metadata, and
            // presentation provenance. Parsing is deterministic, so equal
            // keys prove equal typed syntax even across distinct allocations.
            (Ok(left), Ok(right)) => left.source_revision() == right.source_revision(),
            (Err(left), Err(right)) => compile_errors_equal(left, right),
            _ => false,
        }
    }

    fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool {
        diagnostic_batches_equal(&left.diagnostics, &right.diagnostics)
    }

    fn diagnostics(record: &Self::Record) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        Some(&record.diagnostics)
    }

    fn record_is_consistent(record: &Self::Record) -> bool {
        match &record.key {
            ParseQueryKey::Ordinary(key) => {
                record.snapshot.source_revision() == &key.source.revision
                    && record.snapshot.metadata() == &key.source.metadata
                    && record
                        .snapshot
                        .files()
                        .map(|source| source.file_id)
                        .eq(key.file_order.iter().copied())
                    && match &record.result {
                        Ok(program) => program.source_revision() == &key.source.revision,
                        Err(_) => true,
                    }
                    && record.diagnostics.source_revision() == &key.source.revision
                    && record.diagnostics.identity() == &FrontendDiagnosticIdentity::Syntax
                    && record.diagnostics.provenance == key.presentation
            }
            ParseQueryKey::Successor { segment, .. } => {
                // Content identity is pinned by the published lineage in the
                // key; consistency stays O(segment) — a predecessor entry is
                // never re-enumerated here.
                record.snapshot.len() >= segment.len()
                    && match &record.result {
                        Ok(program) => program.modules_len() == record.snapshot.len(),
                        Err(_) => true,
                    }
                    && record.diagnostics.identity() == &FrontendDiagnosticIdentity::Syntax
            }
        }
    }
}

/// Metric-family markers for canonical projections; they do not own terminals.
#[derive(Debug)]
pub(super) struct ImportDiagnosticQuery;
#[derive(Debug)]
pub(super) struct MergeQuery;
#[derive(Debug)]
pub(super) struct RirQuery;

macro_rules! session_query_metrics_family {
    ($query:ty, $name:literal, $field:ident) => {
        impl SessionQueryMetricsFamily for $query {
            const NAME: &'static str = $name;

            fn projection(work: &mut CompilerSessionWork) -> &mut FrontendQueryWork {
                &mut work.$field
            }
        }
    };
}

session_query_metrics_family!(
    ImportDiagnosticQuery,
    "import-diagnostics",
    import_diagnostics
);
session_query_metrics_family!(MergeQuery, "merge", merge);
session_query_metrics_family!(RirQuery, "rir", rir);

/// Explicit compiler inputs read by a terminal attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExactSourceInput {
    pub(super) revision: SourceRevision,
    pub(super) metadata: crate::SourceMetadata,
}

pub(super) fn compile_errors_equal(left: &CompileErrors, right: &CompileErrors) -> bool {
    left.iter().eq(right.iter())
}

pub(super) fn diagnostic_batches_equal(
    left: &FrontendDiagnosticSnapshot,
    right: &FrontendDiagnosticSnapshot,
) -> bool {
    left.stage == right.stage
        && left.provenance == right.provenance
        && left.errors == right.errors
        && left.warnings == right.warnings
}

impl ExactSourceInput {
    pub(crate) fn new(snapshot: &SourceSnapshot) -> Self {
        Self {
            revision: snapshot.source_revision().clone(),
            metadata: snapshot.metadata().clone(),
        }
    }
}
