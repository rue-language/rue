//! RUE-1091 rFinal whole-body differential harness.
//!
//! Side A is the production `BodyTransaction` path. Side B is the test-only
//! provider-driven analyzer: for every body in the shape corpus it starts from
//! the requested RIR body and discovers its own demand through
//! `analyze_provider_body`, materializing each discovered fact through the five
//! real provider drivers
//! (`ProviderTypeFacts`, `SignatureFacts` via the comptime-call arms,
//! `ProviderCallFacts`, `ProviderEndpointFacts`, `ProviderAggregateFacts`)
//! plus the body-local `BodySemanticOverlay`, and records exactly the query
//! edges those drivers observe.
//!
//! Side B consumes NO epoch table: its fact sources are the durable
//! declaration set (`bind_canonical_declaration_semantics` — the r4b-3
//! sanctioned fill source), the shared whole-program RIR (a body-query input),
//! the `CompilerBodyFactProvider` boundary, and the drivers' own pools and
//! overlays. The production side's published `BodyTransaction` is compared
//! only after side B finishes: it is never passed into the analyzer or used to
//! choose a provider query.
//!
//! Divergences are never silently tolerated: every fact side B cannot yet
//! supply lands in the named [`SIDE_B_FACT_EXCEPTIONS`] table, and every
//! edge-family difference between the two sides lands in the named
//! [`EDGE_FAMILY_EXCEPTIONS`] table (r4a-1: provider edges are the post-flip
//! truth — the finer materialization edges are the more-correct behavior).
//! No production path selects between analyzers; everything here is test-only.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use super::*;
use crate::body_query::BodyReference;
use crate::declaration_candidate::{
    DeclarationCandidateCategory, DeclarationCandidateKey, DeclarationCandidateOwner,
};
use crate::revisioned_query_database::test_support::{
    DurableDeclSource, endpoint_display, endpoint_nominal_render,
};
use crate::revisioned_query_database::{
    CompilerBodyFactProvider, ProviderTypeFacts, ReceiverTypeIdentity, RevisionedQueryDatabase,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DifferentialSide {
    Production,
    /// The provider-driven side, filled by the r2-r6 drivers (RUE-1091 rFinal).
    ProviderFactsOverlay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HarnessError {
    Compile(String),
    BodyNotFound(String),
}

struct ProductionAnalyzer;

/// Side B: provider-driven body analysis over the five drivers + overlay.
struct ProviderFactsOverlayAnalyzer;

struct DirectProductionInputs {
    merged: crate::CanonicalMergedProgram,
    rir: crate::CanonicalRirOutput,
    options: CompileOptions,
    imports: crate::CanonicalImportGraph,
    query_shells: Vec<rue_air::SemanticDeclarationShell>,
    query_declarations: Arc<[crate::durable_semantics::DurableDeclarationSemantic]>,
}

impl DirectProductionInputs {
    fn build(source: &str) -> Result<Self, HarnessError> {
        let snapshot = SourceSnapshot::single("main.rue", source)
            .map_err(|error| HarnessError::Compile(error.to_string()))?;
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
            .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let merged = crate::merge_parsed_modules(&parsed)
            .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let rir = crate::lower_canonical_rir(&merged)
            .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let options = CompileOptions::default();
        let imports = crate::bound_definitions::test_fixture_import_graph(&merged)
            .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let (_, query_declarations, _) =
            crate::bound_definitions::bind_canonical_declaration_semantics(
                &merged,
                &rir,
                options.preview_features.clone(),
                options.target,
            )
            .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let query_shells = crate::canonical_semantic::query_owned_declaration_shells_for_test(
            &merged,
            &rir,
            options.preview_features.clone(),
            options.target,
            &imports,
        )
        .map_err(|errors| HarnessError::Compile(errors.to_string()))?
        .declaration_shells()
        .cloned()
        .collect();
        Ok(Self {
            merged,
            rir,
            options,
            imports,
            query_shells,
            query_declarations,
        })
    }

    fn analyze(
        &self,
        key: &crate::body_query::BodyQueryKey,
    ) -> Result<
        (
            crate::body_query::BodyTransaction,
            rue_air::PerBodyDeclarationContextWork,
        ),
        HarnessError,
    > {
        let mut work = rue_air::PerBodyDeclarationContextWork::default();
        let transaction = crate::canonical_semantic::analyze_body_query(
            &self.merged,
            &self.rir,
            &self.options,
            &self.imports,
            &self.query_shells,
            &self.query_declarations,
            &[],
            &crate::body_query::WellKnownOptionResolution::default(),
            key,
            &rue_query::CancellationToken::new(),
            &mut work,
        )
        .map_err(|abort| HarnessError::Compile(format!("{abort:?}")))?;
        Ok((transaction, work))
    }
}

#[derive(Debug, Clone)]
struct HarnessCase {
    name: &'static str,
    source: &'static str,
    bodies: &'static [&'static str],
    expected_failure: bool,
    /// Names the failing body consulted and found absent — the demand data the
    /// side-B replay drives through the lookup boundary for failure cases.
    absent_lookups: &'static [&'static str],
}

