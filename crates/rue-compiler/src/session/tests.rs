fn evict_diagnostic_index(session: &mut CompilerSession) {
    for revision in 0..=FRONTEND_DIAGNOSTIC_RETENTION_LIMIT {
        let source = snapshot(
            &[(
                91,
                "/eviction/main.rue",
                "main.rue",
                &format!("fn main() -> i32 {{ {revision} }}"),
            )],
            91,
        );
        let snapshot = Arc::new(FrontendDiagnosticSnapshot {
            source,
            stage: FrontendDiagnosticIdentity::Syntax,
            provenance: DiagnosticAttemptProvenance::Canonical,
            errors: Arc::from([]),
            warnings: Arc::from([]),
        });
        session.diagnostics.select_test_snapshot(snapshot);
    }
}

/// Publish `source` and, when it imports, commit its graph through a real
/// discovery epoch served from the fixture's own modules. The returned work
/// is the epoch's accumulated parse work, which is the parse an
/// import-bearing revision actually performs.

fn publish_with_test_imports(
    session: &mut CompilerSession,
    source: &SourceSnapshot,
) -> ParsedModulesWork {
    if !crate::test_support::fixture_has_imports(source).unwrap() {
        let update = session.update(source);
        let work = update.work();
        update.into_owner_result().unwrap();
        return work;
    }
    crate::test_support::TestDiscoveryHost::new(source)
        .unwrap()
        .drive(session)
        .unwrap()
        .parse_work
}

fn body_query_key(
    _session: &mut CompilerSession,
    options: &CompileOptions,
    name: &str,
) -> crate::body_query::BodyQueryKey {
    body_query_key_in(options, "main.rue", name)
}

fn body_query_key_in(
    options: &CompileOptions,
    module: &str,
    name: &str,
) -> crate::body_query::BodyQueryKey {
    let function =
        crate::FunctionInstanceKey::Definition(crate::StableDefinitionKey::from_stable_parts(
            crate::ModuleId::from_logical_path(module).unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            name,
            None,
        ));
    crate::body_query::BodyQueryKey::new(
        function,
        crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: options.target,
            preview_features: StablePreviewFeatures::new(&options.preview_features),
        },
    )
}

/// The comptime arguments of a specialization of the definition named
/// `source_name`, or `None` for any other callable.
///
/// A live callable name is never a source name: an ordinary definition's
/// internal symbol is module-qualified (RUE-1125) and a specialization
/// appends its argument mangling to that. Tests therefore select a
/// specialization through its durable identity.
fn specialization_arguments<'a>(
    function: &'a RootedCfgUnit,
    source_name: &str,
) -> Option<&'a crate::CanonicalArguments> {
    let crate::FunctionInstanceKey::Specialization { base, arguments } = &function.function else {
        return None;
    };
    let crate::FunctionInstanceKey::Definition(definition) = base.as_ref() else {
        return None;
    };
    (definition.name() == source_name).then_some(arguments)
}

fn retained_body_query_stamps(
    session: &CompilerSession,
    key: &crate::body_query::BodyQueryKey,
) -> (u64, u64) {
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .unwrap();
    let cancellation = rue_query::CancellationToken::new();
    let transaction = session
        .queries
        .revisioned
        .body_transaction(revision, key.clone(), cancellation.clone())
        .unwrap();
    let produced_anonymous = session
        .queries
        .revisioned
        .body_produced_anonymous_projection(revision, key.clone(), cancellation)
        .unwrap();
    (transaction.stamp(), produced_anonymous.stamp())
}

fn retained_body_transaction(
    session: &CompilerSession,
    key: &crate::body_query::BodyQueryKey,
) -> (
    u64,
    rue_query::QueryTerminalKind,
    crate::body_query::BodyTransaction,
) {
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .unwrap();
    let terminal = session
        .queries
        .revisioned
        .body_transaction(revision, key.clone(), rue_query::CancellationToken::new())
        .unwrap();
    let rue_query::QueryOutcome::Success(transaction) = terminal.outcome() else {
        unreachable!("BodyTransaction publishes typed values")
    };
    (terminal.stamp(), terminal.kind(), transaction.clone())
}

fn retained_body_closure_stamps(
    session: &CompilerSession,
    key: &crate::body_query::BodyQueryKey,
) -> (u64, u64) {
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .unwrap();
    let crate::FunctionInstanceKey::Definition(definition) = &key.instance else {
        panic!("test closure root must be an ordinary definition")
    };
    let request = session
        .queries
        .revisioned
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([definition.module().clone()]),
                roots: Arc::from([key.instance.clone()]),
                configuration: key.configuration.clone(),
            },
            rue_query::CancellationToken::new(),
        )
        .unwrap();
    let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
        unreachable!("BodyClosure publishes typed values")
    };
    let body = output
        .bodies
        .iter()
        .find(|body| body.key == *key)
        .expect("test closure contains its root body");
    (request.terminal.stamp(), body.bundle.stamp())
}

fn retained_body_source_basis(
    session: &CompilerSession,
    key: &crate::body_query::BodyQueryKey,
) -> (u64, crate::body_query::BodySourceLocator) {
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .unwrap();
    let terminal = session
        .queries
        .revisioned
        .body_source_basis_projection(revision, key.clone(), rue_query::CancellationToken::new())
        .unwrap();
    let rue_query::QueryOutcome::Success(Some(locator)) = terminal.outcome() else {
        panic!("ordinary test body has a current source locator")
    };
    (terminal.stamp(), locator.clone())
}

fn retained_body_dependency_nodes(
    session: &CompilerSession,
    key: &crate::body_query::BodyQueryKey,
) -> Vec<String> {
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .unwrap();
    session
        .queries
        .revisioned
        .body_transaction(revision, key.clone(), rue_query::CancellationToken::new())
        .unwrap()
        .dependencies()
        .iter()
        .map(|dependency| format!("{:?}", dependency.node))
        .collect()
}

/// A trusted-std `Option` snapshot for the well-known query-edge isolation
/// regression: the root is `root_source`, and the trusted std `Option` module
/// is provided at `\0rue-std/option.rue`, reached with
/// `@import("std/option.rue")` (physical-suffix match).

fn well_known_option_isolation_snapshot(root_source: &str) -> SourceSnapshot {
    well_known_option_snapshot_with_source(
        root_source,
        "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }",
    )
}

fn well_known_option_snapshot_with_source(
    root_source: &str,
    option_source: &str,
) -> SourceSnapshot {
    let root = FileId::new(1);
    let option = FileId::new(2);
    let metadata = SourceMetadata::new_with_trusted_standard_library(
        root,
        AHashMap::from([
            (root, "/project/main.rue".to_owned()),
            (option, "/project/std/option.rue".to_owned()),
        ]),
        AHashMap::from([
            (root, "main.rue".to_owned()),
            (option, "\0rue-std/option.rue".to_owned()),
        ]),
        AHashSet::from([option]),
    )
    .unwrap();
    SourceSnapshot::new(
        metadata,
        vec![
            (root, Arc::new(root_source.to_owned())),
            (option, Arc::new(option_source.to_owned())),
        ],
    )
    .unwrap()
}

use ahash::{AHashMap, AHashSet};
use std::sync::Arc;

use rue_span::FileId;

use super::*;
use crate::{OptLevel, PreviewFeature, PreviewFeatures, SourceMetadata, SourceSnapshot, Target};

#[test]
fn phase_three_module_queries_close_the_exact_import_deletion_gate() {
    let production = SESSION_SOURCE
        .split("\n#[cfg(test)]\nmod tests {")
        .next()
        .unwrap();
    assert!(!production.contains("RUE-1024 DELETION GATE"));
    let discovery = include_str!("../import_discovery.rs");
    assert!(!discovery.contains("pub fn pending_requests("));
    let revisioned = crate::revisioned_query_database::REVISIONED_DATABASE_SOURCE;
    for family in [
        "compiler.parse-module",
        "compiler.module-index",
        "compiler.lookup-name",
        "compiler.resolve-import",
        "compiler.declaration-body-plan-artifacts",
    ] {
        assert!(
            revisioned.contains(family),
            "missing canonical family {family}"
        );
    }
    assert_eq!(
        revisioned
            .matches(
                "family_with_equality_and_evaluator_and_retained_charge(\n                \
                     \"compiler.declaration-body-plan-artifacts\","
            )
            .count(),
        1,
        "candidate lowering must have one registered artifact family"
    );
    assert!(!revisioned.contains("\"compiler.module-rir\""));
    assert!(!revisioned.contains(&["ModuleRir", "Value"].concat()));
    let candidate_evaluator = include_str!(
        "../revisioned_query_database/registrations/semantic/declaration_body_plan_artifacts.rs"
    );
    assert!(candidate_evaluator.contains("lower_parsed_declaration_body_plan"));
    assert!(!candidate_evaluator.contains("lower_declaration_body_plan("));
    assert!(!candidate_evaluator.contains("lower_module_rir"));

    let canonical_lower = include_str!("../canonical_lower.rs");
    assert!(canonical_lower.contains("compose_module_rir_from_candidate_artifacts"));
    assert!(!canonical_lower.contains("pub(crate) fn lower_declaration_body_plan("));
    for oracle in ["fn lower_module_rir_with_work_internal("] {
        let before = canonical_lower.split(oracle).next().unwrap();
        assert!(
            before.trim_end().ends_with("#[cfg(test)]"),
            "former module-wide lowering helper must remain test-only: {oracle}"
        );
    }
    assert!(!revisioned.contains("ImportModuleDemand"));
    assert!(!revisioned.contains("compiler.import-module-frontier"));
    assert_eq!(
        revisioned
            .matches("RUE-1026 DELETION GATE: this selected-revision compatibility")
            .count(),
        0
    );
    let unstable = include_str!("../unstable.rs");
    assert_eq!(
        unstable
            .matches("Full-plan host compatibility adapter. RUE-1026")
            .count(),
        0
    );
}

#[test]
fn import_discovery_has_no_public_bypass_authority() {
    let discovery = include_str!("../import_discovery.rs")
        .split("\n#[cfg(test)]\nmod tests {")
        .next()
        .unwrap();
    let session = SESSION_SOURCE
        .split("\n#[cfg(test)]\nmod tests {")
        .next()
        .unwrap();
    assert!(!discovery.contains("RUE-1033 DELETION/REPLACEMENT GATE"));
    for declaration in [
        "pub fn import_discovery_plan(",
        "pub fn stage_import_discovery(",
        "pub fn close_import_discovery(",
    ] {
        assert!(
            !session.contains(declaration),
            "public import-discovery bypass returned: {declaration}"
        );
    }

    let unstable = include_str!("../unstable.rs");
    let begin = unstable
        .split_once("pub fn begin_import_input_request(")
        .unwrap()
        .1
        .split_once(") -> crate::CompileResult<ImportInputRevision>")
        .unwrap()
        .0;
    assert!(!begin.contains("ImportObservationLedger"));
    assert!(!begin.contains("carried_ledger"));
    for boundary in [
        "pub fn stage_import_input_request(",
        "pub fn close_import_input_request(",
    ] {
        assert!(
            unstable.contains(boundary),
            "canonical import boundary is missing: {boundary}"
        );
    }
}

fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
    let physical = entries
        .iter()
        .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
        .collect::<AHashMap<_, _>>();
    let logical = entries
        .iter()
        .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
        .collect::<AHashMap<_, _>>();
    let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
    SourceSnapshot::new(
        metadata,
        entries
            .iter()
            .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
            .collect(),
    )
    .unwrap()
}

#[test]
fn syntax_artifact_retains_enum_directive_child() {
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "@non_exhaustive pub enum Color { Red, Green }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    let syntax = session.update(&source).into_result().unwrap();
    let item = syntax
        .modules()
        .next()
        .expect("the syntax view has one module")
        .nodes()
        .next()
        .expect("the module has one item");
    assert_eq!(item.kind(), "enum");
    let children = item.children().collect::<Vec<_>>();
    assert_eq!(
        children.first().map(|child| child.kind()),
        Some("directive")
    );
    assert_eq!(
        children.first().and_then(|child| child.name()),
        Some("non_exhaustive")
    );
}

#[test]
fn o3_publishes_unrolled_work_for_canonical_slot_loop() {
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn main() -> i32 { let mut i: i32 = 0; while i < 3 { i = i + 1; } i }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let o2 = session
        .rooted_cfg(&CompileOptions {
            opt_level: OptLevel::O2,
            ..CompileOptions::default()
        })
        .unwrap();
    assert_eq!(o2.work().cfg.optimization_loops_unrolled, 0);
    session.update(&source).into_result().unwrap();
    let o3 = session
        .rooted_cfg(&CompileOptions {
            opt_level: OptLevel::O3,
            ..CompileOptions::default()
        })
        .unwrap();
    assert!(o3.work().cfg.optimization_loops_unrolled > 0);
    assert!(o3.work().cfg.optimization_loops_unrolled <= o3.work().cfg.optimization_loops_analyzed);
}

fn c_ffi_options() -> CompileOptions {
    CompileOptions {
        preview_features: PreviewFeatures::from([PreviewFeature::CFfi]),
        ..CompileOptions::default()
    }
}

#[test]
fn rooted_foreign_conflict_diagnostic_orders_sites_by_source_not_query_order() {
    let source = snapshot(
        &[
            (
                10,
                "/p/main.rue",
                "main.rue",
                "const a = @import(\"a.rue\");\n\
                     const b = @import(\"b.rue\");\n\
                     fn main() -> i32 { 0 }",
            ),
            (
                40,
                "/p/a.rue",
                "a.rue",
                "extern \"C\" { fn shared(x: i64) -> i64; }",
            ),
            (
                2,
                "/p/b.rue",
                "b.rue",
                "extern \"C\" { fn shared() -> bool; }",
            ),
        ],
        10,
    );
    let mut session = CompilerSession::new();
    publish_with_test_imports(&mut session, &source);
    let errors = session.rooted_cfg(&c_ffi_options()).unwrap_err();
    let error = errors
        .iter()
        .find(|error| matches!(error.kind, ErrorKind::ForeignSignatureConflict(_)))
        .unwrap_or_else(|| panic!("rooted declaration projection reports E1107: {errors:?}"));
    let ErrorKind::ForeignSignatureConflict(payload) = &error.kind else {
        unreachable!("just matched")
    };
    assert_eq!(payload.symbol, "shared");
    assert_eq!(payload.declared, "fn() -> bool");
    assert_eq!(payload.previously_declared, "fn(i64) -> i64");
    let primary = error.span().expect("E1107 has a primary declaration span");
    let first = error
        .diagnostic()
        .labels
        .iter()
        .find(|label| label.message == "first declared here")
        .expect("E1107 labels the first declaration")
        .span;
    assert!(
        (first.file_id.index(), first.start) < (primary.file_id.index(), primary.start),
        "diagnostic source order must not depend on projection traversal: {error:?}"
    );
}

#[test]
fn rooted_foreign_conflict_explains_same_spelling_with_distinct_nominals() {
    let source = snapshot(
        &[
            (
                1,
                "/p/main.rue",
                "main.rue",
                "const a = @import(\"a.rue\");\n\
                     const b = @import(\"b.rue\");\n\
                     fn main() -> i32 { 0 }",
            ),
            (
                2,
                "/p/a.rue",
                "a.rue",
                "@repr(c)\n\
                     pub struct Point { x: i32, y: i32 }\n\
                     extern \"C\" { fn takes(p: Point) -> i32; }",
            ),
            (
                3,
                "/p/b.rue",
                "b.rue",
                "@repr(c)\n\
                     pub struct Point { x: i64 }\n\
                     extern \"C\" { fn takes(p: Point) -> i32; }",
            ),
        ],
        1,
    );
    let mut session = CompilerSession::new();
    publish_with_test_imports(&mut session, &source);
    let errors = session.rooted_cfg(&c_ffi_options()).unwrap_err();
    let error = errors
        .iter()
        .find(|error| matches!(error.kind, ErrorKind::ForeignSignatureConflict(_)))
        .unwrap_or_else(|| panic!("distinct nominal identities report E1107: {errors:?}"));
    assert!(error.diagnostic().notes.iter().any(|note| {
        note.0
            .contains("spelled alike but resolve to different types")
    }));
}

