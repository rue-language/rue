//! Cold-versus-reused `CompilerSession` differential oracle.
//!
//! The corpus is deliberately deterministic and bounded. A failure is reduced
//! by deletion to a locally minimal sequence before it is reported, so CI logs
//! identify the shortest reproducer rather than the full seed corpus.
//!
//! The emitted-output comparison covers both per-function assembly and the
//! final internal-linker executable bytes. This keeps the oracle on real
//! production query paths through backend emission and object/link layout.

use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use rue_cfg::OptLevel;
use rue_compiler::unstable::{
    AcceptedImportSource, DifferentialOracleFault, DiscoverySourceAssembler, ImportDemandMode,
    ImportObservation, PresentationRequest, PresentationStage, begin_import_input_request,
    close_import_input_request, discovery_attempt, import_demand_frontier_for_roots,
    import_discovery_accepted_reads_debug, import_discovery_graph_input_debug,
    import_discovery_observation_ledger_debug, inject_stale_query_for_oracle, oracle_executable,
    publish_import_observation_batch, rooted_cfg, stage_import_input_request,
};
use rue_compiler::{
    CompileOptions, CompilerSession, FileMetadataFingerprint, ImportDiscoveryContext,
    PhysicalFileIdentity, PreviewFeature, PreviewFeatures, SourceMetadata, SourceSnapshot,
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
    accepted_reads: rue_compiler::AcceptedReadManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Observation {
    update: String,
    syntax: String,
    rir: String,
    semantic: String,
    semantic_hash: String,
    executable_hash: String,
    identities: String,
    imports: String,
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

fn close_discovery(session: &mut CompilerSession, step: &Step) -> String {
    let Some(discovery) = &step.discovery else {
        return "direct".to_owned();
    };
    let mut revision = begin_import_input_request(
        session,
        &step.snapshot,
        discovery.context.clone(),
        discovery.accepted_reads.clone(),
    )
    .unwrap();
    loop {
        let plan = match stage_import_input_request(session, revision) {
            Ok(plan) => plan,
            Err(errors) => return format!("stage-error:{errors:?}"),
        };
        let frontier = import_demand_frontier_for_roots(
            session,
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .unwrap();
        if frontier.requests().is_empty() {
            return match close_import_input_request(session, revision) {
                Ok(artifact) => render_import_discovery(&artifact),
                Err(errors) => format!("close-error:{errors:?}"),
            };
        }
        let observations = frontier
            .requests()
            .iter()
            .cloned()
            .map(|request| {
                let accepted = discovery
                    .accepted_reads
                    .iter()
                    .find(|read| read.requested_path() == request.requested_path());
                if let Some(read) = accepted {
                    let source = step
                        .snapshot
                        .files()
                        .find(|source| {
                            step.snapshot.module_id(source.file_id) == Some(read.module())
                        })
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
                    ImportObservation::absent(request)
                }
            })
            .collect();
        revision = publish_import_observation_batch(
            session,
            &frontier,
            &step.snapshot,
            discovery.accepted_reads.clone(),
            observations,
        )
        .unwrap();
    }
}

fn render_selected_import(session: &CompilerSession) -> String {
    let artifact = discovery_attempt(session)
        .expect("injected import fault retains a selected closure artifact");
    render_import_discovery(&artifact)
}

fn render_import_discovery(artifact: &rue_compiler::ImportDiscoveryView) -> String {
    format!(
        "status={:?};input={:?};reads={:?};ledger={:?}",
        artifact.status(),
        import_discovery_graph_input_debug(artifact),
        import_discovery_accepted_reads_debug(artifact),
        import_discovery_observation_ledger_debug(artifact)
    )
}

fn observe_with_fault(
    session: &mut CompilerSession,
    step: &Step,
    fault: Option<DifferentialOracleFault>,
) -> Observation {
    let update = if step.discovery.is_some() {
        "discovery".to_owned()
    } else {
        let update = session.update(&step.snapshot);
        format!(
            "result={:?};diagnostics={:?}",
            update
                .result()
                .map(|syntax| syntax.source_revision().clone()),
            update.diagnostics().errors()
        )
    };
    let mut imports = close_discovery(session, step);
    let file_order = step
        .snapshot
        .files()
        .map(|source| source.file_id)
        .collect::<Vec<_>>();
    let present = |session: &mut CompilerSession, stage| {
        session
            .unstable_present(PresentationRequest {
                stage,
                options: &step.options,
                file_order: &file_order,
            })
            .map_or_else(
                |errors| format!("error:{errors:?}"),
                |artifact| artifact.as_str().to_owned(),
            )
    };
    let syntax = format!(
        "tokens={};ast={}",
        present(session, PresentationStage::Tokens),
        present(session, PresentationStage::Ast)
    );
    let rir = present(session, PresentationStage::Rir);
    if fault == Some(DifferentialOracleFault::Semantic) {
        let _ = rooted_cfg(session, &step.options);
        assert!(inject_stale_query_for_oracle(
            session,
            DifferentialOracleFault::Semantic
        ));
    }
    let semantic = rooted_cfg(session, &step.options);
    let (semantic, semantic_hash, executable_hash, identities) = match semantic {
        Ok(output) => {
            let functions = output
                .functions()
                .iter()
                .map(|function| {
                    (
                        function.source_name().to_owned(),
                        function.air().len(),
                        function.cfg().blocks().len(),
                    )
                })
                .collect::<Vec<_>>();
            let strings = output
                .functions()
                .iter()
                .flat_map(|function| function.strings().iter().map(String::as_str))
                .collect::<Vec<_>>();
            let air = session
                .unstable_present(PresentationRequest {
                    stage: PresentationStage::Air,
                    options: &step.options,
                    file_order: &file_order,
                })
                .expect("oracle corpus must have stable AIR presentation");
            let cfg = session
                .unstable_present(PresentationRequest {
                    stage: PresentationStage::Cfg,
                    options: &step.options,
                    file_order: &file_order,
                })
                .expect("oracle corpus must have stable CFG presentation");
            let artifact = format!(
                "functions={:?};air={};cfg={};strings={:?};warnings={:?}",
                functions,
                air.as_str(),
                cfg.as_str(),
                strings,
                output.warnings()
            );
            let emitted = session
                .unstable_present(PresentationRequest {
                    stage: PresentationStage::Asm,
                    options: &step.options,
                    file_order: &file_order,
                })
                .expect("oracle corpus must have platform-stable assembly emission");
            let hash = format!("{:x}", Sha256::digest(emitted.as_str().as_bytes()));
            let executable_hash = match oracle_executable(session, &step.snapshot, &step.options) {
                Ok(executable) => format!("{:x}", Sha256::digest(&executable.elf)),
                Err(errors) => format!("error:{errors:?}"),
            };
            (
                artifact,
                hash,
                executable_hash,
                format!(
                    "source={:?};codegen={:?}",
                    step.snapshot.source_revision(),
                    format_args!("{:?}", step.options)
                ),
            )
        }
        Err(errors) => (
            format!("error:{errors:?}"),
            "not-emitted".to_owned(),
            "not-linked".to_owned(),
            format!("source={:?}", step.snapshot.source_revision()),
        ),
    };
    if fault == Some(DifferentialOracleFault::Import) {
        assert!(inject_stale_query_for_oracle(
            session,
            DifferentialOracleFault::Import
        ));
        imports = render_selected_import(session);
    }
    Observation {
        update,
        syntax,
        rir,
        semantic,
        semantic_hash,
        executable_hash,
        identities,
        imports,
    }
}

fn observe(session: &mut CompilerSession, step: &Step) -> Observation {
    observe_with_fault(session, step, None)
}

fn differing_fields(left: &Observation, right: &Observation) -> Vec<&'static str> {
    [
        (left.update != right.update, "update"),
        (left.syntax != right.syntax, "syntax"),
        (left.rir != right.rir, "rir"),
        (left.semantic != right.semantic, "semantic"),
        (
            left.semantic_hash != right.semantic_hash,
            "emitted-assembly-hash",
        ),
        (
            left.executable_hash != right.executable_hash,
            "executable-hash",
        ),
        (left.identities != right.identities, "stable-identities"),
        (left.imports != right.imports, "import-discovery"),
    ]
    .into_iter()
    .filter_map(|(different, field)| different.then_some(field))
    .collect()
}

fn first_mismatch(
    steps: &[Step],
    fault: Option<DifferentialOracleFault>,
) -> Option<(usize, String)> {
    let mut reused = CompilerSession::new();
    for (index, step) in steps.iter().enumerate() {
        let injected = (index + 1 == steps.len() && index > 0)
            .then_some(fault)
            .flatten();
        let warm = observe_with_fault(&mut reused, step, injected);
        let mut fresh_session = CompilerSession::new();
        let fresh = observe(&mut fresh_session, step);
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
    }
    None
}

fn minimized_failure(steps: &[Step], fault: Option<DifferentialOracleFault>) -> Option<String> {
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
    let mut preview_change = options_change.clone();
    preview_change.name = "preview-change";
    preview_change.options.preview_features = PreviewFeatures::from([PreviewFeature::TestInfra]);

    vec![
        step(
            "base",
            snapshot(&[(7, "/p/main.rue", "main.rue", base_source)], 7),
        ),
        step(
            "base-noop",
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
        preview_change,
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
            "query-native-drop-glue",
            snapshot(
                &[(
                    7,
                    "/p/main.rue",
                    "main.rue",
                    r#"
                    struct D { value: i32 }
                    drop fn D(self) {}
                    struct Inner { item: D, items: [D; 2] }
                    enum E { Full(Inner, D), Empty }
                    fn Box() -> type {
                        struct { item: D, drop fn(self) {} }
                    }
                    fn main() -> i32 {
                        let values = [
                            E.Full(
                                Inner {
                                    item: D { value: 1 },
                                    items: [D { value: 2 }, D { value: 3 }],
                                },
                                D { value: 4 },
                            ),
                            E.Empty,
                        ];
                        let B = Box();
                        let anonymous = B { item: D { value: 5 } };
                        0
                    }
                    "#,
                )],
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
    let glue = corpus
        .iter()
        .find(|step| step.name == "query-native-drop-glue")
        .unwrap();
    let glue_observation = observe(&mut CompilerSession::new(), glue);
    assert!(glue_observation.semantic.contains("__rue_drop_D"));
    assert!(glue_observation.semantic.contains("__rue_drop_Inner"));
    assert!(glue_observation.semantic.contains("__rue_drop_E"));
    assert!(glue_observation.semantic.contains("__rue_drop_array_"));
    assert!(
        glue_observation.semantic.matches("__rue_drop_").count() >= 5,
        "named, anonymous, array, and enum plans must all reach glue synthesis: {}",
        glue_observation.semantic
    );
}

#[test]
fn option_variants_produce_independent_canonical_semantic_outputs() {
    let source = snapshot(
        &[(
            7,
            "/p/main.rue",
            "main.rue",
            "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
        )],
        7,
    );
    let mut session = CompilerSession::new();
    session.update(&source).into_result().unwrap();
    let default = CompileOptions::default();
    let first = rooted_cfg(&mut session, &default).unwrap();
    let target = CompileOptions {
        target: *Target::all()
            .iter()
            .find(|&&target| target != default.target)
            .unwrap(),
        ..default.clone()
    };
    let optimized = CompileOptions {
        opt_level: OptLevel::O1,
        ..default.clone()
    };
    let preview = CompileOptions {
        preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
        ..default.clone()
    };
    let variants = [
        first,
        rooted_cfg(&mut session, &target).unwrap(),
        rooted_cfg(&mut session, &optimized).unwrap(),
        rooted_cfg(&mut session, &preview).unwrap(),
    ];
    for semantic in variants {
        assert_eq!(
            semantic.functions().first().unwrap().source_name(),
            "helper"
        );
        assert_eq!(semantic.functions().get(1).unwrap().source_name(), "main");
    }
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
    let metrics = session.unstable_metrics();
    assert!(metrics.retention().diagnostic_entries < steps.len());
}

#[test]
fn fault_injection_proves_semantic_and_import_cache_detection() {
    let corpus = corpus();
    for (steps, fault, affected) in [
        (
            &corpus[1..3],
            DifferentialOracleFault::Semantic,
            "affected fields: semantic, emitted-assembly-hash, executable-hash, stable-identities",
        ),
        (
            &corpus[corpus.len() - 2..],
            DifferentialOracleFault::Import,
            "affected fields: import-discovery",
        ),
    ] {
        let failure = minimized_failure(steps, Some(fault)).expect("injected fault must be caught");
        assert!(failure.contains("minimized sequence ["));
        assert_eq!(failure.lines().next().unwrap().matches(" -> ").count(), 1);
        assert!(failure.contains(affected), "{failure}");
    }
}
