//! Cold-versus-reused `CompilerSession` differential oracle.
//!
//! The corpus is deliberately deterministic and bounded. A failure is reduced
//! by deletion to a locally minimal sequence before it is reported, so CI logs
//! identify the shortest reproducer rather than the full seed corpus.
//!
//! The emitted-output comparison hashes actual per-function assembly. That is
//! the strongest public, platform-stable boundary before linking: object bytes
//! are crate-private, while the public executable query folds in target object
//! format, the embedded runtime archive, and linker layout. Exercising that
//! boundary here would add link/runtime fragility to a frontend reuse oracle.

use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use rue_cfg::OptLevel;
use rue_compiler::{
    AcceptedImportSource, AcceptedReadManifestEntry, CompileOptions, CompilerSession,
    DiscoverySourceAssembler, FRONTEND_DIAGNOSTIC_RETENTION_LIMIT,
    FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT, FileMetadataFingerprint,
    FrontendDiagnosticSnapshot, ImportDiscoveryContext, ImportObservation, ImportObservationLedger,
    PhysicalFileIdentity, SourceMetadata, SourceSnapshot, generate_emitted_asm,
};
use rue_span::FileId;
use rue_target::Target;
use sha2::{Digest, Sha256};

#[derive(Clone)]
struct Step {
    name: &'static str,
    snapshot: SourceSnapshot,
    options: CompileOptions,
    discovery: Option<DiscoveryInput>,
}

#[derive(Clone)]
struct DiscoveryInput {
    context: ImportDiscoveryContext,
    accepted_reads: Arc<[AcceptedReadManifestEntry]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Observation {
    update: String,
    diagnostics: String,
    semantic: String,
    semantic_hash: String,
    identities: String,
    manifest: String,
    imports: String,
}

#[derive(Clone, Copy)]
enum InjectedFault {
    Semantic,
    Diagnostic,
    ImportCache,
}

fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
    let physical = entries
        .iter()
        .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
        .collect::<HashMap<_, _>>();
    let logical = entries
        .iter()
        .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
        .collect::<HashMap<_, _>>();
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

fn step(name: &'static str, snapshot: SourceSnapshot) -> Step {
    Step {
        name,
        snapshot,
        options: CompileOptions::default(),
        discovery: None,
    }
}

fn import_step(name: &'static str, epoch: u64, value: i32) -> Step {
    let context = ImportDiscoveryContext::new(epoch, "/p", None, "oracle").unwrap();
    let metadata = |length| FileMetadataFingerprint::new(length, epoch, epoch);
    let root = Arc::new("const a = @import(\"a.rue\"); fn main() -> i32 { a.value() }".to_owned());
    let imported = Arc::new(format!("pub fn value() -> i32 {{ {value} }}"));
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/p/main.rue",
        "/p/main.rue",
        PhysicalFileIdentity::new(1, 1),
        metadata(root.len() as u64),
        root,
    )
    .unwrap();
    assembler
        .add_explicit(
            "/p/a.rue",
            "/p/a.rue",
            PhysicalFileIdentity::new(1, 2),
            metadata(imported.len() as u64),
            imported,
        )
        .unwrap();
    Step {
        name,
        snapshot: assembler.snapshot().unwrap(),
        options: CompileOptions::default(),
        discovery: Some(DiscoveryInput {
            context,
            accepted_reads: assembler.accepted_read_manifest(),
        }),
    }
}

fn normalize_diagnostics(snapshot: Option<&Arc<FrontendDiagnosticSnapshot>>) -> String {
    snapshot.map_or_else(
        || "none".to_owned(),
        |diagnostics| {
            format!(
                "stage={:?};errors={:?};warnings={:?}",
                diagnostics.stage(),
                diagnostics.errors(),
                diagnostics.warnings()
            )
        },
    )
}