fn base() -> SourceSnapshot {
    snapshot(
        &[
            (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            (2, "/p/a.rue", "a.rue", "fn a() {}"),
        ],
        7,
    )
}

#[test]
fn warm_compiler_queries_report_bounded_runtime_retention() {
    let source = base();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();

    let cold = session.unstable_metrics().retention();
    assert!(cold.retained_query_records > 0);
    assert!(cold.retained_bytes > 0);
    assert!(cold.dependency_pins > 0);
    assert!(cold.retained_bytes <= cold.retained_byte_budget);
    assert!(cold.dependency_pins <= cold.dependency_pin_budget);

    session.rooted_cfg(&options).unwrap();
    let warm = session.unstable_metrics().retention();
    assert_eq!(warm.retained_query_records, cold.retained_query_records);
    assert_eq!(warm.retained_bytes, cold.retained_bytes);
    assert_eq!(warm.dependency_pins, cold.dependency_pins);
    assert!(warm.peak_retained_bytes >= warm.retained_bytes);
    assert!(warm.peak_dependency_pins >= warm.dependency_pins);
}

#[test]
fn unstable_metrics_observe_live_query_runtime_work() {
    let source = base();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let merged = session.merge().unwrap();
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .unwrap();
    let before = session.queries.revisioned.runtime_metrics_for_test();

    session
        .queries
        .revisioned
        .projected_declaration_shells(revision, merged.ast(), rue_query::CancellationToken::new())
        .unwrap();

    let live = session.queries.revisioned.runtime_metrics_for_test();
    assert!(live.validation.traversals > before.validation.traversals);
    let observed = session.unstable_metrics().query_runtime();
    assert_eq!(observed.claims, live.claims);
    assert_eq!(observed.reuses, live.reuses);
    assert_eq!(observed.joins, live.joins);
    assert_eq!(observed.declined_joins, live.declined_joins);
    assert_eq!(observed.body_completions, live.body_completions);
    assert_eq!(observed.red_publications, live.red_publications);
    assert_eq!(observed.green_publications, live.green_publications);
    assert_eq!(observed.cancellations, live.cancellations);
    assert_eq!(observed.cycles, live.cycles);
    assert_eq!(observed.validation, live.validation.into());
    assert_eq!(observed.retention_enforcements, live.retention_enforcements);
    assert_eq!(observed.retention_scan_entries, live.retention_scan_entries);
}

#[test]
fn absent_trusted_option_parks_the_rooted_attempt_with_exact_demand_and_anchor() {
    // RUE-1112: a freestanding program whose reached `main` body uses a
    // fallible intrinsic while NO trusted std module is present. The
    // rooted attempt must park with exactly the `option.rue` demand, anchored
    // on the demanding body (`main`), and must NOT run or publish any body
    // transaction — the unsatisfied prerequisite stops the worklist before it
    // enters `body_transaction`.
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();

    let park = match session.rooted_or_toolchain_park(&CompileOptions::default()) {
        RootedParkOutcome::Parked(park) => park,
        RootedParkOutcome::Ready => {
            panic!("expected a trusted-toolchain park, got successful analysis")
        }
        RootedParkOutcome::Errors(errors) => {
            panic!("expected a trusted-toolchain park, got errors: {errors:?}")
        }
    };

    // Exact demand set: exactly the trusted std `Option` module.
    let demands: Vec<&str> = park
        .demands()
        .iter()
        .map(crate::TrustedToolchainModuleDemand::logical_path)
        .collect();
    assert_eq!(demands, vec![crate::OPTION_MODULE_LOGICAL_PATH]);

    // Exact requester anchor: the demanding body's stable key (`main`).
    assert_eq!(park.requesters().len(), 1);
    let anchor = &park.requesters()[0];
    assert_eq!(anchor.name(), "main");
    assert_eq!(anchor.kind(), crate::StableDefinitionKind::Function);

    // No body transaction ran or published a terminal.
    assert!(
        !session.queries.revisioned.any_body_transaction_terminal(),
        "the park must precede any body transaction",
    );
}

#[test]
fn already_reached_parks_batch_into_one_park_with_unioned_demands_and_anchors() {
    // RUE-1112 C2: two reached helper bodies demand different trusted modules
    // (a: parse -> Option; b: read_line -> Option+StrBuf) while no trusted std
    // is present. `main` reaches both, then the first to park must batch the
    // remaining already-reached body: ONE park carrying the UNION of absent
    // modules ([Option, StrBuf]) and BOTH requester anchors, so a single
    // successor acquisition satisfies everything.
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn a() -> i32 { let _ = @parse_i64(\"1\"); 0 }\n\
                 fn b() -> i32 { let _ = @read_line(); 0 }\n\
                 fn main() -> i32 { a() + b() }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();

    let park = match session.rooted_or_toolchain_park(&CompileOptions::default()) {
        RootedParkOutcome::Parked(park) => park,
        RootedParkOutcome::Ready => panic!("expected a batched park, got ready analysis"),
        RootedParkOutcome::Errors(errors) => {
            panic!("expected a batched park, got errors: {errors:?}")
        }
    };

    // Union of absent modules across both already-reached bodies, sorted.
    let demands: Vec<&str> = park
        .demands()
        .iter()
        .map(crate::TrustedToolchainModuleDemand::logical_path)
        .collect();
    assert_eq!(
        demands,
        vec![
            crate::OPTION_MODULE_LOGICAL_PATH,
            crate::STRBUF_MODULE_LOGICAL_PATH
        ]
    );

    // Both demanding bodies contribute a requester anchor. That both `a` and
    // `b` appear proves neither transacted before the park — each was still
    // pending and got projected into the one batch (`main`, which has no
    // fallible intrinsic, does run its transaction first, as expected).
    let anchors: std::collections::BTreeSet<&str> =
        park.requesters().iter().map(|key| key.name()).collect();
    assert_eq!(anchors, std::collections::BTreeSet::from(["a", "b"]));
}

// ---- RUE-1112: trusted-toolchain continuation + successor publication ----

fn continuation_std_context() -> crate::ImportDiscoveryContext {
    crate::ImportDiscoveryContext::new(1, "/project", Some("/sdk"), "test-policy").unwrap()
}

fn continuation_metadata() -> crate::FileMetadataFingerprint {
    crate::FileMetadataFingerprint::new(10, 20, 30)
}

/// Drive `root_source` to a canonical import-discovery close, then run the
/// rooted body-closure attempt so its park atomically attaches the demanded-missing
/// set to the closed continuation. Returns the session (now holding an
/// AUTHORIZING continuation), its token, the empty closure-witness frontier, the
/// predecessor snapshot, its accepted reads, and the assembler ready to add
/// trusted leaves. Panics unless the attempt parked — the caller supplies a
/// reached-fallible-intrinsic root whose demand set is the acquisition batch.
///
/// This exercises the real protocol (close → park → attach → mint): demand
/// authority is never seeded by direct field assignment, so a close whose
/// attempt never parks yields no token.
fn closed_continuation_for(
    root_source: &str,
) -> (
    CompilerSession,
    ClosedDiscoveryContinuation,
    crate::ImportDemandFrontier,
    SourceSnapshot,
    crate::AcceptedReadManifest,
    crate::DiscoverySourceAssembler,
) {
    let ctx = continuation_std_context();
    let mut assembler = crate::DiscoverySourceAssembler::new(
        ctx.clone(),
        "/project/main.rue",
        "/project/main.rue",
        crate::PhysicalFileIdentity::new(1, 1),
        continuation_metadata(),
        Arc::new(root_source.to_owned()),
    )
    .unwrap();
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let mut session = CompilerSession::new();
    let revision = session
        .begin_import_input_request(&snapshot, ctx.clone(), reads.clone())
        .unwrap();
    let plan = session
        .stage_import_discovery(
            &snapshot,
            ctx.clone(),
            reads.shared_slice(),
            crate::ImportObservationLedger::default(),
        )
        .unwrap();
    let roots = plan.demand_roots();
    let frontier = session
        .import_demand_frontier_for_roots(revision, &plan, crate::ImportDemandMode::Rooted, &roots)
        .unwrap();
    assert!(
        frontier.requests().is_empty(),
        "a freestanding root closes with an empty frontier",
    );
    let ledger = session.import_observation_ledger(revision).unwrap();
    session.close_import_discovery(ledger).unwrap();
    // A bare close is non-authorizing: no demand set has been attached yet.
    assert!(
        session.closed_discovery_continuation().is_none(),
        "a close mints no token until a rooted park attaches a demanded set",
    );
    // The rooted attempt parks; the park attaches its exact demanded-missing
    // set to this closed state, making the continuation authorizing.
    match session.rooted_or_toolchain_park(&CompileOptions::default()) {
        RootedParkOutcome::Parked(_) => {}
        RootedParkOutcome::Ready => {
            panic!("expected the reached fallible intrinsic to park the rooted attempt")
        }
        RootedParkOutcome::Errors(errors) => {
            panic!("expected a trusted-toolchain park, got errors: {errors:?}")
        }
    }
    let token = session
        .closed_discovery_continuation()
        .expect("an attached rooted park makes the closed continuation authorizing");
    (session, token, frontier, snapshot, reads, assembler)
}

/// The common single-module case: a reached `@parse_i64` parks on exactly the
/// trusted std `Option` module.
fn closed_continuation() -> (
    CompilerSession,
    ClosedDiscoveryContinuation,
    crate::ImportDemandFrontier,
    SourceSnapshot,
    crate::AcceptedReadManifest,
    crate::DiscoverySourceAssembler,
) {
    closed_continuation_for("fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }")
}

fn add_trusted_option(assembler: &mut crate::DiscoverySourceAssembler) {
    assembler
        .add_explicit(
            "/sdk/option.rue",
            "/sdk/option.rue",
            crate::PhysicalFileIdentity::new(2, 2),
            continuation_metadata(),
            Arc::new(
                "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }".to_owned(),
            ),
        )
        .unwrap();
}

fn add_trusted_strbuf(assembler: &mut crate::DiscoverySourceAssembler) {
    assembler
        .add_explicit(
            "/sdk/strbuf.rue",
            "/sdk/strbuf.rue",
            crate::PhysicalFileIdentity::new(3, 3),
            continuation_metadata(),
            Arc::new("pub struct StrBuf { len: i64 }".to_owned()),
        )
        .unwrap();
}

/// Stage an OPEN freestanding import discovery (no pending imports) and
/// return the staging session with its staged snapshot (RUE-1823).
fn staged_open_discovery(sources: &[(&str, &str)]) -> (CompilerSession, SourceSnapshot) {
    let ctx = continuation_std_context();
    let mut assembler = crate::DiscoverySourceAssembler::new(
        ctx.clone(),
        sources[0].0,
        sources[0].0,
        crate::PhysicalFileIdentity::new(1, 1),
        continuation_metadata(),
        Arc::new(sources[0].1.to_owned()),
    )
    .unwrap();
    for (index, (path, text)) in sources.iter().enumerate().skip(1) {
        assembler
            .add_explicit(
                *path,
                *path,
                crate::PhysicalFileIdentity::new(1 + index as u64, 1 + index as u64),
                continuation_metadata(),
                Arc::new((*text).to_owned()),
            )
            .unwrap();
    }
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let mut session = CompilerSession::new();
    session
        .stage_import_discovery(
            &snapshot,
            ctx,
            reads.shared_slice(),
            crate::ImportObservationLedger::default(),
        )
        .unwrap();
    assert!(
        session.open_discovery.is_some(),
        "staging leaves an open artifact"
    );
    (session, snapshot)
}

/// Rebuild `snapshot` from its own records — same files, identities, and
/// texts — optionally reversing the caller-supplied file order or
/// relocating every physical path under `/moved` (RUE-1823).
fn rebuilt_snapshot(snapshot: &SourceSnapshot, reverse: bool, relocate: bool) -> SourceSnapshot {
    let physical = snapshot
        .metadata()
        .physical_paths()
        .map(|(id, path)| {
            let path = if relocate {
                format!("/moved{path}")
            } else {
                path.to_owned()
            };
            (id, path)
        })
        .collect();
    let logical = snapshot
        .metadata()
        .logical_paths()
        .map(|(id, path)| (id, path.to_owned()))
        .collect();
    let metadata =
        SourceMetadata::new(snapshot.metadata().root_file_id(), physical, logical).unwrap();
    let mut files: Vec<_> = snapshot
        .files()
        .map(|view| {
            (
                view.file_id,
                snapshot.shared_source_text(view.file_id).unwrap(),
            )
        })
        .collect();
    if reverse {
        files.reverse();
    }
    SourceSnapshot::new(metadata, files).unwrap()
}

/// The stale close must reject with no published graph, diagnostics
/// snapshot replacement, continuation, or successor capability, and the
/// published snapshot must remain the replacement update's (RUE-1823).
fn assert_stale_close_rejected(session: &mut CompilerSession, replacement: &SourceSnapshot) {
    assert!(
        session.open_discovery.is_none(),
        "a replacement update supersedes the open discovery artifact"
    );
    let errors = session
        .close_import_discovery(crate::ImportObservationLedger::default())
        .unwrap_err();
    assert!(
        matches!(
            &errors.as_slice()[0].kind,
            ErrorKind::InvalidCompilerInput(reason)
                if reason.contains("no successful parsed program")
        ),
        "stale close must be rejected outright: {errors:?}"
    );
    assert!(
        session
            .published_snapshot
            .as_ref()
            .unwrap()
            .is_same_exact_snapshot(replacement),
        "the rejected close must not replace the published snapshot"
    );
    assert!(session.continuation.is_none());
    assert!(session.closed_discovery_continuation().is_none());
    assert!(session.successor_delta_nonce.is_none());
}

/// A byte-identical snapshot relocated to new physical paths shares the
/// source revision, so revision-deep invalidation would let the old open
/// artifact close over it and republish the superseded physical state.
#[test]
fn relocated_snapshot_update_supersedes_the_open_discovery_artifact() {
    let (mut session, staged) =
        staged_open_discovery(&[("/project/main.rue", "fn main() -> i32 { 0 }")]);
    let relocated = rebuilt_snapshot(&staged, false, true);
    assert_eq!(
        staged.source_revision(),
        relocated.source_revision(),
        "the relocated snapshot shares the source revision — that is the trap"
    );
    assert!(!staged.is_same_exact_snapshot(&relocated));
    session.update(&relocated).into_result().unwrap();
    assert_stale_close_rejected(&mut session, &relocated);
}

/// A byte-identical presentation-order change is likewise a replacement
/// publication: the old close would re-select the superseded order.
#[test]
fn presentation_reorder_update_supersedes_the_open_discovery_artifact() {
    let (mut session, staged) = staged_open_discovery(&[
        ("/project/main.rue", "fn main() -> i32 { 0 }"),
        ("/project/lib.rue", "pub fn value() -> i32 { 1 }"),
    ]);
    let reordered = rebuilt_snapshot(&staged, true, false);
    assert!(!staged.is_same_exact_snapshot(&reordered));
    session
        .update_for_presentation(&reordered)
        .into_result()
        .unwrap();
    assert_stale_close_rejected(&mut session, &reordered);
}

/// Ordinary content-changing updates were already an invalidation
/// boundary through the source revision; that stays true.
#[test]
fn content_changing_update_supersedes_the_open_discovery_artifact() {
    let (mut session, _) =
        staged_open_discovery(&[("/project/main.rue", "fn main() -> i32 { 0 }")]);
    let ctx = continuation_std_context();
    let mut changed_assembler = crate::DiscoverySourceAssembler::new(
        ctx,
        "/project/main.rue",
        "/project/main.rue",
        crate::PhysicalFileIdentity::new(1, 1),
        continuation_metadata(),
        Arc::new("fn main() -> i32 { 1 }".to_owned()),
    )
    .unwrap();
    let changed = changed_assembler.snapshot().unwrap();
    session.update(&changed).into_result().unwrap();
    assert_stale_close_rejected(&mut session, &changed);
}

/// An exact no-op update republishes the artifact's own snapshot, so the
/// open discovery stays valid and its close still succeeds: the
/// invalidation boundary is replacement, not republication.
#[test]
fn exact_noop_update_retains_the_open_discovery_artifact() {
    let (mut session, staged) =
        staged_open_discovery(&[("/project/main.rue", "fn main() -> i32 { 0 }")]);
    let rebuilt = rebuilt_snapshot(&staged, false, false);
    assert!(staged.is_same_exact_snapshot(&rebuilt));
    session.update(&rebuilt).into_result().unwrap();
    assert!(
        session.open_discovery.is_some(),
        "an exact no-op update must not supersede the open artifact"
    );
    let artifact = session
        .close_import_discovery(crate::ImportObservationLedger::default())
        .unwrap();
    assert!(artifact.graph().is_some());
}

#[test]
fn trusted_successor_publishes_additive_leaf_in_same_generation() {
    let (mut session, token, frontier, predecessor, _reads, mut assembler) = closed_continuation();
    let predecessor_modules = predecessor.source_revision().modules().to_vec();
    add_trusted_option(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let successor_reads = assembler.accepted_read_manifest();

    let delta = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, successor_reads)
        .expect("a strictly-additive trusted successor publishes");
    // The publish mints an opaque delta authority bound to the appended set.
    // Its module identities are private; the successor stage/close derive and
    // verify them from the snapshot, so the host cannot edit them here.
    let published = delta.revision();

    // Same request generation as the predecessor close; the frontier round
    // advances by one (a successor of that same observation epoch).
    assert_eq!(
        published.request_generation,
        frontier.revision().request_generation
    );
    assert_eq!(
        published.frontier_round,
        frontier.revision().frontier_round + 1
    );

    // Every pre-existing module leaf is preserved byte-identical. Its exact
    // ModuleRevision — and therefore its SourceId, the parse key — reappears in
    // the successor, so no pre-existing module is re-read or reparsed across
    // acquisition; only the trusted Option leaf is appended.
    for old in &predecessor_modules {
        assert!(
            successor.source_revision().modules().contains(old),
            "pre-existing module {old:?} must be preserved byte-identical",
        );
    }
    assert_eq!(
        successor.source_revision().modules().len(),
        predecessor_modules.len() + 1,
    );
}

#[test]
fn trusted_successor_reused_token_is_rejected() {
    let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
    add_trusted_option(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    // The first publish consumes the single-use token.
    session
        .publish_trusted_toolchain_successor(token.clone(), &frontier, &successor, reads.clone())
        .unwrap();
    // Reusing it finds no outstanding continuation.
    let err = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
        .unwrap_err();
    assert!(
        err.first().unwrap().to_string().contains("already used"),
        "{err:?}",
    );
}

#[test]
fn trusted_successor_stale_token_is_rejected() {
    let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
    // Simulate a newer close superseding this token: advance the outstanding
    // state's nonce so the presented token no longer matches (stale).
    session.next_continuation_nonce += 7;
    session.continuation.as_mut().unwrap().nonce = session.next_continuation_nonce;
    add_trusted_option(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let err = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
        .unwrap_err();
    assert!(
        err.first().unwrap().to_string().contains("stale"),
        "{err:?}"
    );
}

#[test]
fn trusted_successor_new_request_invalidates_the_token() {
    let (mut session, token, frontier, predecessor, reads, mut assembler) = closed_continuation();
    // A fresh import-input request invalidates any outstanding continuation.
    session
        .begin_import_input_request(&predecessor, continuation_std_context(), reads)
        .unwrap();
    add_trusted_option(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let successor_reads = assembler.accepted_read_manifest();
    let err = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, successor_reads)
        .unwrap_err();
    assert!(
        err.first().unwrap().to_string().contains("already used"),
        "{err:?}",
    );
}

#[test]
fn trusted_successor_mutated_predecessor_is_rejected() {
    let (mut session, token, frontier, _pred, _reads, _assembler) = closed_continuation();
    // A successor whose pre-existing root content differs is a mutated
    // predecessor: source evolution must be strictly additive.
    let ctx = continuation_std_context();
    let mut other = crate::DiscoverySourceAssembler::new(
        ctx,
        "/project/main.rue",
        "/project/main.rue",
        crate::PhysicalFileIdentity::new(1, 1),
        continuation_metadata(),
        Arc::new("fn main() -> i32 { 1 }".to_owned()),
    )
    .unwrap();
    add_trusted_option(&mut other);
    let successor = other.snapshot().unwrap();
    let reads = other.accepted_read_manifest();
    let err = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
        .unwrap_err();
    assert!(
        err.first()
            .unwrap()
            .to_string()
            .contains("strictly additive"),
        "{err:?}",
    );
}

#[test]
fn trusted_successor_arbitrary_module_is_rejected() {
    let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
    // StrBuf is a trusted module the park did NOT demand here (the reached
    // `@parse_i64` parks on Option only), so the added set {StrBuf} does not
    // equal the demanded set {Option} and may not ride in on this continuation.
    add_trusted_strbuf(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let err = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
        .unwrap_err();
    assert!(
        err.first()
            .unwrap()
            .to_string()
            .contains("must equal the rooted park's demanded missing set"),
        "{err:?}",
    );
}

#[test]
fn trusted_successor_ready_close_is_non_authorizing() {
    // A close whose rooted body-closure attempt is READY (no fallible intrinsic,
    // no park) attaches no demanded set, so the closed continuation mints no
    // token. Demand authority lives only in an attached park, so a ready close
    // can never inherit an earlier park's demand set and admit an uninvited
    // trusted leaf.
    let ctx = continuation_std_context();
    let mut assembler = crate::DiscoverySourceAssembler::new(
        ctx.clone(),
        "/project/main.rue",
        "/project/main.rue",
        crate::PhysicalFileIdentity::new(1, 1),
        continuation_metadata(),
        Arc::new("fn main() -> i32 { 0 }".to_owned()),
    )
    .unwrap();
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let mut session = CompilerSession::new();
    let revision = session
        .begin_import_input_request(&snapshot, ctx.clone(), reads.clone())
        .unwrap();
    let plan = session
        .stage_import_discovery(
            &snapshot,
            ctx.clone(),
            reads.shared_slice(),
            crate::ImportObservationLedger::default(),
        )
        .unwrap();
    let roots = plan.demand_roots();
    let _frontier = session
        .import_demand_frontier_for_roots(revision, &plan, crate::ImportDemandMode::Rooted, &roots)
        .unwrap();
    let ledger = session.import_observation_ledger(revision).unwrap();
    session.close_import_discovery(ledger).unwrap();
    // The rooted attempt is ready: no park, so no demanded set is attached.
    assert!(matches!(
        session.rooted_or_toolchain_park(&CompileOptions::default()),
        RootedParkOutcome::Ready
    ));
    assert!(
        session.closed_discovery_continuation().is_none(),
        "a ready close is non-authorizing and mints no continuation token",
    );
}

#[test]
fn trusted_successor_partial_batch_is_rejected_without_consuming_token() {
    // A reached `@read_line` parks on BOTH Option and StrBuf. A successor that
    // adds only Option is a partial batch — added {Option} does not equal the
    // demanded {Option, StrBuf} — so it is rejected. A rejection never consumes
    // the single-use token, so completing the batch and retrying with the same
    // token then publishes.
    let (mut session, token, frontier, _pred, _reads, mut assembler) =
        closed_continuation_for("fn main() -> i32 { let _ = @read_line(); 0 }");
    add_trusted_option(&mut assembler);
    let partial = assembler.snapshot().unwrap();
    let partial_reads = assembler.accepted_read_manifest();
    let err = session
        .publish_trusted_toolchain_successor(token.clone(), &frontier, &partial, partial_reads)
        .unwrap_err();
    assert!(
        err.first()
            .unwrap()
            .to_string()
            .contains("must equal the rooted park's demanded missing set"),
        "{err:?}",
    );
    // The token survived the rejection; completing the two-module batch and
    // retrying publishes with the SAME token.
    add_trusted_strbuf(&mut assembler);
    let full = assembler.snapshot().unwrap();
    let full_reads = assembler.accepted_read_manifest();
    session
        .publish_trusted_toolchain_successor(token, &frontier, &full, full_reads)
        .expect("the completed two-module batch publishes with the un-consumed token");
}

#[test]
fn trusted_successor_altered_predecessor_provenance_is_rejected() {
    let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
    add_trusted_option(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let full_reads = assembler.accepted_read_manifest();
    // Drop the predecessor root's accepted-read provenance, keeping only the
    // added leaf's: the old provenance is no longer byte-identical.
    let tampered: Vec<_> = full_reads
        .iter()
        .filter(|entry| entry.module().is_trusted_standard_library())
        .cloned()
        .collect();
    let err = session
        .publish_trusted_toolchain_successor(
            token,
            &frontier,
            &successor,
            crate::AcceptedReadManifest::from_entries(tampered),
        )
        .unwrap_err();
    assert!(
        err.first()
            .unwrap()
            .to_string()
            .contains("altered or removed"),
        "{err:?}",
    );
}

/// A successor-delta capability minted by one session cannot authorize a
/// successor stage on a different session: the delta is bound to its issuing
/// session, so a cross-session value is rejected without staging anything.
#[test]
fn successor_delta_from_another_session_is_rejected() {
    let (mut issuer, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
    add_trusted_option(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let delta = issuer
        .publish_trusted_toolchain_successor(token, &frontier, &successor, reads.clone())
        .expect("a strictly-additive successor publishes");

    let mut other = CompilerSession::new();
    let err = other.stage_import_discovery_successor(&delta).unwrap_err();
    assert!(
        err.first()
            .unwrap()
            .to_string()
            .contains("different session"),
        "{err:?}",
    );
}

/// A successor-delta capability is single-generation: a new import-input
/// request invalidates it, so a stale delta can neither stage nor close.
#[test]
fn stale_successor_delta_cannot_stage() {
    let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
    add_trusted_option(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let delta = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, reads.clone())
        .expect("a strictly-additive successor publishes");

    // A fresh observation generation invalidates the outstanding delta.
    session
        .begin_import_input_request(&successor, continuation_std_context(), reads.clone())
        .unwrap();
    let err = session
        .stage_import_discovery_successor(&delta)
        .unwrap_err();
    assert!(
        err.first()
            .unwrap()
            .to_string()
            .contains("no outstanding successor-delta authority"),
        "{err:?}",
    );
}

/// The successor parse terminal is a REAL runtime query dependent of the
/// exact predecessor parse terminal — the graph carries the
/// successor-after-predecessor edge — and the successor close re-selects
/// the staged terminal itself: same terminal identity, no second parse
/// dispatch, and no second empty-extension publication.
#[test]
fn successor_close_reuses_the_staged_terminal_with_a_predecessor_edge() {
    let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
    let predecessor_terminal = session
        .selected_parse_terminal()
        .expect("the committed close selects its staged parse terminal");
    add_trusted_option(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let delta = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
        .expect("a strictly-additive successor publishes");
    session
        .stage_import_discovery_successor(&delta)
        .expect("the successor stages");
    let staged_terminal = session
        .selected_parse_terminal()
        .expect("the successor stage selects its parse terminal");
    assert!(
        !Arc::ptr_eq(&predecessor_terminal, &staged_terminal),
        "the successor stage computes its own terminal"
    );
    // (a) The successor terminal observes the exact predecessor parse
    // terminal as a runtime query dependency — the FULL captured identity
    // (node, incarnation, AND stamp), not an equivalent replacement under
    // the same display node — so red/green validation and leases flow
    // successor-after-predecessor through the graph.
    let observation = staged_terminal
        .dependencies()
        .iter()
        .find(|dependency| dependency.node == *predecessor_terminal.node())
        .unwrap_or_else(|| {
            panic!(
                "the successor terminal must depend on the exact predecessor parse terminal: {:?}",
                staged_terminal.dependencies(),
            )
        });
    assert_eq!(
        observation.incarnation,
        predecessor_terminal.node_incarnation(),
        "the dependency must carry the captured terminal's exact node incarnation"
    );
    assert_eq!(
        observation.stamp,
        predecessor_terminal.stamp(),
        "the dependency must carry the captured terminal's exact stamp"
    );
    // That the adoption touched no predecessor content-key Hash/Eq is
    // proven mechanically by the rue-query frozen-key regression
    // (`adoption_never_hashes_or_compares_the_predecessor_key`).
    // (b) The close re-selects the staged terminal itself: identical
    // terminal identity, no parse dispatch, and no second publication.
    let dispatched = session.parse_modules_dispatched();
    let materialized = session.parse_sources_materialized();
    session
        .close_import_discovery_successor(&delta)
        .expect("the successor closes");
    let adopted_terminal = session
        .selected_parse_terminal()
        .expect("the successor close selects the staged parse terminal");
    assert!(
        Arc::ptr_eq(&staged_terminal, &adopted_terminal),
        "the successor close must re-select the exact staged parse terminal"
    );
    assert_eq!(
        session.parse_modules_dispatched(),
        dispatched,
        "the successor close dispatches no parse work"
    );
    assert_eq!(
        session.parse_sources_materialized(),
        materialized,
        "the successor close materializes no whole-program projection"
    );
}

/// A strictly-additive successor adoption must leave the predecessor's
/// immutable source leaf live: retained frontend terminals that correctly
/// depend on it (however many variants are prewarmed) stay valid, and the
/// acquisition contributes ZERO dependency-graph invalidation events —
/// the successor becomes current without walking or invalidating the
/// predecessor's retained downstream.
#[test]
fn successor_adoption_invalidates_no_retained_frontend_variants() {
    let acquisition_invalidations = |prewarm_retained_downstream: bool| -> u64 {
        let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
        if prewarm_retained_downstream {
            // Retain additional terminals depending on the predecessor's
            // source leaf. Semantic — and the definition/manifest variants
            // that observe it — cannot complete on this predecessor (the
            // reached fallible intrinsic parks semantic until
            // acquisition), so the retained downstream of the leaf is the
            // pre-semantic tier: merged RIR and canonical import
            // diagnostics.
            session
                .rir()
                .expect("pre-semantic RIR completes on the parked predecessor");
            session
                .import_diagnostics()
                .expect("import diagnostics retain on the closed predecessor");
        }
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let delta = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
            .expect("a strictly-additive successor publishes");
        let before = session.frontend_query_invalidations();
        session
            .stage_import_discovery_successor(&delta)
            .expect("the successor stages");
        session
            .close_import_discovery_successor(&delta)
            .expect("the successor closes");
        session.frontend_query_invalidations() - before
    };
    let bare = acquisition_invalidations(false);
    let prewarmed = acquisition_invalidations(true);
    assert_eq!(
        bare, 0,
        "additive successor adoption must not invalidate retained frontend terminals"
    );
    assert_eq!(
        prewarmed, 0,
        "additive successor adoption must not invalidate retained frontend terminals regardless of how much retained downstream depends on the predecessor leaf"
    );
}

/// A successor-delta capability outstanding across an intervening source or
/// presentation update is invalidated: the update replaced the retained
/// parse artifact the successor would extend, so the stale capability can
/// neither stage nor close — a mixed parsed program (foreign retained
/// modules under the successor's claimed source revision) is never
/// produced.
#[test]
fn intervening_presentation_update_invalidates_successor_delta() {
    let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
    add_trusted_option(&mut assembler);
    let successor = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let delta = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
        .expect("a strictly-additive successor publishes");

    // An intervening presentation update installs a successful parse of a
    // DIFFERENT snapshot (unrelated content and file order).
    let foreign = snapshot(
        &[
            (2, "/q/aux.rue", "aux.rue", "pub fn v() -> i32 { 2 }"),
            (1, "/q/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
        ],
        1,
    );
    session
        .update_for_presentation(&foreign)
        .into_result()
        .expect("the foreign presentation update parses");

    let stage_err = session
        .stage_import_discovery_successor(&delta)
        .unwrap_err();
    assert!(
        stage_err
            .first()
            .unwrap()
            .to_string()
            .contains("no outstanding successor-delta authority"),
        "{stage_err:?}",
    );
    let close_err = session
        .close_import_discovery_successor(&delta)
        .unwrap_err();
    assert!(
        close_err
            .first()
            .unwrap()
            .to_string()
            .contains("no outstanding successor-delta authority"),
        "{close_err:?}",
    );
}

/// Substituted snapshots, contexts, provenance manifests, and ledgers are
/// INEXPRESSIBLE at the successor stage/close: those APIs consume only the
/// compiler-published view and the opaque capability. The one remaining host
/// input surface on a same-generation lineage is the observation-batch
/// publication, so the tampering regressions below attack through it; the
/// overlay publication re-derives and justifies every addition, rejecting
/// each attack before anything is published.
///
/// Run one tampered batch publication against a closed lineage whose rooted
/// frontier witness is empty, returning the rejection text.
fn tampered_batch_error(
    build: impl FnOnce(&crate::ImportDiscoveryContext) -> crate::DiscoverySourceAssembler,
) -> String {
    let (mut session, _token, frontier, _pred, _reads, _assembler) = closed_continuation();
    let ctx = continuation_std_context();
    let mut tampered = build(&ctx);
    let snapshot = tampered.snapshot().unwrap();
    let reads = tampered.accepted_read_manifest();
    session
        .publish_import_observation_batch(&frontier, &snapshot, reads, Vec::new())
        .unwrap_err()
        .to_string()
}

/// A batch cannot INJECT a module: a snapshot carrying a module no accepted
/// observation of that batch resolves is rejected at publication, so an
/// unrelated module can never enter the published lineage (and therefore can
/// never reach a successor stage/close, which read only the published view).
#[test]
fn observation_batch_rejects_an_injected_module() {
    let error = tampered_batch_error(|ctx| {
        let mut assembler = crate::DiscoverySourceAssembler::new(
            ctx.clone(),
            "/project/main.rue",
            "/project/main.rue",
            crate::PhysicalFileIdentity::new(1, 1),
            continuation_metadata(),
            Arc::new("fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }".to_owned()),
        )
        .unwrap();
        // An extra module with provenance but NO justifying observation.
        add_trusted_option(&mut assembler);
        assembler
    });
    assert!(
        error.contains("must equal this step's authorized additions exactly"),
        "{error}",
    );
}

/// A batch cannot MUTATE a predecessor module under its ID: a snapshot whose
/// root module has the same identity but different content is rejected at
/// publication (the lineage is strictly additive at that boundary).
#[test]
fn observation_batch_rejects_a_mutated_predecessor_source() {
    let error = tampered_batch_error(|ctx| {
        crate::DiscoverySourceAssembler::new(
            ctx.clone(),
            "/project/main.rue",
            "/project/main.rue",
            crate::PhysicalFileIdentity::new(1, 1),
            continuation_metadata(),
            // Same root identity, DIFFERENT body.
            Arc::new("fn main() -> i32 { let _ = @parse_i64(\"1\"); 42 }".to_owned()),
        )
        .unwrap()
    });
    assert!(
        error.contains("mutates a predecessor module source"),
        "{error}",
    );
}

/// A batch cannot OMIT an accepted module: publishing the exact
/// compiler-issued accepted observation for a newly resolved module while
/// omitting that module from the successor snapshot is rejected — the
/// additions must EQUAL the batch's accepted resolutions in both
/// directions, so topology can never claim "resolved" without the module's
/// source leaf behind it.
#[test]
fn observation_batch_rejects_omitting_an_accepted_module() {
    let ctx = continuation_std_context();
    let root_source = "const a = @import(\"a.rue\"); fn main() -> i32 { 0 }";
    let mut assembler = crate::DiscoverySourceAssembler::new(
        ctx.clone(),
        "/project/main.rue",
        "/project/main.rue",
        crate::PhysicalFileIdentity::new(1, 1),
        continuation_metadata(),
        Arc::new(root_source.to_owned()),
    )
    .unwrap();
    let snapshot = assembler.snapshot().unwrap();
    let reads = assembler.accepted_read_manifest();
    let mut session = CompilerSession::new();
    let revision = session
        .begin_import_input_request(&snapshot, ctx.clone(), reads.clone())
        .unwrap();
    let plan = session
        .stage_import_discovery(
            &snapshot,
            ctx.clone(),
            reads.shared_slice(),
            crate::ImportObservationLedger::default(),
        )
        .unwrap();
    let roots = plan.demand_roots();
    let frontier = session
        .import_demand_frontier_for_roots(revision, &plan, crate::ImportDemandMode::Rooted, &roots)
        .unwrap();
    assert!(
        !frontier.requests().is_empty(),
        "an unresolved import demands host reads",
    );

    // Answer the frontier honestly for a.rue: the exact compiler-issued
    // accepted observation, absent elsewhere.
    let module_source = "pub fn value() -> i32 { 1 }";
    let observations: Vec<crate::ImportObservation> = frontier
        .requests()
        .iter()
        .map(|request| {
            if request.requested_path() == "/project/a.rue" {
                crate::ImportObservation::accepted(
                    request.clone(),
                    crate::AcceptedImportSource::new(
                        Arc::from("/project/a.rue"),
                        Arc::from("/project/a.rue"),
                        crate::PhysicalFileIdentity::new(5, 5),
                        continuation_metadata(),
                        Arc::new(module_source.to_owned()),
                    )
                    .unwrap(),
                )
                .unwrap()
            } else {
                crate::ImportObservation::absent(request.clone())
            }
        })
        .collect();

    // A manifest carrying the resolved module's provenance, but a snapshot
    // OMITTING the module itself.
    let mut with_module = crate::DiscoverySourceAssembler::new(
        ctx.clone(),
        "/project/main.rue",
        "/project/main.rue",
        crate::PhysicalFileIdentity::new(1, 1),
        continuation_metadata(),
        Arc::new(root_source.to_owned()),
    )
    .unwrap();
    with_module
        .add_explicit(
            "/project/a.rue",
            "/project/a.rue",
            crate::PhysicalFileIdentity::new(5, 5),
            continuation_metadata(),
            Arc::new(module_source.to_owned()),
        )
        .unwrap();
    let reads_with_module = with_module.accepted_read_manifest();
    let err = session
        .publish_import_observation_batch(&frontier, &snapshot, reads_with_module, observations)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("must equal this step's authorized additions exactly"),
        "{err}",
    );
}

/// A batch cannot SUBSTITUTE predecessor provenance: an accepted-read entry
/// for an existing module with altered physical identity is rejected at
/// publication.
#[test]
fn observation_batch_rejects_substituted_provenance() {
    let error = tampered_batch_error(|ctx| {
        crate::DiscoverySourceAssembler::new(
            ctx.clone(),
            "/project/main.rue",
            "/project/main.rue",
            // Same content, DIFFERENT physical identity: the module revision
            // matches but its provenance record does not.
            crate::PhysicalFileIdentity::new(7, 7),
            continuation_metadata(),
            Arc::new("fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }".to_owned()),
        )
        .unwrap()
    });
    assert!(
        error.contains("mutates a predecessor accepted-read provenance"),
        "{error}",
    );
}

#[test]
fn stable_no_filesystem_boundary_classifies_unsatisfied_toolchain_input_not_ice() {
    // RUE-1112 C3: the stable no-filesystem `rooted_cfg` entry cannot
    // acquire, so an unsatisfied trusted-toolchain demand for otherwise-valid
    // source is a deterministic CONTRACT failure, never an ICE (E9000).
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let errors = session.rooted_cfg(&CompileOptions::default()).unwrap_err();
    let error = errors.first().expect("unsatisfied toolchain input error");
    assert!(
        matches!(
            error.kind,
            rue_error::ErrorKind::UnsatisfiedTrustedToolchainInput(_)
        ),
        "expected an unsatisfied-trusted-toolchain-input classification, got {:?}",
        error.kind
    );
    assert_ne!(error.kind.code(), rue_error::ErrorCode::INTERNAL_ERROR);
    assert_eq!(
        error.kind.code(),
        rue_error::ErrorCode::UNSATISFIED_TRUSTED_TOOLCHAIN_INPUT
    );
}

#[test]
fn provider_observation_counters_record_exact_production_work() {
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn helper(x: i32) -> i32 { x + 1 } fn main() -> i32 { helper(2) }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session
        .rooted_cfg(&CompileOptions::default())
        .expect("the program analyzes");
    let metrics = crate::unstable::provider_observation_metrics(&session);
    assert_eq!(metrics.name_lookups, 3);
    assert_eq!(metrics.import_lookups, 0);
    assert_eq!(metrics.method_candidates, 0);
    assert_eq!(metrics.operator_candidates, 0);
    assert_eq!(metrics.declaration_facts, 6, "{metrics:?}");
    assert_eq!(
        metrics.identity_facts, 3,
        "one exact function payload must serve every body-transaction consumer: {metrics:?}"
    );
    assert_eq!(metrics.signature_facts, 3, "{metrics:?}");
    assert_eq!(metrics.materializations, 2, "{metrics:?}");
    assert_eq!(metrics.function_materializations, 2, "{metrics:?}");
    assert_eq!(
        metrics.function_materialization_reuses, 3,
        "the provider host must pass its already-read function payload into call-signature minting: {metrics:?}"
    );
    assert_eq!(
        metrics.materializations,
        metrics.shared_payload_materializations + metrics.owned_payload_materializations,
        "the durable materialization aggregate must be exactly partitioned by payload ownership"
    );
    assert_eq!(
        metrics.materializations,
        metrics.const_materializations
            + metrics.nominal_materializations
            + metrics.function_materializations
            + metrics.method_materializations,
        "the durable materialization aggregate must be exactly partitioned by declaration kind"
    );
    assert_eq!(
        metrics.declaration_facts,
        metrics.identity_facts + metrics.signature_facts + metrics.type_facts + metrics.const_facts,
        "the declaration aggregate must be exactly partitioned by real fact families"
    );
    assert_eq!(metrics.anonymous_facts, 0);
    assert_eq!(metrics.producer_facts, 0);
    assert_eq!(metrics.toolchain_facts, 4);
    assert_eq!(metrics.import_named_nominal_probes, 0, "{metrics:?}");
    assert_eq!(metrics.import_named_nominals_registered, 0, "{metrics:?}");
}

#[test]
fn repeated_named_imports_register_their_identity_closure_once_per_body() {
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "struct Item { value: i32 } \
                 fn identity(item: Item) -> Item { item } \
                 fn main() -> i32 { identity(Item { value: 7 }).value }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session
        .rooted_cfg(&CompileOptions::default())
        .expect("the program analyzes");

    let metrics = crate::unstable::provider_observation_metrics(&session);
    assert_eq!(
        metrics.name_lookups, 11,
        "the first Item payload must serve type minting and endpoint installation without repeating candidate/destructor lookups: {metrics:?}"
    );
    assert_eq!(metrics.declaration_facts, 10, "{metrics:?}");
    assert_eq!(metrics.identity_facts, 5, "{metrics:?}");
    assert_eq!(metrics.signature_facts, 5, "{metrics:?}");
    assert_eq!(metrics.materializations, 3, "{metrics:?}");
    assert_eq!(metrics.nominal_materializations, 1, "{metrics:?}");
    assert_eq!(metrics.nominal_materialization_reuses, 3, "{metrics:?}");
    assert_eq!(metrics.function_materializations, 2, "{metrics:?}");
    assert_eq!(
        metrics.function_materialization_reuses, 3,
        "the provider host must not re-fetch an already-read callable payload: {metrics:?}"
    );
    assert_eq!(
        metrics.materializations,
        metrics.shared_payload_materializations + metrics.owned_payload_materializations,
        "the durable materialization aggregate must be exactly partitioned by payload ownership"
    );
    assert_eq!(
        metrics.materializations,
        metrics.const_materializations
            + metrics.nominal_materializations
            + metrics.function_materializations
            + metrics.method_materializations,
        "the durable materialization aggregate must be exactly partitioned by declaration kind"
    );
    assert!(
        metrics.import_nominal_registration_requests > 0,
        "named types must enter the imported-type registration path: {metrics:?}"
    );
    assert!(
        metrics.import_named_nominal_complete_hits > 0,
        "repeated named imports in one body must hit the complete closure cache: {metrics:?}"
    );
    assert_eq!(
        metrics.import_named_nominal_probes,
        metrics.import_named_nominal_complete_hits
            + metrics.import_named_nominal_cycle_hits
            + metrics.import_named_nominals_registered,
        "every successful named probe must be a complete hit, a cycle break, or a fresh registration"
    );
}

#[test]
fn recursive_named_import_identity_closure_terminates() {
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "enum List { Nil, More(ptr const List) } \
                 fn identity(value: List) -> List { value } \
                 fn main() -> i32 { let _value = identity(List.Nil); 0 }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session
        .rooted_cfg(&CompileOptions::default())
        .expect("recursive named imports use the in-progress cycle break");
    let metrics = crate::unstable::provider_observation_metrics(&session);
    assert!(
        metrics.import_named_nominal_cycle_hits > 0,
        "the recursive List edge must exercise the in-progress cycle marker: {metrics:?}"
    );
    assert_eq!(
        metrics.import_named_nominal_probes,
        metrics.import_named_nominal_complete_hits
            + metrics.import_named_nominal_cycle_hits
            + metrics.import_named_nominals_registered,
        "every successful recursive named probe must be classified exactly once"
    );
}

#[test]
fn published_lookup_root_lease_retains_production_body_lookups() {
    // Production body analysis publishes its exact lookup-name terminals
    // into the session lease. The lease owns retention independently from
    // the request-scoped provider that observed those terminals.
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn helper(x: i32) -> i32 { x + 1 } fn main() -> i32 { helper(2) }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session
        .rooted_cfg(&CompileOptions::default())
        .expect("the program analyzes");

    let metrics = crate::unstable::lookup_pressure_metrics(&session);
    assert!(metrics.published_roots > 0, "{metrics:?}");
    assert!(metrics.leased_terminals > 0, "{metrics:?}");
    assert!(metrics.retained_logical_keys > 0, "{metrics:?}");
    assert!(metrics.retained_family_nodes > 0, "{metrics:?}");
    assert!(metrics.retained_family_terminals > 0, "{metrics:?}");
    assert_eq!(
        metrics.protected_growth, 0,
        "no lease supersession grew a family"
    );
    assert_eq!(
        metrics.evictions, 0,
        "no lease supersession evicted a terminal"
    );
    assert_eq!(metrics.rederivations_after_eviction, 0);
    // Exact lookup collection runs through the same provider-backed body
    // transaction as production analysis.
    assert!(
        crate::unstable::provider_observation_metrics(&session).name_lookups > 0,
        "production lookup terminals must be observed by the provider"
    );
}

#[test]
fn successful_closure_retires_unreachable_and_deleted_body_lookup_roots() {
    let first = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
        )],
        1,
    );
    let unreachable = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 } fn main() -> i32 { 0 }",
        )],
        1,
    );
    let deleted = snapshot(
        &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session
        .rooted_cfg(&CompileOptions::default())
        .expect("initial closure analyzes");
    let initial = crate::unstable::lookup_pressure_metrics(&session);
    assert_eq!(initial.published_roots, 2, "{initial:?}");

    session.update(&unreachable).into_result().unwrap();
    session
        .rooted_cfg(&CompileOptions::default())
        .expect("unreachable successor analyzes");
    let after_unreachable = crate::unstable::lookup_pressure_metrics(&session);
    assert_eq!(
        after_unreachable.published_roots, 1,
        "helper root must retire when it leaves the reached closure: {after_unreachable:?}"
    );

    session.update(&deleted).into_result().unwrap();
    session
        .rooted_cfg(&CompileOptions::default())
        .expect("deleted successor analyzes");
    let after_deleted = crate::unstable::lookup_pressure_metrics(&session);
    assert_eq!(
        after_deleted.published_roots, 1,
        "deleting an already-unreachable body cannot resurrect its root: {after_deleted:?}"
    );
}

#[test]
fn nested_duplicate_parameter_diagnostics_rejoin_the_exact_current_occurrence() {
    for source_text in [
        "struct S {\n    fn m(self, a: i32, a: i32) {}\n}\nfn main() {}",
        "struct S {\n    fn make(a: i32, a: i32) {}\n}\nfn main() {}",
    ] {
        let duplicate_start = source_text
            .rfind("a: i32")
            .expect("fixture contains the duplicate parameter");
        let expected = rue_span::Span::with_file(
            FileId::new(1),
            duplicate_start as u32,
            (duplicate_start + "a: i32".len()) as u32,
        );
        let source = snapshot(&[(1, "/p/main.rue", "main.rue", source_text)], 1);
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let errors = session.rooted_cfg(&CompileOptions::default()).unwrap_err();
        let error = errors.first().expect("duplicate parameter diagnostic");
        assert!(
            error.to_string().contains("duplicate parameter name 'a'"),
            "unexpected diagnostic: {error}"
        );
        assert_eq!(error.span(), Some(expected));
    }
}

#[test]
fn anonymous_comptime_producer_failure_is_a_deterministic_diagnostic() {
    let source = SourceSnapshot::single(
            "main.rue",
            "fn empty() -> type {\n    struct { }\n}\nfn main() -> i32 {\n    let E = empty();\n    0\n}",
        )
        .unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();

    let first = session
        .rooted_cfg(&CompileOptions::default())
        .expect_err("empty anonymous struct is rejected");
    let second = session
        .rooted_cfg(&CompileOptions::default())
        .expect_err("the retained producer failure remains deterministic");

    assert_eq!(first, second);
    assert_eq!(first.len(), 1, "unexpected diagnostics: {first:?}");
    let diagnostic = first.first().unwrap();
    assert!(
        matches!(&diagnostic.kind, ErrorKind::EmptyStruct),
        "producer failure must remain a source diagnostic, not request cancellation: {diagnostic:?}"
    );
}

#[test]
fn keyed_destructor_validity_preserves_production_diagnostics_and_spans() {
    for (source_text, marker, message) in [
        (
            "drop fn Missing(self) {}\nfn main() {}",
            "drop fn Missing(self) {}",
            "unknown type 'Missing' in destructor",
        ),
        (
            "struct S {}\ndrop fn S(self) {}\ndrop fn S(self) {}\nfn main() {}",
            "drop fn S(self) {}",
            "duplicate destructor for type 'S'",
        ),
    ] {
        let declaration_start = source_text
            .rfind(marker)
            .expect("fixture contains the rejected destructor");
        let expected = rue_span::Span::with_file(
            FileId::new(1),
            declaration_start as u32,
            (declaration_start + marker.len()) as u32,
        );
        let source = snapshot(&[(1, "/p/main.rue", "main.rue", source_text)], 1);
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let errors = session.rooted_cfg(&CompileOptions::default()).unwrap_err();
        let error = errors.first().expect("destructor validity diagnostic");
        assert!(
            error.to_string().contains(message),
            "unexpected diagnostic: {error}"
        );
        assert_eq!(error.span(), Some(expected));
    }
}

#[test]
fn nested_layout_change_invalidates_only_layout_consumers() {
    let first = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "struct Inner { a: i32 }\nstruct Outer { inner: Inner }\nfn consume(value: Outer) -> i32 { value.inner.a }\nfn unaffected() -> i32 { 7 }\nfn main() -> i32 { consume(Outer { inner: Inner { a: 1 } }) + unaffected() }",
        )],
        1,
    );
    let second = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "struct Inner { a: i32, b: i32 }\nstruct Outer { inner: Inner }\nfn consume(value: Outer) -> i32 { value.inner.a }\nfn unaffected() -> i32 { 7 }\nfn main() -> i32 { consume(Outer { inner: Inner { a: 1, b: 2 } }) + unaffected() }",
        )],
        1,
    );
    let options = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let consume_key = body_query_key(&mut session, &options, "consume");
    let consume_transaction = retained_body_transaction(&session, &consume_key).2;
    assert!(
        consume_transaction.references().0.iter().any(|reference| {
            matches!(
                reference,
                crate::body_query::BodyReference::Type(crate::TypeInstanceKey::Nominal(
                    crate::NominalInstanceKey::Named(definition)
                )) if definition.name() == "Inner"
            )
        }),
        "{consume_transaction:?}"
    );
    let dependency_nodes = retained_body_dependency_nodes(&session, &consume_key);
    assert!(
        dependency_nodes
            .iter()
            .any(|node| node.contains("signature") && node.contains("Inner")),
        "{dependency_nodes:?}"
    );
    session.update(&second).into_result().unwrap();
    let warm = session.rooted_cfg(&options).unwrap();
    assert_eq!(warm.work().cfg.cfg_reuses, 1);
    assert_eq!(warm.work().cfg.cfg_builds_attempted, 2);
    let mut fresh = CompilerSession::new();
    fresh.update(&second).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&options).unwrap();
    assert_eq!(
        format!("{:?}", warm.functions()),
        format!("{:?}", fresh.functions())
    );
}

