use super::super::RootedParkOutcome;
use super::super::tests::snapshot;
use super::*;
use crate::{CompileOptions, SourceMetadata};

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
    let plan = session.stage_import_input_request(revision).unwrap();
    let roots = plan.demand_roots();
    let frontier = session
        .import_demand_frontier_for_roots(revision, &plan, crate::ImportDemandMode::Rooted, &roots)
        .unwrap();
    assert!(
        frontier.requests().is_empty(),
        "a freestanding root closes with an empty frontier",
    );
    session.close_import_input_request(revision).unwrap();
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
        session.imports.open_discovery.is_some(),
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
        session.imports.open_discovery.is_none(),
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
    assert!(session.imports.continuation.is_none());
    assert!(session.closed_discovery_continuation().is_none());
    assert!(session.imports.successor_delta_nonce.is_none());
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
        session.imports.open_discovery.is_some(),
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
fn failed_trusted_successor_reclose_restores_exact_committed_selectors() {
    let (mut session, token, frontier, _predecessor, _reads, mut assembler) = closed_continuation();
    let retry_token = token.clone();
    let committed_revision = session.queries.revisioned.current_import_revision();
    let committed_attempt = session
        .imports
        .discovery_attempt
        .clone()
        .expect("the predecessor close selects its discovery attempt");
    let committed_prior = session.imports.prior_discovery.clone();
    let committed_order = session.batch_diagnostic_order.clone();
    let committed_diagnostics = session
        .diagnostics
        .latest()
        .cloned()
        .expect("the predecessor close selects its diagnostic batch");
    let committed_parse = session
        .selected_parse_terminal()
        .expect("the predecessor close selects its parse terminal");
    let (committed_validated_snapshot, committed_validated_reads) = session
        .imports
        .validated_accepted_reads
        .clone()
        .expect("the predecessor close validates its accepted reads");

    assembler
        .add_explicit(
            "/sdk/option.rue",
            "/sdk/option.rue",
            crate::PhysicalFileIdentity::new(2, 2),
            continuation_metadata(),
            Arc::new(
                r#"const missing = @import("missing.rue"); pub fn Option(comptime T: type) -> type { enum { Some(T), None } }"#
                    .to_owned(),
            ),
        )
        .unwrap();
    let successor = assembler.snapshot().unwrap();
    let successor_reads = assembler.accepted_read_manifest();
    let delta = session
        .publish_trusted_toolchain_successor(token, &frontier, &successor, successor_reads.clone())
        .expect("the trusted successor publishes");
    session
        .stage_import_discovery_successor(&delta)
        .expect("the successor stage moves the provisional selectors");
    session
        .close_import_discovery_successor(&delta)
        .expect_err("the unresolved successor import records a failed close");
    assert!(
        !Arc::ptr_eq(
            session
                .imports
                .discovery_attempt
                .as_ref()
                .expect("the failed close selects its attempted discovery"),
            &committed_attempt,
        ),
        "the regression must move the discovery-attempt selector before abort"
    );
    assert!(
        session
            .imports
            .prior_discovery
            .as_ref()
            .is_some_and(|prior| Arc::ptr_eq(prior, &committed_attempt)),
        "the failed close must retain the committed predecessor as prior discovery"
    );
    assert!(
        !Arc::ptr_eq(
            session
                .diagnostics
                .latest()
                .expect("the failed close selects its diagnostics"),
            &committed_diagnostics,
        ),
        "the regression must move the diagnostic selector before abort"
    );
    assert!(
        !Arc::ptr_eq(
            &session
                .selected_parse_terminal()
                .expect("the failed successor retains its staged parse terminal"),
            &committed_parse,
        ),
        "the regression must move the parse selector before abort"
    );

    // A newer filesystem observation supersedes this failed successor before
    // the driver aborts it. Beginning that request must invalidate the delta
    // without checkpointing provisional selectors or a consumed continuation
    // as though they were committed state.
    session
        .begin_import_input_request(
            &successor,
            continuation_std_context(),
            successor_reads.clone(),
        )
        .expect("the superseding fresh request begins");
    assert!(
        session.stage_import_discovery_successor(&delta).is_err(),
        "the fresh request invalidates the provisional successor delta"
    );

    session
        .abort_import_input_request()
        .expect("the failed successor round rolls back exactly");

    assert_eq!(
        session.queries.revisioned.current_import_revision(),
        committed_revision,
        "abort must reselect the committed predecessor revision"
    );
    assert!(Arc::ptr_eq(
        session
            .imports
            .discovery_attempt
            .as_ref()
            .expect("abort restores the committed discovery attempt"),
        &committed_attempt,
    ));
    match (
        session.imports.prior_discovery.as_ref(),
        committed_prior.as_ref(),
    ) {
        (Some(restored), Some(committed)) => assert!(Arc::ptr_eq(restored, committed)),
        (None, None) => {}
        _ => panic!("abort changed the prior-discovery selector"),
    }
    assert_eq!(session.batch_diagnostic_order, committed_order);
    assert!(Arc::ptr_eq(
        session
            .diagnostics
            .latest()
            .expect("abort restores the committed diagnostic selector"),
        &committed_diagnostics,
    ));
    assert!(Arc::ptr_eq(
        &session
            .selected_parse_terminal()
            .expect("abort restores the committed parse selector"),
        &committed_parse,
    ));
    let (restored_snapshot, restored_reads) = session
        .imports
        .validated_accepted_reads
        .as_ref()
        .expect("abort restores validated accepted reads");
    assert!(restored_snapshot.is_same_exact_snapshot(&committed_validated_snapshot));
    assert_eq!(restored_reads, &committed_validated_reads);
    assert!(session.imports.import_request_checkpoint.is_none());
    assert!(session.imports.successor_delta_nonce.is_none());
    assert!(
        session.stage_import_discovery_successor(&delta).is_err(),
        "abort permanently invalidates the provisional successor delta"
    );

    let restored_token = session
        .closed_discovery_continuation()
        .expect("abort restores the exact authorizing continuation");
    assert_eq!(restored_token.nonce, retry_token.nonce);
    assert_eq!(restored_token.revision, retry_token.revision);
    session
        .publish_trusted_toolchain_successor(retry_token, &frontier, &successor, successor_reads)
        .expect("the restored continuation authorizes a clean retry");
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
    session.imports.next_continuation_nonce += 7;
    session.imports.continuation.as_mut().unwrap().nonce = session.imports.next_continuation_nonce;
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