fn close_discovery(session: &mut CompilerSession, step: &Step) -> String {
    let Some(discovery) = &step.discovery else {
        return "direct".to_owned();
    };
    let plan = match session.stage_import_discovery(
        &step.snapshot,
        discovery.context.clone(),
        discovery.accepted_reads.clone(),
        ImportObservationLedger::default(),
    ) {
        Ok(plan) => plan,
        Err(errors) => return format!("stage-error:{errors:?}"),
    };
    let mut ledger = ImportObservationLedger::default();
    for request in plan.pending_requests(&ledger) {
        let accepted = discovery
            .accepted_reads
            .iter()
            .find(|read| read.requested_path() == request.requested_path());
        let observation = if let Some(read) = accepted {
            let source = step
                .snapshot
                .files()
                .find(|source| step.snapshot.module_id(source.file_id) == Some(read.module()))
                .expect("accepted manifest path belongs to the snapshot");
            ImportObservation::accepted(
                request.clone(),
                AcceptedImportSource::new(
                    request.requested_path(),
                    read.canonical_path(),
                    read.metadata_identity(),
                    read.metadata_fingerprint(),
                    Arc::new(source.source.to_owned()),
                )
                .unwrap(),
            )
            .unwrap()
        } else {
            ImportObservation::absent(request.clone())
        };
        ledger.record(observation).unwrap();
    }
    match session.close_import_discovery(ledger) {
        Ok(artifact) => format!(
            "status={:?};input={:?};reads={:?};ledger={:?}",
            artifact.status(),
            artifact.graph().map(|graph| graph.input()),
            artifact.accepted_read_manifest(),
            artifact.ledger()
        ),
        Err(errors) => format!("close-error:{errors:?}"),
    }
}

fn observe(session: &mut CompilerSession, step: &Step) -> Observation {
    let update = if step.discovery.is_some() {
        "discovery".to_owned()
    } else {
        let update = session.update(&step.snapshot);
        format!(
            "result={:?};diagnostics={:?}",
            update.result().map(|program| program.source_revision()),
            update.diagnostics().errors()
        )
    };
    let imports = close_discovery(session, step);
    let semantic = session.semantic(&step.options);
    let (semantic, semantic_hash, identities) = match semantic {
        Ok(output) => {
            let artifact = format!(
                "functions={:?};strings={:?};warnings={:?}",
                output.functions(),
                output.strings(),
                output.warnings()
            );
            let rir = session
                .rir()
                .expect("successful semantic query retains RIR");
            let mut emitted = String::new();
            for function in output.functions() {
                let assembly = generate_emitted_asm(
                    &function.cfg,
                    output.type_pool(),
                    output.strings(),
                    rir.semantic_symbols().interner(),
                    step.options.target,
                )
                .expect("oracle corpus must have platform-stable assembly emission");
                writeln!(&mut emitted, "{}:\n{}", function.analyzed.name, assembly).unwrap();
            }
            let hash = format!("{:x}", Sha256::digest(emitted.as_bytes()));
            (
                artifact,
                hash,
                format!(
                    "source={:?};codegen={:?}",
                    step.snapshot.source_revision(),
                    output.input()
                ),
            )
        }
        Err(errors) => (
            format!("error:{errors:?}"),
            "not-emitted".to_owned(),
            format!("source={:?}", step.snapshot.source_revision()),
        ),
    };
    // Capture the diagnostic artifact belonging to the semantic request before
    // asking for its dependency manifest. The manifest query may internally
    // consult import diagnostics, but that is query work rather than the
    // semantic result being compared here.
    let diagnostics = normalize_diagnostics(session.latest_diagnostics());
    let manifest = match session.semantic_dependency_inputs(&step.options, None) {
        Ok(manifest) => format!(
            "input={:?};imports={:?};definitions={:?};fingerprints={:?};module-imports={:?};free={:?};methods={:?};destructors={:?};implicit={:?};decl-types={:?};call-heads={:?};builtins={:?};consts={:?};bodies={:?};blockers={:?};complete={}",
            manifest.input(),
            manifest.imports(),
            manifest.definitions(),
            manifest.definition_fingerprints(),
            manifest.module_imports(),
            manifest.free_function_dependencies(),
            manifest.named_method_dependencies(),
            manifest.named_destructor_dependencies(),
            manifest.implicit_named_destructor_dependencies(),
            manifest.declaration_type_dependencies(),
            manifest.declaration_type_call_head_dependencies(),
            manifest.builtin_type_call_head_inputs(),
            manifest.named_const_dependencies(),
            manifest.body_dependencies(),
            manifest.dependency_blockers(),
            manifest.semantic_dependency_graph_complete(),
        ),
        Err(errors) => format!("error:{errors:?}"),
    };
    Observation {
        update,
        diagnostics,
        semantic,
        semantic_hash,
        identities,
        manifest,
        imports,
    }
}