#[test]
fn pointer_only_consumer_ignores_pointee_layout_but_field_consumer_rebuilds() {
    let first = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "struct Foo { a: i32 }\nfn pointer_only(value: ptr const Foo) -> i32 { 7 }\nfn field(value: Foo) -> i32 { value.a }\nfn main() -> i32 { let value = Foo { a: 1 }; checked { pointer_only(@raw(value)) + field(value) } }",
        )],
        1,
    );
    let second = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "struct Foo { a: i32, b: i32 }\nfn pointer_only(value: ptr const Foo) -> i32 { 7 }\nfn field(value: Foo) -> i32 { value.a }\nfn main() -> i32 { let value = Foo { a: 1, b: 2 }; checked { pointer_only(@raw(value)) + field(value) } }",
        )],
        1,
    );
    let options = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    session.update(&second).into_result().unwrap();
    let warm = session.rooted_cfg(&options).unwrap();
    assert_eq!(warm.work().cfg.cfg_reuses, 1);
    assert_eq!(warm.work().cfg.cfg_builds_attempted, 2);
    let mut fresh = CompilerSession::new();
    fresh.update(&second).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&options).unwrap();
    assert_eq!(
        format!("{:?}", warm.functions()),
        format!("{:?}", fresh.functions())
    );
}

#[test]
fn cfg_reuse_is_per_function_and_preserves_exact_build_work() {
    let first = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn a() -> i32 { @dbg(\"same\"); @dbg(\"same\"); @dbg(\"alpha\"); 1 }\n\
                 fn b() -> i32 { @dbg(\"beta\"); 2 }\n\
                 fn main() -> i32 { @dbg(\"gamma\"); a() + b() }",
        )],
        1,
    );
    let second = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "// move every retained body and perturb another body's string projection\n\
                 fn a() -> i32 { @dbg(\"same\"); @dbg(\"same\"); @dbg(\"alpha\"); 1 }\n\
                 fn b() -> i32 { @dbg(\"delta\"); @dbg(\"beta\"); 3 }\n\
                 fn main() -> i32 { @dbg(\"gamma\"); a() + b() }",
        )],
        1,
    );
    let options = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    let cold = session.rooted_cfg(&options).unwrap();
    assert_eq!(cold.work().cfg.cfg_builds_attempted, 3);
    assert_eq!(cold.work().cfg.optimization_attempts, 3);
    assert_eq!(cold.work().cfg.optimization_loops_analyzed, 0);
    assert_eq!(cold.work().cfg.optimization_loops_unrolled, 0);
    assert_eq!(cold.work().cfg.optimization_budget_refusals, 0);
    let cold_atoms = cold
        .functions()
        .iter()
        .flat_map(|function| function.record.local_atoms.iter())
        .collect::<Vec<_>>();
    assert_eq!(cold_atoms.len(), 5);
    assert_eq!(
        cold_atoms
            .iter()
            .filter(|atom| atom.content.as_ref() == "same")
            .count(),
        2
    );
    for function in cold.functions() {
        for atom in function.record.local_atoms.iter() {
            assert_eq!(
                function
                    .record
                    .strings
                    .get(atom.dense_id as usize)
                    .map(String::as_str),
                Some(atom.content.as_ref())
            );
        }
    }
    session.update(&second).into_result().unwrap();
    let warm = session.rooted_cfg(&options).unwrap();
    assert_eq!(
        warm.work().cfg.cfg_reuses,
        2,
        "cfg work: {:?}",
        warm.work().cfg
    );
    assert_eq!(warm.work().cfg.cfg_builds_attempted, 1);
    assert_eq!(warm.work().cfg.optimization_attempts, 1);
    assert_eq!(warm.work().cfg.optimized_level_attempts, 1);
    for function in warm.functions() {
        for atom in function.record.local_atoms.iter() {
            assert_eq!(
                function
                    .record
                    .strings
                    .get(atom.dense_id as usize)
                    .map(String::as_str),
                Some(atom.content.as_ref())
            );
        }
    }

    let mut fresh = CompilerSession::new();
    fresh.update(&second).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&options).unwrap();
    assert_eq!(
        format!("{:?}", warm.functions()),
        format!("{:?}", fresh.functions())
    );
    assert_eq!(
        format!("{:?}", warm.warnings()),
        format!("{:?}", fresh.warnings())
    );
}

