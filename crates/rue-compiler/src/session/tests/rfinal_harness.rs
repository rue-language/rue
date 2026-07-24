//! RUE-1091 rFinal whole-body differential scaffolding.
//!
//! This module is deliberately test-only. It captures the production
//! `BodyTransaction` path today and leaves exactly one explicit slot for the
//! future ProviderFacts + overlay implementation. No production path selects
//! between analyzers.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DifferentialSide {
    Production,
    /// Replaced by the r2-r6 slices once the ProviderFacts analyzer exists.
    FutureProviderFactsOverlay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HarnessError {
    Compile(String),
    BodyNotFound(String),
    /// Intentional: this is the plug-compatible slot the provider slices fill.
    ProviderFactsAnalyzerNotImplemented,
}

trait WholeBodyAnalyzer {
    fn side(&self) -> DifferentialSide;

    fn capture(
        &self,
        case: &HarnessCase,
        requested_body_order: &[&str],
    ) -> Result<Vec<CapturedBody>, HarnessError>;
}

struct ProductionAnalyzer;

struct FutureProviderFactsOverlayAnalyzer;

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

impl WholeBodyAnalyzer for FutureProviderFactsOverlayAnalyzer {
    fn side(&self) -> DifferentialSide {
        DifferentialSide::FutureProviderFactsOverlay
    }

    fn capture(
        &self,
        _case: &HarnessCase,
        _requested_body_order: &[&str],
    ) -> Result<Vec<CapturedBody>, HarnessError> {
        Err(HarnessError::ProviderFactsAnalyzerNotImplemented)
    }
}

#[derive(Debug, Clone)]
struct HarnessCase {
    name: &'static str,
    source: &'static str,
    bodies: &'static [&'static str],
    expected_failure: bool,
}