fn inject_stale(observation: &mut Observation, previous: &Observation, fault: InjectedFault) {
    match fault {
        InjectedFault::Semantic => {
            assert_ne!(observation.semantic, previous.semantic);
            assert_ne!(observation.semantic_hash, previous.semantic_hash);
            assert_ne!(observation.identities, previous.identities);
            observation.semantic.clone_from(&previous.semantic);
            observation
                .semantic_hash
                .clone_from(&previous.semantic_hash);
            observation.identities.clone_from(&previous.identities);
        }
        InjectedFault::Diagnostic => {
            assert_ne!(observation.diagnostics, previous.diagnostics);
            observation.diagnostics.clone_from(&previous.diagnostics);
        }
        InjectedFault::ImportCache => {
            assert_ne!(observation.imports, previous.imports);
            observation.imports.clone_from(&previous.imports);
        }
    }
}

fn differing_fields(left: &Observation, right: &Observation) -> Vec<&'static str> {
    [
        (left.update != right.update, "update"),
        (left.diagnostics != right.diagnostics, "diagnostics"),
        (left.semantic != right.semantic, "semantic"),
        (
            left.semantic_hash != right.semantic_hash,
            "emitted-assembly-hash",
        ),
        (left.identities != right.identities, "stable-identities"),
        (left.manifest != right.manifest, "dependency-manifest"),
        (left.imports != right.imports, "import-discovery"),
    ]
    .into_iter()
    .filter_map(|(different, field)| different.then_some(field))
    .collect()
}

fn first_mismatch(steps: &[Step], fault: Option<InjectedFault>) -> Option<(usize, String)> {
    let mut reused = CompilerSession::new();
    let mut last_good = None;
    let mut previous_warm = None;
    for (index, step) in steps.iter().enumerate() {
        let before_failure = last_good.clone();
        let mut warm = observe(&mut reused, step);
        let mut fresh_session = CompilerSession::new();
        let fresh = observe(&mut fresh_session, step);
        if let Some(fault) = fault
            && index + 1 == steps.len()
            && index > 0
        {
            inject_stale(
                &mut warm,
                previous_warm
                    .as_ref()
                    .expect("stale injection requires an earlier reused-session result"),
                fault,
            );
        }
        if warm != fresh {
            let mut difference = String::new();
            write!(
                &mut difference,
                "affected fields: {}\nwarm={warm:#?}\nfresh={fresh:#?}",
                differing_fields(&warm, &fresh).join(", ")
            )
            .unwrap();
            return Some((index, difference));
        }

        let current = reused.last_good_semantic_diagnostics().cloned();
        if warm.semantic.starts_with("error:") {
            assert_eq!(
                current.as_ref().map(|value| format!("{:?}", value.stage())),
                before_failure
                    .as_ref()
                    .map(|value: &Arc<FrontendDiagnosticSnapshot>| format!("{:?}", value.stage())),
                "{} replaced last-good diagnostics on failure",
                step.name
            );
        } else {
            assert!(
                current.is_some(),
                "{} did not publish last-good diagnostics",
                step.name
            );
            last_good = current;
        }
        previous_warm = Some(warm);
    }
    None
}

fn minimized_failure(steps: &[Step], fault: Option<InjectedFault>) -> Option<String> {
    first_mismatch(steps, fault)?;
    let mut reduced = steps.to_vec();
    let mut chunk = reduced.len() / 2;
    while chunk > 0 {
        let mut start = 0;
        let mut deleted = false;
        while start + chunk <= reduced.len() {
            let mut candidate = reduced.clone();
            candidate.drain(start..start + chunk);
            if candidate.len() >= 2 && first_mismatch(&candidate, fault).is_some() {
                reduced = candidate;
                deleted = true;
                break;
            }
            start += 1;
        }
        if !deleted {
            chunk /= 2;
        }
    }
    let (index, difference) = first_mismatch(&reduced, fault).unwrap();
    Some(format!(
        "minimized sequence [{}], mismatch at {} ({})\n{}",
        reduced
            .iter()
            .map(|step| step.name)
            .collect::<Vec<_>>()
            .join(" -> "),
        index,
        reduced[index].name,
        difference
    ))
}

fn assert_equivalent(steps: &[Step]) {
    if let Some(failure) = minimized_failure(steps, None) {
        panic!("{failure}");
    }
}