#[test]
fn cfg_local_materialization_preserves_body_callable_names() {
    let source = SourceSnapshot::single(
        "main.rue",
        "fn probe() -> u32 { @random_u32() }\nfn main() { probe(); }",
    )
    .unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let semantic = session.rooted_cfg(&CompileOptions::default()).unwrap();
    let cfg_work = semantic.work().cfg;
    assert_eq!(cfg_work.materialization_index_builds, 1, "{cfg_work:?}");
    assert_eq!(
        cfg_work.materialization_fact_selections,
        cfg_work.functions_considered + cfg_work.drop_glue_functions_synthesized,
        "{cfg_work:?}"
    );
    assert_eq!(
        cfg_work.local_epochs, cfg_work.materialization_fact_selections,
        "one rooted CFG record reports one local semantic epoch: {cfg_work:?}"
    );
    assert_eq!(
        cfg_work.materialization_declarations_scanned, 0,
        "CFG selection reuses the semantic projection's declaration index: {cfg_work:?}"
    );
    assert_eq!(
        cfg_work.materialization_type_nodes_scanned, 0,
        "CFG selection reuses the semantic projection's destructor index: {cfg_work:?}"
    );
    assert!(cfg_work.local_air_payload_bytes > 0, "{cfg_work:?}");
    assert!(cfg_work.local_type_entries > 0, "{cfg_work:?}");
    assert!(cfg_work.local_interner_entries > 0, "{cfg_work:?}");
    assert!(
        cfg_work.local_interner_utf8_bytes >= cfg_work.local_interner_entries,
        "{cfg_work:?}"
    );
    assert_eq!(
        cfg_work.retained_interner_charge_scans, 0,
        "logical interner charge is maintained during insertion, not reconstructed at CFG publication: {cfg_work:?}"
    );
    assert_eq!(
        cfg_work.prerequisite_drop_glue_requests, 0,
        "a CFG containing only statically dropless types needs no drop-glue terminals: {cfg_work:?}"
    );
    assert_eq!(
        cfg_work.retained_interner_entries_scanned, 0,
        "{cfg_work:?}"
    );
    assert_eq!(
        cfg_work.retained_interner_utf8_bytes_scanned, 0,
        "{cfg_work:?}"
    );
    let names = semantic
        .functions()
        .iter()
        .map(|function| {
            (
                function.record.source_name.as_ref(),
                function.record.cfg.fn_name(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        names.get("__rue_fn_main_2erue__probe"),
        Some(&"__rue_fn_main_2erue__probe")
    );
    assert_eq!(names.get("main"), Some(&"main"));
}

#[test]
fn cfg_drop_facts_invalidate_same_layout_cleanup_changes() {
    let first = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "struct Resource { value: i32 }\n\
                 fn main() -> i32 { let resource = Resource { value: 1 }; 0 }",
        )],
        1,
    );
    let second = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "struct Resource { value: i32 }\n\
                 drop fn Resource(self) { @dbg(self.value); }\n\
                 fn main() -> i32 { let resource = Resource { value: 1 }; 0 }",
        )],
        1,
    );
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();

    session.update(&second).into_result().unwrap();
    let warm = session.rooted_cfg(&options).unwrap();
    assert!(
        warm.work().cfg.cfg_builds_attempted > 0,
        "same-layout drop-fact changes must rebuild affected CFGs: {:?}",
        warm.work().cfg
    );
    let mut fresh = CompilerSession::new();
    fresh.update(&second).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&options).unwrap();
    assert_eq!(
        normalize_session_local_spurs(format!("{:?}", warm.functions())),
        normalize_session_local_spurs(format!("{:?}", fresh.functions()))
    );
}

#[test]
fn cfg_relocation_covers_runtime_param_drop_and_nominal_field_domains() {
    let program = |prefix: &str| {
        format!(
            "{prefix}\
                 struct Leaf {{ value: i32 }}\n\
                 drop fn Leaf(self) {{ @dbg(self.value); }}\n\
                 struct Holder {{ leaf: Leaf }}\n\
                 fn consume(value: Holder) -> i32 {{\n\
                     @dbg(value.leaf.value);\n\
                     value.leaf.value\n\
                 }}\n\
                 fn main() -> i32 {{ consume(Holder {{ leaf: Leaf {{ value: 7 }} }}) }}"
        )
    };
    let first_text = program("");
    let second_text = program(
        "struct Noise { pad: i64 }\n\
             fn noise(value: Noise) -> i64 { @assert(value.pad >= 0); value.pad }\n",
    );
    let first = snapshot(&[(1, "/p/main.rue", "main.rue", first_text.as_str())], 1);
    let second = snapshot(&[(1, "/p/main.rue", "main.rue", second_text.as_str())], 1);
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();

    session.update(&second).into_result().unwrap();
    let warm = session.rooted_cfg(&options).unwrap();
    assert_eq!(
        warm.work().cfg.cfg_builds_attempted,
        0,
        "live-domain relocation must not rebuild reusable unoptimized CFGs: {:?}",
        warm.work().cfg
    );
    assert_eq!(
        warm.work().cfg.cfg_reuses,
        5,
        "every unchanged reachable CFG must be reused: {:?}",
        warm.work().cfg
    );
    assert_eq!(
        warm.work().cfg.optimization_attempts,
        0,
        "complete relocation domains must reuse optimized terminals: {:?}",
        warm.work().cfg
    );
    let mut fresh = CompilerSession::new();
    fresh.update(&second).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&options).unwrap();
    assert_eq!(
        normalize_session_local_spurs(format!("{:?}", warm.functions())),
        normalize_session_local_spurs(format!("{:?}", fresh.functions()))
    );
    assert_eq!(
        format!("{:?}", warm.warnings()),
        format!("{:?}", fresh.warnings())
    );
}

#[test]
fn cfg_terminal_owned_domain_relocates_without_rebuild() {
    let first = snapshot(
        &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 7 }")],
        1,
    );
    let second = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "// relocate the retained body\nfn main() -> i32 { 7 }",
        )],
        1,
    );
    let options = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();

    session.update(&second).into_result().unwrap();
    let warm = session.rooted_cfg(&options).unwrap();
    assert_eq!(warm.work().cfg.cfg_reuse_candidates, 1);
    assert_eq!(warm.work().cfg.cfg_reuses, 1);
    assert_eq!(warm.work().cfg.cfg_builds_attempted, 0);
    assert_eq!(warm.work().cfg.cfg_builds_succeeded, 0);
    assert_eq!(warm.work().cfg.cfg_builds_failed, 0);
    assert_eq!(warm.work().cfg.optimization_attempts, 0);
    assert_eq!(warm.work().cfg.optimization_completions, 0);
    assert_eq!(warm.work().cfg.optimized_level_attempts, 0);

    let mut fresh = CompilerSession::new();
    fresh.update(&second).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&options).unwrap();
    assert_eq!(
        normalize_session_local_spurs(format!("{:?}", warm.functions())),
        normalize_session_local_spurs(format!("{:?}", fresh.functions()))
    );
}

#[test]
fn reused_parse_runtime_symbol_relocates_to_the_current_interner() {
    let program = |prefix: &str| {
        format!(
            "{prefix}\
                 const opt = @import(\"std/option.rue\");\n\
                 fn parse_runtime() -> i32 {{ let _ = @parse_i64(\"7\"); 0 }}\n\
                 fn main() -> i32 {{ parse_runtime() }}"
        )
    };
    let first_text = program("");
    let second_text =
        program("struct Noise { value: i64 }\nfn noise(value: Noise) -> i64 { value.value }\n");
    let first = well_known_option_isolation_snapshot(&first_text);
    let second = well_known_option_isolation_snapshot(&second_text);
    let options = CompileOptions::default();
    let runtime_symbols = |output: &crate::RootedCfgOutput| {
        output
            .functions()
            .iter()
            .flat_map(|function| {
                let cfg = &function.record.cfg;
                cfg.blocks()
                    .iter()
                    .flat_map(|block| block.insts.iter())
                    .filter_map(|value| match cfg.get_inst(*value).data {
                        rue_cfg::CfgInstData::Intrinsic {
                            runtime: Some(rue_air::RuntimeCallKind::ParseI64),
                            name,
                            ..
                        } => Some(function.record.interner.resolve(&name).to_owned()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };

    let mut session = CompilerSession::new();
    publish_with_test_imports(&mut session, &first);
    session.rooted_cfg(&options).unwrap();
    publish_with_test_imports(&mut session, &second);
    let warm = session.rooted_cfg(&options).unwrap();
    assert_eq!(warm.work().cfg.cfg_builds_attempted, 0);
    assert!(warm.work().cfg.cfg_reuses >= 2, "{:?}", warm.work().cfg);
    assert_eq!(
        warm.work().cfg.optimization_attempts,
        0,
        "the collector must relocate the complete optimized terminal: {:?}",
        warm.work().cfg
    );
    let warm_symbols = runtime_symbols(&warm);
    assert_eq!(warm_symbols.len(), 1);
    assert_eq!(warm_symbols[0], "parse_i64");

    let mut fresh = CompilerSession::new();
    publish_with_test_imports(&mut fresh, &second);
    let fresh_output = fresh.rooted_cfg(&options).unwrap();
    let fresh_symbols = runtime_symbols(&fresh_output);
    assert_eq!(fresh_symbols.len(), 1);
    assert_eq!(fresh_symbols[0], "parse_i64");
    assert_eq!(
        normalize_session_local_spurs(format!("{:?}", warm.functions())),
        normalize_session_local_spurs(format!("{:?}", fresh_output.functions()))
    );
}

#[test]
fn reused_print_runtime_call_relocates_to_the_current_helper_symbol() {
    let program = |prefix: &str| {
        format!(
            "{prefix}\
                 fn probe_print() {{ println(\"literal\"); }}\n\
                 fn main() -> i32 {{ probe_print(); 0 }}"
        )
    };
    let runtime_calls = |output: &crate::RootedCfgOutput| {
        output
            .functions()
            .iter()
            .flat_map(|function| {
                let cfg = &function.record.cfg;
                cfg.blocks()
                    .iter()
                    .flat_map(|block| block.insts.iter())
                    .filter_map(|value| match cfg.get_inst(*value).data {
                        rue_cfg::CfgInstData::Call {
                            runtime: Some(runtime),
                            name,
                            ..
                        } => Some((runtime, function.record.interner.resolve(&name).to_owned())),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let first_text = program("");
    let second_text = program("fn noise() -> i32 { let interner_churn = 1; interner_churn }\n");
    let first = snapshot(&[(1, "/p/main.rue", "main.rue", &first_text)], 1);
    let second = snapshot(&[(1, "/p/main.rue", "main.rue", &second_text)], 1);
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    let cold = session.rooted_cfg(&options).unwrap();
    let cold_calls = runtime_calls(&cold);
    assert_eq!(cold_calls.len(), 1);
    session.update(&second).into_result().unwrap();
    let warm = session.rooted_cfg(&options).unwrap();
    assert_eq!(warm.work().cfg.cfg_builds_attempted, 0);
    assert_eq!(warm.work().cfg.cfg_reuses, 2);
    assert_eq!(warm.work().cfg.optimization_attempts, 0);
    let warm_calls = runtime_calls(&warm);
    assert_eq!(warm_calls.len(), 1);
    assert_eq!(
        cold_calls, warm_calls,
        "function-local domains keep stable runtime-call spellings"
    );
    for (runtime, symbol) in &warm_calls {
        assert_eq!(symbol, runtime.helper().helper().symbol);
    }

    let mut fresh = CompilerSession::new();
    fresh.update(&second).into_result().unwrap();
    let fresh_output = fresh.rooted_cfg(&options).unwrap();
    let fresh_calls = runtime_calls(&fresh_output);
    assert_eq!(fresh_calls.len(), 1);
    for (runtime, symbol) in &fresh_calls {
        assert_eq!(symbol, runtime.helper().helper().symbol);
    }
    assert_eq!(
        normalize_session_local_spurs(format!("{:?}", warm.functions())),
        normalize_session_local_spurs(format!("{:?}", fresh_output.functions()))
    );
}

#[test]
fn opt_level_only_change_reuses_cfg_and_recomputes_optimization_per_function() {
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn left() -> i32 { 20 }\n\
                 fn right() -> i32 { 22 }\n\
                 fn main() -> i32 { left() + right() }",
        )],
        1,
    );
    let o0 = CompileOptions {
        opt_level: OptLevel::O0,
        ..CompileOptions::default()
    };
    let o1 = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();

    let cold = session.rooted_cfg(&o0).unwrap();
    assert_eq!(cold.functions().len(), 3);
    assert_eq!(cold.work().cfg.cfg_builds_attempted, 3);
    assert_eq!(cold.work().cfg.optimization_attempts, 3);

    let optimized = session.rooted_cfg(&o1).unwrap();
    assert_eq!(optimized.functions().len(), 3);
    assert_eq!(optimized.work().cfg.cfg_builds_attempted, 0);
    assert_eq!(optimized.work().cfg.cfg_builds_succeeded, 0);
    assert_eq!(optimized.work().cfg.cfg_builds_failed, 0);
    assert_eq!(optimized.work().cfg.cfg_reuses, 3);
    assert_eq!(optimized.work().cfg.optimization_attempts, 3);
    assert_eq!(optimized.work().cfg.optimization_completions, 3);
    assert_eq!(optimized.work().cfg.optimized_level_attempts, 3);
}

#[test]
fn specialized_cfg_reuse_is_stable_across_unrelated_body_edits() {
    let first = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn choose(comptime n: i32) -> i32 { n }\nfn b() -> i32 { 2 }\nfn main() -> i32 { choose(40) + b() }",
        )],
        1,
    );
    let second = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn choose(comptime n: i32) -> i32 { n }\nfn b() -> i32 { 3 }\nfn main() -> i32 { choose(40) + b() }",
        )],
        1,
    );
    let options = CompileOptions {
        opt_level: OptLevel::O0,
        ..CompileOptions::default()
    };
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    session.update(&second).into_result().unwrap();
    let warm = session.rooted_cfg(&options).unwrap();
    assert!(warm.functions().iter().any(|function| {
        matches!(
            function.function,
            crate::FunctionInstanceKey::Specialization { .. }
        )
    }));
}

#[test]
#[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
fn platform_native_cfg_reuse_rejects_target_and_callable_identity_changes() {
    let first = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn old() -> i32 { 1 }\nfn stable() -> i32 { 2 }\nfn main() -> i32 { old() + stable() }",
        )],
        1,
    );
    let renamed = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn new() -> i32 { 1 }\nfn stable() -> i32 { 2 }\nfn main() -> i32 { new() + stable() }",
        )],
        1,
    );
    let host = Target::host().unwrap();
    let other = if host == Target::Aarch64Linux {
        Target::X86_64Linux
    } else {
        Target::Aarch64Linux
    };
    let first_options = CompileOptions {
        target: host,
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let other_options = CompileOptions {
        target: other,
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&first_options).unwrap();
    let cross_target = session.rooted_cfg(&other_options).unwrap();
    assert_eq!(cross_target.work().cfg.cfg_reuses, 0);
    assert_eq!(cross_target.work().cfg.cfg_builds_attempted, 3);
    session.update(&renamed).into_result().unwrap();
    let changed = session.rooted_cfg(&other_options).unwrap();
    assert_eq!(changed.work().cfg.cfg_reuses, 1);
    assert_eq!(changed.work().cfg.cfg_builds_attempted, 2);
    let mut fresh = CompilerSession::new();
    fresh.update(&renamed).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&other_options).unwrap();
    assert_eq!(
        format!("{:?}", changed.functions()),
        format!("{:?}", fresh.functions())
    );
}

#[test]
fn value_constants_install_from_the_semantic_nucleus_without_fallback() {
    let first = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "const n: i32 = 1; fn main() -> i32 { n }",
        )],
        1,
    );
    let second = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "const n: i32 = 1; fn main() -> i32 { n + 1 }",
        )],
        1,
    );
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let main = body_query_key(&mut session, &options, "main");
    let (first_stamp, _, first_transaction) = retained_body_transaction(&session, &main);
    session.update(&second).into_result().unwrap();
    let output = session.rooted_cfg(&options).unwrap();
    let (second_stamp, _, second_transaction) = retained_body_transaction(&session, &main);
    assert_ne!(first_stamp, second_stamp);
    assert!(matches!(
        first_transaction,
        crate::body_query::BodyTransaction::Success { .. }
    ));
    assert!(matches!(
        second_transaction,
        crate::body_query::BodyTransaction::Success { .. }
    ));
    assert_eq!(output.work().body_analysis.body_analyses_computed, 1);
}

fn assert_rooted_cfg_parity(
    session: &CompilerSession,
    actual: &RootedCfgOutput,
    fresh: &RootedCfgOutput,
) {
    assert_eq!(
        normalize_session_local_spurs(format!("{:?}", actual.functions())),
        normalize_session_local_spurs(format!("{:?}", fresh.functions()))
    );
    assert_eq!(
        actual.string_domains().collect::<Vec<_>>(),
        fresh.string_domains().collect::<Vec<_>>()
    );
    assert_eq!(
        format!("{:?}", actual.warnings()),
        format!("{:?}", fresh.warnings())
    );
    let diagnostics = session
        .diagnostics
        .latest()
        .expect("semantic query publishes diagnostics");
    assert!(diagnostics.is_success());
    assert_eq!(
        format!("{:?}", diagnostics.warnings()),
        format!("{:?}", fresh.warnings())
    );
}

fn normalize_session_local_spurs(value: String) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut rest = value.as_str();
    while let Some(start) = rest.find("Spur(") {
        normalized.push_str(&rest[..start]);
        normalized.push_str("Spur(_)");
        let after = &rest[start + "Spur(".len()..];
        let Some(end) = after.find(')') else {
            normalized.push_str(after);
            return normalized;
        };
        rest = &after[end + 1..];
    }
    normalized.push_str(rest);
    normalized
}

fn assert_body_artifact_parity(actual: &RootedCfgOutput, fresh: &RootedCfgOutput) {
    assert_eq!(
        format!("{:?}", actual.functions()),
        format!("{:?}", fresh.functions())
    );
    assert_eq!(
        actual.string_domains().collect::<Vec<_>>(),
        fresh.string_domains().collect::<Vec<_>>()
    );
    assert_eq!(
        format!("{:?}", actual.warnings()),
        format!("{:?}", fresh.warnings())
    );
    assert_eq!(actual.type_pool_stats(), fresh.type_pool_stats());
}

fn assert_diagnostic_parity(actual: &CompilerSession, fresh: &CompilerSession) {
    let actual = actual.diagnostics.latest().unwrap();
    let fresh = fresh.diagnostics.latest().unwrap();
    assert_eq!(
        format!("{:?}", actual.stage()),
        format!("{:?}", fresh.stage())
    );
    assert_eq!(
        format!("{:?}", actual.errors()),
        format!("{:?}", fresh.errors())
    );
    assert_eq!(
        format!("{:?}", actual.warnings()),
        format!("{:?}", fresh.warnings())
    );
}

#[test]
fn semantic_failure_preserves_the_last_good_semantic_diagnostic_baseline() {
    let good = snapshot(
        &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
        1,
    );
    let broken = snapshot(
        &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { missing }")],
        1,
    );
    let recovered = snapshot(
        &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }")],
        1,
    );
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&good).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let baseline = session
        .diagnostics
        .last_good_semantic()
        .cloned()
        .expect("successful semantic query publishes a last-good baseline");
    assert!(baseline.is_success());

    session.update(&broken).into_result().unwrap();
    session.rooted_cfg(&options).unwrap_err();
    let retained = session
        .diagnostics
        .last_good_semantic()
        .cloned()
        .expect("semantic failure retains the last-good baseline");
    assert!(
        Arc::ptr_eq(&retained, &baseline),
        "a semantic failure must never replace the last-good semantic baseline"
    );

    session.update(&recovered).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let updated = session
        .diagnostics
        .last_good_semantic()
        .cloned()
        .expect("recovery publishes a new last-good baseline");
    assert!(updated.is_success());
    assert!(
        !Arc::ptr_eq(&updated, &baseline),
        "recovery must advance the last-good semantic baseline"
    );
}

#[test]
fn injected_diagnostic_oracle_fault_diverges_from_a_fresh_session() {
    let source = snapshot(
        &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
        1,
    );
    let options = CompileOptions::default();
    let mut warm = CompilerSession::new();
    warm.update(&source).into_result().unwrap();
    warm.rooted_cfg(&options).unwrap();
    let mut fresh = CompilerSession::new();
    fresh.update(&source).into_result().unwrap();
    fresh.rooted_cfg(&options).unwrap();
    assert_diagnostic_parity(&warm, &fresh);

    assert!(
        warm.inject_stale_query_for_oracle(crate::unstable::DifferentialOracleFault::Diagnostic)
    );
    let corrupted = warm.diagnostics.latest().unwrap();
    assert!(
        !corrupted.is_success(),
        "the injected fault selects a distinct failing canonical attempt"
    );
    assert!(fresh.diagnostics.latest().unwrap().is_success());
}

#[test]
fn composite_generic_signature_reuses_across_relocation_and_specialization_edit() {
    let source = |file, physical: &str, value| {
        snapshot(
            &[(
                file,
                physical,
                "main.rue",
                &format!(
                    "fn first(comptime T: type, values: [[T; 2]; 2]) -> T {{ values[0][0] }} fn main() -> i32 {{ first(i32, [[1, 2], [3, {value}]]) }}"
                ),
            )],
            file,
        )
    };
    let first = source(1, "/old/main.rue", 4);
    let relocated_edit = source(99, "/new/main.rue", 5);
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();

    session.update(&relocated_edit).into_result().unwrap();
    let reused = session.rooted_cfg(&options).unwrap();
    assert!(reused.work().body_analysis.body_analyses_computed > 0);

    let mut fresh = CompilerSession::new();
    fresh.update(&relocated_edit).into_result().unwrap();
    let ordinary = fresh.rooted_cfg(&options).unwrap();
    assert_rooted_cfg_parity(&session, &reused, &ordinary);
    assert_diagnostic_parity(&session, &fresh);
}

#[test]
fn semantic_nucleus_installs_nested_generic_signatures_without_fallback() {
    let source = |value| {
        snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                &format!(
                    "fn first(comptime T: type, values: [[T; 2]; 2]) -> T {{ values[0][0] }} fn main() -> i32 {{ first(i32, [[1, 2], [3, {value}]]) }}"
                ),
            )],
            1,
        )
    };
    let first = source(4);
    let edited = source(5);
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();

    session.update(&edited).into_result().unwrap();
    let actual = session.rooted_cfg(&options).unwrap();
    assert_eq!(actual.work().body_analysis.body_analyses_computed, 1);

    let mut fresh = CompilerSession::new();
    fresh.update(&edited).into_result().unwrap();
    let ordinary = fresh.rooted_cfg(&options).unwrap();
    assert_rooted_cfg_parity(&session, &actual, &ordinary);
    assert_eq!(actual.type_pool_stats(), ordinary.type_pool_stats());
    assert_diagnostic_parity(&session, &fresh);
}

#[test]
fn comptime_named_method_reuses_declarations_while_body_reuse_fails_closed() {
    let source = |body: &str| snapshot(&[(1, "/p/main.rue", "main.rue", body)], 1);
    let first = source(
        "struct Value { fn choose(borrow self, comptime n: i32) -> i32 { n } } fn main() -> i32 { let value = Value {}; value.choose(1) }",
    );
    let edited = source(
        "struct Value { fn choose(borrow self, comptime n: i32) -> i32 { n + 1 } } fn main() -> i32 { let value = Value {}; value.choose(1) }",
    );
    let supported = source("fn main() -> i32 { 1 }");
    let supported_edit = source("fn main() -> i32 { 2 }");
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    let cold = session.rooted_cfg(&options).unwrap();
    assert!(cold.work().body_analysis.body_analyses_computed > 0);

    session.update(&edited).into_result().unwrap();
    let ordinary = session.rooted_cfg(&options).unwrap();
    assert!(ordinary.work().body_analysis.body_analyses_computed > 0);
    let mut fresh = CompilerSession::new();
    fresh.update(&edited).into_result().unwrap();
    let expected = fresh.rooted_cfg(&options).unwrap();
    assert_rooted_cfg_parity(&session, &ordinary, &expected);

    // Moving to a different declaration universe seeds a new baseline, and
    // its next body edit can reuse normally.
    session.update(&supported).into_result().unwrap();
    let seeded = session.rooted_cfg(&options).unwrap();
    assert!(seeded.work().body_analysis.body_analyses_computed > 0);
    session.update(&supported_edit).into_result().unwrap();
    let recovered = session.rooted_cfg(&options).unwrap();
    assert!(recovered.work().body_analysis.body_analyses_computed > 0);
}

