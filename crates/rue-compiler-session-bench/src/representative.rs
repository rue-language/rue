//! Small representative multi-module scenarios for RUE-901.
//!
//! The fixture files are also compiled by the real batch driver. This module
//! embeds those exact bytes and varies them only through the fixed edits below,
//! so cold and reused measurements cannot drift to different program semantics.

use std::{collections::BTreeMap, sync::Arc};

use rue_compiler::unstable::{
    DiscoverySourceAssembler, ImportDemandMode, begin_import_input_request,
    committed_import_discovery, import_demand_frontier_for_roots, import_observation_ledger,
    publish_import_observation_batch,
};
use rue_compiler::{
    AcceptedImportSource, AcceptedReadManifest, CompileOptions, CompilerSession,
    FileMetadataFingerprint, ImportDiscoveryContext, ImportObservation, PhysicalFileIdentity,
    SemanticView, SourceSnapshot,
};
use serde_json::{Value, json};

use super::{
    assert_cold_reused_parity_with_discovery, assert_diagnostic_parity, count, measure, named,
    with_parity, work_projection,
};

const ROOT: &str = "/bench/main.rue";
const FILES: &[(&str, &str)] = &[
    ("/bench/labels.rue", include_str!("fixtures/labels.rue")),
    (
        "/bench/labels_alt.rue",
        include_str!("fixtures/labels_alt.rue"),
    ),
    ("/bench/main.rue", include_str!("fixtures/main.rue")),
    ("/bench/model.rue", include_str!("fixtures/model.rue")),
    ("/bench/report.rue", include_str!("fixtures/report.rue")),
    ("/bench/std/_std.rue", include_str!("fixtures/std/_std.rue")),
    ("/bench/std/math.rue", include_str!("fixtures/std/math.rue")),
    ("/bench/worker.rue", include_str!("fixtures/worker.rue")),
];

#[derive(Clone, Copy)]
enum Variant {
    Base,
    LeafEdit,
    WidelyDependedDefinitionEdit,
    ImportChange,
    DiagnosticError,
}

fn variant_sources(variant: Variant) -> BTreeMap<String, Arc<String>> {
    FILES
        .iter()
        .filter(|(path, _)| match variant {
            Variant::ImportChange => *path != "/bench/labels.rue",
            _ => *path != "/bench/labels_alt.rue",
        })
        .map(|&(path, source)| {
            let source = match (variant, path) {
                (Variant::LeafEdit, "/bench/labels.rue") => source.replace("{ 12 }", "{ 13 }"),
                (Variant::WidelyDependedDefinitionEdit, "/bench/model.rue") => {
                    source.replace("value * 3", "value * 4")
                }
                (Variant::ImportChange, "/bench/report.rue") => {
                    source.replace("labels.rue", "labels_alt.rue")
                }
                (Variant::DiagnosticError, "/bench/report.rue") => {
                    source.replace("model.weight(value)", "missing_weight")
                }
                _ => source.to_owned(),
            };
            (path.to_owned(), Arc::new(source))
        })
        .collect()
}

fn assemble(
    sources: &BTreeMap<String, Arc<String>>,
) -> (SourceSnapshot, ImportDiscoveryContext, AcceptedReadManifest) {
    let context =
        ImportDiscoveryContext::new(1, "/bench", Some("/bench/std"), "rue-901-scenario").unwrap();
    let root = sources.get(ROOT).unwrap().clone();
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        ROOT,
        ROOT,
        PhysicalFileIdentity::new(901, 1),
        FileMetadataFingerprint::new(root.len() as u64, 0, 1),
        root,
    )
    .unwrap();
    for (path, source) in sources.iter().filter(|(path, _)| path.as_str() != ROOT) {
        let identity = identity_for_path(path);
        assembler
            .add_explicit(
                path,
                path,
                PhysicalFileIdentity::new(901, identity),
                FileMetadataFingerprint::new(source.len() as u64, 0, identity),
                source.clone(),
            )
            .unwrap();
    }
    (
        assembler.snapshot().unwrap(),
        context,
        assembler.accepted_read_manifest(),
    )
}