const SHAPE_CORPUS: &[HarnessCase] = &[
    HarnessCase {
        name: "plain_functions",
        source: "fn helper() -> i32 { 40 } fn main() -> i32 { helper() + 2 }",
        bodies: &["helper", "main"],
        expected_failure: false,
        absent_lookups: &[],
    },
    HarnessCase {
        name: "three_body_permutation",
        source: "fn first() -> i32 { 11 } fn second() -> i32 { 22 } \
                 fn main() -> i32 { first() + second() }",
        bodies: &["first", "second", "main"],
        expected_failure: false,
        absent_lookups: &[],
    },
    HarnessCase {
        name: "method",
        source: "struct Counter { value: i32, fn get(self) -> i32 { self.value } } \
                 fn main() -> i32 { Counter { value: 3 }.get() }",
        bodies: &["get", "main"],
        expected_failure: false,
        absent_lookups: &[],
    },
    HarnessCase {
        name: "associated_function",
        source: "struct Counter { value: i32, fn make(value: i32) -> Counter { \
                 Counter { value: value } } } \
                 fn main() -> i32 { Counter.make(3).value }",
        bodies: &["make", "main"],
        expected_failure: false,
        absent_lookups: &[],
    },
    HarnessCase {
        name: "named_destructor",
        source: "struct Resource { value: i32 } \
                 drop fn Resource(self) {} \
                 fn main() { let resource = Resource { value: 1 }; }",
        bodies: &["Resource", "main"],
        expected_failure: false,
        absent_lookups: &[],
    },
    HarnessCase {
        name: "specialization",
        source: "fn choose(comptime N: i32) -> i32 { N } \
                 fn main() -> i32 { choose(42) }",
        bodies: &["choose", "main"],
        expected_failure: false,
        absent_lookups: &[],
    },
    HarnessCase {
        name: "anonymous_producer",
        source: "fn Pair() -> type { struct { a: i32, b: i32 } } \
                 fn main() -> i32 { let P = Pair(); let p: P = P { a: 1, b: 2 }; \
                 p.a + p.b }",
        bodies: &["Pair", "main"],
        expected_failure: false,
        absent_lookups: &[],
    },
    HarnessCase {
        name: "well_known_option_shape",
        // The trusted-toolchain Option registry needs a multi-file continuation
        // protocol whose production-path gap stays recorded below. This
        // freestanding structural Option shape still exercises the production
        // anonymous-enum/fallible path and, on side B, the r6b anonymous arm.
        source: "fn Option(comptime T: type) -> type { enum { Some(T), None } } \
                 fn main() { let O = Option(i32); }",
        bodies: &["main"],
        expected_failure: false,
        absent_lookups: &[],
    },
    HarnessCase {
        name: "absent_lookup",
        source: "fn main() -> i32 { missing() }",
        bodies: &["main"],
        expected_failure: true,
        absent_lookups: &["missing"],
    },
    HarnessCase {
        name: "private_lookup",
        // A same-module private item is legal; the cross-module form is closed
        // by `side_b_cross_module_private_lookup_is_driver_supplied`.
        source: "fn hidden() -> i32 { 1 } fn main() -> i32 { hidden() }",
        bodies: &["hidden", "main"],
        expected_failure: false,
        absent_lookups: &[],
    },
    HarnessCase {
        name: "multi_diagnostic_body",
        source: "fn main() -> i32 { missing_one(); missing_two(); true }",
        bodies: &["main"],
        expected_failure: true,
        absent_lookups: &["missing_one", "missing_two"],
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoverageStatus {
    Constructed,
    Gap(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NamedCoverage {
    name: &'static str,
    status: CoverageStatus,
}

// Exhaustive, reviewable inventory of every TypeInstanceKey arm. The three
// NominalInstanceKey subarms are intentionally separate named cases.
const TYPE_INSTANCE_KEY_COVERAGE: &[NamedCoverage] = &[
    NamedCoverage {
        name: "type_i8",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_i16",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_i32",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_i64",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_u8",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_u16",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_u32",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_u64",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_bool",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_unit",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_never",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_comptime_type",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_builtin_nominal",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_nominal_builtin",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_nominal_named",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_nominal_anonymous",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_array",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_slice",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_ptr_const",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_ptr_mut",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_module",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "type_generic_parameter",
        status: CoverageStatus::Constructed,
    },
];

const CANONICAL_ARGUMENT_VALUE_COVERAGE: &[NamedCoverage] = &[
    NamedCoverage {
        name: "argument_integer",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "argument_bool",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "argument_type_module_typed_comptime_value",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "argument_function_function_valued_comptime_arg",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "argument_unit",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "argument_string",
        status: CoverageStatus::Constructed,
    },
];

// Review carry-forwards. An entry flips from `Gap` to `Constructed` exactly
// when a live side-B assertion closes it (the closing test is named in
// `CLOSED_GAP_LIVE_TESTS`); remaining gaps stay recorded — never deleted or
// silently bypassed.
const REVIEW_CARRY_FORWARDS: &[NamedCoverage] = &[
    NamedCoverage {
        name: "trusted_well_known_option_registry",
        status: CoverageStatus::Gap(
            "production-path gap (r6c rider d): the trusted StrBuf/Option module fixture is \
             flip-corpus work; the pool install itself is proven by \
             provider_well_known_option_install_matches_epoch",
        ),
    },
    NamedCoverage {
        name: "ambiguous_body_lookup",
        status: CoverageStatus::Gap(
            "today's duplicate candidate is rejected before a BodyTransaction is published",
        ),
    },
    NamedCoverage {
        name: "cross_module_private_lookup",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "unrelated_same_named_const_preserves_recorded_edges",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "e0481_candidate_hint_records_no_resolution_edges",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "forced_eviction_and_cancellation",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "const_candidate_facts",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "pool_drop_metadata_parity",
        status: CoverageStatus::Constructed,
    },
    NamedCoverage {
        name: "instance_keyed_producer_body_facts",
        status: CoverageStatus::Constructed,
    },
];

/// Closed gap -> the live side-B test that closed it. Executable data so a
/// reviewer can jump from the flipped row to its assertion.
const CLOSED_GAP_LIVE_TESTS: &[(&str, &str)] = &[
    (
        "cross_module_private_lookup",
        "side_b_cross_module_private_lookup_is_driver_supplied",
    ),
    (
        "unrelated_same_named_const_preserves_recorded_edges",
        "side_b_unrelated_same_named_const_preserves_recorded_edges",
    ),
    (
        "e0481_candidate_hint_records_no_resolution_edges",
        "side_b_absent_lookup_records_only_its_keyed_terminals",
    ),
    (
        "forced_eviction_and_cancellation",
        "side_b_forced_eviction_and_cancellation_leave_no_partial_state",
    ),
    (
        "pool_drop_metadata_parity",
        "provider_named_destructor_metadata_matches_live_epoch",
    ),
    (
        "const_candidate_facts",
        "side_b_provider_drivers_supply_previously_stubbed_facts",
    ),
    (
        "instance_keyed_producer_body_facts",
        "side_b_provider_drivers_supply_previously_stubbed_facts",
    ),
];

// ---------------------------------------------------------------------------
// Side-B exception tables (executable data, asserted non-vacuous)
// ---------------------------------------------------------------------------

/// A fact side B cannot yet supply from the drivers, named and justified. The
/// replay records exactly which rows each body hit; the corpus test asserts the
/// hit set equals [`EXPECTED_FACT_EXCEPTION_HITS`] — no silent tolerance, no
/// stale rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SideBFactException {
    name: &'static str,
    reason: &'static str,
}

const SIDE_B_FACT_EXCEPTIONS: &[SideBFactException] = &[SideBFactException {
    name: "drop_glue_overlay_method_installation",
    reason: "drop-glue instance keys belong to the flip's overlay method installation",
}];

/// The materialization families a side-B edge may belong to — "edges at
/// materialization, never at render".
const SIDE_B_MATERIALIZATION_FAMILIES: &[&str] = &[
    "compiler.lookup-name",
    "compiler.lookup-import",
    "compiler.semantic-nucleus",
    // The producer-facts op reconstructs the producer's comptime call from its
    // declaration shell — a declaration-level fact terminal.
    "compiler.declaration-shell",
    "compiler.body-produced-anonymous",
    "compiler.body-toolchain-demands",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeExceptionSide {
    /// The epoch side records the family; side B does not (deleted or
    /// re-pointed at the flip).
    EpochOnly,
    /// Side B records the family; the epoch side does not (r4a-1: the finer
    /// provider materialization edges are the post-flip truth).
    ProviderOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EdgeFamilyException {
    family: &'static str,
    side: EdgeExceptionSide,
    reason: &'static str,
}

const EDGE_FAMILY_EXCEPTIONS: &[EdgeFamilyException] = &[
    EdgeFamilyException {
        family: "compiler.raw-declaration-body",
        side: EdgeExceptionSide::EpochOnly,
        reason: "the body's own raw-source terminal; the flip re-points the compute closure, \
                 keeping this input on the transaction, not on the fact drivers",
    },
    EdgeFamilyException {
        family: "compiler.module-declaration-set",
        side: EdgeExceptionSide::EpochOnly,
        reason: "the conservative whole-program declaration-set edge the flip DELETES (plan \
                 section 5 step 3) — exactly the no-whole-program-context locality delta",
    },
    EdgeFamilyException {
        family: "compiler.lookup-name",
        side: EdgeExceptionSide::ProviderOnly,
        reason: "r4a-1 ruling: keyed name-lookup edges recorded at materialization are the \
                 richer post-flip truth the epoch's table reads mask",
    },
    EdgeFamilyException {
        family: "compiler.declaration-shell",
        side: EdgeExceptionSide::ProviderOnly,
        reason: "the body-local analyzer asks the exact requested producer instance for its \
                 anonymous output; reconstructing that instance is backed by its declaration \
                 shell and does not enumerate a declaration universe",
    },
];

// ---------------------------------------------------------------------------
// Side A (production) capture
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ObservationCounters {
    declaration_context: rue_air::PerBodyDeclarationContextWork,
    // These session-owned metrics are captured once after case setup. Direct
    // production body analysis does not mutate them, so they are deliberately
    // framed as case snapshots rather than per-body work.
    case_provider_snapshot: crate::unstable::ProviderObservationMetrics,
    case_overlay_snapshot: crate::unstable::OverlayMaterializationMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedBody {
    case: &'static str,
    body: String,
    artifact: Vec<u8>,
    diagnostics: Vec<u8>,
    dependency_edges: Vec<u8>,
    observation_counters: Vec<u8>,
}

/// Production peer captured for the post-analysis differential. Only
/// `instance` is an input to side B; references and edge families are read
/// after provider demand discovery completes.
#[derive(Debug, Clone)]
struct ProductionDemandPeer {
    body: String,
    /// The request identity is an input to both analyzers, not discovered
    /// demand. Side B may use it to locate the requested RIR body.
    instance: crate::FunctionInstanceKey,
    references: Vec<BodyReference>,
    failed: bool,
    edge_families: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Divergence {
    case: String,
    body: String,
    field: &'static str,
    byte_offset: usize,
}

impl fmt::Display for Divergence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "body differential diverged: case={}, body={}, field={}, byte offset={}",
            self.case, self.body, self.field, self.byte_offset
        )
    }
}

fn first_byte_offset(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter()
        .zip(right)
        .position(|(left, right)| left != right)
        .or_else(|| (left.len() != right.len()).then_some(left.len().min(right.len())))
}

fn captured_bodies_equal(left: &[CapturedBody], right: &[CapturedBody]) -> Result<(), Divergence> {
    let left_by_body = left
        .iter()
        .map(|body| ((body.case, body.body.as_str()), body))
        .collect::<BTreeMap<_, _>>();
    let right_by_body = right
        .iter()
        .map(|body| ((body.case, body.body.as_str()), body))
        .collect::<BTreeMap<_, _>>();
    let keys = left_by_body
        .keys()
        .chain(right_by_body.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for (case, body) in keys {
        let Some(left) = left_by_body.get(&(case, body)) else {
            return Err(Divergence {
                case: case.to_owned(),
                body: body.to_owned(),
                field: "body_set",
                byte_offset: 0,
            });
        };
        let Some(right) = right_by_body.get(&(case, body)) else {
            return Err(Divergence {
                case: case.to_owned(),
                body: body.to_owned(),
                field: "body_set",
                byte_offset: 0,
            });
        };
        for (field, left, right) in [
            (
                "artifact",
                left.artifact.as_slice(),
                right.artifact.as_slice(),
            ),
            (
                "diagnostics",
                left.diagnostics.as_slice(),
                right.diagnostics.as_slice(),
            ),
            (
                "dependency_edges",
                left.dependency_edges.as_slice(),
                right.dependency_edges.as_slice(),
            ),
            (
                "observation_counters",
                left.observation_counters.as_slice(),
                right.observation_counters.as_slice(),
            ),
        ] {
            if let Some(byte_offset) = first_byte_offset(left, right) {
                return Err(Divergence {
                    case: case.to_owned(),
                    body: body.to_owned(),
                    field,
                    byte_offset,
                });
            }
        }
    }
    Ok(())
}

fn body_key_for_name(
    session: &mut CompilerSession,
    options: &CompileOptions,
    semantic: Option<&crate::CanonicalSemanticOutput>,
    name: &str,
) -> Result<crate::body_query::BodyQueryKey, HarnessError> {
    fn definition_name(instance: &crate::FunctionInstanceKey) -> Option<&str> {
        match instance {
            crate::FunctionInstanceKey::Definition(definition) => Some(definition.name()),
            crate::FunctionInstanceKey::Specialization { base, .. } => definition_name(base),
            crate::FunctionInstanceKey::AnonymousMember { .. }
            | crate::FunctionInstanceKey::DropGlue(_) => None,
        }
    }

    if let Some(instance) = semantic
        .into_iter()
        .flat_map(|semantic| semantic.functions())
        .map(|function| &function.semantic_identity)
        .find(|instance| {
            matches!(instance, crate::FunctionInstanceKey::Specialization { .. })
                && definition_name(instance) == Some(name)
        })
    {
        return Ok(crate::body_query::BodyQueryKey {
            instance: instance.clone(),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: options.target,
                preview_features: StablePreviewFeatures::new(&options.preview_features),
            },
        });
    }

    let definitions = session
        .stable_definitions(options)
        .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
    let definition = definitions
        .definitions()
        .iter()
        .find(|record| record.stable_key().kind().owns_body() && record.stable_key().name() == name)
        .map(|record| record.stable_key().clone())
        .ok_or_else(|| HarnessError::BodyNotFound(name.to_owned()))?;
    Ok(crate::body_query::BodyQueryKey {
        instance: crate::FunctionInstanceKey::Definition(definition),
        configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: options.target,
            preview_features: StablePreviewFeatures::new(&options.preview_features),
        },
    })
}

fn capture_terminal(
    session: &CompilerSession,
    key: &crate::body_query::BodyQueryKey,
    case: &'static str,
    body_name: &str,
    counters: ObservationCounters,
    directly_analyzed: &crate::body_query::BodyTransaction,
) -> Result<(CapturedBody, BTreeSet<String>), HarnessError> {
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .ok_or_else(|| HarnessError::BodyNotFound(body_name.to_owned()))?;
    let terminal = session
        .queries
        .revisioned
        .body_transaction(
            revision,
            key.clone(),
            Arc::new(BTreeMap::new()),
            Arc::from([]),
            false,
            Arc::from([]),
            rue_query::CancellationToken::new(),
            |_, _| {
                panic!(
                    "production semantic request must retain the body terminal for {case}::{body_name}"
                )
            },
        )
        .map_err(|failure| HarnessError::Compile(format!("{failure:?}")))?;
    let rue_query::QueryOutcome::Success(transaction) = terminal.outcome() else {
        unreachable!("BodyTransaction publishes typed values")
    };
    if !crate::body_query::transaction_equal(transaction, directly_analyzed) {
        return Err(HarnessError::Compile(format!(
            "direct production analysis diverged from retained terminal for {case}::{body_name}"
        )));
    }
    let (artifact, diagnostics) = match directly_analyzed {
        crate::body_query::BodyTransaction::Success {
            body,
            references,
            produced_anonymous_nominals,
        } => (
            format!("{body:?}\n{references:?}\n{produced_anonymous_nominals:?}").into_bytes(),
            Vec::new(),
        ),
        crate::body_query::BodyTransaction::DeterministicFailure { errors, references } => (
            format!("{references:?}").into_bytes(),
            // CompileErrors::to_string() is the production ordered rendered
            // diagnostic stream; do not sort it.
            errors.to_string().into_bytes(),
        ),
    };
    // Dependency edges are a semantic set. Query observation order is not part
    // of the body transaction contract, so canonicalize it and avoid importing
    // HashMap traversal order into the harness.
    let dependency_edges = terminal
        .dependencies()
        .iter()
        .map(|dependency| format!("{:?}", dependency.node))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes();
    let edge_families = terminal
        .dependencies()
        .iter()
        .map(|dependency| dependency.node.family().to_owned())
        .collect::<BTreeSet<_>>();
    Ok((
        CapturedBody {
            case,
            body: body_name.to_owned(),
            artifact,
            diagnostics,
            dependency_edges,
            observation_counters: format!("{counters:?}").into_bytes(),
        },
        edge_families,
    ))
}

impl ProductionAnalyzer {
    fn side(&self) -> DifferentialSide {
        DifferentialSide::Production
    }

    fn capture(
        &self,
        case: &HarnessCase,
        requested_body_order: &[&str],
    ) -> Result<Vec<CapturedBody>, HarnessError> {
        Ok(self.capture_with_peer(case, requested_body_order)?.0)
    }

    /// Capture production for the post-analysis differential. Side B receives
    /// only the same requested body identity, never production's discovered
    /// references.
    fn capture_with_peer(
        &self,
        case: &HarnessCase,
        requested_body_order: &[&str],
    ) -> Result<(Vec<CapturedBody>, Vec<ProductionDemandPeer>), HarnessError> {
        let source = SourceSnapshot::single("main.rue", case.source)
            .map_err(|error| HarnessError::Compile(error.to_string()))?;
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        let mut failure_keys = BTreeMap::new();
        if case.expected_failure {
            let valid = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }")
                .map_err(|error| HarnessError::Compile(error.to_string()))?;
            session
                .update(&valid)
                .into_result()
                .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
            let valid_semantic = session
                .canonical_semantic(&options)
                .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
            for body in case.bodies {
                failure_keys.insert(
                    *body,
                    body_key_for_name(&mut session, &options, Some(valid_semantic.as_ref()), body)?,
                );
            }
        }
        session
            .update(&source)
            .into_result()
            .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let semantic = match session.canonical_semantic(&options) {
            Ok(semantic) if case.expected_failure => {
                return Err(HarnessError::Compile(format!(
                    "{} unexpectedly compiled",
                    case.name
                )));
            }
            Ok(semantic) => Some(semantic),
            Err(_) if case.expected_failure => None,
            Err(errors) => return Err(HarnessError::Compile(errors.to_string())),
        };
        let case_provider_snapshot = crate::unstable::provider_observation_metrics(&session);
        let case_overlay_snapshot = crate::unstable::overlay_materialization_metrics(&session);
        let direct = DirectProductionInputs::build(case.source)?;
        let mut captured = Vec::new();
        let mut peers = Vec::new();
        // This is the actual body-analysis schedule permutation: each call
        // executes production `analyze_body_query` against one shared immutable
        // corpus input in the requested order. Terminal capture below supplies
        // that body's query edges; transaction parity proves both views describe
        // the same production analysis.
        for body in requested_body_order {
            let key = if let Some(key) = failure_keys.get(body) {
                key.clone()
            } else {
                body_key_for_name(&mut session, &options, semantic.as_deref(), body)?
            };
            let (directly_analyzed, declaration_context) = direct.analyze(&key)?;
            let counters = ObservationCounters {
                declaration_context,
                case_provider_snapshot,
                case_overlay_snapshot,
            };
            let (capture, edge_families) = capture_terminal(
                &session,
                &key,
                case.name,
                body,
                counters,
                &directly_analyzed,
            )?;
            let (references, failed) = match &directly_analyzed {
                crate::body_query::BodyTransaction::Success { references, .. } => {
                    (references.0.to_vec(), false)
                }
                crate::body_query::BodyTransaction::DeterministicFailure { references, .. } => {
                    (references.0.to_vec(), true)
                }
            };
            peers.push(ProductionDemandPeer {
                body: (*body).to_owned(),
                instance: key.instance.clone(),
                references,
                failed,
                edge_families,
            });
            captured.push(capture);
        }
        Ok((captured, peers))
    }
}

// ---------------------------------------------------------------------------
// Side B (provider-driven body analyzer)
// ---------------------------------------------------------------------------

/// One replayed body on side B: the canonical driver-fact lines, the named
/// exception rows the body hit, and the exact recorded provider edges.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SideBBody {
    case: &'static str,
    body: String,
    facts: Vec<String>,
    exceptions: BTreeSet<&'static str>,
    edge_families: BTreeSet<String>,
    edge_identities: BTreeSet<String>,
    discovered_references: BTreeSet<BodyReference>,
}

fn reference_definition_roots(reference: &BodyReference) -> BTreeSet<crate::StableDefinitionKey> {
    fn function(
        instance: &crate::FunctionInstanceKey,
        roots: &mut BTreeSet<crate::StableDefinitionKey>,
    ) {
        match instance {
            crate::FunctionInstanceKey::Definition(key) => {
                roots.insert(key.clone());
            }
            crate::FunctionInstanceKey::Specialization { base, .. } => function(base, roots),
            crate::FunctionInstanceKey::AnonymousMember { owner, .. }
            | crate::FunctionInstanceKey::DropGlue(owner) => ty(owner, roots),
        }
    }
    fn producer(
        producer: &rue_air::StableProducerId<crate::StableDefinitionKey, ModuleId>,
        roots: &mut BTreeSet<crate::StableDefinitionKey>,
    ) {
        match producer {
            rue_air::StableProducerId::Definition(key) => {
                roots.insert(key.clone());
            }
            rue_air::StableProducerId::Function(instance) => function(instance, roots),
        }
    }
    fn ty(value: &crate::TypeInstanceKey, roots: &mut BTreeSet<crate::StableDefinitionKey>) {
        use rue_air::{NominalInstanceKey as N, TypeInstanceKey as T};
        match value {
            T::Nominal(N::Named(key)) => {
                roots.insert(key.clone());
            }
            T::Nominal(N::Anonymous(identity)) => {
                producer(&identity.producer, roots);
                for argument_type in identity.arguments.types.iter() {
                    ty(argument_type, roots);
                }
            }
            T::Array { element, .. }
            | T::Slice { element, .. }
            | T::PtrConst(element)
            | T::PtrMut(element) => ty(element, roots),
            _ => {}
        }
    }

    let mut roots = BTreeSet::new();
    match reference {
        BodyReference::Callable(instance) => function(instance, &mut roots),
        BodyReference::Definition(key) => {
            roots.insert(key.clone());
        }
        BodyReference::Type(value) => ty(value, &mut roots),
    }
    roots
}

/// Side-B durable inputs for one case: the parsed/merged program, the shared
/// whole-program RIR (body-query inputs), and the durable declaration set (the
/// r4b-3 sanctioned fill source). No bound epoch is constructed here.
struct SideBCase {
    snapshot: SourceSnapshot,
    rir: crate::CanonicalRirOutput,
    decls: Arc<[crate::durable_semantics::DurableDeclarationSemantic]>,
    module_files: BTreeMap<ModuleId, FileId>,
}

impl SideBCase {
    fn build_single(source: &str) -> Result<Self, HarnessError> {
        let snapshot = SourceSnapshot::single("main.rue", source)
            .map_err(|error| HarnessError::Compile(error.to_string()))?;
        Self::build_from_snapshot(snapshot)
    }

    fn build_from_snapshot(snapshot: SourceSnapshot) -> Result<Self, HarnessError> {
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(&snapshot)
            .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let merged = crate::merge_parsed_modules(&parsed)
            .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let rir = crate::lower_canonical_rir(&merged)
            .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let options = CompileOptions::default();
        let (_, decls, _) = crate::bound_definitions::bind_canonical_declaration_semantics(
            &merged,
            &rir,
            options.preview_features.clone(),
            options.target,
        )
        .map_err(|errors| HarnessError::Compile(errors.to_string()))?;
        let module_files = merged
            .ast()
            .modules()
            .iter()
            .map(|module| (module.module_id().clone(), module.file_id()))
            .collect();
        Ok(Self {
            snapshot,
            rir,
            decls,
            module_files,
        })
    }

    fn revision(&self, database: &mut RevisionedQueryDatabase) -> rue_query::Revision {
        database.source_revision(&ExactSourceInput::new(&self.snapshot), &self.snapshot)
    }
}

fn side_b_configuration() -> crate::semantic_query_nucleus::SemanticQueryConfiguration {
    let options = CompileOptions::default();
    crate::semantic_query_nucleus::SemanticQueryConfiguration {
        target: options.target,
        preview_features: StablePreviewFeatures::new(&options.preview_features),
    }
}

fn candidate_category(kind: crate::StableDefinitionKind) -> Option<DeclarationCandidateCategory> {
    use crate::StableDefinitionKind as K;
    use DeclarationCandidateCategory as Cat;
    Some(match kind {
        K::Function => Cat::Function,
        K::Struct => Cat::Struct,
        K::Enum => Cat::Enum,
        K::ValueConst | K::ModuleBinding => Cat::ConstCandidate,
        K::Destructor => Cat::Destructor,
        K::Method => Cat::Method,
        K::AssociatedFunction => Cat::AssociatedFunction,
    })
}

fn candidate_for(key: &crate::StableDefinitionKey) -> Option<DeclarationCandidateKey> {
    let category = candidate_category(key.kind())?;
    let owner = key.owner().map(|owner| DeclarationCandidateOwner {
        category: candidate_category(owner.kind()).expect("owner kinds are nominal"),
        name: Arc::from(owner.name()),
    });
    Some(DeclarationCandidateKey {
        module: key.module().clone(),
        category,
        name: Arc::from(key.name()),
        owner,
        duplicate_discriminator: 0,
    })
}

/// The type-syntax spelling of a durable primitive, or `None` for non-primitive
/// arms (which route through the endpoint driver instead).
fn primitive_syntax(key: &crate::TypeInstanceKey) -> Option<&'static str> {
    use rue_air::TypeInstanceKey as T;
    Some(match key {
        T::I8 => "i8",
        T::I16 => "i16",
        T::I32 => "i32",
        T::I64 => "i64",
        T::U8 => "u8",
        T::U16 => "u16",
        T::U32 => "u32",
        T::U64 => "u64",
        T::Bool => "bool",
        T::Unit => "()",
        T::Never => "!",
        T::ComptimeType => "type",
        _ => return None,
    })
}