#[test]
fn anonymous_structural_body_operations_export_durably_after_declaration_reuse() {
    let first = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn Box(comptime T: type) -> type { struct { value: T, fn get(borrow self) -> T { self.value } } } fn main() -> i32 { let B = Box(i32); let value = B { value: 1 }; value.get() }",
        )],
        1,
    );
    let edited = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn Box(comptime T: type) -> type { struct { value: T, fn get(borrow self) -> T { self.value } } } fn main() -> i32 { let B = Box(i32); let value = B { value: 2 }; value.get() }",
        )],
        1,
    );
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    let cold = session.rooted_cfg(&options).unwrap();
    assert!(cold.work().body_analysis.body_analyses_computed > 0);

    session.update(&edited).into_result().unwrap();
    let ordinary = session.rooted_cfg(&options).unwrap();
    assert!(ordinary.work().body_analysis.body_analyses_computed > 0);
    // Type producers are query inputs, not runtime function bodies. The
    // reached executable set is `main` plus the anonymous `get` method.
    assert_eq!(ordinary.functions().len(), 2);
    let mut fresh = CompilerSession::new();
    fresh.update(&edited).into_result().unwrap();
    let expected = fresh.rooted_cfg(&options).unwrap();
    assert_rooted_cfg_parity(&session, &ordinary, &expected);

    let supported = snapshot(
        &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }")],
        1,
    );
    let supported_edit = snapshot(
        &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 2 }")],
        1,
    );
    session.update(&supported).into_result().unwrap();
    let seeded = session.rooted_cfg(&options).unwrap();
    assert!(seeded.work().body_analysis.body_analyses_computed > 0);
    session.update(&supported_edit).into_result().unwrap();
    let recovered = session.rooted_cfg(&options).unwrap();
    assert!(recovered.work().body_analysis.body_analyses_computed > 0);
}

#[test]
fn signature_target_and_failed_body_changes_fail_closed_and_recovery_reuses() {
    let base = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn value() -> i32 { 1 } fn main() { value(); }",
        )],
        1,
    );
    let signature = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn value() -> i64 { 1 } fn main() { value(); }",
        )],
        1,
    );
    let broken_body = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn value() -> i64 { missing } fn main() { value(); }",
        )],
        1,
    );
    let recovered = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn value() -> i64 { 2 } fn main() { value(); }",
        )],
        1,
    );
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&base).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();

    session.update(&signature).into_result().unwrap();
    let changed = session.rooted_cfg(&options).unwrap();
    assert!(changed.work().body_analysis.body_analyses_computed > 0);

    session.update(&broken_body).into_result().unwrap();
    assert!(session.rooted_cfg(&options).is_err());
    session.update(&recovered).into_result().unwrap();
    let recovered = session.rooted_cfg(&options).unwrap();
    assert!(recovered.work().body_analysis.body_analyses_computed > 0);

    let mut other_target = options.clone();
    other_target.target = *Target::all()
        .iter()
        .find(|target| **target != options.target)
        .unwrap();
    let target_changed = session.rooted_cfg(&other_target).unwrap();
    assert!(target_changed.work().body_analysis.body_analyses_computed > 0);
}

#[test]
fn root_relocation_file_id_and_logical_changes_invalidate_correctly() {
    let base = snapshot(
        &[
            (1, "/old/a.rue", "a.rue", "fn a() {}"),
            (2, "/old/b.rue", "b.rue", "fn b() {}"),
        ],
        1,
    );
    let root_only = snapshot(
        &[
            (1, "/old/a.rue", "a.rue", "fn a() {}"),
            (2, "/old/b.rue", "b.rue", "fn b() {}"),
        ],
        2,
    );
    let relocated = snapshot(
        &[
            (1, "/new/a.rue", "a.rue", "fn a() {}"),
            (2, "/new/b.rue", "b.rue", "fn b() {}"),
        ],
        2,
    );
    let reassigned = snapshot(
        &[
            (11, "/new/a.rue", "a.rue", "fn a() {}"),
            (12, "/new/b.rue", "b.rue", "fn b() {}"),
        ],
        12,
    );
    let renamed = snapshot(
        &[
            (11, "/new/a2.rue", "a2.rue", "fn a() {}"),
            (12, "/new/b.rue", "b.rue", "fn b() {}"),
        ],
        12,
    );
    let mut session = CompilerSession::new();
    session.update(&base).into_result().unwrap();
    session.canonical_rir().unwrap();

    let root = session.update(&root_only);
    assert!(root.downstream_invalidated);
    assert_eq!(root.work().modules_reused, 2);
    root.into_result().unwrap();
    session.canonical_rir().unwrap();
    let moved = session.update(&relocated);
    assert!(moved.downstream_invalidated);
    assert_eq!(moved.work().modules_rebound, 2);
    moved.into_result().unwrap();
    session.canonical_rir().unwrap();
    let ids = session.update(&reassigned);
    assert!(ids.downstream_invalidated);
    assert_eq!(ids.work().modules_reparsed, 0);
    assert_eq!(ids.work().modules_rebound, 2);
    ids.into_result().unwrap();
    session.canonical_rir().unwrap();
    let rename = session.update(&renamed);
    assert!(rename.downstream_invalidated);
    assert_eq!(rename.invalidation().added.len(), 1);
    assert_eq!(rename.invalidation().removed.len(), 1);
    // ParseModule is keyed by stable logical module identity. A logical
    // rename is a removed leaf plus a new demanded leaf, so its syntax is
    // recomputed even when the source bytes happen to match.
    assert_eq!(rename.work().modules_reparsed, 1);
}

#[test]
fn retained_body_failure_reprojects_spans_after_leading_trivia_edit() {
    let first_text = "fn main() -> i32 { missing_name }";
    let shifted_text = "// newly inserted leading trivia\n\nfn main() -> i32 { missing_name }";
    let first = snapshot(&[(1, "/p/main.rue", "main.rue", first_text)], 1);
    let shifted = snapshot(&[(1, "/p/main.rue", "main.rue", shifted_text)], 1);
    let valid = snapshot(
        &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
        1,
    );
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();

    session.update(&valid).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let key = body_query_key(&mut session, &options, "main");
    session.update(&first).into_result().unwrap();
    let first_errors = session.rooted_cfg(&options).unwrap_err();
    let (first_stamp, _, _) = retained_body_transaction(&session, &key);
    let first_closure_stamps = retained_body_closure_stamps(&session, &key);
    let (first_locator_stamp, _) = retained_body_source_basis(&session, &key);
    assert_eq!(
        first_errors
            .first()
            .and_then(|error| error.span())
            .unwrap()
            .start,
        u32::try_from(first_text.find("missing_name").unwrap()).unwrap(),
    );

    session.update(&shifted).into_result().unwrap();
    let shifted_errors = session.rooted_cfg(&options).unwrap_err();
    let (shifted_stamp, _, _) = retained_body_transaction(&session, &key);
    let shifted_closure_stamps = retained_body_closure_stamps(&session, &key);
    let (shifted_locator_stamp, _) = retained_body_source_basis(&session, &key);
    assert_eq!(
        shifted_stamp, first_stamp,
        "a locator-only edit must reuse the semantic body transaction",
    );
    assert_eq!(
        shifted_closure_stamps, first_closure_stamps,
        "positioned diagnostic payload must not restamp the semantic body closure",
    );
    assert_eq!(
        shifted_locator_stamp, first_locator_stamp,
        "absolute relocation keeps the semantic source-basis stamp green",
    );
    let shifted_span = shifted_errors
        .first()
        .and_then(|error| error.span())
        .unwrap();
    assert_eq!(
        shifted_span.start,
        u32::try_from(shifted_text.find("missing_name").unwrap()).unwrap(),
    );
    assert_eq!(shifted_span.file_id, crate::FileId::new(1));
}

#[test]
fn whitespace_above_definition_reuses_semantic_shards_and_body_closure() {
    let first_text = "fn main() -> i32 { 0 }";
    let shifted_text = "// position-only leading trivia\n\nfn main() -> i32 { 0 }";
    let first = snapshot(&[(1, "/p/main.rue", "main.rue", first_text)], 1);
    let shifted = snapshot(&[(1, "/p/main.rue", "main.rue", shifted_text)], 1);
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();

    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    session.canonical_rir().unwrap();
    let key = body_query_key(&mut session, &options, "main");
    let first_body_stamps = retained_body_query_stamps(&session, &key);
    let first_closure_stamps = retained_body_closure_stamps(&session, &key);
    let (first_locator_stamp, first_locator) = retained_body_source_basis(&session, &key);

    session.update(&shifted).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    session.canonical_rir().unwrap();
    let shifted_body_stamps = retained_body_query_stamps(&session, &key);
    let shifted_closure_stamps = retained_body_closure_stamps(&session, &key);
    let (shifted_locator_stamp, shifted_locator) = retained_body_source_basis(&session, &key);

    assert_eq!(
        shifted_body_stamps, first_body_stamps,
        "position-only edits keep the body transaction and anonymous-production projection green",
    );
    assert_eq!(
        shifted_closure_stamps, first_closure_stamps,
        "the body transaction retained by the body closure stays green",
    );
    assert_eq!(
        shifted_locator_stamp, first_locator_stamp,
        "position-only relocation keeps the source-basis stamp green",
    );
    assert_eq!(first_locator.declaration_start, 0);
    assert_eq!(
        shifted_locator.declaration_start,
        u32::try_from(shifted_text.find("fn main").unwrap()).unwrap(),
    );
    assert_eq!(
        shifted_locator.body_start,
        u32::try_from(shifted_text.find("{ 0 }").unwrap()).unwrap(),
    );
    // The retained terminal stamps above are the canonical evidence that
    // the body and closure stayed green. Root-level reuse may satisfy the
    // request before per-body execution counters are accrued.

    let merge = session.unstable_metrics().merge_metrics();
    assert_eq!(merge.definition_shards_indexed, 1);
    assert_eq!(merge.definition_shards_reused, 1);
    assert_eq!(merge.definition_shards_rebuilt, 0);
    let merged = session.merge().unwrap();
    let main = merged
        .definitions()
        .definitions()
        .find(|definition| definition.name_key().name() == "main")
        .unwrap();
    assert_eq!(
        main.declaration_span().start,
        u32::try_from(shifted_text.find("fn main").unwrap()).unwrap(),
        "reusing the position-free shard must still rebuild current navigation records",
    );
}

#[test]
fn comptime_depth_boundary_fits_an_eight_mib_worker_stack() {
    let sources = [
        "fn count(comptime n: i64) -> i64 { comptime { if n == 0 { 0 } else { count(n - 1) + 1 } } } const X: i64 = count(64); fn main() -> i32 { if X != 64 { return 1; } 0 }",
        "fn count(comptime n: i64) -> i64 { if n <= 0 { 0 } else { 1 + count(n - 1) } } fn main() -> i32 { let x: i64 = comptime { count(64) }; if x != 64 { return 1; } 0 }",
        "fn count(comptime n: i64) -> i64 { if n <= 0 { 0 } else { 1 + count(n - 1) } } fn main() -> i32 { let x: i64 = count(64); if x != 64 { return 1; } 0 }",
    ];
    std::thread::Builder::new()
        .name("rue-comptime-depth-8mib".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            for source in sources {
                let mut session = CompilerSession::new();
                let input = snapshot(&[(1, "/p/main.rue", "main.rue", source)], 1);
                session.update(&input).into_result().unwrap();
                session
                    .rooted_cfg(&CompileOptions::default())
                    .expect("64-level comptime recursion fits an 8 MiB worker stack");
            }
        })
        .expect("8 MiB comptime worker must spawn")
        .join()
        .expect("64-level comptime recursion must not overflow its worker stack");
}

#[test]
fn many_shallow_specializations_compile() {
    // Regression (RUE-1083): breadth is not depth. A program may reach far
    // more than `MAX_COMPTIME_CALL_DEPTH` distinct specializations as long
    // as each sits at a shallow instantiation depth. Here `tag` has a
    // compile-time-known base case, so every `tag(k)` is a leaf
    // specialization at nesting depth 1; `main` reaches
    // `MAX_COMPTIME_CALL_DEPTH + 8` of them. The retired total-count budget
    // failed this program with E1200; the chain-depth budget compiles it.
    let count = rue_air::specialize::MAX_COMPTIME_CALL_DEPTH + 8;
    let mut body = String::from("fn main() -> i32 {\n    let mut total = 0;\n");
    for k in 0..count {
        body.push_str(&format!("    total = total + tag({k});\n"));
    }
    body.push_str("    total\n}\n");
    let program = format!("fn tag(comptime n: i32) -> i32 {{ n }}\n{body}");
    let valid = snapshot(&[(1, "/p/main.rue", "main.rue", program.as_str())], 1);
    let mut session = CompilerSession::new();
    session.update(&valid).into_result().unwrap();
    session
        .rooted_cfg(&CompileOptions::default())
        .expect("many shallow specializations must compile");
}

#[test]
fn revision_shared_semantic_and_cfg_interning_exhaustion_is_typed() {
    // Search the owner-controlled bound, rather than relying on a public
    // thread-local override. The first successful canonical projection
    // followed by a failing rooted-CFG query proves that the exhaustion
    // occurs in post-lexer semantic/CFG work.
    let valid = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn id(comptime T: type, value: T) -> T { value } fn main() -> i32 { id(i32, 42) }",
        )],
        1,
    );
    let mut observed = None;
    for limit in 20..256 {
        let mut session = CompilerSession::with_interner_limit(limit);
        session.update(&valid).into_result().unwrap();
        if session.canonical_rir().is_err() {
            continue;
        }
        if let Err(errors) = session.rooted_cfg(&CompileOptions::default()) {
            let resource_errors = errors
                .iter()
                .filter(|error| {
                    error.kind.code() == rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT
                        || error.kind.code() == rue_error::ErrorCode::COMPILER_RESOURCE_EXHAUSTION
                })
                .collect::<Vec<_>>();
            if resource_errors.len() == 1
                && errors.len() == 1
                && !errors
                    .iter()
                    .any(|error| matches!(error.kind, ErrorKind::InternalError(_)))
                && format!("{:?}", resource_errors[0].kind)
                    .to_ascii_lowercase()
                    .contains("provider")
            {
                observed = Some(limit);
                break;
            }
        }
    }
    assert!(
        observed.is_some(),
        "a successful canonical projection must expose a typed revision-shared semantic/CFG exhaustion"
    );
}

#[test]
fn two_lazy_named_nominals_after_bound_report_one_typed_error() {
    let valid = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "struct First { second: Second } struct Second { value: i32 } fn main() -> i32 { 0 }",
        )],
        1,
    );
    let mut observed = false;
    for limit in 20..256 {
        let mut session = CompilerSession::with_interner_limit(limit);
        session.update(&valid).into_result().unwrap();
        if session.canonical_rir().is_err() {
            continue;
        }
        let Err(errors) = session.rooted_cfg(&CompileOptions::default()) else {
            continue;
        };
        if errors.len() == 1
            && errors.iter().all(|error| {
                matches!(
                    error.kind,
                    ErrorKind::CompilerResourceLimit(_) | ErrorKind::CompilerResourceExhaustion(_)
                )
            })
            && !errors
                .iter()
                .any(|error| matches!(error.kind, ErrorKind::InternalError(_)))
        {
            observed = true;
            break;
        }
    }
    assert!(
        observed,
        "lazy named nominal materialization must fail once with a typed resource diagnostic"
    );
}

#[test]
fn bounded_request_local_cfg_projection_reports_typed_exhaustion() {
    let valid = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn main() -> i32 { let value = 42; value }",
        )],
        1,
    );
    let mut observed = false;
    for limit in 0..64 {
        let mut session = CompilerSession::with_cfg_interner_limit(limit);
        session.update(&valid).into_result().unwrap();
        if session.canonical_rir().is_err() {
            continue;
        }
        let Err(errors) = session.rooted_cfg(&CompileOptions::default()) else {
            continue;
        };
        if errors.len() == 1
            && errors.iter().any(|error| {
                matches!(error.kind, ErrorKind::CompilerResourceLimit(_))
                    && format!("{:?}", error.kind).contains("request-local CFG")
            })
            && !errors
                .iter()
                .any(|error| matches!(error.kind, ErrorKind::InternalError(_)))
        {
            observed = true;
            break;
        }
    }
    assert!(
        observed,
        "the production CFG query must classify request-local symbol exhaustion"
    );
}

#[test]
fn accessor_cfg_failure_preserves_callee_source_span() {
    let main = r#"const lib = @import("lib.rue");
fn main() -> i32 {
    let value = lib.Box { value: 7 };
    if value.get() == 7 { 0 } else { 1 }
}"#;
    let lib = r#"pub struct Box {
    value: i32,
    fn get(borrow self) -> borrow i32 { yield self.value; }
}"#;
    let source = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", main),
            (2, "/p/lib.rue", "lib.rue", lib),
        ],
        1,
    );
    let options = CompileOptions::default();
    let mut session = CompilerSession::with_cfg_accessor_failure();
    publish_with_test_imports(&mut session, &source);
    let errors = session
        .rooted_cfg(&options)
        .expect_err("the accessor CFG hook must publish a failure");
    assert_eq!(errors.len(), 1, "unexpected diagnostics: {errors:?}");
    assert!(
        matches!(
            errors.first().map(|error| &error.kind),
            Some(ErrorKind::InternalError(message)) if message == "test CFG accessor failure"
        ),
        "unexpected diagnostics: {errors:?}"
    );
    assert_eq!(
        errors
            .first()
            .and_then(CompileError::span)
            .map(|span| span.file_id),
        Some(FileId::new(2)),
        "accessor failure must remain anchored in the callee module: {errors:?}"
    );

    let first_span = errors.first().and_then(CompileError::span).unwrap();
    let shifted_lib = format!("// shift the callee body\n{lib}");
    let shifted = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", main),
            (2, "/p/lib.rue", "lib.rue", shifted_lib.as_str()),
        ],
        1,
    );
    publish_with_test_imports(&mut session, &shifted);
    let shifted_errors = session
        .rooted_cfg(&options)
        .expect_err("the shifted accessor CFG must still publish a failure");
    let shifted_span = shifted_errors
        .first()
        .and_then(CompileError::span)
        .expect("the shifted failure must retain its span");
    assert_eq!(shifted_span.file_id, FileId::new(2));
    assert_eq!(
        shifted_span.start,
        first_span.start + "// shift the callee body\n".len() as u32,
        "a reused accessor failure must be remapped to the current callee body"
    );
    assert_ne!(shifted_span, first_span);
    assert!(
        session
            .rooted_cfg_executions()
            .iter()
            .any(|(function, execution)| {
                matches!(
                    function,
                    crate::FunctionInstanceKey::Definition(definition)
                        if definition.name() == "main"
                ) && *execution == rue_query::RequestExecution::Reused
            }),
        "the shifted diagnostic must come from the retained caller optimized-CFG terminal"
    );
}

#[test]
fn same_file_accessor_cfg_failure_reprojects_without_caller_remap() {
    let program = r#"struct Box {
    value: i32,
    fn get(borrow self) -> borrow i32 { yield self.value; }
}
fn main() -> i32 {
    let value = Box { value: 7 };
    if value.get() == 7 { 0 } else { 1 }
}"#;
    let options = CompileOptions::default();
    let mut session = CompilerSession::with_cfg_accessor_failure();
    let source = SourceSnapshot::single("main.rue", program).unwrap();
    session.update(&source).into_result().unwrap();
    let first = session
        .rooted_cfg(&options)
        .expect_err("the accessor CFG hook must publish a same-file failure");
    let first_span = first.first().and_then(CompileError::span).unwrap();
    assert_eq!(first_span.file_id, FileId::new(0));

    let prefix = "// shift the accessor\n";
    let shifted_source =
        SourceSnapshot::single("main.rue", format!("{prefix}{program}").as_str()).unwrap();
    session.update(&shifted_source).into_result().unwrap();
    let shifted = session
        .rooted_cfg(&options)
        .expect_err("the shifted same-file accessor must still fail");
    let shifted_span = shifted.first().and_then(CompileError::span).unwrap();
    assert_eq!(shifted_span.file_id, FileId::new(0));
    assert_eq!(
        shifted_span.start,
        first_span.start + prefix.len() as u32,
        "same-file retained failures must not be remapped into the caller"
    );
}

#[test]
fn specialization_symbol_exhaustion_is_typed_at_the_session_boundary() {
    let mut main = String::from("fn choose(comptime n: i32) -> i32 { n } fn main() -> i32 {\n");
    for value in 0..32 {
        main.push_str(&format!("    choose({value}) +\n"));
    }
    main.push_str("    0\n}\n");
    let specialization_call = main.find("choose(0)").expect("generated call") as u32;
    let specialization_span = rue_span::Span::with_file(
        FileId::new(1),
        specialization_call,
        specialization_call + "choose(0)".len() as u32,
    );
    let valid = snapshot(&[(1, "/p/main.rue", "main.rue", main.as_str())], 1);
    let mut observed = None;
    for limit in 20..256 {
        let mut session = CompilerSession::with_interner_limit(limit);
        session.update(&valid).into_result().unwrap();
        if session.canonical_rir().is_err() {
            continue;
        }
        let Err(errors) = session.rooted_cfg(&CompileOptions::default()) else {
            continue;
        };
        if errors.len() == 1
            && !errors
                .iter()
                .any(|error| matches!(error.kind, ErrorKind::InternalError(_)))
            && errors.iter().any(|error| {
                error.kind.code() == rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT
                    && format!("{:?}", error.kind)
                        .contains("specialization symbol interning failed")
                    && error.span() == Some(specialization_span)
            })
        {
            observed = Some(limit);
            break;
        }
    }
    assert!(
        observed.is_some(),
        "specialization materialization must report the owning symbol-space exhaustion"
    );
}

#[test]
fn cross_body_specialization_chain_still_overflows() {
    // The chain-depth budget must still reject unbounded cross-body
    // instantiation chains: `deepen<n>` instantiates `deepen<n + 1>`, so
    // each body publishes a strictly deeper specialization and the nesting
    // depth grows without bound. This must fail with the same E1200
    // (`maximum nesting depth`) diagnostic as before.
    let invalid = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "fn deepen(comptime n: i32) -> i32 { deepen(n + 1) }\n\
                 fn main() -> i32 { deepen(0) }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&invalid).into_result().unwrap();
    let errors = session.rooted_cfg(&CompileOptions::default()).unwrap_err();
    assert!(
        matches!(
            errors.first().map(|error| &error.kind),
            Some(ErrorKind::ComptimeEvaluationFailed { reason })
                if reason.contains("maximum nesting depth")
        ),
        "runaway cross-body specialization chain must overflow with E1200"
    );
}

#[test]
fn file_const_anonymous_types_use_epoch_local_comptime_producers() {
    for source in [
        r#"
const T: type = struct { value: i32 };
fn main() -> i32 {
    let value: T = T { value: 42 };
    value.value
}
"#,
        r#"
const T: type = enum { A, B(i32) };
fn main() -> i32 { 0 }
"#,
    ] {
        let source = snapshot(&[(7, "/p/main.rue", "main.rue", source)], 7);
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let semantic = session.rooted_cfg(&CompileOptions::default()).unwrap();
        assert!(!semantic.functions().is_empty());
    }
}

#[test]
fn declaration_failures_surface_one_exact_diagnostic_per_fixture() {
    for (text, expected) in [
        (
            "const value: i32 = 1; const value: i32 = 2; fn main() {}",
            "duplicate constant 'value'",
        ),
        (
            "struct Value {} drop fn Value(self) {} drop fn Value(self) {} fn main() {}",
            "duplicate destructor for type 'Value'",
        ),
        (
            "drop fn Missing(self) {} fn main() {}",
            "unknown type 'Missing' in destructor",
        ),
    ] {
        let source = snapshot(&[(1, "/main.rue", "main.rue", text)], 1);
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let query = match session.rooted_cfg(&CompileOptions::default()) {
            Err(errors) => errors,
            Ok(_) => panic!("query path unexpectedly accepted failure fixture"),
        };
        let messages = query.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec![expected.to_owned()],
            "declaration failure diagnostics changed for fixture: {text}"
        );
    }
}

#[test]
fn import_graph_requires_committed_discovery_for_import_bearing_revisions() {
    let source = snapshot(
        &[
            (
                1,
                "/p/app/main.rue",
                "app/main.rue",
                "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
            ),
            (2, "/p/app/helper.rue", "app/helper.rue", "fn helper() {}"),
        ],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let error = session.committed_import_graph().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no closed-valid import discovery revision is committed")
    );
}