const SHAPE_CORPUS: &[HarnessCase] = &[
    HarnessCase {
        name: "plain_functions",
        source: "fn helper() -> i32 { 40 } fn main() -> i32 { helper() + 2 }",
        bodies: &["helper", "main"],
        expected_failure: false,
    },
    HarnessCase {
        name: "method",
        source: "struct Counter { value: i32, fn get(self) -> i32 { self.value } } \
                 fn main() -> i32 { Counter { value: 3 }.get() }",
        bodies: &["get", "main"],
        expected_failure: false,
    },
    HarnessCase {
        name: "associated_function",
        source: "struct Counter { value: i32, fn make(value: i32) -> Counter { \
                 Counter { value: value } } } \
                 fn main() -> i32 { Counter.make(3).value }",
        bodies: &["make", "main"],
        expected_failure: false,
    },
    HarnessCase {
        name: "named_destructor",
        source: "struct Resource { value: i32 } \
                 drop fn Resource(self) {} \
                 fn main() { let resource = Resource { value: 1 }; }",
        bodies: &["Resource", "main"],
        expected_failure: false,
    },
    HarnessCase {
        name: "specialization",
        source: "fn choose(comptime N: i32) -> i32 { N } \
                 fn main() -> i32 { choose(42) }",
        bodies: &["choose", "main"],
        expected_failure: false,
    },
    HarnessCase {
        name: "anonymous_producer",
        source: "fn Pair() -> type { struct { a: i32, b: i32 } } \
                 fn main() -> i32 { let P = Pair(); let p: P = P { a: 1, b: 2 }; \
                 p.a + p.b }",
        bodies: &["Pair", "main"],
        expected_failure: false,
    },
    HarnessCase {
        name: "well_known_option_shape",
        // The trusted-toolchain Option registry needs a multi-file continuation
        // protocol that this no-provider scaffolding must not duplicate. This
        // freestanding structural Option shape still exercises the production
        // anonymous-enum/fallible path; the trusted registry case is recorded
        // below as a carry-forward.
        source: "fn Option(comptime T: type) -> type { enum { Some(T), None } } \
                 fn main() { let O = Option(i32); }",
        bodies: &["main"],
        expected_failure: false,
    },
    HarnessCase {
        name: "absent_lookup",
        source: "fn main() -> i32 { missing() }",
        bodies: &["main"],
        expected_failure: true,
    },
    HarnessCase {
        name: "private_lookup",
        // A same-module private item is legal; the cross-module form needs the
        // import-fixture protocol and is retained as an explicit gap below.
        source: "fn hidden() -> i32 { 1 } fn main() -> i32 { hidden() }",
        bodies: &["hidden", "main"],
        expected_failure: false,
    },
    HarnessCase {
        name: "multi_diagnostic_body",
        source: "fn main() -> i32 { missing_one(); missing_two(); true }",
        bodies: &["main"],
        expected_failure: true,
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

// Review carry-forwards that require ProviderFacts or a coordination-owned
// production hook. They remain named executable data, never prose hidden in a
// review thread.
const REVIEW_CARRY_FORWARDS: &[NamedCoverage] = &[
    NamedCoverage {
        name: "trusted_well_known_option_registry",
        status: CoverageStatus::Gap(
            "needs the future ProviderFacts toolchain-fact side, not a second test registry",
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
        status: CoverageStatus::Gap("needs the future provider-side multi-module corpus adapter"),
    },
    NamedCoverage {
        name: "unrelated_same_named_const_preserves_recorded_edges",
        status: CoverageStatus::Gap(
            "r0 carry-forward: exact provider lookup edges are unavailable until side B exists",
        ),
    },
    NamedCoverage {
        name: "e0481_candidate_hint_records_no_resolution_edges",
        status: CoverageStatus::Gap(
            "r0 carry-forward: requires provider-side diagnostic observation capture",
        ),
    },
    NamedCoverage {
        name: "forced_eviction_and_cancellation",
        status: CoverageStatus::Gap(
            "rFinal behavior test plugs into this capture after the provider analyzer exists",
        ),
    },
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ObservationCounters {
    declaration_context: rue_air::PerBodyDeclarationContextWork,
    provider: crate::unstable::ProviderObservationMetrics,
    overlay: crate::unstable::OverlayMaterializationMetrics,
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
) -> Result<CapturedBody, HarnessError> {
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
    Ok(CapturedBody {
        case,
        body: body_name.to_owned(),
        artifact,
        diagnostics,
        dependency_edges,
        observation_counters: format!("{counters:?}").into_bytes(),
    })
}

impl WholeBodyAnalyzer for ProductionAnalyzer {
    fn side(&self) -> DifferentialSide {
        DifferentialSide::Production
    }

    fn capture(
        &self,
        case: &HarnessCase,
        requested_body_order: &[&str],
    ) -> Result<Vec<CapturedBody>, HarnessError> {
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
        let provider = crate::unstable::provider_observation_metrics(&session);
        let overlay = crate::unstable::overlay_materialization_metrics(&session);
        let direct = DirectProductionInputs::build(case.source)?;
        let mut captured = Vec::new();
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
                provider,
                overlay,
            };
            captured.push(capture_terminal(
                &session,
                &key,
                case.name,
                body,
                counters,
                &directly_analyzed,
            )?);
        }
        Ok(captured)
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

#[test]
fn future_provider_facts_side_is_an_explicit_unimplemented_slot() {
    let analyzer = FutureProviderFactsOverlayAnalyzer;
    assert_eq!(
        analyzer.side(),
        DifferentialSide::FutureProviderFactsOverlay
    );
    assert_eq!(
        analyzer.capture(&SHAPE_CORPUS[0], SHAPE_CORPUS[0].bodies),
        Err(HarnessError::ProviderFactsAnalyzerNotImplemented)
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
fn review_carry_forwards_are_named_documented_gaps() {
    assert!(
        REVIEW_CARRY_FORWARDS.iter().all(|case| {
            matches!(case.status, CoverageStatus::Gap(reason) if !reason.is_empty())
        })
    );
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
        ]
    );
}