/// Convert a durable type-instance key to the durable import-type algebra the
/// pool's slice registration consumes. `None` for arms with no durable import
/// form.
fn durable_import_type(key: &crate::TypeInstanceKey) -> Option<crate::DurableType> {
    use crate::DurableType as D;
    use rue_air::NominalInstanceKey as N;
    use rue_air::TypeInstanceKey as T;
    Some(match key {
        T::I8 => D::I8,
        T::I16 => D::I16,
        T::I32 => D::I32,
        T::I64 => D::I64,
        T::U8 => D::U8,
        T::U16 => D::U16,
        T::U32 => D::U32,
        T::U64 => D::U64,
        T::Bool => D::Bool,
        T::Unit => D::Unit,
        T::Never => D::Never,
        T::Nominal(N::Named(named)) => D::Nominal(named.clone()),
        T::BuiltinNominal { kind, name } | T::Nominal(N::Builtin { kind, name }) => {
            D::BuiltinNominal {
                kind: match kind {
                    rue_air::AnonymousNominalKind::Struct => {
                        rue_air::SemanticImportNominalKind::Struct
                    }
                    rue_air::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
                },
                name: name.clone(),
            }
        }
        T::Array { element, len } => D::Array {
            element: Box::new(durable_import_type(element)?),
            len: *len,
        },
        T::Slice { element, name } => D::Slice {
            element: Box::new(durable_import_type(element)?),
            name: name.clone(),
        },
        T::PtrConst(pointee) => D::PtrConst(Box::new(durable_import_type(pointee)?)),
        T::PtrMut(pointee) => D::PtrMut(Box::new(durable_import_type(pointee)?)),
        _ => return None,
    })
}