#[test]
fn committed_discovery_graph_is_consumed_without_resolution_fallback() {
    let source = snapshot(
        &[
            (
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { let s = @import(\"helper.rue\"); 0 }",
            ),
            (2, "/p/helper.rue", "helper.rue", "fn helper() {}"),
        ],
        1,
    );
    let mut session = CompilerSession::new();
    publish_with_test_imports(&mut session, &source);
    assert!(session.committed_import_graph().is_ok());
}

#[test]
fn empty_import_graph_is_send_sync_and_concurrently_readable() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CanonicalImportGraphOutput>();
    let mut session = CompilerSession::new();
    crate::test_support::TestDiscoveryHost::new(&base())
        .unwrap()
        .drive(&mut session)
        .unwrap();
    let graph = session.committed_import_graph().unwrap();
    assert!(graph.graph().records().is_empty());
    std::thread::spawn(move || assert!(graph.validation().is_valid()))
        .join()
        .unwrap();
}

#[test]
fn parse_family_reselects_canonical_origin_after_diagnostic_index_eviction() {
    let source = base();
    let mut session = CompilerSession::new();
    let origin = session.update(&source).diagnostics().clone();

    evict_diagnostic_index(&mut session);
    assert!(
        session
            .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Syntax)
            .is_none()
    );
    let publications = session.work().diagnostic_publications;
    let invalidations = session.work().diagnostic_invalidations;
    let reuses = session.work().diagnostic_reuses;

    let exact = session.update(&source);

    assert!(Arc::ptr_eq(exact.diagnostics(), &origin));
    assert_eq!(exact.work(), ParsedModulesWork::default());
    assert_eq!(session.work().diagnostic_publications, publications);
    assert_eq!(session.work().diagnostic_invalidations, invalidations);
    assert_eq!(session.work().diagnostic_reuses, reuses + 1);
    assert!(Arc::ptr_eq(
        session
            .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Syntax)
            .unwrap(),
        &origin
    ));
}

#[test]
fn parse_family_reselects_presentation_origin_after_diagnostic_index_eviction() {
    let source = base();
    let mut session = CompilerSession::new();
    let origin = session
        .update_for_presentation(&source)
        .diagnostics()
        .clone();

    evict_diagnostic_index(&mut session);
    assert!(
        session
            .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Syntax)
            .is_none()
    );
    let publications = session.work().diagnostic_publications;
    let invalidations = session.work().diagnostic_invalidations;
    let reuses = session.work().diagnostic_reuses;

    let exact = session.update_for_presentation(&source);

    assert!(Arc::ptr_eq(exact.diagnostics(), &origin));
    assert_eq!(exact.work(), ParsedModulesWork::default());
    assert_eq!(session.work().diagnostic_publications, publications);
    assert_eq!(session.work().diagnostic_invalidations, invalidations);
    assert_eq!(session.work().diagnostic_reuses, reuses + 1);
    assert!(Arc::ptr_eq(
        session
            .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Syntax)
            .unwrap(),
        &origin
    ));
}

#[test]
fn reselected_parse_terminal_is_the_only_baseline_for_the_next_miss() {
    let a = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            (2, "/p/helper.rue", "helper.rue", "fn helper() -> i32 { 0 }"),
        ],
        1,
    );
    let b = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
            (2, "/p/helper.rue", "helper.rue", "fn helper() -> i32 { 0 }"),
        ],
        1,
    );
    let c = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            (2, "/p/helper.rue", "helper.rue", "fn helper() -> i32 { 2 }"),
        ],
        1,
    );
    let mut session = CompilerSession::new();
    session.update(&a).into_result().unwrap();
    session.update(&b).into_result().unwrap();

    let reselected = session.update(&a);
    assert_eq!(reselected.work(), ParsedModulesWork::default());

    let next = session.update(&c);
    assert_eq!(next.work().modules_reused, 1);
    assert_eq!(next.work().modules_reparsed, 1);
}

#[test]
fn direct_import_cache_reselects_its_origin_after_diagnostic_index_eviction() {
    let source = base();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let origin = session.import_diagnostics().unwrap();
    let stage = origin.identity().clone();

    evict_diagnostic_index(&mut session);
    assert!(
        session
            .most_recent_diagnostics_for(&source, &stage)
            .is_none()
    );
    let publications = session.work().diagnostic_publications;

    let reused = session.import_diagnostics().unwrap();

    assert!(Arc::ptr_eq(&reused, &origin));
    assert_eq!(session.work().import_diagnostics.executions, 1);
    assert_eq!(session.work().import_diagnostics.reuses, 1);
    assert_eq!(session.work().diagnostic_publications, publications);
    assert!(Arc::ptr_eq(
        session
            .most_recent_diagnostics_for(&source, &stage)
            .unwrap(),
        &origin
    ));
}

#[test]
fn specialized_reuse_survives_relocation_file_ids_and_input_order() {
    let original = snapshot(
        &[
            (
                71,
                "/old/main.rue",
                "main.rue",
                "const lib = @import(\"lib.rue\"); fn main() -> i32 { lib.id(i32, 42) }",
            ),
            (
                72,
                "/old/lib.rue",
                "lib.rue",
                "pub fn id(comptime T: type, value: T) -> T { value }",
            ),
        ],
        71,
    );
    let relocated = snapshot(
        &[
            (
                4,
                "/new/lib.rue",
                "lib.rue",
                "pub fn id(comptime T: type, value: T) -> T { value }",
            ),
            (
                9,
                "/new/main.rue",
                "main.rue",
                "const lib = @import(\"lib.rue\"); fn main() -> i32 { lib.id(i32, 42) }",
            ),
        ],
        9,
    );
    let mut session = CompilerSession::new();
    publish_with_test_imports(&mut session, &original);
    session.rooted_cfg(&CompileOptions::default()).unwrap();
    publish_with_test_imports(&mut session, &relocated);
    let options = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let reused = session.rooted_cfg(&options).unwrap();
    let mut fresh_session = CompilerSession::new();
    publish_with_test_imports(&mut fresh_session, &relocated);
    let fresh = fresh_session.rooted_cfg(&options).unwrap();
    assert_eq!(
        reused
            .functions()
            .iter()
            .map(|function| (
                &function.function,
                function.record.codegen.defined_symbol.as_ref()
            ))
            .collect::<Vec<_>>(),
        fresh
            .functions()
            .iter()
            .map(|function| (
                &function.function,
                function.record.codegen.defined_symbol.as_ref()
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        reused.string_domains().collect::<Vec<_>>(),
        fresh.string_domains().collect::<Vec<_>>()
    );
    assert_eq!(
        format!("{:?}", reused.warnings()),
        format!("{:?}", fresh.warnings())
    );
    assert_diagnostic_parity(&session, &fresh_session);
}

#[test]
fn specialized_target_and_preview_boundaries_fail_closed_exactly() {
    let source = snapshot(
        &[(
            42,
            "/p/main.rue",
            "main.rue",
            "fn id(comptime T: type, value: T) -> T { value } fn main() -> i32 { id(i32, 42) }",
        )],
        42,
    );
    let run = |options: CompileOptions| {
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.rooted_cfg(&CompileOptions::default()).unwrap();
        session.rooted_cfg(&options).unwrap()
    };
    let other_target = *Target::all()
        .iter()
        .find(|target| **target != CompileOptions::default().target)
        .unwrap();
    let target = run(CompileOptions {
        target: other_target,
        ..CompileOptions::default()
    });
    assert_eq!(target.functions().len(), 2);

    let preview = run(CompileOptions {
        preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
        ..CompileOptions::default()
    });
    assert_eq!(preview.functions().len(), 2);
}

#[test]
fn warning_specializations_recompute_once_and_are_never_published() {
    let source = snapshot(
        &[(
            42,
            "/p/main.rue",
            "main.rue",
            "fn noisy(comptime n: i32) -> i32 { let unused = 0; n } fn main() -> i32 { noisy(1) + noisy(1) }",
        )],
        42,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let cold = session.rooted_cfg(&CompileOptions::default()).unwrap();
    assert_eq!(cold.warnings().len(), 1);

    let warm = session
        .rooted_cfg(&CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        })
        .unwrap();
    assert_eq!(warm.warnings().len(), 1);
    assert_eq!(
        format!("{:?}", warm.warnings()),
        format!("{:?}", cold.warnings())
    );
}

/// Warnings are presented in module-path order, then by span, then by
/// rendered text — never in file-id, import, or query-scheduling order.
///
/// The fixture crosses all three sources of warnings that reach a rooted
/// CFG: imported body warnings from a called module, imported body
/// warnings from the root, and unused-function warnings collected from the
/// declaration set.
#[test]
fn warnings_are_ordered_by_module_then_span_across_modules() {
    const ROOT: &str = "const zeta = @import(\"zeta.rue\");\n\
             const alpha = @import(\"alpha.rue\");\n\
             fn main() -> i32 {\n\
             let unused_main = 1;\n\
             zeta.z() + alpha.a()\n\
             }\n";
    const ALPHA: &str = "pub fn a() -> i32 {\n\
             let unused_alpha = 2;\n\
             3\n\
             }\n\
             fn dead_alpha() -> i32 { 4 }\n";
    const ZETA: &str = "pub fn z() -> i32 {\n\
             let unused_zeta = 5;\n\
             6\n\
             }\n\
             fn dead_zeta() -> i32 { 7 }\n";
    // The root module's own path sorts between its two imports, so
    // module-path order (alpha, main, zeta) and the root-first order that
    // discovery and file ids follow (main, alpha, zeta) disagree.
    let source = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", ROOT),
            (2, "/p/zeta.rue", "zeta.rue", ZETA),
            (3, "/p/alpha.rue", "alpha.rue", ALPHA),
        ],
        1,
    );

    let observe = || {
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        let rooted = session.rooted_cfg(&CompileOptions::default()).unwrap();
        let published = session
            .published_snapshot
            .clone()
            .expect("the rooted CFG published its program");
        rooted
            .warnings()
            .iter()
            .map(|warning| {
                let span = warning.span().expect("every fixture warning is located");
                let module = published
                    .metadata()
                    .logical_path(span.file_id)
                    .expect("every fixture warning names a published module")
                    .to_owned();
                (module, span.start, warning.to_string())
            })
            .collect::<Vec<_>>()
    };
    let observed = observe();

    let placement = observed
        .iter()
        .map(|(module, start, _)| (module.clone(), *start))
        .collect::<Vec<_>>();
    let modules = placement
        .iter()
        .map(|(module, _)| module.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        modules,
        ["alpha.rue", "alpha.rue", "main.rue", "zeta.rue", "zeta.rue"],
        "warnings group by module path, not by file id: {observed:?}"
    );
    for window in placement.windows(2) {
        let (left, right) = (&window[0], &window[1]);
        assert!(
            left.0 != right.0 || left.1 < right.1,
            "warnings within one module ascend by span: {observed:?}"
        );
    }

    let named = |index: usize, needle: &str| {
        assert!(
            observed[index].2.contains(needle),
            "warning {index} should name `{needle}`: {observed:?}"
        );
    };
    named(0, "unused_alpha");
    named(1, "dead_alpha");
    named(2, "unused_main");
    named(3, "unused_zeta");
    named(4, "dead_zeta");

    assert_eq!(
        observe(),
        observed,
        "the same program must present the same warnings in the same order"
    );
}

#[test]
fn callable_alias_is_rejected_as_comptime_value_argument() {
    let source = snapshot(
        &[(
            42,
            "/p/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 } const F = helper; fn Witness(comptime T: type, comptime value: T) -> type { struct { marker: i32 } } fn bad(value: Witness(type, F)) -> i32 { value.marker } fn main() -> i32 { 0 }",
        )],
        42,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let errors = session.rooted_cfg(&CompileOptions::default()).unwrap_err();
    assert!(errors.iter().any(|error| {
        error
            .to_string()
            .contains("callable alias cannot be passed as a comptime value argument")
    }));
}

#[test]
fn nested_specialized_bodies_reuse_and_close_over_changed_callees() {
    let source_text = "fn inner(comptime T: type, value: T) -> T { value }\n\
             fn outer(comptime T: type, value: T) -> T { inner(T, value) }\n\
             fn main() -> i32 { outer(i32, 41) + outer(i32, 1) }";
    let source = snapshot(&[(42, "/p/main.rue", "main.rue", source_text)], 42);
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();

    let optimized = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    session.rooted_cfg(&optimized).unwrap();

    let unrelated_text = format!("{source_text}\nfn unrelated() -> i32 {{ 7 }}");
    let unrelated = snapshot(
        &[(42, "/p/main.rue", "main.rue", unrelated_text.as_str())],
        42,
    );
    session.update(&unrelated).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();

    let changed_text = "fn inner(comptime T: type, value: T) -> T { let copy = value; copy }\n\
             fn outer(comptime T: type, value: T) -> T { inner(T, value) }\n\
             fn main() -> i32 { outer(i32, 41) + outer(i32, 1) }\n\
             fn unrelated() -> i32 { 7 }";
    let changed_source = snapshot(&[(42, "/p/main.rue", "main.rue", changed_text)], 42);
    session.update(&changed_source).into_result().unwrap();
    let changed = session.rooted_cfg(&CompileOptions::default()).unwrap();
    let mut fresh = CompilerSession::new();
    fresh.update(&changed_source).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&CompileOptions::default()).unwrap();
    assert_eq!(
        normalize_session_local_spurs(format!("{:?}", changed.functions())),
        normalize_session_local_spurs(format!("{:?}", fresh.functions()))
    );
}

#[test]
fn recursive_specialized_candidates_reenter_the_fixed_point_once_each() {
    let source = snapshot(
        &[(
            42,
            "/p/main.rue",
            "main.rue",
            r#"fn fib(comptime n: i32) -> i32 {
                       if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
                   }
                   fn main() -> i32 { fib(5) + fib(5) }"#,
        )],
        42,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();

    let optimized = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let warm = session.rooted_cfg(&optimized).unwrap();
    assert_eq!(warm.functions().len(), 7);
}

#[test]
fn evaluated_away_named_const_provenance_invalidates_only_affected_instance() {
    let first_text = "const answer: i32 = 41;\n\
             fn choose(comptime use_answer: bool) -> i32 {\n\
                 if use_answer { answer } else { 1 }\n\
             }\n\
             fn main() -> i32 { choose(true) + choose(false) }";
    let first = snapshot(&[(42, "/p/main.rue", "main.rue", first_text)], 42);
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    let cold = session.rooted_cfg(&CompileOptions::default()).unwrap();
    let choose_true = cold
        .functions()
        .iter()
        .find(|function| {
            specialization_arguments(function, "choose").is_some_and(|arguments| {
                arguments.values.as_ref() == [crate::CanonicalArgumentValue::Bool(true)]
            })
        })
        .unwrap();
    let choose_true_key = crate::body_query::BodyQueryKey::new(
        choose_true.function.clone(),
        crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: CompileOptions::default().target,
            preview_features: StablePreviewFeatures::new(
                &CompileOptions::default().preview_features,
            ),
        },
    );
    let transaction = retained_body_transaction(&session, &choose_true_key).2;
    assert!(
        transaction.references().0.iter().any(|reference| matches!(
            reference,
            crate::body_query::BodyReference::Definition(definition)
                if definition.name() == "answer"
        )),
        "{transaction:?}"
    );
    let dependency_nodes = retained_body_dependency_nodes(&session, &choose_true_key);
    assert!(
        dependency_nodes
            .iter()
            .any(|node| node.contains("const:") && node.contains("answer")),
        "{dependency_nodes:?}"
    );

    let changed_text = first_text.replace("41", "42");
    let changed_source = snapshot(
        &[(42, "/p/main.rue", "main.rue", changed_text.as_str())],
        42,
    );
    session.update(&changed_source).into_result().unwrap();
    let changed = session.rooted_cfg(&CompileOptions::default()).unwrap();
    let mut fresh = CompilerSession::new();
    fresh.update(&changed_source).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&CompileOptions::default()).unwrap();
    assert_eq!(
        format!("{:?}", changed.functions()),
        format!("{:?}", fresh.functions())
    );
}

#[test]
fn specialized_drop_provenance_invalidates_only_the_owning_instance() {
    let first_text = "fn cleanup() {}\n\
             struct Resource { value: i32 }\n\
             drop fn Resource(self) { cleanup(); }\n\
             fn borrowed(comptime n: i32, borrow resource: Resource) -> i32 {\n\
                 resource.value + n\n\
             }\n\
             fn owned(comptime n: i32, resource: Resource) -> i32 {\n\
                 resource.value + n\n\
             }\n\
             fn main() -> i32 {\n\
                 let left = Resource { value: 20 };\n\
                 let right = Resource { value: 20 };\n\
                 borrowed(1, borrow left) + owned(1, right)\n\
             }";
    let first = snapshot(&[(43, "/p/main.rue", "main.rue", first_text)], 43);
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();

    let changed_text = first_text.replace("cleanup();", "cleanup(); let marker = 0;");
    let changed_source = snapshot(
        &[(43, "/p/main.rue", "main.rue", changed_text.as_str())],
        43,
    );
    session.update(&changed_source).into_result().unwrap();
    let changed = session.rooted_cfg(&CompileOptions::default()).unwrap();
    let mut fresh_session = CompilerSession::new();
    fresh_session.update(&changed_source).into_result().unwrap();
    let fresh = fresh_session
        .rooted_cfg(&CompileOptions::default())
        .unwrap();
    assert_body_artifact_parity(&changed, &fresh);
    assert_diagnostic_parity(&session, &fresh_session);
}

#[test]
fn unreachable_composite_body_candidate_is_never_imported() {
    let original = snapshot(
        &[(
            71,
            "/p/main.rue",
            "main.rue",
            "fn helper() -> [i32; 2] { [1, 2] }\nfn main() -> i32 { helper()[0] }",
        )],
        71,
    );
    let edited = snapshot(
        &[(
            71,
            "/p/main.rue",
            "main.rue",
            "fn helper() -> [i32; 2] { [1, 2] }\nfn main() -> i32 { 0 }",
        )],
        71,
    );
    let mut session = CompilerSession::new();
    session.update(&original).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();
    session.update(&edited).into_result().unwrap();
    let reused = session.rooted_cfg(&CompileOptions::default()).unwrap();
    let mut fresh = CompilerSession::new();
    fresh.update(&edited).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&CompileOptions::default()).unwrap();
    assert_eq!(
        format!("{:?}", reused.functions()),
        format!("{:?}", fresh.functions())
    );
    assert_eq!(reused.type_pool_stats(), fresh.type_pool_stats());
}

#[test]
fn exact_semantic_cache_hit_restores_its_successful_body_baseline() {
    let a = snapshot(
        &[(
            73,
            "/p/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }",
        )],
        73,
    );
    let b = snapshot(
        &[(
            73,
            "/p/main.rue",
            "main.rue",
            "fn helper() -> i32 { 2 }\nfn main() -> i32 { helper() }",
        )],
        73,
    );
    let a_prime = snapshot(
        &[(
            73,
            "/p/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() + 1 }",
        )],
        73,
    );
    let mut session = CompilerSession::new();
    session.update(&a).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();
    session.update(&b).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();
    session.update(&a).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();
    session.update(&a_prime).into_result().unwrap();
    let output = session.rooted_cfg(&CompileOptions::default()).unwrap();
    let mut fresh = CompilerSession::new();
    fresh.update(&a_prime).into_result().unwrap();
    let fresh = fresh.rooted_cfg(&CompileOptions::default()).unwrap();
    assert_eq!(
        format!("{:?}", output.functions()),
        format!("{:?}", fresh.functions())
    );
}

#[test]
fn mutual_recursion_edit_rebuilds_the_cycle_and_callers_only() {
    let original = snapshot(
        &[(
            82,
            "/p/main.rue",
            "main.rue",
            r#"
            fn a(n: i32) -> i32 { if n == 0 { 0 } else { b(n - 1) } }
            fn b(n: i32) -> i32 { if n == 0 { 0 } else { a(n - 1) } }
            fn spare() -> i32 { 7 }
            fn main() -> i32 { a(2) + spare() }
        "#,
        )],
        82,
    );
    let edited = snapshot(
        &[(
            82,
            "/p/main.rue",
            "main.rue",
            r#"
            fn a(n: i32) -> i32 { if n == 0 { 1 } else { b(n - 1) } }
            fn b(n: i32) -> i32 { if n == 0 { 0 } else { a(n - 1) } }
            fn spare() -> i32 { 7 }
            fn main() -> i32 { a(2) + spare() }
        "#,
        )],
        82,
    );
    let mut session = CompilerSession::new();
    session.update(&original).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();
    session.update(&edited).into_result().unwrap();
    let actual = session.rooted_cfg(&CompileOptions::default()).unwrap();
    let mut fresh_session = CompilerSession::new();
    fresh_session.update(&edited).into_result().unwrap();
    let fresh = fresh_session
        .rooted_cfg(&CompileOptions::default())
        .unwrap();
    assert_body_artifact_parity(&actual, &fresh);
    assert_diagnostic_parity(&session, &fresh_session);
}

#[test]
fn recursive_body_edit_rebuilds_self_and_transitive_caller_only() {
    let original = snapshot(
        &[(
            83,
            "/p/main.rue",
            "main.rue",
            "fn recurse(n: i32) -> i32 { if n == 0 { 0 } else { recurse(n - 1) } } fn spare() -> i32 { 3 } fn main() -> i32 { recurse(2) + spare() }",
        )],
        83,
    );
    let edited = snapshot(
        &[(
            83,
            "/p/main.rue",
            "main.rue",
            "fn recurse(n: i32) -> i32 { if n == 0 { 1 } else { recurse(n - 1) } } fn spare() -> i32 { 3 } fn main() -> i32 { recurse(2) + spare() }",
        )],
        83,
    );
    let mut session = CompilerSession::new();
    session.update(&original).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();
    session.update(&edited).into_result().unwrap();
    let actual = session.rooted_cfg(&CompileOptions::default()).unwrap();
    let mut fresh_session = CompilerSession::new();
    fresh_session.update(&edited).into_result().unwrap();
    let fresh = fresh_session
        .rooted_cfg(&CompileOptions::default())
        .unwrap();
    assert_body_artifact_parity(&actual, &fresh);
    assert_diagnostic_parity(&session, &fresh_session);
}

#[test]
fn body_reuse_survives_relocation_file_ids_and_input_permutation() {
    let original = snapshot(
        &[
            (
                91,
                "/one/main.rue",
                "main.rue",
                "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
            ),
            (92, "/one/dead.rue", "dead.rue", "fn dead() -> i32 { 2 }"),
        ],
        91,
    );
    let relocated = snapshot(
        &[
            (4, "/else/dead.rue", "dead.rue", "fn dead() -> i32 { 2 }"),
            (
                7,
                "/else/main.rue",
                "main.rue",
                "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
            ),
        ],
        7,
    );
    let mut session = CompilerSession::new();
    session.update(&original).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();
    session.update(&relocated).into_result().unwrap();
    let options = CompileOptions {
        opt_level: OptLevel::O1,
        ..CompileOptions::default()
    };
    let actual = session.rooted_cfg(&options).unwrap();
    let mut fresh_session = CompilerSession::new();
    fresh_session.update(&relocated).into_result().unwrap();
    let fresh = fresh_session.rooted_cfg(&options).unwrap();
    assert_body_artifact_parity(&actual, &fresh);
    assert_diagnostic_parity(&session, &fresh_session);
}

