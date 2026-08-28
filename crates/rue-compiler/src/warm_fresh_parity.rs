//! Unstable adapter exposing the compiler's canonical warm/fresh parity oracle
//! to fuzz targets. Scaling rows and fuzz sequences both call this one
//! compiler-owned implementation.

use crate::{CompileErrors, CompileOptions, CompilerSession, RootedCfgOutput, SourceSnapshot};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn close_fuzz_discovery(session: &mut CompilerSession, source: &SourceSnapshot, epoch: u64) {
    use crate::unstable::{
        AcceptedImportSource, DiscoverySourceAssembler, ImportDemandMode, ImportObservation,
        begin_import_input_request, close_import_input_request, import_demand_frontier_for_roots,
        publish_import_observation_batch, stage_import_input_request,
    };

    let context = crate::ImportDiscoveryContext::new(epoch, "/p", None, "warm-session-fuzz")
        .expect("fuzz discovery context is valid");
    let root = source.metadata().root_file_id();
    let root_path = source
        .metadata()
        .physical_path(root)
        .expect("snapshot root has a physical path")
        .to_owned();
    let root_source = source
        .shared_source_text(root)
        .expect("snapshot root has source text");
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        root_path.clone(),
        root_path,
        crate::PhysicalFileIdentity::new(1807, root.index() as u64 + 1),
        crate::FileMetadataFingerprint::new(root_source.len() as u64, epoch, epoch),
        root_source,
    )
    .expect("fuzz root is accepted");
    for file in source.files().filter(|file| file.file_id != root) {
        let path = source
            .metadata()
            .physical_path(file.file_id)
            .expect("snapshot file has a physical path");
        assembler
            .add_explicit(
                path,
                path,
                crate::PhysicalFileIdentity::new(1807, file.file_id.index() as u64 + 1),
                crate::FileMetadataFingerprint::new(file.source.len() as u64, epoch, epoch),
                Arc::new(file.source.to_owned()),
            )
            .expect("fuzz import source is accepted");
    }
    let discovered = assembler.snapshot().expect("fuzz snapshot assembles");
    let accepted_reads = assembler.accepted_read_manifest();
    let mut revision = begin_import_input_request(
        session,
        &discovered,
        context.clone(),
        accepted_reads.clone(),
    )
    .expect("fuzz import request begins");
    loop {
        let plan = stage_import_input_request(session, revision).expect("fuzz plan stages");
        let frontier = import_demand_frontier_for_roots(
            session,
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .expect("fuzz demand frontier roots");
        if frontier.requests().is_empty() {
            close_import_input_request(session, revision).expect("fuzz discovery closes");
            return;
        }
        let observations = frontier
            .requests()
            .iter()
            .map(|request| {
                let read = accepted_reads
                    .iter()
                    .find(|read| read.requested_path() == request.requested_path())
                    .expect("fuzz accepts every demanded read");
                let module = discovered
                    .files()
                    .find(|file| discovered.module_id(file.file_id) == Some(read.module()))
                    .expect("fuzz accepted module exists");
                ImportObservation::accepted(
                    request.clone(),
                    AcceptedImportSource::new(
                        request.requested_path(),
                        read.canonical_path(),
                        read.metadata_identity(),
                        read.metadata_fingerprint(),
                        Arc::new(module.source.to_owned()),
                    )
                    .expect("fuzz accepted source is valid"),
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .expect("fuzz observations are valid");
        revision = publish_import_observation_batch(
            session,
            &frontier,
            &discovered,
            accepted_reads.clone(),
            observations,
        )
        .expect("fuzz discovery publishes");
    }
}

fn render_diagnostics(errors: &CompileErrors) -> String {
    errors
        .iter()
        .map(|error| format!("{error:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_diagnostic_snapshot(snapshot: Option<&Arc<crate::FrontendDiagnosticSnapshot>>) -> String {
    snapshot.map_or_else(
        || "<none>".to_owned(),
        |snapshot| {
            // Do not Debug-render the whole snapshot: its SourceMetadata contains
            // hash maps whose iteration order is intentionally not a parity
            // contract. These are the deterministic diagnostic inputs and output.
            format!(
                "stage={:?}\nrevision={:?}\nerrors={:?}\nwarnings={:?}",
                snapshot.stage(),
                snapshot.source_revision(),
                snapshot.errors(),
                snapshot.warnings()
            )
        },
    )
}

fn assert_no_graceful_ice(errors: &CompileErrors) {
    for error in errors.iter() {
        if matches!(
            error.kind,
            crate::ErrorKind::CompilerProducerInvariant(_)
                | crate::ErrorKind::InternalError(_)
                | crate::ErrorKind::InternalCodegenError(_)
        ) {
            panic!("graceful ICE: {error:?}");
        }
    }
}

fn body_query_identity(instance: &crate::FunctionInstanceKey, options: &CompileOptions) -> String {
    format!(
        "{:?}:{:?}",
        instance,
        crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: options.target,
            preview_features: crate::StablePreviewFeatures::new(&options.preview_features),
        }
    )
}

pub(crate) fn reachable_body_identities(
    label: &str,
    output: &RootedCfgOutput,
    states: &BTreeMap<String, Option<crate::BodyTransaction>>,
    options: &CompileOptions,
) -> BTreeSet<String> {
    let mut pending = output
        .functions()
        .iter()
        .map(|function| body_query_identity(&function.function, options))
        .collect::<Vec<_>>();
    let mut reachable = BTreeSet::new();
    while let Some(identity) = pending.pop() {
        if !reachable.insert(identity.clone()) {
            continue;
        }
        let Some(Some(transaction)) = states.get(&identity) else {
            panic!("{label}: reachable successful body identity has no transaction: {identity}");
        };
        for reference in transaction.references().0.iter() {
            if let crate::body_query::BodyReference::Callable(instance) = reference {
                pending.push(body_query_identity(instance, options));
            }
        }
    }
    reachable
}

pub(crate) fn assert_successful_output_body_presence(
    label: &str,
    output: &RootedCfgOutput,
    states: &BTreeMap<String, Option<crate::BodyTransaction>>,
) {
    for function in output.functions() {
        let prefix = format!("{:?}:", function.function);
        let matching = states
            .iter()
            .filter(|(identity, _)| identity.starts_with(&prefix))
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            1,
            "{label}: successful output body identity is absent or ambiguous"
        );
        assert!(
            matching[0].1.is_some(),
            "{label}: output body has no retained transaction"
        );
    }
}

#[cfg(test)]
pub(crate) fn assert_reachable_body_key_set_parity(
    label: &str,
    warm_bodies: &BTreeMap<String, Option<crate::BodyTransaction>>,
    fresh_bodies: &BTreeMap<String, Option<crate::BodyTransaction>>,
) {
    assert_eq!(
        warm_bodies.keys().collect::<Vec<_>>(),
        fresh_bodies.keys().collect::<Vec<_>>(),
        "{label}: successful warm/fresh body-key sets differ"
    );
}

/// Compare deterministic-failure body terminals that both sessions requested.
/// A failed rooted query may stop before requesting an unchanged helper, so
/// only overlapping failure identities are authoritative for this check.
pub(crate) fn assert_deterministic_failure_body_transaction_parity(
    label: &str,
    warm_bodies: &BTreeMap<String, Option<crate::BodyTransaction>>,
    fresh_bodies: &BTreeMap<String, Option<crate::BodyTransaction>>,
) {
    let warm_failures = warm_bodies
        .iter()
        .filter(|(_, transaction)| {
            matches!(
                transaction,
                Some(crate::BodyTransaction::DeterministicFailure { .. })
            )
        })
        .map(|(identity, transaction)| (identity.clone(), transaction.as_ref().unwrap().clone()))
        .collect::<BTreeMap<_, _>>();
    let fresh_failures = fresh_bodies
        .iter()
        .filter(|(_, transaction)| {
            matches!(
                transaction,
                Some(crate::BodyTransaction::DeterministicFailure { .. })
            )
        })
        .map(|(identity, transaction)| (identity.clone(), transaction.as_ref().unwrap().clone()))
        .collect::<BTreeMap<_, _>>();
    for identity in warm_failures
        .keys()
        .filter(|identity| fresh_failures.contains_key(*identity))
    {
        assert!(
            crate::transaction_equal(&warm_failures[identity], &fresh_failures[identity]),
            "{label}: exact failed BodyTransaction diverged for {identity}"
        );
    }
}

/// The single warm/fresh oracle for incremental correctness. It compares the
/// reachable retained body/key set, exact body transactions, public semantic
/// artifacts, diagnostics, warnings, and linked executable bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParityObservation {
    pub rooted_success: bool,
    pub executable_success: bool,
}

pub(crate) fn assert_rooted_parity(
    label: &str,
    warm_session: &mut CompilerSession,
    fresh_session: &mut CompilerSession,
    _source: &SourceSnapshot,
    options: &CompileOptions,
    warm: &Result<RootedCfgOutput, CompileErrors>,
    fresh: &Result<RootedCfgOutput, CompileErrors>,
) -> ParityObservation {
    let warm_bodies = warm_session.retained_body_identity_states_for_test(options);
    let fresh_bodies = fresh_session.retained_body_identity_states_for_test(options);
    let rooted_success = warm.is_ok();
    let mut executable_success = false;
    match (warm, fresh) {
        (Ok(warm), Ok(fresh)) => {
            let warm_reachable = reachable_body_identities(label, warm, &warm_bodies, options);
            let fresh_reachable = reachable_body_identities(label, fresh, &fresh_bodies, options);
            assert_eq!(
                warm_reachable, fresh_reachable,
                "{label}: reachable body-key sets diverged"
            );
            for identity in &warm_reachable {
                let warm_transaction = warm_bodies.get(identity).and_then(Option::as_ref);
                let fresh_transaction = fresh_bodies.get(identity).and_then(Option::as_ref);
                assert_eq!(
                    warm_transaction.is_some(),
                    fresh_transaction.is_some(),
                    "{label}: body presence diverged for {identity}"
                );
                if let (Some(warm_transaction), Some(fresh_transaction)) =
                    (warm_transaction, fresh_transaction)
                {
                    assert!(
                        crate::transaction_equal(warm_transaction, fresh_transaction),
                        "{label}: BodyTransaction diverged for {identity}"
                    );
                }
            }
            assert_successful_output_body_presence(label, warm, &warm_bodies);
            assert_successful_output_body_presence(label, fresh, &fresh_bodies);
            assert_eq!(
                format!("{:?}", warm.functions()),
                format!("{:?}", fresh.functions()),
                "{label}: functions diverged"
            );
            assert_eq!(
                format!("{:?}", warm.warnings()),
                format!("{:?}", fresh.warnings()),
                "{label}: warnings diverged"
            );
            assert_eq!(
                warm.string_domains().collect::<Vec<_>>(),
                fresh.string_domains().collect::<Vec<_>>(),
                "{label}: string domains diverged"
            );
            assert_eq!(
                format!("{:?}", warm.declarations()),
                format!("{:?}", fresh.declarations()),
                "{label}: declarations diverged"
            );
            assert_eq!(
                format!("{:?}", warm.anonymous_nominals()),
                format!("{:?}", fresh.anonymous_nominals()),
                "{label}: anonymous nominals diverged"
            );
            assert_eq!(
                render_diagnostic_snapshot(warm_session.latest_diagnostics_for_test()),
                render_diagnostic_snapshot(fresh_session.latest_diagnostics_for_test()),
                "{label}: diagnostics diverged"
            );
            // The discovery protocol may canonicalize file identities and
            // metadata. Use the exact snapshots that each session published;
            // passing the pre-discovery host snapshot would intentionally fail
            // the compiler's exact-snapshot guard and skip executable parity.
            let warm_snapshot = warm_session.committed_snapshot_for_executable();
            let fresh_snapshot = fresh_session.committed_snapshot_for_executable();
            let warm_executable = warm_snapshot
                .and_then(|snapshot| warm_session.oracle_executable(&snapshot, options));
            let fresh_executable = fresh_snapshot
                .and_then(|snapshot| fresh_session.oracle_executable(&snapshot, options));
            if let Err(errors) = &warm_executable {
                assert_no_graceful_ice(errors);
            }
            if let Err(errors) = &fresh_executable {
                assert_no_graceful_ice(errors);
            }
            match (warm_executable, fresh_executable) {
                (Ok(warm), Ok(fresh)) => {
                    assert_eq!(warm.elf, fresh.elf, "{label}: executable bytes diverged");
                    assert_eq!(
                        format!("{:?}", warm.warnings),
                        format!("{:?}", fresh.warnings),
                        "{label}: executable warnings diverged"
                    );
                    executable_success = true;
                }
                (Err(warm), Err(fresh)) => assert_eq!(
                    render_diagnostics(&warm),
                    render_diagnostics(&fresh),
                    "{label}: executable diagnostics diverged"
                ),
                (Ok(_), Err(fresh)) => panic!(
                    "{label}: warm executable succeeded but fresh failed: {}",
                    render_diagnostics(&fresh)
                ),
                (Err(warm), Ok(_)) => panic!(
                    "{label}: warm executable failed but fresh succeeded: {}",
                    render_diagnostics(&warm)
                ),
            }
        }
        (Err(warm), Err(fresh)) => {
            assert_no_graceful_ice(warm);
            assert_no_graceful_ice(fresh);
            assert_eq!(
                render_diagnostics(warm),
                render_diagnostics(fresh),
                "{label}: failure diagnostics diverged"
            );
            assert_eq!(
                render_diagnostic_snapshot(warm_session.latest_diagnostics_for_test()),
                render_diagnostic_snapshot(fresh_session.latest_diagnostics_for_test()),
                "{label}: failure snapshots diverged"
            );
            // A failed rooted body/CFG query may stop before unchanged helper
            // bodies are requested. Compare deterministic failures demanded
            // by both sides, preserving early-stop behavior for helpers that
            // exist only in one retained family.
            assert_deterministic_failure_body_transaction_parity(
                label,
                &warm_bodies,
                &fresh_bodies,
            );
        }
        (Ok(_), Err(fresh)) => panic!(
            "{label}: warm compiled but fresh failed: {}",
            render_diagnostics(fresh)
        ),
        (Err(warm), Ok(_)) => panic!(
            "{label}: warm failed but fresh compiled: {}",
            render_diagnostics(warm)
        ),
    }
    ParityObservation {
        rooted_success,
        executable_success,
    }
}

/// Compile one already-adopted revision on the retained session and compare it
/// with a fresh session through the canonical full artifact/oracle path.
pub fn assert_warm_fresh_parity(
    label: &str,
    warm_session: &mut CompilerSession,
    source: &SourceSnapshot,
    options: &CompileOptions,
) -> ParityObservation {
    // Run every revision through the canonical in-memory discovery protocol,
    // including import-free revisions. The latter are important topology
    // removals: leaving the predecessor's closed graph selected would make a
    // warm removal look like a discovery failure rather than a real edit.
    close_fuzz_discovery(warm_session, source, 1);
    let warm = warm_session.rooted_cfg(options);
    let mut fresh_session = CompilerSession::new();
    // Invalid revisions are intentionally retained by the session. Their
    // rooted query returns deterministic diagnostics, which the shared oracle
    // compares; update errors therefore must not short-circuit this call.
    close_fuzz_discovery(&mut fresh_session, source, 1);
    let fresh = fresh_session.rooted_cfg(options);
    assert_rooted_parity(
        label,
        warm_session,
        &mut fresh_session,
        source,
        options,
        &warm,
        &fresh,
    )
}