fn collect_type_parts(
    key: &crate::TypeInstanceKey,
    named: &mut BTreeSet<crate::StableDefinitionKey>,
    producers: &mut BTreeSet<crate::StableDefinitionKey>,
    anonymous: &mut Vec<crate::AnonymousNominalKey>,
    slices: &mut Vec<(crate::TypeInstanceKey, Arc<str>)>,
    has_module: &mut bool,
    has_generic: &mut bool,
) {
    use rue_air::NominalInstanceKey as N;
    use rue_air::TypeInstanceKey as T;
    match key {
        T::Nominal(N::Named(definition)) => {
            named.insert(definition.clone());
        }
        T::Nominal(N::Anonymous(anon)) => {
            if !anonymous.contains(anon) {
                anonymous.push(anon.clone());
            }
            collect_function_parts(
                None,
                Some(&anon.producer),
                named,
                producers,
                anonymous,
                slices,
                has_module,
                has_generic,
            );
            for ty in anon.arguments.types.iter() {
                collect_type_parts(
                    ty,
                    named,
                    producers,
                    anonymous,
                    slices,
                    has_module,
                    has_generic,
                );
            }
        }
        T::Array { element, .. } => collect_type_parts(
            element,
            named,
            producers,
            anonymous,
            slices,
            has_module,
            has_generic,
        ),
        T::Slice { element, name } => {
            slices.push(((**element).clone(), name.clone()));
            collect_type_parts(
                element,
                named,
                producers,
                anonymous,
                slices,
                has_module,
                has_generic,
            );
        }
        T::PtrConst(pointee) | T::PtrMut(pointee) => collect_type_parts(
            pointee,
            named,
            producers,
            anonymous,
            slices,
            has_module,
            has_generic,
        ),
        T::Module(_) => *has_module = true,
        T::GenericParameter(_) => *has_generic = true,
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_function_parts(
    function: Option<&crate::FunctionInstanceKey>,
    producer: Option<&rue_air::StableProducerId<crate::StableDefinitionKey, ModuleId>>,
    named: &mut BTreeSet<crate::StableDefinitionKey>,
    producers: &mut BTreeSet<crate::StableDefinitionKey>,
    anonymous: &mut Vec<crate::AnonymousNominalKey>,
    slices: &mut Vec<(crate::TypeInstanceKey, Arc<str>)>,
    has_module: &mut bool,
    has_generic: &mut bool,
) {
    use crate::FunctionInstanceKey as F;
    use rue_air::StableProducerId as P;
    if let Some(producer) = producer {
        match producer {
            P::Definition(definition) => {
                producers.insert(definition.clone());
            }
            P::Function(function) => collect_function_parts(
                Some(function),
                None,
                named,
                producers,
                anonymous,
                slices,
                has_module,
                has_generic,
            ),
        }
    }
    if let Some(function) = function {
        match function {
            F::Definition(definition) => {
                producers.insert(definition.clone());
            }
            F::Specialization { base, arguments } => {
                collect_function_parts(
                    Some(base),
                    None,
                    named,
                    producers,
                    anonymous,
                    slices,
                    has_module,
                    has_generic,
                );
                for ty in arguments.types.iter() {
                    collect_type_parts(
                        ty,
                        named,
                        producers,
                        anonymous,
                        slices,
                        has_module,
                        has_generic,
                    );
                }
            }
            F::AnonymousMember { owner, .. } => collect_type_parts(
                owner,
                named,
                producers,
                anonymous,
                slices,
                has_module,
                has_generic,
            ),
            F::DropGlue(pointee) => collect_type_parts(
                pointee,
                named,
                producers,
                anonymous,
                slices,
                has_module,
                has_generic,
            ),
        }
    }
}

/// The comptime-call syntax a definition-rooted anonymous producer spells
/// (`Pair()`, `Option(i32)`), or `None` when an argument has no primitive
/// spelling.
fn producer_call_syntax(anon: &crate::AnonymousNominalKey) -> Option<(ModuleId, String)> {
    use crate::FunctionInstanceKey as F;
    use rue_air::StableProducerId as P;
    let canonical = anon.with_canonical_producer().into_owned();
    let (base, arguments) = match &canonical.producer {
        P::Definition(definition) => (definition.clone(), None),
        P::Function(function) => match &**function {
            F::Definition(definition) => (definition.clone(), None),
            F::Specialization { base, arguments } => match &**base {
                F::Definition(definition) => (definition.clone(), Some(arguments.clone())),
                _ => return None,
            },
            _ => return None,
        },
    };
    let mut rendered = Vec::new();
    if let Some(arguments) = &arguments {
        for ty in arguments.types.iter() {
            rendered.push(primitive_syntax(ty)?.to_owned());
        }
        for value in arguments.values.iter() {
            match value {
                rue_air::CanonicalArgumentValue::Integer(value) => rendered.push(value.to_string()),
                rue_air::CanonicalArgumentValue::Bool(value) => rendered.push(value.to_string()),
                _ => return None,
            }
        }
    }
    Some((
        base.module().clone(),
        format!("{}({})", base.name(), rendered.join(", ")),
    ))
}

/// The per-body replay context: the five drivers plus the body-local overlay,
/// all constructed inside ONE provider probe so the probe's recorded edge set
/// is exactly this body's side-B observation footprint.
struct ReplayCtx<'a, 'db, 'r> {
    provider: &'a CompilerBodyFactProvider<'db>,
    endpoint: rue_air::ProviderEndpointFacts<
        'a,
        CompilerBodyFactProvider<'db>,
        DurableDeclSource,
        crate::StableDefinitionKey,
        ModuleId,
    >,
    calls: rue_air::ProviderCallFacts<
        'a,
        CompilerBodyFactProvider<'db>,
        DurableDeclSource,
        crate::StableDefinitionKey,
        ModuleId,
    >,
    aggregate:
        rue_air::ProviderAggregateFacts<crate::StableDefinitionKey, ModuleId, DurableDeclSource>,
    overlay: crate::body_overlay::BodySemanticOverlay,
    decls: &'r [crate::durable_semantics::DurableDeclarationSemantic],
    module_files: &'r BTreeMap<ModuleId, FileId>,
    scope: ModuleId,
    body_key: crate::StableDefinitionKey,
    rir: &'r rue_rir::Rir,
    rir_interner: &'r lasso::ThreadedRodeo,
    tokens: BTreeMap<crate::StableDefinitionKey, rue_air::SemanticDefinitionToken>,
    replayed_nominals: BTreeSet<crate::StableDefinitionKey>,
    replayed_functions: BTreeSet<crate::StableDefinitionKey>,
    checked_producers: BTreeSet<crate::FunctionInstanceKey>,
    producer_instances: BTreeMap<crate::AnonymousNominalKey, crate::FunctionInstanceKey>,
    minted: Vec<(String, rue_air::Type, Option<crate::StableDefinitionKey>)>,
    agg_seed: Vec<(crate::StableDefinitionKey, String)>,
    facts: Vec<String>,
    exceptions: BTreeSet<&'static str>,
    discovered_references: BTreeSet<BodyReference>,
}

fn durable_nominal_has_destructor(
    decls: &[crate::durable_semantics::DurableDeclarationSemantic],
    nominal: &crate::StableDefinitionKey,
) -> bool {
    decls.iter().any(|decl| {
        decl.key.kind() == crate::StableDefinitionKind::Destructor
            && decl.key.owner().is_some_and(|owner| {
                owner.module() == nominal.module()
                    && owner.kind() == nominal.kind()
                    && owner.name() == nominal.name()
            })
    })
}

impl<'a, 'db, 'r> ReplayCtx<'a, 'db, 'r> {
    fn file_of(&self, module: &ModuleId) -> FileId {
        *self
            .module_files
            .get(module)
            .unwrap_or_else(|| panic!("module {module:?} has no file in the merged program"))
    }

    fn token_for(&mut self, key: &crate::StableDefinitionKey) -> rue_air::SemanticDefinitionToken {
        if let Some(token) = self.tokens.get(key) {
            return *token;
        }
        let file = self.file_of(key.module());
        let token =
            self.endpoint
                .register_named_nominal(key.clone(), file.index(), key.name(), key.kind());
        self.tokens.insert(key.clone(), token);
        token
    }

    /// Resolve the name of a durable nominal as TYPE SYNTAX through
    /// `ProviderTypeFacts` (r2) into the body-local overlay, asserting the
    /// resolved durable identity and the overlay-materialized metadata equal
    /// the durable declaration truth.
    fn replay_named_nominal(&mut self, key: &crate::StableDefinitionKey) {
        if !self.replayed_nominals.insert(key.clone()) {
            return;
        }
        // r2: type-syntax resolution through the boundary + overlay
        // materialization, byte-compared against the durable declaration.
        let scope = key.module().clone();
        let mut type_facts = ProviderTypeFacts::new(self.provider, &mut self.overlay);
        let resolved = rue_air::resolve_semantic_type_syntax(&mut type_facts, &scope, key.name());
        match resolved {
            Ok(resolved) => {
                assert_eq!(
                    resolved,
                    crate::DurableType::Nominal(key.clone()),
                    "ProviderTypeFacts resolved `{}` to a different durable identity",
                    key.name()
                );
            }
            Err(error) => panic!(
                "ProviderTypeFacts failed to resolve nominal `{}`: {error:?}",
                key.name()
            ),
        }
        let durable = self
            .decls
            .iter()
            .find(|decl| decl.key == *key)
            .unwrap_or_else(|| panic!("nominal `{}` missing from the durable set", key.name()));
        let materialized = self
            .overlay
            .materialized_nominal(key)
            .unwrap_or_else(|| panic!("`{}` was not materialized into the overlay", key.name()));
        assert_eq!(
            materialized,
            &durable.payload,
            "overlay materialization for `{}` diverged from the durable declaration",
            key.name()
        );
        self.facts.push(format!(
            "type-facts nominal `{}` resolved + overlay-materialized (durable-equal)",
            key.name()
        ));

        // r4b-2: the endpoint driver mints the pool identity for the same key.
        let token = self.token_for(key);
        let instance = rue_air::TypeInstanceKey::Nominal(rue_air::NominalInstanceKey::Named(token));
        let minted = self
            .endpoint
            .resolve_instance_type(&instance)
            .unwrap_or_else(|error| {
                panic!("endpoint driver failed to mint `{}`: {error:?}", key.name())
            });
        let render = self
            .endpoint
            .with_type_pool(|pool| endpoint_nominal_render(pool, minted));
        self.facts
            .push(format!("endpoint nominal {} => {render:?}", key.name()));
        self.minted
            .push((key.name().to_owned(), minted, Some(key.clone())));
        self.agg_seed.push((key.clone(), key.name().to_owned()));
        let file = self.file_of(key.module());
        self.aggregate
            .register_named_nominal(key.clone(), file, key.name());
        let aggregate_ty = match self.aggregate.select_qualified_type(file, key.name()) {
            rue_air::ProviderQualifiedType::Struct(ty)
            | rue_air::ProviderQualifiedType::Enum(ty) => ty,
            rue_air::ProviderQualifiedType::Absent => {
                panic!("aggregate driver omitted `{}`", key.name())
            }
        };
        assert_eq!(
            aggregate_ty, minted,
            "all provider drivers must share one body-local type identity"
        );
    }

    fn replay_reference(&mut self, reference: &BodyReference) {
        match reference {
            BodyReference::Callable(instance) => self.replay_callable(instance),
            BodyReference::Definition(key) => self.replay_definition(key),
            BodyReference::Type(instance) => self.replay_type(instance),
        }
    }

    fn replay_callable(&mut self, instance: &crate::FunctionInstanceKey) {
        use crate::FunctionInstanceKey as F;
        match instance {
            F::Definition(key) => self.replay_definition(key),
            F::Specialization { base, arguments } => {
                self.facts
                    .push(format!("specialization arguments {arguments:?}"));
                self.replay_callable(base);
            }
            F::AnonymousMember { owner, .. } => {
                self.exceptions
                    .insert("drop_glue_overlay_method_installation");
                self.replay_type(owner);
            }
            F::DropGlue(pointee) => {
                self.exceptions
                    .insert("drop_glue_overlay_method_installation");
                self.replay_type(pointee);
            }
        }
    }

    fn replay_definition(&mut self, key: &crate::StableDefinitionKey) {
        use crate::StableDefinitionKind as K;
        use rue_air::BodyFactProvider;
        match key.kind() {
            K::Function => {
                if !self.replayed_functions.insert(key.clone()) {
                    return;
                }
                let module = key.module().clone();
                let present = self.calls.function_contains_in_module(&module, key.name());
                assert!(
                    present,
                    "the lookup boundary must classify `{}` as a declared free function",
                    key.name()
                );
                let file = self.file_of(&module);
                let info = self
                    .calls
                    .function_info(&key.clone(), key.name(), file)
                    .unwrap_or_else(|| {
                        panic!(
                            "ProviderCallFacts must assemble FunctionInfo for `{}`",
                            key.name()
                        )
                    });
                let mut params = Vec::new();
                let (names, types) = self.calls.with_param_arena(|arena| {
                    (
                        arena.names(info.params).to_vec(),
                        arena.types(info.params).to_vec(),
                    )
                });
                self.calls.with_type_pool(|pool| {
                    for (name, ty) in names.iter().zip(types.iter()) {
                        params.push(format!(
                            "{}: {}",
                            self.calls.resolve_symbol(*name).to_owned(),
                            endpoint_display(pool, *ty)
                        ));
                    }
                });
                let ret = self
                    .calls
                    .with_type_pool(|pool| endpoint_display(pool, info.return_type));
                self.facts.push(format!(
                    "call-facts fn `{}`({}) -> {} generic={} pub={}",
                    key.name(),
                    params.join(", "),
                    ret,
                    info.is_generic,
                    info.is_pub,
                ));
            }
            K::Method | K::AssociatedFunction => {
                let owner = key
                    .owner()
                    .expect("method-namespace keys carry their owner");
                let receiver = ReceiverTypeIdentity::new(
                    key.module().clone(),
                    owner.name(),
                    candidate_category(owner.kind()).expect("nominal owner"),
                );
                let candidates = self.provider.method_candidates(&receiver, key.name());
                let want = match key.kind() {
                    K::Method => rue_air::MemberKind::Method,
                    _ => rue_air::MemberKind::AssociatedFunction,
                };
                let mut matching = candidates.iter().filter(|candidate| candidate.kind == want);
                let member = matching.next().unwrap_or_else(|| {
                    panic!(
                        "the boundary must supply the member candidate `{}.{}`",
                        owner.name(),
                        key.name()
                    )
                });
                assert!(matching.next().is_none(), "unique member candidate");
                let signature = self.provider.signature(&member.declaration);
                assert!(
                    signature.is_some(),
                    "the member candidate handle fetches its signature"
                );
                self.facts.push(format!(
                    "member `{}.{}` kind={:?} has_self={} pub={} signature=present",
                    owner.name(),
                    key.name(),
                    member.kind,
                    member.has_self_receiver,
                    member.is_public,
                ));
                let owner_file = self.file_of(key.module());
                let info = self
                    .calls
                    .method_info(&key.clone(), owner_file, owner.name(), key.name())
                    .unwrap_or_else(|| {
                        panic!(
                            "ProviderCallFacts must assemble MethodInfo for `{}.{}`",
                            owner.name(),
                            key.name()
                        )
                    });
                let (receiver_display, return_display) = self.calls.with_type_pool(|pool| {
                    (
                        endpoint_display(pool, info.struct_type),
                        endpoint_display(pool, info.return_type),
                    )
                });
                self.facts.push(format!(
                    "call-facts member `{}.{}` receiver={} -> {} has_self={} self_mode={:?}",
                    owner.name(),
                    key.name(),
                    receiver_display,
                    return_display,
                    info.has_self,
                    info.self_mode,
                ));
            }
            K::Destructor => {
                let owner = key.owner().expect("destructor keys carry their owner");
                let file = self.file_of(key.module());
                let declaration = self.endpoint.destructor(file, owner.name());
                assert!(
                    declaration.is_some(),
                    "the endpoint driver locates the `{}` destructor declaration",
                    owner.name()
                );
                let receiver = ReceiverTypeIdentity::new(
                    key.module().clone(),
                    owner.name(),
                    candidate_category(owner.kind()).expect("nominal owner"),
                );
                let metadata = self
                    .provider
                    .drop_copy_metadata(&receiver)
                    .expect("drop/copy metadata is supplied by the boundary");
                assert!(
                    metadata.has_destructor,
                    "the boundary observes the `{}` destructor",
                    owner.name()
                );
                self.facts.push(format!(
                    "destructor `{}` located; boundary has_destructor=true is_copy={}",
                    owner.name(),
                    metadata.is_copy,
                ));
            }
            K::ValueConst | K::ModuleBinding => {
                let resolution = self.provider.lookup_unqualified(
                    key.module(),
                    rue_air::ProviderNamespace::ModuleItem,
                    key.name(),
                );
                let present = matches!(
                    resolution.of_kind(rue_air::ProviderDefinitionKind::Const),
                    rue_air::NameResolution::Unique(_) | rue_air::NameResolution::Ambiguous(_)
                );
                assert!(
                    present,
                    "the lookup boundary classifies const candidate `{}`",
                    key.name()
                );
                let file = self.file_of(key.module());
                let durable = self
                    .decls
                    .iter()
                    .find(|declaration| declaration.key == *key)
                    .expect("const candidate has durable declaration");
                let info = match &durable.payload {
                    crate::durable_semantics::DurableDeclarationPayload::Const { .. } => self
                        .endpoint
                        .const_info(key, file, key.name())
                        .expect("value const materializes through the shared pool"),
                    crate::durable_semantics::DurableDeclarationPayload::ModuleBinding {
                        target,
                    } => self
                        .endpoint
                        .module_binding_info(file, key.name(), target, durable.is_public)
                        .expect("module binding joins the shared module registry"),
                    _ => unreachable!("const kind has const payload"),
                };
                if key.kind() == K::ValueConst {
                    self.calls
                        .register_value_const(file, key.name(), info.clone());
                    self.aggregate
                        .register_value_const(file, key.name(), info.clone());
                    assert!(self.calls.value_const(file, key.name()).is_some());
                } else {
                    self.calls
                        .register_module_binding(file, key.name(), info.clone());
                    self.aggregate
                        .register_module_binding(file, key.name(), info.clone());
                    assert!(self.calls.module_binding(file, key.name()).is_some());
                }
                assert!(matches!(
                    self.aggregate.select_module_type_member(file, key.name()),
                    rue_air::ProviderModuleMember::Const
                ));
                self.facts.push(format!(
                    "const-candidate `{}` materialized into call + aggregate facts",
                    key.name()
                ));
            }
            K::Struct | K::Enum => self.replay_named_nominal(key),
        }
    }

    /// Cross-check an anonymous producer through the exact instance-keyed
    /// producer-facts operation. Wrapper/specialization identity is preserved.
    fn replay_producer(&mut self, anon: &crate::AnonymousNominalKey) {
        use crate::FunctionInstanceKey as F;
        use rue_air::StableProducerId as P;
        let canonical = anon.with_canonical_producer().into_owned();
        let mut producer_instance = self
            .producer_instances
            .get(&canonical)
            .cloned()
            .unwrap_or_else(|| match &anon.producer {
                P::Definition(definition) => F::Specialization {
                    base: Box::new(F::Definition(definition.clone())),
                    arguments: crate::CanonicalArguments::default(),
                },
                P::Function(function) => (**function).clone(),
            });
        if let F::Definition(definition) = &producer_instance {
            producer_instance = F::Specialization {
                base: Box::new(F::Definition(definition.clone())),
                arguments: crate::CanonicalArguments::default(),
            };
        }
        let producer_key = match &producer_instance {
            F::Definition(definition) => definition,
            F::Specialization { base, .. } => match base.as_ref() {
                F::Definition(definition) => definition,
                _ => return,
            },
            _ => return,
        };
        if producer_key.kind() != crate::StableDefinitionKind::Function
            || !self.checked_producers.insert(producer_instance.clone())
        {
            return;
        }
        match self
            .provider
            .producer_instance_body_facts(&producer_instance)
        {
            Some(crate::body_query::ProducedAnonymous::Produced(produced)) => {
                let boundary: BTreeSet<_> = produced
                    .0
                    .iter()
                    .map(|nominal| nominal.identity.with_canonical_producer().into_owned())
                    .collect();
                assert!(
                    boundary.contains(&canonical),
                    "the boundary producer facts for `{}` must include the consumed identity",
                    producer_key.name()
                );
                self.facts.push(format!(
                    "producer-facts `{}` supplied {} anonymous nominal(s) via the boundary",
                    producer_key.name(),
                    produced.0.len()
                ));
            }
            Some(crate::body_query::ProducedAnonymous::ProducerFailed(failure)) => {
                panic!("producer `{}` failed: {failure:?}", producer_key.name())
            }
            None => panic!(
                "instance-keyed producer facts unavailable for `{}`",
                producer_key.name()
            ),
        }
    }

    fn replay_type(&mut self, instance: &crate::TypeInstanceKey) {
        use rue_air::NominalInstanceKey as N;
        use rue_air::TypeInstanceKey as T;

        // r2: primitives resolve through the type-syntax boundary with no edge.
        if let Some(syntax) = primitive_syntax(instance) {
            let scope = self.any_module();
            let mut type_facts = ProviderTypeFacts::new(self.provider, &mut self.overlay);
            let resolved = rue_air::resolve_semantic_type_syntax(&mut type_facts, &scope, syntax)
                .unwrap_or_else(|error| panic!("primitive `{syntax}` resolves: {error:?}"));
            assert_eq!(
                Some(&resolved),
                durable_import_type(instance).as_ref(),
                "primitive `{syntax}` durable identity"
            );
            self.facts.push(format!("type-facts primitive `{syntax}`"));
        }

        // Collect the durable parts of this key.
        let mut named = BTreeSet::new();
        let mut producers = BTreeSet::new();
        let mut anonymous = Vec::new();
        let mut slices = Vec::new();
        let mut has_module = false;
        let mut has_generic = false;
        collect_type_parts(
            instance,
            &mut named,
            &mut producers,
            &mut anonymous,
            &mut slices,
            &mut has_module,
            &mut has_generic,
        );
        for key in &named {
            self.replay_named_nominal(key);
        }
        for key in &producers {
            self.token_for(key);
        }
        for (element, name) in &slices {
            match durable_import_type(element) {
                Some(element) => {
                    let registered = self.endpoint.register_generated_slice(&element, name);
                    assert!(registered.is_some(), "slice `{name}` mints its struct");
                    self.facts.push(format!("endpoint slice `{name}` minted"));
                }
                None => panic!("slice element for `{name}` has no durable import form"),
            }
        }
        for anon in &anonymous {
            // The boundary supplies the producer's published nominals under
            // its exact function-instance identity.
            self.replay_producer(anon);
            // r5a wiring: the constructor head + argument + reduction arms run
            // through SignatureFacts; a reduction to an anonymous nominal is
            // the pinned r6b deferral, recorded per hit.
            if let Some((scope, syntax)) = producer_call_syntax(anon) {
                let mut type_facts = ProviderTypeFacts::new(self.provider, &mut self.overlay);
                match rue_air::resolve_semantic_type_syntax(&mut type_facts, &scope, &syntax) {
                    Ok(resolved) => self
                        .facts
                        .push(format!("comptime-call `{syntax}` reduced to {resolved:?}")),
                    Err(error) => {
                        let rendered = format!("{error:?}");
                        assert!(
                            rendered.contains("Deferred"),
                            "comptime call `{syntax}` failed for a non-deferral reason: {rendered}"
                        );
                        self.facts.push(format!(
                            "comptime-call `{syntax}` head+arguments driven; anonymous \
                             reduction deferred (r6b)"
                        ));
                        self.exceptions
                            .insert("comptime_call_reduces_to_anonymous_nominal");
                    }
                }
            }
            // r6b arm: seed the issued->durable seam and mint through the pool.
            let tokens = &self.tokens;
            let issued = anon.try_map_identities(
                &|key: &crate::StableDefinitionKey| {
                    tokens
                        .get(key)
                        .copied()
                        .ok_or("module_identity_deferred_to_flip")
                },
                &|_module: &ModuleId| Err("module_identity_deferred_to_flip"),
            );
            match issued {
                Ok(issued) => {
                    self.endpoint
                        .register_anonymous_nominal(issued, anon.clone());
                }
                Err(_) => {
                    // A module-typed argument has no token relocation yet; the
                    // direct durable mint below still covers the identity.
                }
            }
            let minted = self
                .endpoint
                .mint_anonymous(anon)
                .unwrap_or_else(|| panic!("the pool must mint the anonymous nominal {anon:?}"));
            let render = self
                .endpoint
                .with_type_pool(|pool| endpoint_nominal_render(pool, minted));
            self.facts.push(format!("endpoint anonymous => {render:?}"));
            self.minted.push((format!("{render:?}"), minted, None));
        }

        if has_generic || has_module {
            // Module identity / generic parameters are pool-refused arms; the
            // full-key resolve below would fail closed, which is the recorded
            // behavior, not a silent divergence.
            if has_generic {
                self.facts
                    .push("generic-parameter arm fails closed (r5 substitution)".to_owned());
            }
            if has_module {
                self.exceptions.insert("module_identity_deferred_to_flip");
            }
            return;
        }

        // r4b-2: the endpoint driver resolves the WHOLE relocated key through
        // the shared `resolve_instance_type` algebra over the pool.
        let tokens = &self.tokens;
        let relocated = instance.try_map_identities(
            &|key: &crate::StableDefinitionKey| {
                tokens.get(key).copied().ok_or("unminted definition token")
            },
            &|_module: &ModuleId| Err("module identity unreachable here"),
        );
        let relocated = relocated.expect("every definition identity was minted above");
        // The anonymous arm of the relocated key resolves only when the issued
        // relocation succeeded; a bare anonymous key already minted above.
        let contains_unseeded_anonymous = matches!(
            &relocated,
            T::Nominal(N::Anonymous(_))
        ) && anonymous.iter().any(|anon| {
            anon.try_map_identities::<rue_air::SemanticDefinitionToken, rue_air::SemanticModuleToken, ()>(
                &|key: &crate::StableDefinitionKey| tokens.get(key).copied().ok_or(()),
                &|_module: &ModuleId| Err(()),
            )
            .is_err()
        });
        if contains_unseeded_anonymous {
            return;
        }
        let resolved = self
            .endpoint
            .resolve_instance_type(&relocated)
            .unwrap_or_else(|error| panic!("endpoint resolve of {instance:?} failed: {error:?}"));
        let display = self
            .endpoint
            .with_type_pool(|pool| endpoint_display(pool, resolved));
        self.facts
            .push(format!("endpoint type {instance:?} => {display}"));
        self.minted.push((display, resolved, None));
    }

    fn any_module(&self) -> ModuleId {
        self.module_files
            .keys()
            .next()
            .expect("the merged program has a module")
            .clone()
    }

    fn candidate(
        &self,
        name: &str,
        category: DeclarationCandidateCategory,
    ) -> DeclarationCandidateKey {
        DeclarationCandidateKey {
            module: self.scope.clone(),
            category,
            name: Arc::from(name),
            owner: None,
            duplicate_discriminator: 0,
        }
    }

    fn selected_named(
        &mut self,
        name: &str,
        kind: rue_air::ProviderDefinitionKind,
        category: DeclarationCandidateCategory,
    ) -> Option<crate::StableDefinitionKey> {
        use rue_air::BodyFactProvider;
        let resolution = self
            .provider
            .lookup_unqualified(&self.scope, rue_air::ProviderNamespace::ModuleItem, name)
            .of_kind(kind);
        if !matches!(resolution, rue_air::NameResolution::Unique(_)) {
            return None;
        }
        let candidate = self.candidate(name, category);
        let identity = self.provider.declaration_identity(&candidate)?;
        Some(identity.key)
    }

    fn receiver_identity(&mut self, receiver: rue_rir::InstRef) -> Option<ReceiverTypeIdentity> {
        match &self.rir.get(receiver).data {
            rue_rir::InstData::StructInit {
                module: None,
                type_name,
                ..
            } => {
                let name = self.rir_interner.resolve(type_name).to_owned();
                let key = self.selected_named(
                    &name,
                    rue_air::ProviderDefinitionKind::Struct,
                    DeclarationCandidateCategory::Struct,
                )?;
                Some(ReceiverTypeIdentity::new(
                    key.module().clone(),
                    key.name(),
                    DeclarationCandidateCategory::Struct,
                ))
            }
            rue_rir::InstData::VarRef { name, .. } if self.rir_interner.resolve(name) == "self" => {
                let owner = self.body_key.owner()?;
                Some(ReceiverTypeIdentity::new(
                    self.body_key.module().clone(),
                    owner.name(),
                    candidate_category(owner.kind())?,
                ))
            }
            rue_rir::InstData::VarRef { name, .. } => {
                let name = self.rir_interner.resolve(name).to_owned();
                let key = self.selected_named(
                    &name,
                    rue_air::ProviderDefinitionKind::Struct,
                    DeclarationCandidateCategory::Struct,
                )?;
                self.replay_named_nominal(&key);
                self.discovered_references.insert(BodyReference::Type(
                    crate::TypeInstanceKey::Nominal(rue_air::NominalInstanceKey::Named(
                        key.clone(),
                    )),
                ));
                Some(ReceiverTypeIdentity::new(
                    key.module().clone(),
                    key.name(),
                    DeclarationCandidateCategory::Struct,
                ))
            }
            _ => None,
        }
    }

    /// The rFinal freeze-hook exercise (r4a-2a rider): drop/linearity reads are
    /// un-finalized until the pool-side freeze runs at the same point
    /// production freezes; afterwards every minted type answers.
    fn exercise_finalize_seam(&mut self) {
        if let Some((display, ty, _)) = self.minted.first().cloned() {
            assert_eq!(
                self.endpoint.type_needs_drop(ty),
                None,
                "drop metadata must be un-finalized before the freeze hook"
            );
            let _ = display;
        }
        self.endpoint
            .finalize_containment_metadata()
            .expect("the minted universe has no containment cycle");
        let mut lines = Vec::new();
        for (display, ty, key) in &self.minted {
            let needs_drop = self
                .endpoint
                .type_needs_drop(*ty)
                .expect("finalized pool answers needs-drop");
            let carries_linear = self
                .endpoint
                .type_carries_linear(*ty)
                .expect("finalized pool answers linearity");
            let has_destructor = key
                .as_ref()
                .is_some_and(|key| durable_nominal_has_destructor(self.decls, key));
            if has_destructor {
                assert!(
                    needs_drop,
                    "a provider-minted nominal with `__drop` must need drop"
                );
            }
            lines.push(format!(
                "containment {display}: needs_drop={needs_drop} carries_linear={carries_linear}"
            ));
        }
        self.facts.extend(lines);
        if !self.minted.is_empty() {
            self.facts
                .push("finalize_containment_metadata seam exercised".to_owned());
        }
    }
}

impl rue_air::ProviderBodyAnalysisContext for ReplayCtx<'_, '_, '_> {
    fn demand_value(&mut self, name: &str, _span: rue_span::Span) {
        use rue_air::BodyFactProvider;
        if let Some(key) = self.selected_named(
            name,
            rue_air::ProviderDefinitionKind::Const,
            DeclarationCandidateCategory::ConstCandidate,
        ) {
            let candidate = self.candidate(name, DeclarationCandidateCategory::ConstCandidate);
            let _ = self.provider.const_comptime(&candidate);
            self.discovered_references
                .insert(BodyReference::Definition(key));
        }
    }

    fn demand_call(&mut self, name: &str, arguments: &[rue_rir::InstRef], _span: rue_span::Span) {
        use rue_air::BodyFactProvider;
        if let Some((owner_name, member_name)) = name.split_once('.') {
            let Some(owner) = self.selected_named(
                owner_name,
                rue_air::ProviderDefinitionKind::Struct,
                DeclarationCandidateCategory::Struct,
            ) else {
                self.facts.push(format!(
                    "body-walk associated call `{name}` => absent owner"
                ));
                return;
            };
            self.replay_named_nominal(&owner);
            self.discovered_references.insert(BodyReference::Type(
                crate::TypeInstanceKey::Nominal(rue_air::NominalInstanceKey::Named(owner.clone())),
            ));
            let receiver = ReceiverTypeIdentity::new(
                owner.module().clone(),
                owner.name(),
                DeclarationCandidateCategory::Struct,
            );
            let candidates = self.provider.method_candidates(&receiver, member_name);
            let mut associated = candidates
                .iter()
                .filter(|candidate| candidate.kind == rue_air::MemberKind::AssociatedFunction);
            if let Some(selected) = associated.next()
                && associated.next().is_none()
            {
                let _ = self.provider.signature(&selected.declaration);
                if let Some(identity) = self.provider.declaration_identity(&selected.declaration) {
                    self.discovered_references.insert(BodyReference::Callable(
                        crate::FunctionInstanceKey::Definition(identity.key.clone()),
                    ));
                    self.replay_definition(&identity.key);
                }
            }
            return;
        }
        let Some(key) = self.selected_named(
            name,
            rue_air::ProviderDefinitionKind::Function,
            DeclarationCandidateCategory::Function,
        ) else {
            // The keyed absent lookup above is itself the complete demand for
            // a missing call head.
            self.facts
                .push(format!("body-walk call `{name}` => absent"));
            return;
        };
        let candidate = self.candidate(name, DeclarationCandidateCategory::Function);
        let signature = self.provider.signature(&candidate);
        assert!(signature.is_some(), "selected call head has a signature");

        // The base callable is the conservative identity for the discovery
        // report. Comptime-call reduction below records the exact
        // argument-parameterized fact; production references may spell the
        // resulting specialization more narrowly.
        self.discovered_references.insert(BodyReference::Callable(
            crate::FunctionInstanceKey::Definition(key.clone()),
        ));
        self.replay_definition(&key);

        // Bind the primitive comptime arguments that are expressible directly
        // in RIR and drive the real argument-parameterized provider operation.
        if let Some(signature) = signature
            && let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                parameters,
                result,
                ..
            } = &signature.signature
        {
            let mut type_arguments = Vec::new();
            let mut value_arguments = Vec::new();
            for (parameter, argument) in parameters.iter().zip(arguments) {
                if !parameter.is_comptime {
                    continue;
                }
                match &self.rir.get(*argument).data {
                    rue_rir::InstData::TypeConst { type_name } => {
                        let syntax = self.rir_interner.resolve(type_name);
                        if let Some(ty) = durable_type_for_primitive_syntax(syntax) {
                            type_arguments.push((parameter.name.clone(), ty));
                        }
                    }
                    rue_rir::InstData::IntConst(value) => value_arguments.push((
                        parameter.name.clone(),
                        crate::durable_semantics::DurableConstValue::Integer(*value as i128),
                    )),
                    rue_rir::InstData::BoolConst(value) => value_arguments.push((
                        parameter.name.clone(),
                        crate::durable_semantics::DurableConstValue::Bool(*value),
                    )),
                    rue_rir::InstData::UnitConst => value_arguments.push((
                        parameter.name.clone(),
                        crate::durable_semantics::DurableConstValue::Unit,
                    )),
                    _ => {}
                }
            }
            if !type_arguments.is_empty() || !value_arguments.is_empty() || parameters.is_empty() {
                let _ = self.provider.reduce_comptime_call(
                    &candidate,
                    &type_arguments,
                    &value_arguments,
                );
            }
            if matches!(result, crate::durable_semantics::DurableType::ComptimeType) {
                let producer = crate::FunctionInstanceKey::Specialization {
                    base: Box::new(crate::FunctionInstanceKey::Definition(key)),
                    arguments: crate::CanonicalArguments::default(),
                };
                let _ = self.provider.producer_instance_body_facts(&producer);
            }
        }
    }

    fn demand_type(&mut self, syntax: &str, _span: rue_span::Span) {
        let mut facts = ProviderTypeFacts::new(self.provider, &mut self.overlay);
        if let Ok(resolved) = rue_air::resolve_semantic_type_syntax(&mut facts, &self.scope, syntax)
        {
            collect_durable_type_references(&resolved, &mut self.discovered_references);
        }
    }

    fn demand_struct(
        &mut self,
        _module: Option<rue_rir::InstRef>,
        type_name: &str,
        _span: rue_span::Span,
    ) {
        if let Some(key) = self.selected_named(
            type_name,
            rue_air::ProviderDefinitionKind::Struct,
            DeclarationCandidateCategory::Struct,
        ) {
            self.replay_named_nominal(&key);
            self.discovered_references.insert(BodyReference::Type(
                crate::TypeInstanceKey::Nominal(rue_air::NominalInstanceKey::Named(key)),
            ));
        }
    }

    fn demand_enum(
        &mut self,
        _module: Option<rue_rir::InstRef>,
        type_name: &str,
        _span: rue_span::Span,
    ) {
        if let Some(key) = self.selected_named(
            type_name,
            rue_air::ProviderDefinitionKind::Enum,
            DeclarationCandidateCategory::Enum,
        ) {
            self.replay_named_nominal(&key);
            self.discovered_references.insert(BodyReference::Type(
                crate::TypeInstanceKey::Nominal(rue_air::NominalInstanceKey::Named(key)),
            ));
        }
    }

    fn demand_method(&mut self, receiver: rue_rir::InstRef, method: &str, _span: rue_span::Span) {
        use rue_air::BodyFactProvider;
        let receiver_inst = receiver;
        let Some(receiver) = self.receiver_identity(receiver_inst) else {
            return;
        };
        let wanted = match &self.rir.get(receiver_inst).data {
            rue_rir::InstData::VarRef { name, .. } if self.rir_interner.resolve(name) != "self" => {
                rue_air::MemberKind::AssociatedFunction
            }
            _ => rue_air::MemberKind::Method,
        };
        let candidates = self.provider.method_candidates(&receiver, method);
        let mut methods = candidates
            .iter()
            .filter(|candidate| candidate.kind == wanted);
        let Some(selected) = methods.next() else {
            return;
        };
        if methods.next().is_some() {
            return;
        }
        let _ = self.provider.signature(&selected.declaration);
        if let Some(identity) = self.provider.declaration_identity(&selected.declaration) {
            self.discovered_references.insert(BodyReference::Callable(
                crate::FunctionInstanceKey::Definition(identity.key.clone()),
            ));
            self.replay_definition(&identity.key);
        }
    }

    fn demand_operator(
        &mut self,
        receiver: rue_rir::InstRef,
        operator: rue_air::OperatorName,
        _span: rue_span::Span,
    ) {
        use rue_air::BodyFactProvider;
        let Some(receiver) = self.receiver_identity(receiver) else {
            return;
        };
        for candidate in self.provider.operator_candidates(&receiver, operator) {
            let _ = self.provider.signature(&candidate.declaration);
        }
    }

    fn demand_const_value(&mut self, name: &str, span: rue_span::Span) {
        self.demand_value(name, span);
    }

    fn demand_anonymous_nominal(&mut self, instruction: rue_rir::InstRef, _span: rue_span::Span) {
        self.facts
            .push(format!("body-walk anonymous nominal at {instruction}"));
    }
}