#[test]
fn target_preview_root_and_signature_changes_reject_body_artifacts() {
    let source = snapshot(
        &[(
            101,
            "/p/main.rue",
            "main.rue",
            "fn value() -> i32 { 1 } fn main() -> i32 { value(); 0 }",
        )],
        101,
    );
    let signature = snapshot(
        &[(
            101,
            "/p/main.rue",
            "main.rue",
            "fn value() -> i64 { 1 } fn main() -> i32 { value(); 0 }",
        )],
        101,
    );
    let run = |options: CompileOptions, next: &SourceSnapshot| {
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.rooted_cfg(&CompileOptions::default()).unwrap();
        session.update(next).into_result().unwrap();
        session.rooted_cfg(&options).unwrap()
    };
    let other_target = *Target::all()
        .iter()
        .find(|target| **target != CompileOptions::default().target)
        .unwrap();
    let target = run(
        CompileOptions {
            target: other_target,
            ..CompileOptions::default()
        },
        &source,
    );
    assert_eq!(target.functions().len(), 2);
    let preview = run(
        CompileOptions {
            preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
            ..CompileOptions::default()
        },
        &source,
    );
    assert_eq!(preview.functions().len(), 2);
    let signature = run(CompileOptions::default(), &signature);
    assert_eq!(signature.functions().len(), 2);

    // Both files carry a top-level `main` so either can serve as the root
    // (RUE-920: a non-root `main` is an ordinary, inert function). Only the
    // designated root's `main` is the entry point, so switching the root
    // still forces a body re-analysis without a duplicate-main error.
    let both_roots = snapshot(
        &[
            (101, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
            (102, "/p/other.rue", "other.rue", "fn main() -> i32 { 2 }"),
        ],
        101,
    );
    let other_root = snapshot(
        &[
            (101, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
            (102, "/p/other.rue", "other.rue", "fn main() -> i32 { 2 }"),
        ],
        102,
    );
    let mut session = CompilerSession::new();
    session.update(&both_roots).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();
    session.update(&other_root).into_result().unwrap();
    let root = session.rooted_cfg(&CompileOptions::default()).unwrap();
    assert_eq!(root.functions().len(), 1);
}

#[test]
fn malformed_well_known_option_repairs_to_fresh_rooted_cfgs() {
    let options = CompileOptions::default();
    let program = r#"
const opt = @import("std/option.rue");
fn main() -> i32 {
    let O = opt.Option(i32);
    match @parse_i32("42") {
        O.Some(value) => value,
        O.None => 0,
    }
}
"#;
    let malformed = well_known_option_snapshot_with_source(
        program,
        "pub fn Option(comptime T: type) -> type { missing }",
    );
    let repaired = well_known_option_isolation_snapshot(program);

    let mut warm = CompilerSession::new();
    publish_with_test_imports(&mut warm, &malformed);
    let errors = warm
        .rooted_cfg(&options)
        .expect_err("the malformed trusted Option specialization must fail");
    assert!(
        errors.to_string().contains("missing"),
        "the failed attempt must retain the trusted declaration diagnostic: {errors}"
    );
    assert!(
        warm.queries.revisioned.any_body_transaction_terminal(),
        "typed body-control classification must be published atomically"
    );
    publish_with_test_imports(&mut warm, &repaired);
    let warm_repaired = warm
        .rooted_cfg(&options)
        .expect("the repaired successor must compile");

    let mut fresh = CompilerSession::new();
    publish_with_test_imports(&mut fresh, &repaired);
    let fresh_repaired = fresh
        .rooted_cfg(&options)
        .expect("the repaired snapshot must compile fresh");

    crate::test_support::assert_rooted_cfg_value_parity(
        "malformed-state warming must not change repaired canonical semantics",
        &warm_repaired,
        &fresh_repaired,
    );
    assert!(warm.queries.revisioned.any_body_transaction_terminal());
}

/// Query-edge isolation. Two reached bodies demand DIFFERENT well-known
/// `Option` payloads: `left` uses `@parse_i32` (payload i32), `right` uses
/// `@parse_i64` (payload i64). Each body derives its OWN exact payload set from
/// its canonical raw body and roots only that payload's `Option`
/// specialization, so:
///
/// 1. `left`'s dependency edges reach the i32 `Option` specialization and NOT
///    the i64 one; `right`'s reach the i64 specialization and NOT the i32 one.
///    A body therefore cannot inherit failure or cancellation from an
///    unrelated body's specialization — it has no edge to it.
/// 2. Invalidating the i64 specialization's owning body (editing `right`)
///    leaves `left`'s terminal identity (its published stamp) unchanged: with
///    no edge to the churned specialization, `left` is reused, not recomputed.
#[test]
fn sibling_body_retains_no_edge_to_a_distinct_payload_specialization() {
    let options = CompileOptions::default();

    // Locate the ComptimeCall specialization edges by the payload spelled in
    // the dependency node's Debug rendering. The two bodies demand distinct
    // payloads, so distinct nodes carry the i32 and i64 type arguments.
    let has_i32_option_edge = |nodes: &[String]| {
        nodes.iter().any(|node| {
            node.contains("comptime:") && node.contains("Option") && node.contains("I32)")
        })
    };
    let has_i64_option_edge = |nodes: &[String]| {
        nodes.iter().any(|node| {
            node.contains("comptime:") && node.contains("Option") && node.contains("I64)")
        })
    };

    let program_v1 = r#"
const opt = @import("std/option.rue");
fn left(s: str) -> opt.Option(i32) {
    let O = opt.Option(i32);
    O.Some(@parse_i32(s)?)
}
fn right(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let OA = opt.Option(i32);
    let OB = opt.Option(i64);
    let a = match left("1") { OA.Some(v) => v, OA.None => 0 };
    let b = match right("2") { OB.Some(v) => @intCast(v), OB.None => 0 };
    a + b
}
"#;

    let source_v1 = well_known_option_isolation_snapshot(program_v1);
    let mut session = CompilerSession::new();
    publish_with_test_imports(&mut session, &source_v1);
    session.rooted_cfg(&options).unwrap();

    let left_key = body_query_key(&mut session, &options, "left");
    let right_key = body_query_key(&mut session, &options, "right");

    let left_nodes = retained_body_dependency_nodes(&session, &left_key);
    let right_nodes = retained_body_dependency_nodes(&session, &right_key);

    assert!(
        has_i32_option_edge(&left_nodes),
        "left (i32) must have an edge to the Option(i32) specialization: {left_nodes:?}",
    );
    assert!(
        !has_i64_option_edge(&left_nodes),
        "left (i32) must have NO edge to the sibling's Option(i64) specialization: {left_nodes:?}",
    );
    assert!(
        has_i64_option_edge(&right_nodes),
        "right (i64) must have an edge to the Option(i64) specialization: {right_nodes:?}",
    );
    assert!(
        !has_i32_option_edge(&right_nodes),
        "right (i64) must have NO edge to the sibling's Option(i32) specialization: {right_nodes:?}",
    );

    // `left`'s terminal identity before the sibling churns.
    let left_stamp_v1 = retained_body_transaction(&session, &left_key).0;

    // Invalidate the i64 specialization's owning body: edit ONLY `right`,
    // keeping its i64 payload demand. `left`'s raw body and its i32 demand are
    // untouched, and it has no edge to the i64 specialization, so its terminal
    // must be reused with an unchanged stamp.
    let program_v2 = r#"
const opt = @import("std/option.rue");
fn left(s: str) -> opt.Option(i32) {
    let O = opt.Option(i32);
    O.Some(@parse_i32(s)?)
}
fn right(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    let _churn = 7 + 8;
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let OA = opt.Option(i32);
    let OB = opt.Option(i64);
    let a = match left("1") { OA.Some(v) => v, OA.None => 0 };
    let b = match right("2") { OB.Some(v) => @intCast(v), OB.None => 0 };
    a + b
}
"#;
    let source_v2 = well_known_option_isolation_snapshot(program_v2);
    publish_with_test_imports(&mut session, &source_v2);
    session.rooted_cfg(&options).unwrap();

    let left_key_v2 = body_query_key(&mut session, &options, "left");
    let left_stamp_v2 = retained_body_transaction(&session, &left_key_v2).0;

    assert_eq!(
        left_stamp_v1, left_stamp_v2,
        "editing the i64-owning sibling must not disturb left's terminal identity: \
             left has no edge to the i64 specialization",
    );
}

#[allow(dead_code)]
fn projected_anonymous_nominals(
    session: &mut CompilerSession,
    options: &CompileOptions,
) -> Arc<[crate::durable_semantics::DurableAnonymousNominal]> {
    let merged = session.merge().unwrap();
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .unwrap();
    session
        .queries
        .revisioned
        .projected_declaration_semantics(
            revision,
            merged.ast(),
            options.target,
            &options.preview_features,
            rue_query::CancellationToken::new(),
        )
        .unwrap()
        .anonymous_nominals
}

/// Warm-session locality of callable identity (RUE-1125).
///
/// Inserting an unreachable, same-named free function into an unrelated
/// module must not touch an existing function at all. Identity is derived
/// from a declaration's own module and source name, so `helpers.value`
/// keeps its semantic identity, its body/declaration terminals, its
/// dependency set, its machine symbol, and its presentation name, and its
/// body is reused rather than recomputed.
#[test]
fn an_unrelated_same_named_declaration_does_not_disturb_a_warm_body() {
    const ROOT: &str = "const helpers = @import(\"helpers.rue\");\n\
             const spare = @import(\"spare.rue\");\n\
             fn main() -> i32 { helpers.value() + spare.unrelated() }";
    const HELPERS: &str = "pub fn value() -> i32 { 10 }";
    const SPARE: &str = "pub fn unrelated() -> i32 { 20 }";
    let program = |spare: &str| {
        snapshot(
            &[
                (1, "/p/main.rue", "main.rue", ROOT),
                (2, "/p/helpers.rue", "helpers.rue", HELPERS),
                (3, "/p/spare.rue", "spare.rue", spare),
            ],
            1,
        )
    };
    let options = CompileOptions::default();

    let mut session = CompilerSession::new();
    publish_with_test_imports(&mut session, &program(SPARE));
    let cold = session.rooted_cfg(&options).unwrap();
    let value = body_query_key_in(&options, "helpers.rue", "value");

    // Everything RUE-1125 requires to be a function of `value`'s own
    // declaration: its query terminals, its dependency set, its emitted
    // symbols, and how it is presented.
    let observed = |session: &CompilerSession, semantic: &crate::RootedCfgOutput| {
        let function = semantic
            .functions()
            .iter()
            .find(|function| function.definition_source_name() == Some("value"))
            .expect("value is reached from main");
        (
            retained_body_query_stamps(session, &value),
            retained_body_closure_stamps(session, &value),
            retained_body_dependency_nodes(session, &value),
            function.function.clone(),
            function.legacy_name().to_owned(),
            function.record.codegen.defined_symbol.clone(),
        )
    };
    let before = observed(&session, &cold);
    assert_eq!(
        before.4, "__rue_fn_helpers_2erue__value",
        "an ordinary free function is module-qualified from the start"
    );
    let codegen = |session: &mut CompilerSession, semantic| {
        session
            .codegen_units(
                semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        session
            .codegen_executions()
            .iter()
            .find(|(identity, _)| *identity == before.3)
            .map(|(_, execution)| *execution)
            .expect("value publishes a codegen unit")
    };
    assert_eq!(
        codegen(&mut session, &cold),
        rue_query::RequestExecution::Computed
    );

    // Insert an unreachable free function with the same source name into a
    // module `value` has no relationship with. Appending leaves every
    // existing span in `spare.rue` untouched.
    let edited = format!("{SPARE}\n@allow(unused_function)\npub fn value() -> i32 {{ 99 }}\n");
    publish_with_test_imports(&mut session, &program(&edited));
    let warm = session.rooted_cfg(&options).unwrap();
    let after = observed(&session, &warm);

    assert_eq!(
        before.0, after.0,
        "the body transaction and produced-anonymous terminal must keep their stamps"
    );
    assert_eq!(
        before.1, after.1,
        "the body closure and its retained transaction must keep their stamps"
    );
    assert_eq!(before.2, after.2, "the dependency set must be unchanged");
    assert_eq!(
        (&before.3, &before.4, &before.5),
        (&after.3, &after.4, &after.5),
        "semantic identity, internal symbol, and machine symbol must be unchanged"
    );
    assert_eq!(
        warm.work().body_analysis.body_analyses_computed,
        0,
        "no body may be recomputed: {:?}",
        warm.work().body_analysis
    );
    assert_eq!(
        codegen(&mut session, &warm),
        rue_query::RequestExecution::Reused,
        "the machine-code terminal must be reused, not re-emitted"
    );

    // The declaration terminal and the presentation identity are equally
    // unaffected, and the new declaration really is present and distinct.
    let bound = warm
        .declarations()
        .iter()
        .filter(|declaration| declaration.key.name() == "value")
        .map(|declaration| declaration.key.clone())
        .collect::<Vec<_>>();
    let helpers_value = match &before.3 {
        crate::FunctionInstanceKey::Definition(key) => key.clone(),
        other => panic!("value is an ordinary definition: {other:?}"),
    };
    assert!(
        bound.contains(&helpers_value),
        "helpers.value keeps its declaration identity: {bound:?}"
    );
    assert!(
        bound
            .iter()
            .any(|key| key.module().logical_path() == "spare.rue"),
        "the inserted declaration is really bound, as its own module's: {bound:?}"
    );
    assert_eq!(bound.len(), 2, "{bound:?}");
    let presented = session
        .rooted_cfg(&options)
        .unwrap()
        .functions()
        .iter()
        .map(|function| function.source_name().to_owned())
        .collect::<Vec<_>>();
    assert!(
        presented.contains(&"value".to_owned()),
        "presentation names the declaration, not its internal symbol: {presented:?}"
    );

    // Warm and fresh must agree on the whole artifact.
    let mut fresh = CompilerSession::new();
    publish_with_test_imports(&mut fresh, &program(&edited));
    let expected = fresh.rooted_cfg(&options).unwrap();
    assert_rooted_cfg_parity(&session, &warm, &expected);
}

#[test]
fn body_query_stamps_preserve_caller_and_reference_values_across_body_only_edits() {
    let options = CompileOptions::default();
    let first = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
    )
    .unwrap();
    let second = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 2 } fn main() -> i32 { helper() }",
    )
    .unwrap();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let main = body_query_key(&mut session, &options, "main");
    let helper = body_query_key(&mut session, &options, "helper");
    let first_main = retained_body_query_stamps(&session, &main);
    let first_helper = retained_body_query_stamps(&session, &helper);

    session.update(&second).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let second_main = retained_body_query_stamps(&session, &main);
    let second_helper = retained_body_query_stamps(&session, &helper);

    assert_eq!(
        first_main, second_main,
        "callee bodies are not caller inputs"
    );
    assert_ne!(first_helper.0, second_helper.0);
    assert_eq!(first_helper.1, second_helper.1);
}

#[test]
fn codegen_units_observe_exact_owned_dependencies_and_reuse_unchanged_callers() {
    let options = CompileOptions::default();
    let first = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 1 } fn main() -> i32 { if false { helper() } else { 0 } }",
    )
    .unwrap();
    let second = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 2 } fn main() -> i32 { if false { helper() } else { 0 } }",
    )
    .unwrap();
    let mut session = CompilerSession::new();
    let compile_units = |session: &mut CompilerSession| {
        let semantic = session.rooted_cfg(&options).unwrap();
        session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        semantic
    };

    session.update(&first).into_result().unwrap();
    let cold = compile_units(&mut session);
    assert_eq!(session.codegen_collections(), cold.functions().len());
    assert!(
        session
            .codegen_executions()
            .iter()
            .all(|(_, execution)| { *execution == rue_query::RequestExecution::Computed })
    );
    for (_, work) in session.codegen_attempt_work() {
        let amount = |label: &str| {
            work.iter()
                .find_map(|(candidate, amount)| (candidate.as_ref() == label).then_some(*amount))
                .unwrap_or(0)
        };
        assert_eq!(amount("codegen.dependencies.optimized-cfg"), 1);
        assert_eq!(amount("codegen.lowering.local"), 1);
        assert_eq!(amount("codegen.unit.successes"), 1);
    }
    let cold_main = cold
        .functions()
        .iter()
        .find(|function| function.record.codegen.defined_symbol.as_ref() == "main")
        .unwrap();
    let cold_main_work = session
        .codegen_attempt_work()
        .iter()
        .find(|(identity, _)| identity == &cold_main.function)
        .unwrap();
    assert!(cold_main_work.1.iter().any(|(label, amount)| {
        label.as_ref() == "codegen.domain.symbol-aliases" && *amount > 0
    }));

    session.update(&second).into_result().unwrap();
    let warm = compile_units(&mut session);
    assert_eq!(session.codegen_collections(), warm.functions().len());
    let identity_for = |name: &str| {
        warm.functions()
            .iter()
            .find(|function| function.definition_source_name() == Some(name))
            .unwrap()
            .function
            .clone()
    };
    let execution_for = |identity: &crate::FunctionInstanceKey| {
        session
            .codegen_executions()
            .iter()
            .find_map(|(candidate, execution)| (candidate == identity).then_some(*execution))
            .unwrap()
    };
    assert_eq!(
        execution_for(&identity_for("main")),
        rue_query::RequestExecution::Reused,
        "an optimized-away alias does not make a callee implementation an exact caller dependency"
    );
    assert_eq!(
        execution_for(&identity_for("helper")),
        rue_query::RequestExecution::Computed
    );
    let main_work = session
        .codegen_attempt_work()
        .iter()
        .find(|(identity, _)| identity == &identity_for("main"))
        .unwrap();
    assert!(
        main_work.1.is_empty(),
        "reused codegen terminals perform no local lowering or dependency collection"
    );
}

#[test]
fn sibling_only_edit_reuses_anonymous_member_transaction_cfg_and_codegen() {
    let program = |sibling: &str| {
        let text = format!(
            "fn sibling() -> i32 {{ {sibling} }}\n\
                 const T: type = struct {{\n\
                     value: i32,\n\
                     fn get(self) -> i32 {{ self.value }}\n\
                 }};\n\
                 fn main() -> i32 {{\n\
                     let value: T = T {{ value: 5 }};\n\
                     value.get()\n\
                 }}"
        );
        snapshot(&[(1, "/p/main.rue", "main.rue", text.as_str())], 1)
    };
    let options = CompileOptions::default();
    let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
        target: options.target,
        preview_features: StablePreviewFeatures::new(&options.preview_features),
    };
    let observe = |session: &mut CompilerSession| {
        let semantic = session.rooted_cfg(&options).unwrap();
        let get = semantic
            .functions()
            .iter()
            .find_map(|function| {
                matches!(
                    &function.function,
                    crate::FunctionInstanceKey::AnonymousMember { member, .. }
                        if member.name.as_ref() == "get"
                )
                .then(|| function.function.clone())
            })
            .expect("main reaches the anonymous T.get member");
        let cfg_execution = session
            .rooted_cfg_executions()
            .iter()
            .find_map(|(function, execution)| (function == &get).then_some(*execution))
            .expect("rooted CFG records T.get's optimized-CFG request");
        let transaction_stamp = retained_body_transaction(
            session,
            &crate::body_query::BodyQueryKey::new(get.clone(), configuration.clone()),
        )
        .0;
        let units = session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        let unit = units
            .iter()
            .find_map(|unit| (unit.function == get).then(|| unit.unit.clone()))
            .expect("T.get publishes a codegen unit");
        let codegen_execution = session
            .codegen_executions()
            .iter()
            .find_map(|(function, execution)| (function == &get).then_some(*execution))
            .expect("codegen records T.get's request");
        (
            get,
            transaction_stamp,
            cfg_execution,
            codegen_execution,
            unit,
        )
    };

    let mut session = CompilerSession::new();
    session.update(&program("1")).into_result().unwrap();
    let first = observe(&mut session);
    assert_eq!(first.2, rue_query::RequestExecution::Computed);
    assert_eq!(first.3, rue_query::RequestExecution::Computed);

    session.update(&program("123456")).into_result().unwrap();
    let second = observe(&mut session);
    assert_eq!(second.0, first.0);
    assert_eq!(second.1, first.1);
    assert_eq!(second.2, rue_query::RequestExecution::Reused);
    assert_eq!(second.3, rue_query::RequestExecution::Reused);
    assert!(Arc::ptr_eq(&first.4, &second.4));
}

#[test]
fn cfg_keys_are_constructed_once_and_typed_equality_resolves_memo_hash_collisions() {
    fn hash(key: &crate::cfg_query::CfgQueryKey) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(key, &mut hasher);
        std::hash::Hasher::finish(&hasher)
    }

    let first = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
    )
    .unwrap();
    let second = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 2 } fn main() -> i32 { helper() }",
    )
    .unwrap();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();

    session.update(&first).into_result().unwrap();
    let (first_cfgs, constructions) =
        crate::cfg_query::with_test_cfg_query_key_construction_count(|| {
            session.rooted_cfg(&options)
        });
    let first_cfgs = first_cfgs.unwrap();
    assert_eq!(
        constructions,
        first_cfgs.cfgs.len(),
        "a no-accessor production root constructs each final CFG key exactly once"
    );
    let first_helper = first_cfgs
        .cfgs
        .iter()
        .find(|cfg| crate::cfg_query::accessor_source_name(&cfg.function) == "helper")
        .unwrap()
        .optimized_cfg_key
        .cfg
        .clone();
    drop(first_cfgs);

    session.update(&second).into_result().unwrap();
    let second_cfgs = session.rooted_cfg(&options).unwrap();
    let second_helper = second_cfgs
        .cfgs
        .iter()
        .find(|cfg| crate::cfg_query::accessor_source_name(&cfg.function) == "helper")
        .unwrap()
        .optimized_cfg_key
        .cfg
        .clone();

    assert_ne!(first_helper, second_helper);
    assert_eq!(
        hash(&first_helper),
        hash(&second_helper),
        "semantic versions of one function deliberately share the cheap memo partition"
    );
    let mut exact = AHashMap::new();
    exact.insert(first_helper, "first");
    exact.insert(second_helper, "second");
    assert_eq!(exact.len(), 2, "typed equality resolves the hash collision");
}

#[test]
fn canonical_codegen_units_preserve_named_and_anonymous_bytes_on_both_architectures() {
    let source = SourceSnapshot::single(
        "main.rue",
        "fn add(left: i32, right: i32) -> i32 { left + right }\n\
             fn Box() -> type {\n\
                 struct {\n\
                     value: i32,\n\
                     fn make(value: i32) -> Self { Self { value: value } }\n\
                     fn get(borrow self) -> i32 { self.value }\n\
                     drop fn(self) {}\n\
                 }\n\
             }\n\
             fn main() -> i32 {\n\
                 let message = \"owned\";\n\
                 @dbg(message);\n\
                 let B = Box();\n\
                 let boxed = B.make(add(20, 22));\n\
                 boxed.get()\n\
             }",
    )
    .unwrap();
    for target in [Target::X86_64Linux, Target::Aarch64Linux] {
        let options = CompileOptions {
            target,
            ..CompileOptions::default()
        };
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let semantic = session.rooted_cfg(&options).unwrap();
        let units = session
            .codegen_units(
                &semantic,
                &options,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
        assert!(!units.is_empty());
        assert!(units.iter().all(|unit| {
            unit.unit
                .sections
                .iter()
                .any(|section| section.kind == crate::codegen_query::SectionKind::Text)
        }));
    }
}

#[test]
fn unchanged_consumer_observes_function_produced_anonymous_fact_changes() {
    let first = SourceSnapshot::single(
            "main.rue",
            "const N: i32 = 1; fn Make() -> type { struct { values: [i32; N] } } fn size(comptime T: type) -> i32 { @size_of(T) } fn main() -> i32 { size(Make()) }",
        )
        .unwrap();
    let second = SourceSnapshot::single(
            "main.rue",
            "const N: i32 = 2; fn Make() -> type { struct { values: [i32; N] } } fn size(comptime T: type) -> i32 { @size_of(T) } fn main() -> i32 { size(Make()) }",
        )
        .unwrap();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&first).into_result().unwrap();
    let cold = session.rooted_cfg(&options).unwrap();
    let main = body_query_key(&mut session, &options, "main");
    let make_definition = body_query_key(&mut session, &options, "Make");
    let make = crate::body_query::BodyQueryKey::new(
        crate::FunctionInstanceKey::Specialization {
            base: Node::new(make_definition.instance.clone()),
            arguments: crate::CanonicalArguments::default(),
        },
        main.configuration.clone(),
    );
    let size = cold
        .functions()
        .iter()
        .find(|function| specialization_arguments(function, "size").is_some())
        .unwrap();
    let size =
        crate::body_query::BodyQueryKey::new(size.function.clone(), main.configuration.clone());
    let first_make_stamps = retained_body_query_stamps(&session, &make);
    let first_size_stamps = retained_body_query_stamps(&session, &size);
    let make_dependencies = retained_body_dependency_nodes(&session, &make);
    assert!(
        make_dependencies
            .iter()
            .any(|dependency| dependency.contains("const:") && dependency.contains(":N:")),
        "{make_dependencies:?}"
    );
    let main_transaction = retained_body_transaction(&session, &main).2;
    let main_dependencies = retained_body_dependency_nodes(&session, &main);
    assert!(
        main_dependencies.iter().any(|dependency| {
            dependency.contains("body-produced-anonymous") && dependency.contains("Make")
        }),
        "transaction={main_transaction:?}; dependencies={main_dependencies:?}"
    );

    session.update(&second).into_result().unwrap();
    let warm = session.rooted_cfg(&options).unwrap();
    let second_make_stamps = retained_body_query_stamps(&session, &make);
    let second_size_stamps = retained_body_query_stamps(&session, &size);
    assert_ne!(first_make_stamps.1, second_make_stamps.1);
    assert_ne!(first_size_stamps.0, second_size_stamps.0);

    let mut fresh = CompilerSession::new();
    fresh.update(&second).into_result().unwrap();
    let expected = fresh.rooted_cfg(&options).unwrap();
    assert_rooted_cfg_parity(&session, &warm, &expected);
}