fn identity_for_path(path: &str) -> u64 {
    if path == ROOT {
        return 1;
    }
    FILES
        .iter()
        .position(|(candidate, _)| *candidate == path)
        .map(|index| (index + 2) as u64)
        .unwrap()
}

fn snapshot(variant: Variant) -> SourceSnapshot {
    assemble(&variant_sources(variant)).0
}

fn close_discovery(session: &mut CompilerSession, source: &SourceSnapshot) {
    if committed_import_discovery(session)
        .is_some_and(|artifact| artifact.source_revision() == source.source_revision())
    {
        return;
    }
    let sources = source
        .files()
        .map(|file| {
            let path = if file.path.starts_with('/') {
                file.path.to_owned()
            } else {
                format!("/bench/{}", file.path)
            };
            (path, Arc::new(file.source.to_owned()))
        })
        .collect::<BTreeMap<_, _>>();
    let (discovered, context, accepted_reads) = assemble(&sources);
    assert_eq!(discovered.source_revision(), source.source_revision());
    let mut revision = begin_import_input_request(
        session,
        &discovered,
        context.clone(),
        accepted_reads.clone(),
    )
    .unwrap();
    loop {
        let ledger = import_observation_ledger(session, revision).unwrap();
        let plan = session
            .stage_import_discovery(
                &discovered,
                context.clone(),
                accepted_reads.shared_slice(),
                ledger.clone(),
            )
            .unwrap();
        let frontier = import_demand_frontier_for_roots(
            session,
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .unwrap();
        if frontier.requests().is_empty() {
            session.close_import_discovery(ledger).unwrap();
            break;
        }
        let observations = frontier
            .requests()
            .iter()
            .cloned()
            .map(|request| {
                let requested_path = request.requested_path().to_owned();
                if let Some(source) = sources.get(&requested_path) {
                    let identity = identity_for_path(&requested_path);
                    ImportObservation::accepted(
                        request,
                        AcceptedImportSource::new(
                            requested_path.as_str(),
                            requested_path.as_str(),
                            PhysicalFileIdentity::new(901, identity),
                            FileMetadataFingerprint::new(source.len() as u64, 0, identity),
                            source.clone(),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                } else {
                    ImportObservation::absent(request)
                }
            })
            .collect();
        revision = publish_import_observation_batch(
            session,
            &frontier,
            &discovered,
            accepted_reads.clone(),
            observations,
        )
        .unwrap();
    }
}

fn measured_project_semantic(
    session: &mut CompilerSession,
    source: &SourceSnapshot,
    options: &CompileOptions,
) -> (Value, Arc<SemanticView>) {
    let mut output = None;
    let value = measure(session, |session| {
        let update = session.update(source);
        let parse = update.unstable_metrics();
        update.into_result().unwrap();
        close_discovery(session, source);
        output = Some(session.semantic(options).unwrap());
        parse
    });
    (value, output.unwrap())
}

fn successful_edit(name: &str, target: &SourceSnapshot) -> Value {
    let base = snapshot(Variant::Base);
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    measured_project_semantic(&mut session, &base, &options);
    session
        .unstable_dependency_baseline(&options, Some("/bench/std"))
        .unwrap();
    let (value, output) = measured_project_semantic(&mut session, target, &options);
    let parity = assert_cold_reused_parity_with_discovery(
        &mut session,
        &output,
        target,
        &options,
        Some("/bench/std"),
        close_discovery,
        |_, _| {},
    );
    named(name, with_parity(value, parity))
}

fn diagnostic_and_recovery() -> (Value, Value) {
    let base = snapshot(Variant::Base);
    let failed_source = snapshot(Variant::DiagnosticError);
    let recovered_source = snapshot(Variant::LeafEdit);
    let options = CompileOptions::default();
    let mut session = CompilerSession::new();
    measured_project_semantic(&mut session, &base, &options);
    session
        .unstable_dependency_baseline(&options, Some("/bench/std"))
        .unwrap();
    let last_good = session.last_good_semantic_diagnostics().unwrap().clone();
    let mut failed = measure(&mut session, |session| {
        let update = session.update(&failed_source);
        let parse = update.unstable_metrics();
        update.into_result().unwrap();
        close_discovery(session, &failed_source);
        session.semantic(&options).unwrap_err();
        parse
    });
    assert!(Arc::ptr_eq(
        &last_good,
        session.last_good_semantic_diagnostics().unwrap()
    ));

    let mut fresh = CompilerSession::new();
    fresh.update(&failed_source).into_result().unwrap();
    close_discovery(&mut fresh, &failed_source);
    fresh.semantic(&options).unwrap_err();
    assert_diagnostic_parity(session.latest_diagnostics(), fresh.latest_diagnostics());
    let diagnostics = session.latest_diagnostics().unwrap();
    failed["diagnostic_parity"] = json!({
        "fresh_session": "exact",
        "error_count": diagnostics.errors().len(),
        "warning_count": diagnostics.warnings().len(),
        "last_good_preserved": true,
    });
    failed["evidence_schema"] = json!({
        "version": 2,
        "differential_parity_version": 1,
        "locality_work_version": 2,
    });
    failed["required_vs_reused_work"] = work_projection(&failed);
    let failed = named("diagnostic_error_edit", failed);

    let (mut recovered, output) =
        measured_project_semantic(&mut session, &recovered_source, &options);
    let parity = assert_cold_reused_parity_with_discovery(
        &mut session,
        &output,
        &recovered_source,
        &options,
        Some("/bench/std"),
        close_discovery,
        |_, _| {},
    );
    recovered["evidence_schema"] = json!({
        "version": 2,
        "differential_parity_version": 1,
        "locality_work_version": 2,
    });
    recovered["differential_parity"] = parity;
    recovered["recovered_from_diagnostic_error"] = json!(true);
    recovered["required_vs_reused_work"] = work_projection(&recovered);
    let recovered = named("diagnostic_recovery", recovered);
    (failed, recovered)
}

pub(super) fn run_iteration() -> Vec<Value> {
    let base = snapshot(Variant::Base);
    let mut scenarios = vec![
        successful_edit("no_change_query", &base),
        successful_edit("leaf_edit", &snapshot(Variant::LeafEdit)),
        successful_edit(
            "widely_depended_definition_edit",
            &snapshot(Variant::WidelyDependedDefinitionEdit),
        ),
        successful_edit("import_change", &snapshot(Variant::ImportChange)),
    ];
    let (failed, recovered) = diagnostic_and_recovery();
    scenarios.push(failed);
    scenarios.push(recovered);
    assert_structure(&scenarios);
    scenarios
}

fn assert_structure(scenarios: &[Value]) {
    assert_eq!(scenarios.len(), 6);
    for scenario in scenarios {
        let work = &scenario["required_vs_reused_work"];
        if scenario["differential_parity"].is_object() {
            assert_eq!(scenario["differential_parity"]["schema_version"], 1);
            assert_eq!(scenario["differential_parity"]["status"], "pass");
        } else {
            assert!(scenario["diagnostic_parity"].is_object());
        }
        assert_eq!(work["schema_version"], 2);
        assert_eq!(work["counter_source"], "production_metrics");
        for artifact in ["modules", "semantic_bodies", "cfgs", "semantic_queries"] {
            assert!(work[artifact]["required"].is_u64());
            assert!(work[artifact]["reused"].is_u64());
            assert!(work[artifact]["computed"].is_u64());
            assert!(work[artifact]["invalidated"].is_u64());
        }
    }
    let get = |name: &str| {
        scenarios
            .iter()
            .find(|value| value["name"] == name)
            .unwrap()
    };
    let expected = [
        // An exact parse-family terminal hit invokes no parser or adoption
        // producer, so module reuse is represented by the query reuse below
        // rather than synthetic per-module work.
        ("no_change_query", [0, 0, 0, 0, 0, 0, 0, 1]),
        // Opaque bound tokens remove the old textual conversion discard, so
        // the two semantically unchanged CFGs remain reusable.
        ("leaf_edit", [1, 6, 6, 0, 4, 2, 1, 0]),
        ("widely_depended_definition_edit", [1, 6, 6, 0, 4, 2, 1, 0]),
        ("import_change", [2, 5, 6, 0, 4, 2, 1, 0]),
        ("diagnostic_error_edit", [1, 6, 5, 0, 0, 0, 1, 0]),
        // The failed ParseModule terminal is retained. Recovery changes only
        // that module's source leaf, so the other six module terminals remain
        // exact hits rather than being re-adopted after the diagnostic epoch.
        ("diagnostic_recovery", [1, 6, 6, 0, 4, 2, 1, 0]),
    ];
    for (name, counts) in expected {
        let work = &get(name)["required_vs_reused_work"];
        let actual = [
            work["modules"]["required"].as_u64().unwrap(),
            work["modules"]["reused"].as_u64().unwrap(),
            work["semantic_bodies"]["required"].as_u64().unwrap(),
            work["semantic_bodies"]["reused"].as_u64().unwrap(),
            work["cfgs"]["required"].as_u64().unwrap(),
            work["cfgs"]["reused"].as_u64().unwrap(),
            work["semantic_queries"]["required"].as_u64().unwrap(),
            work["semantic_queries"]["reused"].as_u64().unwrap(),
        ];
        assert_eq!(actual, counts, "unexpected structural work for {name}");
    }
    for name in [
        "leaf_edit",
        "widely_depended_definition_edit",
        "import_change",
        "diagnostic_recovery",
    ] {
        let scenario = get(name);
        // Stable-definition tokens resolve directly through the issuing bound
        // definition set. Ordinary source edits therefore rebuild bodies
        // without the former textual-join conversion discard.
        assert_eq!(
            count(
                scenario,
                &["semantic_work", "durable_bodies", "conversion_failures"]
            ),
            0
        );
        assert_eq!(
            count(
                scenario,
                &["semantic_work", "durable_bodies", "atomic_discards"]
            ),
            0
        );
    }
    assert_eq!(
        count(
            get("diagnostic_error_edit"),
            &["semantic_work", "failed_requests"]
        ),
        1
    );
    assert_eq!(
        get("diagnostic_error_edit")["diagnostic_parity"]["fresh_session"],
        "exact"
    );
    assert_eq!(
        get("diagnostic_recovery")["recovered_from_diagnostic_error"],
        true
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_variants_are_exact_transitive_closures() {
        let base = variant_sources(Variant::Base);
        assert_eq!(base.len(), 7);
        for &(path, source) in FILES
            .iter()
            .filter(|(path, _)| *path != "/bench/labels_alt.rue")
        {
            assert_eq!(base[path].as_str(), source);
        }
        let import_change = variant_sources(Variant::ImportChange);
        assert_eq!(import_change.len(), 7);
        assert!(!import_change.contains_key("/bench/labels.rue"));
        assert!(import_change.contains_key("/bench/labels_alt.rue"));
        assert_eq!(identity_for_path("/bench/labels.rue"), 2);
        assert_eq!(identity_for_path("/bench/labels_alt.rue"), 3);
        assert_ne!(
            identity_for_path("/bench/labels.rue"),
            identity_for_path("/bench/labels_alt.rue")
        );
        assert_eq!(identity_for_path("/bench/model.rue"), 5);
    }

    #[test]
    fn representative_scenarios_are_bounded_and_structurally_gated() {
        let scenarios = run_iteration();
        assert_eq!(scenarios.len(), 6);
        assert!(
            scenarios
                .iter()
                .all(|scenario| scenario["wall_time_ns"].is_u64())
        );
    }
}