fn durable_type_for_primitive_syntax(
    syntax: &str,
) -> Option<crate::durable_semantics::DurableType> {
    use crate::durable_semantics::DurableType as T;
    Some(match syntax {
        "i8" => T::I8,
        "i16" => T::I16,
        "i32" => T::I32,
        "i64" => T::I64,
        "u8" => T::U8,
        "u16" => T::U16,
        "u32" => T::U32,
        "u64" => T::U64,
        "bool" => T::Bool,
        "()" => T::Unit,
        "!" => T::Never,
        "type" => T::ComptimeType,
        _ => return None,
    })
}

fn collect_durable_type_references(
    ty: &crate::durable_semantics::DurableType,
    references: &mut BTreeSet<BodyReference>,
) {
    use crate::durable_semantics::DurableType as T;
    match ty {
        T::Nominal(key) => {
            references.insert(BodyReference::Type(crate::TypeInstanceKey::Nominal(
                rue_air::NominalInstanceKey::Named(key.clone()),
            )));
        }
        T::Array { element, .. }
        | T::Slice { element, .. }
        | T::PtrConst(element)
        | T::PtrMut(element) => collect_durable_type_references(element, references),
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn run_body_replay(
    provider: &CompilerBodyFactProvider<'_>,
    decls: &[crate::durable_semantics::DurableDeclarationSemantic],
    module_files: &BTreeMap<ModuleId, FileId>,
    body_key: &crate::StableDefinitionKey,
    body_instance: &crate::FunctionInstanceKey,
    rir: &rue_rir::Rir,
    rir_interner: &lasso::ThreadedRodeo,
) -> (
    Vec<String>,
    BTreeSet<&'static str>,
    Vec<(crate::StableDefinitionKey, String)>,
    BTreeSet<BodyReference>,
) {
    use rue_air::BodyFactProvider;
    let produced = match provider.producer_instance_body_facts(body_instance) {
        Some(crate::body_query::ProducedAnonymous::Produced(produced)) => produced.0.to_vec(),
        Some(crate::body_query::ProducedAnonymous::ProducerFailed(_)) | None => Vec::new(),
    };
    let identity = rue_air::ProviderIdentityContext::new(
        DurableDeclSource::from_declarations(decls).with_anonymous_nominals(&produced),
    );
    let endpoint = rue_air::ProviderEndpointFacts::with_identity(
        provider,
        identity.clone(),
        rir,
        rir_interner,
    );
    let calls =
        rue_air::ProviderCallFacts::with_identity(provider, identity.clone(), rir, rir_interner);
    let aggregate = rue_air::ProviderAggregateFacts::with_identity(identity);
    for (module, file) in module_files {
        let path = module.logical_path();
        endpoint
            .register_module(module.clone(), *file, path, path, path)
            .expect("the body-local module registry accepts each canonical module");
    }
    let mut producer_instances = BTreeMap::new();
    for nominal in &produced {
        let canonical = nominal.identity.with_canonical_producer().into_owned();
        if let rue_air::StableProducerId::Function(instance) = &nominal.identity.producer {
            producer_instances.insert(canonical, instance.as_ref().clone());
        }
    }
    let mut ctx = ReplayCtx {
        provider,
        endpoint,
        calls,
        aggregate,
        overlay: crate::body_overlay::BodySemanticOverlay::new(),
        decls,
        module_files,
        scope: body_key.module().clone(),
        body_key: body_key.clone(),
        rir,
        rir_interner,
        tokens: BTreeMap::new(),
        replayed_nominals: BTreeSet::new(),
        replayed_functions: BTreeSet::new(),
        checked_producers: BTreeSet::new(),
        producer_instances,
        minted: Vec::new(),
        agg_seed: Vec::new(),
        facts: Vec::new(),
        exceptions: BTreeSet::new(),
        discovered_references: BTreeSet::new(),
    };

    let declaration = match body_key.kind() {
        crate::StableDefinitionKind::Function => ctx
            .endpoint
            .first_free_function(body_key.name(), ctx.file_of(body_key.module())),
        crate::StableDefinitionKind::Method | crate::StableDefinitionKind::AssociatedFunction => {
            let owner = body_key.owner().expect("member body has an owner");
            ctx.endpoint.named_method_declaration(
                ctx.file_of(body_key.module()),
                owner.name(),
                body_key.name(),
            )
        }
        crate::StableDefinitionKind::Destructor => {
            let owner = body_key.owner().expect("destructor body has an owner");
            ctx.endpoint
                .destructor(ctx.file_of(body_key.module()), owner.name())
        }
        _ => None,
    }
    .expect("provider endpoint locates the requested body declaration");
    let body = match &rir.get(declaration).data {
        rue_rir::InstData::FnDecl { body, .. } | rue_rir::InstData::DropFnDecl { body, .. } => {
            *body
        }
        other => panic!("requested body declaration has unexpected RIR: {other:?}"),
    };

    // A member/destructor body's receiver nominal is part of the requested
    // body-local context even when the body mentions it only as `self`.
    if let Some(owner) = body_key.owner() {
        let owner = crate::StableDefinitionKey::from_stable_parts(
            owner.module().clone(),
            crate::StableDefinitionNamespace::Type,
            owner.kind(),
            owner.name(),
            None,
        );
        ctx.replay_named_nominal(&owner);
        ctx.discovered_references
            .insert(BodyReference::Type(crate::TypeInstanceKey::Nominal(
                rue_air::NominalInstanceKey::Named(owner),
            )));
    }

    // Flip-A's real analyzer entry. Demand begins at the requested RIR body,
    // not at production's published references.
    rue_air::analyze_provider_body(&mut ctx, rir, rir_interner, body);
    // Materialize the discovered durable identities through the same five
    // drivers used by the detailed fact differential. This is deliberately a
    // replay of the NEW analyzer's own output, never production's transaction.
    for reference in ctx.discovered_references.clone() {
        ctx.replay_reference(&reference);
    }
    ctx.facts.push(format!(
        "body-walk discovered {} durable reference(s)",
        ctx.discovered_references.len()
    ));

    // The body's own trusted-toolchain demand set, observed through the
    // boundary op (function bodies carry the derivable candidate key).
    if body_key.kind() == crate::StableDefinitionKind::Function
        && let Some(candidate) = candidate_for(body_key)
    {
        let demand = provider.trusted_toolchain_facts(&candidate);
        ctx.facts.push(format!(
            "toolchain-demands kinds={}",
            demand.payload_kinds().len()
        ));
    }

    ctx.exercise_finalize_seam();

    let ReplayCtx {
        facts,
        exceptions,
        agg_seed,
        discovered_references,
        ..
    } = ctx;
    (facts, exceptions, agg_seed, discovered_references)
}

/// Replay the aggregate facts for every named nominal the body consumed —
/// OUTSIDE any provider probe: `ProviderAggregateFacts` takes no provider
/// handle, so it records zero provider edges by construction (the r4b-3
/// property, structurally enforced here).
fn aggregate_replay(
    inputs: &SideBCase,
    seed: &[(crate::StableDefinitionKey, String)],
) -> Vec<String> {
    let mut facts = vec![
        "aggregate driver constructed with no provider handle: zero provider edges by \
         construction"
            .to_owned(),
    ];
    let mut aggregate =
        rue_air::ProviderAggregateFacts::<crate::StableDefinitionKey, ModuleId, _>::new(
            DurableDeclSource::from_declarations(&inputs.decls),
        );
    for (key, _) in seed {
        let file = inputs.module_files[key.module()];
        aggregate.register_named_nominal(key.clone(), file, key.name());
    }
    for (key, endpoint_display_name) in seed {
        let file = inputs.module_files[key.module()];
        match (
            key.kind(),
            aggregate.select_qualified_type(file, key.name()),
        ) {
            (crate::StableDefinitionKind::Struct, rue_air::ProviderQualifiedType::Struct(ty)) => {
                let display = aggregate.with_type_pool(|pool| endpoint_display(pool, ty));
                assert_eq!(
                    &display,
                    endpoint_display_name,
                    "aggregate and endpoint drivers agree on `{}`",
                    key.name()
                );
                facts.push(format!("aggregate qualified `{}` => struct", key.name()));
            }
            (crate::StableDefinitionKind::Enum, rue_air::ProviderQualifiedType::Enum(ty)) => {
                let display = aggregate.with_type_pool(|pool| endpoint_display(pool, ty));
                assert_eq!(
                    &display,
                    endpoint_display_name,
                    "aggregate and endpoint drivers agree on `{}`",
                    key.name()
                );
                facts.push(format!("aggregate qualified `{}` => enum", key.name()));
            }
            (kind, _) => panic!(
                "aggregate driver could not select `{}` as {kind:?}",
                key.name()
            ),
        }
        if key.kind() == crate::StableDefinitionKind::Struct {
            match aggregate.select_struct_literal_head(file, key.name()) {
                rue_air::ProviderStructHead::Named(_) => {
                    facts.push(format!("aggregate literal head `{}` => Named", key.name()));
                }
                _ => panic!("aggregate literal head for `{}`", key.name()),
            }
        }
    }
    facts
}

impl ProviderFactsOverlayAnalyzer {
    fn side(&self) -> DifferentialSide {
        DifferentialSide::ProviderFactsOverlay
    }

    #[allow(clippy::too_many_arguments)]
    fn replay_body(
        &self,
        inputs: &SideBCase,
        database: &RevisionedQueryDatabase,
        revision: rue_query::Revision,
        case_name: &'static str,
        body: &str,
        peer: &ProductionDemandPeer,
        run_tag: &str,
    ) -> SideBBody {
        let decls = inputs.decls.clone();
        let module_files = inputs.module_files.clone();
        let body_key = decls
            .iter()
            .map(|decl| decl.key.clone())
            .find(|key| key.kind().owns_body() && key.name() == body);
        let body_key = body_key.expect("requested body has a durable declaration");
        let body_instance = peer.instance.clone();
        let rir = inputs.rir.rir();
        let interner = inputs.rir.semantic_symbols().interner();
        let label = format!("side-b:{run_tag}:{case_name}:{body}");
        let outcome =
            database.probe_body_facts(revision, side_b_configuration(), &label, move |provider| {
                run_body_replay(
                    provider,
                    &decls,
                    &module_files,
                    &body_key,
                    &body_instance,
                    rir,
                    interner,
                )
            });
        let (mut facts, exceptions, agg_seed, discovered_references) = outcome.result;
        if peer.failed {
            facts.push(
                "production body failed deterministically; side B classified its absences \
                 through the keyed lookup boundary"
                    .to_owned(),
            );
        }
        facts.extend(aggregate_replay(inputs, &agg_seed));
        facts.sort();
        facts.dedup();
        let edge_families = outcome
            .dependencies
            .iter()
            .map(|node| node.family().to_owned())
            .collect::<BTreeSet<_>>();
        let edge_identities = outcome
            .dependencies
            .iter()
            .map(|node| format!("{}::{}", node.family(), node.key()))
            .collect::<BTreeSet<_>>();
        SideBBody {
            case: case_name,
            body: body.to_owned(),
            facts,
            exceptions,
            edge_families,
            edge_identities,
            discovered_references,
        }
    }

    fn replay_case(
        &self,
        case: &HarnessCase,
        inputs: &SideBCase,
        peers: &[ProductionDemandPeer],
        requested_body_order: &[&str],
        database: &RevisionedQueryDatabase,
        revision: rue_query::Revision,
        run_tag: &str,
    ) -> Vec<SideBBody> {
        requested_body_order
            .iter()
            .map(|body| {
                let peer = peers
                    .iter()
                    .find(|peer| peer.body == *body)
                    .unwrap_or_else(|| panic!("no production peer for body `{body}`"));
                self.replay_body(inputs, database, revision, case.name, body, peer, run_tag)
            })
            .collect()
    }
}

fn corpus_permutations(case: &HarnessCase) -> Vec<Vec<&'static str>> {
    let forward = case.bodies.to_vec();
    let reverse = case.bodies.iter().rev().copied().collect::<Vec<_>>();
    let rotated = if case.bodies.len() > 1 {
        case.bodies[1..]
            .iter()
            .chain(&case.bodies[..1])
            .copied()
            .collect()
    } else {
        forward.clone()
    };
    vec![forward, reverse, rotated]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn side_a_is_self_equal_across_full_shape_corpus_and_schedule_permutations() {
    let analyzer = ProductionAnalyzer;
    assert_eq!(analyzer.side(), DifferentialSide::Production);
    for case in SHAPE_CORPUS {
        let permutations = corpus_permutations(case);
        let baseline = analyzer
            .capture(case, &permutations[0])
            .unwrap_or_else(|error| {
                panic!("production capture failed for {}: {error:?}", case.name)
            });
        for permutation in &permutations[1..] {
            let candidate = analyzer.capture(case, permutation).unwrap_or_else(|error| {
                panic!("production capture failed for {}: {error:?}", case.name)
            });
            captured_bodies_equal(&baseline, &candidate).unwrap_or_else(|divergence| {
                panic!("{divergence}");
            });
        }
    }
}

/// The corpus-wide expected fact-exception hit set: every row a body hit must
/// be a named table row, and every table row must be hit — no silent
/// tolerance, no stale rows.
// `drop_glue_overlay_method_installation` is deliberately absent: this corpus
// publishes no drop-glue/anonymous-member instance reference, so the row is a
// documented reserve for the arm that would hit it, not an expected hit.
const EXPECTED_FACT_EXCEPTION_HITS: &[&str] = &[];

#[test]
fn side_b_provider_drivers_supply_previously_stubbed_facts() {
    let analyzer = ProviderFactsOverlayAnalyzer;
    assert_eq!(analyzer.side(), DifferentialSide::ProviderFactsOverlay);
    let production = ProductionAnalyzer;
    let table_names: BTreeSet<&str> = SIDE_B_FACT_EXCEPTIONS.iter().map(|row| row.name).collect();
    assert_eq!(
        table_names.len(),
        SIDE_B_FACT_EXCEPTIONS.len(),
        "exception rows are uniquely named"
    );
    let mut hit: BTreeSet<&'static str> = BTreeSet::new();
    for case in SHAPE_CORPUS {
        let (_, peers) = production
            .capture_with_peer(case, case.bodies)
            .unwrap_or_else(|error| {
                panic!("production capture failed for {}: {error:?}", case.name)
            });
        let inputs = SideBCase::build_single(case.source)
            .unwrap_or_else(|error| panic!("side-b inputs failed for {}: {error:?}", case.name));
        let mut database = RevisionedQueryDatabase::default();
        let revision = inputs.revision(&mut database);

        let forward = analyzer.replay_case(
            case,
            &inputs,
            &peers,
            case.bodies,
            &database,
            revision,
            "fwd",
        );
        // Warm/cold invariance across the schedule permutation: the reversed
        // replay runs against the SAME database (warm terminals) and must
        // produce identical facts, exceptions, and edge identities.
        let reversed_order: Vec<&str> = case.bodies.iter().rev().copied().collect();
        let mut reverse = analyzer.replay_case(
            case,
            &inputs,
            &peers,
            &reversed_order,
            &database,
            revision,
            "rev",
        );
        reverse.reverse();
        assert_eq!(
            forward, reverse,
            "side-B replay diverged across schedule permutations for {}",
            case.name
        );

        for body in &forward {
            assert!(
                !body.facts.is_empty(),
                "side B supplied no facts for {}::{}",
                case.name,
                body.body
            );
            for exception in &body.exceptions {
                assert!(
                    table_names.contains(exception),
                    "unnamed side-B exception `{exception}` for {}::{}",
                    case.name,
                    body.body
                );
                hit.insert(exception);
            }
            // Edges land only at materialization families.
            for family in &body.edge_families {
                assert!(
                    SIDE_B_MATERIALIZATION_FAMILIES.contains(&family.as_str()),
                    "side-B edge outside the materialization families for {}::{}: {family}",
                    case.name,
                    body.body
                );
            }
            let peer = peers
                .iter()
                .find(|peer| peer.body == body.body)
                .expect("side-B body has a production differential peer");
            let production_roots = peer
                .references
                .iter()
                .flat_map(reference_definition_roots)
                .collect::<BTreeSet<_>>();
            let provider_roots = body
                .discovered_references
                .iter()
                .flat_map(reference_definition_roots)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                provider_roots, production_roots,
                "body-local provider demand discovery diverged for {}::{}; facts={:?}",
                case.name, body.body, body.facts
            );
            for absent in case.absent_lookups {
                assert!(
                    body.facts
                        .iter()
                        .any(|fact| fact.contains(&format!("call `{absent}` => absent"))),
                    "body-local analyzer did not discover absent call `{absent}` in {}::{}",
                    case.name,
                    body.body
                );
            }
        }
    }
    let expected: BTreeSet<&str> = EXPECTED_FACT_EXCEPTION_HITS.iter().copied().collect();
    assert_eq!(
        hit, expected,
        "the corpus-wide exception hit set drifted from the recorded expectation"
    );
}

#[test]
fn side_b_edge_sets_match_production_with_recorded_exceptions() {
    let analyzer = ProviderFactsOverlayAnalyzer;
    let production = ProductionAnalyzer;
    let mut hit_rows: BTreeSet<&'static str> = BTreeSet::new();
    for case in SHAPE_CORPUS {
        let (_, peers) = production
            .capture_with_peer(case, case.bodies)
            .unwrap_or_else(|error| {
                panic!("production capture failed for {}: {error:?}", case.name)
            });
        let inputs = SideBCase::build_single(case.source).unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = inputs.revision(&mut database);
        let side_b = analyzer.replay_case(
            case,
            &inputs,
            &peers,
            case.bodies,
            &database,
            revision,
            "edge",
        );

        let side_a_families: BTreeSet<&str> = peers
            .iter()
            .flat_map(|peer| peer.edge_families.iter().map(String::as_str))
            .collect();
        let side_b_families: BTreeSet<&str> = side_b
            .iter()
            .flat_map(|body| body.edge_families.iter().map(String::as_str))
            .collect();

        for family in side_a_families.difference(&side_b_families) {
            let row = EDGE_FAMILY_EXCEPTIONS
                .iter()
                .find(|row| row.family == *family && row.side == EdgeExceptionSide::EpochOnly);
            let row = row.unwrap_or_else(|| {
                panic!(
                    "case {}: epoch-only edge family `{family}` has no recorded exception; \
                     side A={side_a_families:?} side B={side_b_families:?}",
                    case.name
                )
            });
            hit_rows.insert(row.family);
        }
        for family in side_b_families.difference(&side_a_families) {
            let row = EDGE_FAMILY_EXCEPTIONS
                .iter()
                .find(|row| row.family == *family && row.side == EdgeExceptionSide::ProviderOnly);
            let row = row.unwrap_or_else(|| {
                panic!(
                    "case {}: provider-only edge family `{family}` has no recorded exception; \
                     side A={side_a_families:?} side B={side_b_families:?}",
                    case.name
                )
            });
            hit_rows.insert(row.family);
        }
    }
    // Every recorded exception row is live somewhere in the corpus — the table
    // cannot silently go stale.
    let all_rows: BTreeSet<&str> = EDGE_FAMILY_EXCEPTIONS
        .iter()
        .map(|row| row.family)
        .collect();
    assert_eq!(
        hit_rows, all_rows,
        "an edge-family exception row was never hit by the corpus (stale row?)"
    );
}

#[test]
fn side_b_unrelated_same_named_const_preserves_recorded_edges() {
    // The r0-carried structural locality assertion, now expressible with side
    // B live: adding a same-named constant in an UNRELATED module changes
    // neither the recorded side-B dependency-edge set nor the supplied facts
    // for an untouched body — no whole-program context path exists on side B.
    let case = &SHAPE_CORPUS[0];
    assert_eq!(case.name, "plain_functions");
    let production = ProductionAnalyzer;
    let (_, peers) = production.capture_with_peer(case, case.bodies).unwrap();
    let analyzer = ProviderFactsOverlayAnalyzer;

    let baseline_inputs = SideBCase::build_single(case.source).unwrap();
    let mut baseline_db = RevisionedQueryDatabase::default();
    let baseline_revision = baseline_inputs.revision(&mut baseline_db);
    let baseline = analyzer.replay_case(
        case,
        &baseline_inputs,
        &peers,
        case.bodies,
        &baseline_db,
        baseline_revision,
        "locality-a",
    );

    // The same program plus an UNRELATED module declaring a `pub const` that
    // shares the free function's name.
    let root = FileId::new(0);
    let unrelated = FileId::new(1);
    let metadata = SourceMetadata::new(
        root,
        HashMap::from([
            (root, "main.rue".to_owned()),
            (unrelated, "unrelated.rue".to_owned()),
        ]),
        HashMap::from([
            (root, "main.rue".to_owned()),
            (unrelated, "unrelated.rue".to_owned()),
        ]),
    )
    .expect("two-file metadata");
    let snapshot = SourceSnapshot::new(
        metadata,
        vec![
            (root, Arc::new(case.source.to_owned())),
            (
                unrelated,
                Arc::new("pub const helper: i64 = 40;".to_owned()),
            ),
        ],
    )
    .expect("two-file snapshot");
    let extended_inputs = SideBCase::build_from_snapshot(snapshot).unwrap();
    let mut extended_db = RevisionedQueryDatabase::default();
    let extended_revision = extended_inputs.revision(&mut extended_db);
    let extended = analyzer.replay_case(
        case,
        &extended_inputs,
        &peers,
        case.bodies,
        &extended_db,
        extended_revision,
        "locality-b",
    );

    for (before, after) in baseline.iter().zip(extended.iter()) {
        assert_eq!(
            before.facts, after.facts,
            "an unrelated same-named const changed the supplied facts for {}",
            before.body
        );
        assert_eq!(
            before.edge_identities, after.edge_identities,
            "an unrelated same-named const changed the recorded edge set for {}",
            before.body
        );
        assert_eq!(before.exceptions, after.exceptions);
    }
}

#[test]
fn side_b_absent_lookup_records_only_its_keyed_terminals() {
    // The r0/E0481 carry-forward: a failing body's provider-side observation
    // capture records ONLY the keyed lookup terminals of the names it
    // consulted — no candidate-scan or whole-universe edge exists on side B.
    let case = SHAPE_CORPUS
        .iter()
        .find(|case| case.name == "absent_lookup")
        .unwrap();
    let inputs = SideBCase::build_single(case.source).unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = inputs.revision(&mut database);
    let module = inputs
        .module_files
        .keys()
        .next()
        .expect("one module")
        .clone();
    let decls = inputs.decls.clone();
    let rir = inputs.rir.rir();
    let interner = inputs.rir.semantic_symbols().interner();
    let outcome = database.probe_body_facts(
        revision,
        side_b_configuration(),
        "side-b:e0481:absent",
        move |provider| {
            let calls = rue_air::ProviderCallFacts::new(
                provider,
                DurableDeclSource::from_declarations(&decls),
                rir,
                interner,
            );
            assert!(!calls.function_contains_in_module(&module, "missing"));
        },
    );
    assert!(
        !outcome.dependencies.is_empty(),
        "the absent lookup records its keyed terminal"
    );
    for node in &outcome.dependencies {
        assert_eq!(
            node.family(),
            "compiler.lookup-name",
            "an absent lookup observes only the name-lookup family: {node:?}"
        );
        assert!(
            node.key().contains("missing"),
            "an absent lookup records only the consulted name's terminal, no candidate \
             scan: {node:?}"
        );
    }
}

#[test]
fn side_b_forced_eviction_and_cancellation_leave_no_partial_state() {
    // The rFinal behavior slot, following the slice-2b overlay cancellation
    // model: a canceled side-B request promotes nothing (never-promote) and a
    // subsequent replay is untainted; forced eviction pressure re-derives
    // invisibly (identical facts + edge identities).
    let case = &SHAPE_CORPUS[0];
    let production = ProductionAnalyzer;
    let (_, peers) = production.capture_with_peer(case, case.bodies).unwrap();
    let analyzer = ProviderFactsOverlayAnalyzer;
    let inputs = SideBCase::build_single(case.source).unwrap();
    // A tiny retention floor so filler pressure genuinely evicts.
    let mut database = RevisionedQueryDatabase::with_declaration_memo_retention(4);
    let revision = inputs.revision(&mut database);

    let baseline = analyzer.replay_case(
        case,
        &inputs,
        &peers,
        case.bodies,
        &database,
        revision,
        "cancel-baseline",
    );

    // CANCELLATION: drive the same lookups under a rooted request that aborts
    // before publishing — nothing is promoted and no lease pin escapes.
    let module = inputs.module_files.keys().next().unwrap().clone();
    let decls = inputs.decls.clone();
    let rir = inputs.rir.rir();
    let interner = inputs.rir.semantic_symbols().interner();
    let published = database.publish_lookup_root(
        revision,
        side_b_configuration(),
        "side-b:cancel:probe",
        "side-b:cancel:root",
        true,
        move |provider| {
            let calls = rue_air::ProviderCallFacts::new(
                provider,
                DurableDeclSource::from_declarations(&decls),
                rir,
                interner,
            );
            let _ = calls.function_contains_in_module(&module, "helper");
            let _ = calls.function_contains_in_module(&module, "main");
        },
    );
    assert!(!published, "a canceled side-B request publishes no root");
    let metrics = database.lookup_pressure_metrics();
    assert_eq!(
        metrics.published_roots, 0,
        "a canceled side-B request promotes nothing (never-promote)"
    );
    assert_eq!(
        metrics.leased_terminals, 0,
        "a canceled side-B request leases no terminal"
    );

    // The cancellation left no partial state: the replay is byte-identical.
    let after_cancel = analyzer.replay_case(
        case,
        &inputs,
        &peers,
        case.bodies,
        &database,
        revision,
        "cancel-after",
    );
    assert_eq!(baseline, after_cancel, "cancellation left partial state");

    // FORCED EVICTION: drive the lookup family far past its floor of 4 so the
    // replay's warm terminals evict, then replay again — the re-derivation is
    // invisible: identical facts and identical edge identities.
    let module = inputs.module_files.keys().next().unwrap().clone();
    let decls = inputs.decls.clone();
    for index in 0..24 {
        let name = format!("Filler{index}");
        let decls = decls.clone();
        let module = module.clone();
        let label = format!("side-b:evict:{index}");
        let outcome =
            database.probe_body_facts(revision, side_b_configuration(), &label, move |provider| {
                let calls = rue_air::ProviderCallFacts::new(
                    provider,
                    DurableDeclSource::from_declarations(&decls),
                    rir,
                    interner,
                );
                calls.function_contains_in_module(&module, &name)
            });
        assert!(!outcome.result, "filler names are absent");
    }
    let after_eviction = analyzer.replay_case(
        case,
        &inputs,
        &peers,
        case.bodies,
        &database,
        revision,
        "evict-after",
    );
    assert_eq!(
        baseline, after_eviction,
        "forced eviction changed the side-B replay (re-derivation must be invisible)"
    );
}

#[test]
fn side_b_cross_module_private_lookup_is_driver_supplied() {
    // The cross-module private form, closed by a side-B multi-module corpus:
    // the boundary supplies the candidate WITH its visibility fact, and the
    // aggregate driver's visibility-domain computation hides a cross-directory
    // private item while admitting the public one.
    let root = FileId::new(0);
    let leaf = FileId::new(1);
    let metadata = SourceMetadata::new(
        root,
        HashMap::from([
            (root, "/project/main.rue".to_owned()),
            (leaf, "/project/sub/leaf.rue".to_owned()),
        ]),
        HashMap::from([
            (root, "main.rue".to_owned()),
            (leaf, "sub/leaf.rue".to_owned()),
        ]),
    )
    .expect("two-file metadata");
    let snapshot = SourceSnapshot::new(
        metadata,
        vec![
            (root, Arc::new("fn main() -> i32 { 0 }".to_owned())),
            (
                leaf,
                Arc::new(
                    "fn hidden_helper() -> i32 { 1 }\npub fn shown_helper() -> i32 { 2 }\n"
                        .to_owned(),
                ),
            ),
        ],
    )
    .expect("two-file snapshot");
    let inputs = SideBCase::build_from_snapshot(snapshot).unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = inputs.revision(&mut database);
    let leaf_module = ModuleId::from_logical_path("sub/leaf.rue").unwrap();

    let outcome = database.probe_body_facts(
        revision,
        side_b_configuration(),
        "side-b:cross-module-private",
        move |provider| {
            use rue_air::BodyFactProvider;
            let hidden = provider.lookup_unqualified(
                &leaf_module,
                rue_air::ProviderNamespace::ModuleItem,
                "hidden_helper",
            );
            let shown = provider.lookup_unqualified(
                &leaf_module,
                rue_air::ProviderNamespace::ModuleItem,
                "shown_helper",
            );
            let hidden_public = match &hidden {
                rue_air::NameResolution::Unique(candidate) => candidate.is_public,
                other => panic!("hidden_helper must be a unique candidate: {other:?}"),
            };
            let shown_public = match &shown {
                rue_air::NameResolution::Unique(candidate) => candidate.is_public,
                other => panic!("shown_helper must be a unique candidate: {other:?}"),
            };
            (hidden_public, shown_public)
        },
    );
    let (hidden_public, shown_public) = outcome.result;
    assert!(!hidden_public, "the boundary supplies the private fact");
    assert!(shown_public, "the boundary supplies the public fact");

    // The aggregate driver's visibility-domain computation over the SAME
    // request-local physical paths: cross-directory private is hidden, public
    // crosses, same-file sees private.
    let mut aggregate =
        rue_air::ProviderAggregateFacts::<crate::StableDefinitionKey, ModuleId, _>::new(
            DurableDeclSource::from_declarations(&inputs.decls),
        );
    aggregate.register_file_path(root, "/project/main.rue");
    aggregate.register_file_path(leaf, "/project/sub/leaf.rue");
    assert!(
        aggregate.is_accessible(leaf, leaf, false),
        "same file sees private"
    );
    assert!(
        !aggregate.is_accessible(root, leaf, false),
        "cross-directory private is hidden"
    );
    assert!(
        aggregate.is_accessible(root, leaf, true),
        "public crosses directories"
    );
}

#[test]
fn side_b_finalize_containment_metadata_freeze_hook() {
    // The r4a-2a rider's seam hook, exercised at the point production freezes:
    // before the freeze the pool's drop/linearity reads are un-finalized;
    // after it every minted nominal answers.
    let case = SHAPE_CORPUS
        .iter()
        .find(|case| case.name == "method")
        .unwrap();
    let inputs = SideBCase::build_single(case.source).unwrap();
    let mut database = RevisionedQueryDatabase::default();
    let revision = inputs.revision(&mut database);
    let counter = inputs
        .decls
        .iter()
        .find(|decl| decl.key.name() == "Counter")
        .expect("Counter is declared")
        .key
        .clone();
    let decls = inputs.decls.clone();
    let file = inputs.module_files[counter.module()];
    let rir = inputs.rir.rir();
    let interner = inputs.rir.semantic_symbols().interner();
    let outcome = database.probe_body_facts(
        revision,
        side_b_configuration(),
        "side-b:freeze-hook",
        move |provider| {
            let identity =
                rue_air::ProviderIdentityContext::new(DurableDeclSource::from_declarations(&decls));
            let endpoint = rue_air::ProviderEndpointFacts::with_identity(
                provider,
                identity.clone(),
                rir,
                interner,
            );
            let mut aggregate = rue_air::ProviderAggregateFacts::with_identity(identity);
            let token = endpoint.register_named_nominal(
                counter.clone(),
                file.index(),
                counter.name(),
                counter.kind(),
            );
            let ty = endpoint
                .resolve_instance_type(&rue_air::TypeInstanceKey::Nominal(
                    rue_air::NominalInstanceKey::Named(token),
                ))
                .expect("Counter mints");
            endpoint.with_type_pool(|pool| {
                assert_eq!(endpoint_display(pool, ty), "Counter");
                assert!(
                    aggregate.builtin_enum("Arch").is_some(),
                    "a shared driver may consult the identity context inside a type-pool read"
                );
            });
            let before = endpoint.type_needs_drop(ty);
            endpoint
                .finalize_containment_metadata()
                .expect("no containment cycle");
            let after_drop = endpoint.type_needs_drop(ty);
            let after_linear = endpoint.type_carries_linear(ty);
            // Repeat freeze is a pure re-finalization.
            endpoint
                .finalize_containment_metadata()
                .expect("repeat freeze");
            aggregate.register_named_nominal(counter, file, "Counter");
            let post_freeze_mint = aggregate.struct_in_file(file, "Counter");
            (before, after_drop, after_linear, post_freeze_mint)
        },
    );
    let (before, after_drop, after_linear, post_freeze_mint) = outcome.result;
    assert_eq!(before, None, "un-finalized before the freeze hook");
    assert_eq!(after_drop, Some(false), "Counter needs no drop");
    assert_eq!(after_linear, Some(false), "Counter carries no linear");
    assert_eq!(
        post_freeze_mint, None,
        "every driver sharing the context refuses mints after freeze"
    );
}

#[test]
fn comparator_reports_the_first_field_and_byte_offset() {
    let body = CapturedBody {
        case: "comparator",
        body: "main".to_owned(),
        artifact: b"abc".to_vec(),
        diagnostics: b"ordered".to_vec(),
        dependency_edges: b"edge".to_vec(),
        observation_counters: b"counter".to_vec(),
    };
    let mut changed = body.clone();
    changed.diagnostics = b"ordXred".to_vec();
    assert_eq!(
        captured_bodies_equal(&[body], &[changed]),
        Err(Divergence {
            case: "comparator".to_owned(),
            body: "main".to_owned(),
            field: "diagnostics",
            byte_offset: 3,
        })
    );
}

#[test]
fn identity_variant_inventory_is_exhaustive_and_constructible() {
    use rue_air::{
        AnonymousNominalKey, AnonymousNominalKind, CanonicalArgumentValue, CanonicalArguments,
        FunctionInstanceKey, NominalInstanceKey, StableProducerId, TypeInstanceKey,
    };

    type T = TypeInstanceKey<&'static str, &'static str>;
    let anchor = rue_rir::RirStructuralAnchor::new(Vec::new());
    let anonymous = AnonymousNominalKey {
        kind: AnonymousNominalKind::Struct,
        producer: StableProducerId::Definition("producer"),
        anchor,
        arguments: CanonicalArguments::default(),
    };
    let types: Vec<(&str, T)> = vec![
        ("type_i8", T::I8),
        ("type_i16", T::I16),
        ("type_i32", T::I32),
        ("type_i64", T::I64),
        ("type_u8", T::U8),
        ("type_u16", T::U16),
        ("type_u32", T::U32),
        ("type_u64", T::U64),
        ("type_bool", T::Bool),
        ("type_unit", T::Unit),
        ("type_never", T::Never),
        ("type_comptime_type", T::ComptimeType),
        (
            "type_builtin_nominal",
            T::BuiltinNominal {
                kind: AnonymousNominalKind::Struct,
                name: Arc::from("Builtin"),
            },
        ),
        (
            "type_nominal_builtin",
            T::Nominal(NominalInstanceKey::Builtin {
                kind: AnonymousNominalKind::Enum,
                name: Arc::from("BuiltinNominal"),
            }),
        ),
        (
            "type_nominal_named",
            T::Nominal(NominalInstanceKey::Named("Named")),
        ),
        (
            "type_nominal_anonymous",
            T::Nominal(NominalInstanceKey::Anonymous(anonymous)),
        ),
        (
            "type_array",
            T::Array {
                element: Box::new(T::I32),
                len: 2,
            },
        ),
        (
            "type_slice",
            T::Slice {
                element: Box::new(T::U8),
                name: Arc::from("slice"),
            },
        ),
        ("type_ptr_const", T::PtrConst(Box::new(T::I32))),
        ("type_ptr_mut", T::PtrMut(Box::new(T::I32))),
        ("type_module", T::Module("module")),
        ("type_generic_parameter", T::GenericParameter(0)),
    ];
    fn guard_type_inventory(value: &T) {
        match value {
            T::I8 => {}
            T::I16 => {}
            T::I32 => {}
            T::I64 => {}
            T::U8 => {}
            T::U16 => {}
            T::U32 => {}
            T::U64 => {}
            T::Bool => {}
            T::Unit => {}
            T::Never => {}
            T::ComptimeType => {}
            T::BuiltinNominal { kind, name } => {
                let _ = (kind, name);
            }
            T::Nominal(nominal) => match nominal {
                NominalInstanceKey::Builtin { kind, name } => {
                    let _ = (kind, name);
                }
                NominalInstanceKey::Named(definition) => {
                    let _ = definition;
                }
                NominalInstanceKey::Anonymous(anonymous) => {
                    let _ = anonymous;
                }
            },
            T::Array { element, len } => {
                let _ = (element, len);
            }
            T::Slice { element, name } => {
                let _ = (element, name);
            }
            T::PtrConst(pointee) => {
                let _ = pointee;
            }
            T::PtrMut(pointee) => {
                let _ = pointee;
            }
            T::Module(module) => {
                let _ = module;
            }
            T::GenericParameter(index) => {
                let _ = index;
            }
        }
    }
    for (_, value) in &types {
        guard_type_inventory(value);
    }
    assert_eq!(
        types.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        TYPE_INSTANCE_KEY_COVERAGE
            .iter()
            .map(|case| case.name)
            .collect::<Vec<_>>()
    );

    type V = CanonicalArgumentValue<&'static str, &'static str>;
    let values: Vec<(&str, V)> = vec![
        ("argument_integer", V::Integer(1)),
        ("argument_bool", V::Bool(true)),
        (
            "argument_type_module_typed_comptime_value",
            V::Type(Box::new(T::Module("module"))),
        ),
        (
            "argument_function_function_valued_comptime_arg",
            V::Function(Box::new(FunctionInstanceKey::Definition("function"))),
        ),
        ("argument_unit", V::Unit),
        ("argument_string", V::String(Arc::from("value"))),
    ];
    fn guard_argument_inventory(value: &V) {
        match value {
            V::Integer(integer) => {
                let _ = integer;
            }
            V::Bool(boolean) => {
                let _ = boolean;
            }
            V::Type(ty) => {
                let _ = ty;
            }
            V::Function(function) => {
                let _ = function;
            }
            V::Unit => {}
            V::String(string) => {
                let _ = string;
            }
        }
    }
    for (_, value) in &values {
        guard_argument_inventory(value);
    }
    assert_eq!(
        values.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        CANONICAL_ARGUMENT_VALUE_COVERAGE
            .iter()
            .map(|case| case.name)
            .collect::<Vec<_>>()
    );
    assert!(
        TYPE_INSTANCE_KEY_COVERAGE
            .iter()
            .chain(CANONICAL_ARGUMENT_VALUE_COVERAGE)
            .all(|case| case.status == CoverageStatus::Constructed)
    );
}

#[test]
fn review_carry_forwards_are_named_documented_gaps_or_closed_by_live_tests() {
    assert_eq!(
        REVIEW_CARRY_FORWARDS
            .iter()
            .map(|case| case.name)
            .collect::<Vec<_>>(),
        [
            "trusted_well_known_option_registry",
            "ambiguous_body_lookup",
            "cross_module_private_lookup",
            "unrelated_same_named_const_preserves_recorded_edges",
            "e0481_candidate_hint_records_no_resolution_edges",
            "forced_eviction_and_cancellation",
            "const_candidate_facts",
            "pool_drop_metadata_parity",
            "instance_keyed_producer_body_facts",
        ]
    );
    let closed: BTreeMap<&str, &str> = CLOSED_GAP_LIVE_TESTS.iter().copied().collect();
    for case in REVIEW_CARRY_FORWARDS {
        match case.status {
            CoverageStatus::Constructed => {
                assert!(
                    closed.contains_key(case.name),
                    "closed gap `{}` must name its live side-B test",
                    case.name
                );
            }
            CoverageStatus::Gap(reason) => {
                assert!(!reason.is_empty());
                assert!(
                    !closed.contains_key(case.name),
                    "gap `{}` cannot both remain recorded and claim a closing test",
                    case.name
                );
            }
        }
    }
    // Every named closing test corresponds to a flipped entry.
    for (name, _test) in CLOSED_GAP_LIVE_TESTS {
        let entry = REVIEW_CARRY_FORWARDS
            .iter()
            .find(|case| case.name == *name)
            .expect("closing tests reference recorded rows");
        assert_eq!(entry.status, CoverageStatus::Constructed);
    }
}