#[test]
fn deferred_value_type_constructor_positions_publish_a_complete_body_closure() {
    let source = SourceSnapshot::single(
        "main.rue",
        r#"
                fn Witness(comptime T: type, comptime value: T) -> type {
                    struct { payload: T }
                }

                fn Wrap(comptime T: type) -> type {
                    struct { inner: T }
                }

                fn read(w: Witness(i32, 7)) -> i32 { w.payload }

                fn main() -> i32 {
                    let W = Witness(i32, 7);
                    let Wrapped = Wrap(Witness(i32, 7));
                    let wrapped = Wrapped { inner: W { payload: 42 } };
                    read(wrapped.inner)
                }
            "#,
    )
    .unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.rooted_cfg(&CompileOptions::default()).unwrap();
}

#[test]
fn anonymous_specialization_dependency_priority_prevents_lexical_starvation() {
    let source = SourceSnapshot::single(
        "main.rue",
        r#"
                fn ABox(comptime T: type) -> type { struct { item: T } }
                fn ZItem() -> type { struct { value: i32 } }
                fn main() -> i32 {
                    let Item = ZItem();
                    let Box = ABox(Item);
                    let boxed = Box { item: Item { value: 42 } };
                    boxed.item.value
                }
            "#,
    )
    .unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let options = CompileOptions::default();
    session.rooted_cfg(&options).unwrap();
    let main = body_query_key(&mut session, &options, "main");
    let transaction = retained_body_transaction(&session, &main).2;
    let mut producers = BTreeSet::new();
    for reference in transaction.references().0.iter() {
        match reference {
            crate::body_query::BodyReference::Callable(function) => {
                if let Some(definition) = stable_function_definition_root(function) {
                    producers.insert(definition.name().to_owned());
                }
            }
            crate::body_query::BodyReference::Type(ty)
            | crate::body_query::BodyReference::DropGlue(ty) => {
                let owner = crate::FunctionInstanceKey::DropGlue(Node::new(ty.clone()));
                producers.extend(
                    crate::revisioned_query_database::collect_instance_anonymous_nominals(&owner)
                        .iter()
                        .filter_map(|identity| {
                            stable_producer_definition_root(&identity.producer)
                                .map(|definition| definition.name().to_owned())
                        }),
                );
            }
            crate::body_query::BodyReference::Definition(definition) => {
                producers.insert(definition.name().to_owned());
            }
        }
    }
    assert!(producers.contains("ABox"));
    assert!(producers.contains("ZItem"));
}

#[test]
fn negative_body_lookup_recomputes_when_a_declaration_is_added() {
    let missing = SourceSnapshot::single("main.rue", "fn main() -> i32 { helper() }").unwrap();
    let resolved = SourceSnapshot::single(
        "main.rue",
        "fn main() -> i32 { helper() } fn helper() -> i32 { 42 }",
    )
    .unwrap();
    let options = CompileOptions::default();
    let mut warm = CompilerSession::new();
    warm.update(&missing).into_result().unwrap();
    assert!(warm.rooted_cfg(&options).is_err());
    let main = crate::body_query::BodyQueryKey::new(
        crate::FunctionInstanceKey::Definition(crate::StableDefinitionKey::from_stable_parts(
            crate::ModuleId::from_logical_path("main.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "main",
            None,
        )),
        crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: options.target,
            preview_features: StablePreviewFeatures::new(&options.preview_features),
        },
    );
    let dependencies = retained_body_dependency_nodes(&warm, &main);
    assert!(
        dependencies
            .iter()
            .any(|node| node.contains("lookup-name") && node.contains("helper")),
        "{dependencies:?}"
    );
    assert!(
        dependencies
            .iter()
            .all(|node| !node.contains("module-declaration-set")),
        "{dependencies:?}"
    );

    warm.update(&resolved).into_result().unwrap();
    let warm_output = warm.rooted_cfg(&options).unwrap();
    let mut fresh = CompilerSession::new();
    fresh.update(&resolved).into_result().unwrap();
    let fresh_output = fresh.rooted_cfg(&options).unwrap();
    assert_eq!(
        format!("{:?}", warm_output.functions()),
        format!("{:?}", fresh_output.functions())
    );
    assert_eq!(warm_output.functions().len(), 2);
}

#[test]
fn qualified_negative_body_lookup_recomputes_when_imported_member_is_added() {
    let main = r#"const lib = @import("lib.rue"); fn main() -> i32 { lib.helper() }"#;
    let missing = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", main),
            (2, "/p/lib.rue", "lib.rue", "pub const value: i32 = 1;"),
        ],
        1,
    );
    let resolved = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", main),
            (2, "/p/lib.rue", "lib.rue", "pub fn helper() -> i32 { 42 }"),
        ],
        1,
    );
    let options = CompileOptions::default();
    let mut warm = CompilerSession::new();
    publish_with_test_imports(&mut warm, &missing);
    assert!(warm.rooted_cfg(&options).is_err());

    publish_with_test_imports(&mut warm, &resolved);
    let warm_output = warm.rooted_cfg(&options).unwrap();
    let mut fresh = CompilerSession::new();
    publish_with_test_imports(&mut fresh, &resolved);
    let fresh_output = fresh.rooted_cfg(&options).unwrap();
    assert_eq!(
        format!("{:?}", warm_output.functions()),
        format!("{:?}", fresh_output.functions())
    );
    assert_eq!(warm_output.functions().len(), 2);
}

#[test]
fn body_query_values_survive_relocation_and_input_order() {
    let program = "fn helper() -> i32 { 41 } fn main() -> i32 { helper() + 1 }";
    let first = snapshot(&[(1, "/old/main.rue", "main.rue", program)], 1);
    let relocated = snapshot(&[(91, "/new/main.rue", "main.rue", program)], 91);
    let options = CompileOptions::default();
    let build = |source: &SourceSnapshot| {
        let mut session = CompilerSession::new();
        session.update(source).into_result().unwrap();
        session.rooted_cfg(&options).unwrap();
        let key = body_query_key(&mut session, &options, "main");
        let transaction = retained_body_transaction(&session, &key).2;
        (key, transaction)
    };
    let (first_key, first_transaction) = build(&first);
    let (relocated_key, relocated_transaction) = build(&relocated);
    assert_eq!(first_key, relocated_key);
    assert!(crate::body_query::transaction_equal(
        &first_transaction,
        &relocated_transaction,
    ));
}

#[test]
fn body_closure_bundle_reuses_the_transaction_canonical_body_arc() {
    let source = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 40 } fn main() -> i32 { helper() + 2 }",
    )
    .unwrap();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();

    let key = body_query_key(&mut session, &options, "main");
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .unwrap();
    let cancellation = rue_query::CancellationToken::new();
    let transaction = session
        .queries
        .revisioned
        .body_transaction(revision, key.clone(), cancellation.clone())
        .unwrap();
    let closure = session
        .queries
        .revisioned
        .body_closure(
            revision,
            crate::body_query::BodyClosureQueryKey {
                modules: Arc::from([crate::ModuleId::from_logical_path("main.rue").unwrap()]),
                roots: Arc::from([key.instance.clone()]),
                configuration: key.configuration.clone(),
            },
            cancellation,
        )
        .unwrap();
    let rue_query::QueryOutcome::Success(output) = closure.terminal.outcome() else {
        panic!("body closure must publish a value");
    };
    let retained = output
        .bodies
        .iter()
        .find(|body| body.key == key)
        .expect("body closure retains its root transaction");
    let rue_query::QueryOutcome::Success(retained) = retained.bundle.outcome() else {
        panic!("body-analysis bundle must publish a value")
    };
    let rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success {
        body: transaction_body,
        ..
    }) = transaction.outcome()
    else {
        panic!("body transaction must publish a canonical body")
    };
    let crate::body_query::BodyTransaction::Success {
        body: retained_body,
        ..
    } = &retained.transaction
    else {
        panic!("body closure must retain the successful transaction value")
    };
    assert!(Arc::ptr_eq(transaction_body, retained_body));
}

#[test]
fn recursive_body_query_publishes_a_terminal_without_a_query_cycle() {
    let source = SourceSnapshot::single(
            "main.rue",
            "fn recurse(n: i32) -> i32 { if n == 0 { 0 } else { recurse(n - 1) } } fn main() -> i32 { recurse(4) }",
        )
        .unwrap();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let key = body_query_key(&mut session, &options, "recurse");
    let transaction = retained_body_transaction(&session, &key).2;
    assert!(matches!(
        transaction,
        crate::body_query::BodyTransaction::Success { .. }
    ));
    assert!(
        transaction
            .references()
            .0
            .iter()
            .any(|reference| match reference {
                crate::body_query::BodyReference::Callable(instance) => instance == &key.instance,
                crate::body_query::BodyReference::Definition(definition) => {
                    matches!(
                        &key.instance,
                        crate::FunctionInstanceKey::Definition(owner) if owner == definition
                    )
                }
                crate::body_query::BodyReference::Type(_)
                | crate::body_query::BodyReference::DropGlue(_) => false,
            })
    );
}

#[test]
fn reachable_comptime_specialization_is_composed_from_its_body_terminal() {
    let source = SourceSnapshot::single(
            "main.rue",
            "fn make(comptime N: i32) -> [i32; N] { [7; N] } fn main() -> i32 { let a: [i32; 3] = make(1 + 2); a[0] + a[1] + a[2] }",
        )
        .unwrap();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let output = session.rooted_cfg(&options).unwrap();

    assert!(
        output.functions().iter().any(|function| matches!(
            function.function,
            crate::FunctionInstanceKey::Specialization { .. }
        )),
        "the reachable specialization must be composed into canonical output"
    );
}

#[test]
fn target_and_preview_configuration_select_distinct_body_terminals() {
    let source = SourceSnapshot::single("main.rue", "fn main() -> i32 { 42 }").unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();

    let default_options = CompileOptions::default();
    session.rooted_cfg(&default_options).unwrap();
    let default_key = body_query_key(&mut session, &default_options, "main");
    let default = retained_body_transaction(&session, &default_key);

    let configured_options = CompileOptions {
        target: if default_options.target == crate::Target::X86_64Linux {
            crate::Target::Aarch64Linux
        } else {
            crate::Target::X86_64Linux
        },
        preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
        ..CompileOptions::default()
    };
    session.rooted_cfg(&configured_options).unwrap();
    let configured_key = body_query_key(&mut session, &configured_options, "main");
    let configured = retained_body_transaction(&session, &configured_key);

    assert_ne!(default_key, configured_key);
    assert!(matches!(
        default.2,
        crate::body_query::BodyTransaction::Success { .. }
    ));
    assert!(matches!(
        configured.2,
        crate::body_query::BodyTransaction::Success { .. }
    ));
}

#[test]
fn ordinary_body_transaction_runs_from_exact_input_and_provider_facts() {
    let source = SourceSnapshot::single(
        "main.rue",
        "fn helper() -> i32 { 40 } fn main() -> i32 { helper() + 2 }",
    )
    .unwrap();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let options = CompileOptions::default();
    session.rooted_cfg(&options).unwrap();

    let key = body_query_key(&mut session, &options, "main");
    let transaction = retained_body_transaction(&session, &key).2;
    let crate::body_query::BodyTransaction::Success {
        body, references, ..
    } = transaction
    else {
        panic!("provider-backed ordinary body must publish success");
    };
    assert!(matches!(
        body.as_ref(),
        crate::body_query::CanonicalBody::Ordinary { owner, .. }
            if owner.name() == "main"
    ));
    assert!(references.0.iter().any(|reference| matches!(
        reference,
        crate::body_query::BodyReference::Callable(
            crate::FunctionInstanceKey::Definition(definition)
        ) if definition.name() == "helper"
    )));
    let dependencies = retained_body_dependency_nodes(&session, &key);
    assert!(
        dependencies
            .iter()
            .any(|dependency| dependency.contains("compiler.declaration-body-plan-artifacts")),
        "{dependencies:?}"
    );
    assert!(
        dependencies
            .iter()
            .any(|dependency| dependency.contains("compiler.body-source-basis")),
        "{dependencies:?}"
    );
    assert!(
        dependencies
            .iter()
            .all(|dependency| !dependency.contains("compiler.body-input")),
        "{dependencies:?}"
    );
}

#[test]
fn failed_body_transaction_retains_every_positive_provider_reference() {
    let source = SourceSnapshot::single(
        "main.rue",
        "struct S { fn value(self) -> i32 { 1 } }\n\
             const C: i32 = 2;\n\
             fn helper() -> i32 { 3 }\n\
             fn main() -> i32 {\n\
                 let s = S {};\n\
                 let resolved = helper() + s.value() + C;\n\
                 resolved + missing\n\
             }",
    )
    .unwrap();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    assert!(session.rooted_cfg(&options).is_err());
    let key = crate::body_query::BodyQueryKey::new(
        crate::FunctionInstanceKey::Definition(crate::StableDefinitionKey::from_stable_parts(
            crate::ModuleId::from_logical_path("main.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            Arc::from("main"),
            None,
        )),
        crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: options.target,
            preview_features: StablePreviewFeatures::new(&options.preview_features),
        },
    );
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .expect("failed semantic request retains its revision");
    let terminal = session
        .queries
        .revisioned
        .body_transaction(revision, key, rue_query::CancellationToken::new())
        .expect("deterministic body error publishes a typed terminal");
    let rue_query::QueryOutcome::Success(
        crate::body_query::BodyTransaction::DeterministicFailure { references, .. },
    ) = terminal.outcome()
    else {
        panic!(
            "expected deterministic body failure: {:?}",
            terminal.outcome()
        );
    };
    assert!(references.0.iter().any(|reference| matches!(
        reference,
        crate::body_query::BodyReference::Callable(
            crate::FunctionInstanceKey::Definition(definition)
        ) if definition.name() == "helper"
    )));
    assert!(references.0.iter().any(|reference| matches!(
        reference,
        crate::body_query::BodyReference::Callable(
            crate::FunctionInstanceKey::Definition(definition)
        ) if definition.name() == "value"
    )));
    assert!(references.0.iter().any(|reference| matches!(
        reference,
        crate::body_query::BodyReference::Definition(definition)
            if definition.name() == "C"
    )));
    assert!(references.0.iter().any(|reference| matches!(
        reference,
        crate::body_query::BodyReference::Type(crate::TypeInstanceKey::Nominal(
            crate::NominalInstanceKey::Named(definition)
        )) if definition.name() == "S"
    )));
}

#[test]
fn body_callable_dependencies_stop_at_exact_semantic_signature_candidates() {
    let source = SourceSnapshot::single(
        "main.rue",
        "extern \"C\" { fn foreign() -> i32; }\n\
             fn helper(value: i32) -> i32 { value + 1 }\n\
             fn unrelated() -> i32 { 7 }\n\
             fn main() -> i32 { helper(1) + checked { foreign() } }",
    )
    .unwrap();
    let options = CompileOptions {
        preview_features: PreviewFeatures::from([PreviewFeature::CFfi]),
        ..CompileOptions::default()
    };
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let main = body_query_key(&mut session, &options, "main");
    let main_transaction = retained_body_transaction(&session, &main).2;
    let dependencies = retained_body_dependency_nodes(&session, &main);

    for name in ["helper", "foreign"] {
        assert!(
            dependencies
                .iter()
                .any(|node| { node.contains("compiler.lookup-name") && node.contains(name) }),
            "body transaction must observe the exact provider lookup for {name}: \
                 transaction={main_transaction:?}; dependencies={dependencies:?}"
        );
    }
    assert!(
        dependencies
            .iter()
            .all(|node| !node.contains("compiler.declaration-signature-projection")),
        "body analysis must not regain the deleted peer signature family: \
             transaction={main_transaction:?}; dependencies={dependencies:?}"
    );
    assert!(
        dependencies.iter().any(|node| {
            node.contains("compiler.semantic-nucleus")
                && node.contains("signature:")
                && node.contains(":Function:")
                && node.contains("helper")
        }),
        "{dependencies:?}"
    );
    for (category, name) in [("ExternFunction", "helper"), ("Function", "foreign")] {
        assert!(
            dependencies.iter().all(|node| {
                !(node.contains("compiler.semantic-nucleus")
                    && node.contains("signature:")
                    && node.contains(&format!(":{category}:"))
                    && node.contains(name))
            }),
            "the unselected opposite-category signature must stay behind the lookup for \
                 {name}: dependencies={dependencies:?}"
        );
    }
    assert!(
        dependencies.iter().any(|node| {
            node.contains("compiler.semantic-nucleus")
                && node.contains("signature:")
                && node.contains(":ExternFunction:")
                && node.contains("foreign")
        }),
        "{dependencies:?}"
    );
    assert!(
        dependencies.iter().all(|node| !node.contains("unrelated")),
        "an unrelated declaration must not become a body dependency: {dependencies:?}"
    );
    assert!(
        dependencies
            .iter()
            .all(|node| !node.contains("compiler.declaration-occurrence-index")),
        "the module-wide occurrence index must stay behind the stable classifier: \
             {dependencies:?}"
    );

    let ambiguous = SourceSnapshot::single(
        "main.rue",
        "extern \"C\" { fn foreign() -> i32; fn helper(value: i32) -> i32; }\n\
             fn helper(value: i32) -> i32 { value + 1 }\n\
             fn unrelated() -> i32 { 7 }\n\
             fn main() -> i32 { helper(1) + checked { foreign() } }",
    )
    .unwrap();
    session.update(&ambiguous).into_result().unwrap();
    assert!(session.rooted_cfg(&options).is_err());
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .expect("failed semantic attempt publishes its revision");
    let result = session.queries.revisioned.body_transaction(
        revision,
        main.clone(),
        rue_query::CancellationToken::new(),
    );
    let terminal = result.expect("ambiguous callable publishes a typed body failure");
    assert!(matches!(
        terminal.outcome(),
        rue_query::QueryOutcome::Success(
            crate::body_query::BodyTransaction::DeterministicFailure { .. }
        )
    ));

    let mut fresh = CompilerSession::new();
    fresh.update(&ambiguous).into_result().unwrap();
    assert!(fresh.rooted_cfg(&options).is_err());
    let fresh_revision = fresh
        .queries
        .revisioned
        .current_semantic_revision()
        .expect("fresh failed semantic attempt publishes its revision");
    let fresh_result = fresh.queries.revisioned.body_transaction(
        fresh_revision,
        main,
        rue_query::CancellationToken::new(),
    );
    let terminal = fresh_result.expect("fresh ambiguity publishes a typed body failure");
    assert!(matches!(
        terminal.outcome(),
        rue_query::QueryOutcome::Success(
            crate::body_query::BodyTransaction::DeterministicFailure { .. }
        )
    ));
}

#[test]
fn body_candidate_classifier_invalidates_same_category_duplicates_warm_and_fresh() {
    let valid = SourceSnapshot::single(
        "main.rue",
        "fn helper(value: i32) -> i32 { value + 1 }\n\
             fn main() -> i32 { helper(1) }",
    )
    .unwrap();
    let duplicate = SourceSnapshot::single(
        "main.rue",
        "fn helper(value: i32) -> i32 { value + 1 }\n\
             fn helper(value: i32) -> i32 { value + 2 }\n\
             fn main() -> i32 { helper(1) }",
    )
    .unwrap();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&valid).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let main = body_query_key(&mut session, &options, "main");
    let dependencies = retained_body_dependency_nodes(&session, &main);
    assert!(
        dependencies
            .iter()
            .any(|node| { node.contains("compiler.lookup-name") && node.contains("helper") }),
        "{dependencies:?}"
    );

    session.update(&duplicate).into_result().unwrap();
    assert!(session.rooted_cfg(&options).is_err());
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .expect("failed semantic attempt publishes its revision");
    let result = session.queries.revisioned.body_transaction(
        revision,
        main.clone(),
        rue_query::CancellationToken::new(),
    );
    let terminal = result.expect("warm duplicate publishes a typed body failure");
    assert!(matches!(
        terminal.outcome(),
        rue_query::QueryOutcome::Success(
            crate::body_query::BodyTransaction::DeterministicFailure { .. }
        )
    ));

    let mut fresh = CompilerSession::new();
    fresh.update(&duplicate).into_result().unwrap();
    assert!(fresh.rooted_cfg(&options).is_err());
    let fresh_revision = fresh
        .queries
        .revisioned
        .current_semantic_revision()
        .expect("fresh failed semantic attempt publishes its revision");
    let fresh_result = fresh.queries.revisioned.body_transaction(
        fresh_revision,
        main,
        rue_query::CancellationToken::new(),
    );
    let terminal = fresh_result.expect("fresh duplicate publishes a typed body failure");
    assert!(matches!(
        terminal.outcome(),
        rue_query::QueryOutcome::Success(
            crate::body_query::BodyTransaction::DeterministicFailure { .. }
        )
    ));
}

#[test]
fn non_exhaustive_directive_invalidates_external_match_body() {
    let root = r#"const colors = @import("colors.rue");
fn main() -> i32 {
    match colors.pick() {
        colors.Color.Red => 1,
        colors.Color.Green => 2,
    }
}"#;
    let closed = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", root),
            (
                2,
                "/p/colors.rue",
                "colors.rue",
                "pub enum Color { Red, Green }\n\
                     pub fn pick() -> Color { Color.Green }",
            ),
        ],
        1,
    );
    let open = snapshot(
        &[
            (1, "/p/main.rue", "main.rue", root),
            (
                2,
                "/p/colors.rue",
                "colors.rue",
                "@non_exhaustive pub enum Color { Red, Green }\n\
                     pub fn pick() -> Color { Color.Green }",
            ),
        ],
        1,
    );
    let options = CompileOptions {
        preview_features: PreviewFeatures::from([PreviewFeature::NonExhaustiveEnums]),
        ..CompileOptions::default()
    };
    let mut session = CompilerSession::new();
    publish_with_test_imports(&mut session, &closed);
    session
        .rooted_cfg(&options)
        .expect("a closed imported enum remains exhaustive");
    let key = body_query_key(&mut session, &options, "main");
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .expect("the closed match publishes a semantic revision");
    let first = session
        .queries
        .revisioned
        .body_transaction(revision, key.clone(), rue_query::CancellationToken::new())
        .expect("the closed match publishes a body terminal");
    assert!(matches!(
        first.outcome(),
        rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success { .. })
    ));

    publish_with_test_imports(&mut session, &open);
    let errors = session
        .rooted_cfg(&options)
        .expect_err("an external match must require a wildcard after the directive is added");
    assert!(
        errors
            .iter()
            .any(|error| matches!(error.kind, ErrorKind::NonExhaustiveMatch)),
        "unexpected diagnostics: {errors:?}"
    );
    let revision = session
        .queries
        .revisioned
        .current_semantic_revision()
        .expect("the failed match publishes a semantic revision");
    let second = session
        .queries
        .revisioned
        .body_transaction(revision, key, rue_query::CancellationToken::new())
        .expect("the non-exhaustive match publishes a deterministic body failure");
    assert_ne!(
        second.stamp(),
        first.stamp(),
        "changing @non_exhaustive must invalidate the external match body terminal"
    );
    assert!(matches!(
        second.outcome(),
        rue_query::QueryOutcome::Success(
            crate::body_query::BodyTransaction::DeterministicFailure { .. }
        )
    ));
}

#[test]
fn unreachable_body_is_not_requested_by_production_reachability() {
    let source =
        SourceSnapshot::single("main.rue", "fn dead() -> i32 { 1 } fn main() -> i32 { 42 }")
            .unwrap();
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    session.rooted_cfg(&options).unwrap();
    let dead = body_query_key(&mut session, &options, "dead");
    assert!(
        !session.queries.revisioned.has_retained_body_key(&dead),
        "an unreachable body must not have a retained transaction"
    );
}