fn corpus() -> Vec<Step> {
    let base_source = "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }";
    let mut target_change = step(
        "target-change",
        snapshot(&[(7, "/moved/main.rue", "renamed.rue", base_source)], 7),
    );
    target_change.options.target = if target_change.options.target == Target::Aarch64Linux {
        Target::X86_64Linux
    } else {
        Target::Aarch64Linux
    };
    let mut options_change = target_change.clone();
    options_change.name = "options-change";
    options_change.options.opt_level = OptLevel::O1;

    vec![
        step(
            "base",
            snapshot(&[(7, "/p/main.rue", "main.rue", base_source)], 7),
        ),
        step(
            "source-edit",
            snapshot(
                &[(
                    7,
                    "/p/main.rue",
                    "main.rue",
                    "fn helper() -> i32 { 2 }\nfn main() -> i32 { helper() }",
                )],
                7,
            ),
        ),
        step(
            "physical-relocation",
            snapshot(&[(7, "/moved/main.rue", "main.rue", base_source)], 7),
        ),
        step(
            "logical-relocation",
            snapshot(&[(7, "/moved/main.rue", "renamed.rue", base_source)], 7),
        ),
        step(
            "root-change",
            snapshot(
                &[
                    (7, "/moved/main.rue", "renamed.rue", base_source),
                    (8, "/moved/other.rue", "other.rue", "fn main() -> i32 { 8 }"),
                ],
                8,
            ),
        ),
        target_change,
        options_change,
        step(
            "semantic-failure",
            snapshot(
                &[(7, "/p/main.rue", "main.rue", "fn main() -> i32 { missing }")],
                7,
            ),
        ),
        step(
            "recovery",
            snapshot(
                &[(7, "/p/main.rue", "main.rue", "fn main() -> i32 { 9 }")],
                7,
            ),
        ),
        step(
            "incomplete-manifest",
            snapshot(
                &[(
                    (7),
                    "/p/main.rue",
                    "main.rue",
                    "fn Box(comptime T: type) -> type { struct { v: T, drop fn(self) {} } }\nfn main() { let B = Box(i32); let value = B { v: 1 }; }",
                )],
                7,
            ),
        ),
        import_step("import-cold", 1, 1),
        import_step("import-edit", 2, 2),
    ]
}

#[test]
fn bounded_corpus_matches_stepwise_fresh_sessions() {
    let corpus = corpus();
    assert_equivalent(&corpus);
    let incomplete = corpus
        .iter()
        .find(|step| step.name == "incomplete-manifest")
        .unwrap();
    let mut fresh = CompilerSession::new();
    assert!(
        observe(&mut fresh, incomplete)
            .manifest
            .contains("complete=false")
    );
}

#[test]
fn failure_recovery_and_bounded_eviction_match_fresh_sessions() {
    let mut steps = vec![step(
        "good",
        snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        ),
    )];
    for index in 0..24 {
        let name = Box::leak(format!("failure-{index}").into_boxed_str());
        steps.push(step(
            name,
            snapshot(
                &[(
                    1,
                    "/p/main.rue",
                    "main.rue",
                    &format!("fn main() -> i32 {{ missing{index} }}"),
                )],
                1,
            ),
        ));
    }
    steps.push(step(
        "recovered",
        snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }")],
            1,
        ),
    ));
    assert_equivalent(&steps);

    let mut session = CompilerSession::new();
    for step in &steps {
        observe(&mut session, step);
    }
    assert!(session.work().retention.diagnostic_entries <= FRONTEND_DIAGNOSTIC_RETENTION_LIMIT);
    assert!(
        session.work().retention.invalidation_plans <= FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT
    );
}

#[test]
fn fault_injection_proves_semantic_diagnostic_and_import_cache_detection() {
    let corpus = corpus();
    for (steps, fault, affected) in [
        (
            &corpus[0..2],
            InjectedFault::Semantic,
            "affected fields: semantic, emitted-assembly-hash, stable-identities",
        ),
        (
            &corpus[7..9],
            InjectedFault::Diagnostic,
            "affected fields: diagnostics",
        ),
        (
            &corpus[corpus.len() - 2..],
            InjectedFault::ImportCache,
            "affected fields: import-discovery",
        ),
    ] {
        let failure = minimized_failure(steps, Some(fault)).expect("injected fault must be caught");
        assert!(failure.contains("minimized sequence ["));
        assert_eq!(failure.lines().next().unwrap().matches(" -> ").count(), 1);
        assert!(failure.contains(affected), "{failure}");
    }
}
