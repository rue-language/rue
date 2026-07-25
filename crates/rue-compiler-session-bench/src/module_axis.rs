//! Module-count axis for the completion locality workload (RUE-1091).
//!
//! # Why this exists
//!
//! Two checked-in workloads disagree about warm body locality:
//!
//! * the `--modules N` completion scenarios reanalyze exactly the edited leaf
//!   after a reachable-body edit (`bodies_attempted == 1` at N=128); and
//! * the `--representative` fixture reanalyzes every body after a leaf edit
//!   (`bodies_attempted == 6` of 6), including three bodies that are provably
//!   outside the edit's reverse closure.
//!
//! Those two fixtures differ on more than one axis at once. The completion
//! corpus is a single file assembled by `single_root_snapshot`; the
//! representative fixture is seven files reached through import discovery. Body
//! count and module count are therefore confounded, and the existing benchmarks
//! cannot say whether the representative result is caused by the program being
//! small enough that its reverse closure covers everything, or by something
//! about the multi-module import path defeating durable body reuse.
//!
//! This corpus holds the call graph, the body count, and the edit shape fixed
//! and varies two things independently: how many modules the bodies are spread
//! across, and which protocol commits import discovery. That separates the two
//! candidate causes without authoring a second hand-maintained fixture.
//!
//! # What it found
//!
//! Module count is not the variable — one module and eight behave identically.
//! The discovery protocol is: the same program under the same edit reanalyzes
//! one body when discovery is staged and every body under the rooted
//! import-demand protocol, which is the protocol any program with an import must
//! use. See `docs/notes/module-axis-locality-findings.md`.
//!
//! # What it is not
//!
//! This is a diagnostic workload, not a value gate. It reports the production
//! counters for each configuration and asserts the invariants that must hold
//! regardless of the answer — reached body count, warm/fresh parity for every
//! measured edit, and the staged locality result the existing audit claim rests
//! on. The rooted-demand locality numbers are the observation being collected,
//! so this module deliberately does not encode an expected value for them: a
//! repair must not read as a failure.

use std::{collections::BTreeMap, sync::Arc};

use rue_compiler::unstable::{
    DiscoverySourceAssembler, ImportDemandMode, begin_import_input_request,
    committed_import_discovery, import_demand_frontier_for_roots, import_observation_ledger,
    publish_import_observation_batch,
};
use rue_compiler::{
    AcceptedImportSource, AcceptedReadManifest, CompileOptions, CompilerSession,
    FileMetadataFingerprint, ImportDiscoveryContext, ImportObservation, ImportObservationLedger,
    PhysicalFileIdentity, SemanticView, SourceSnapshot,
};
use serde_json::{Value, json};

use super::{assert_cold_reused_parity_with_discovery, count, measure, named, with_parity};

const ROOT: &str = "/bench/main.rue";

/// Which body the corpus edits, relative to the base revision.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// The unmodified program.
    Base,
    /// Change the body of `f000`, the first reached leaf. Under exact reverse
    /// invalidation this reanalyzes `f000` and `main` and nothing else.
    LeafEdit,
    /// Change the body of `dead`, which nothing calls. Under exact reverse
    /// invalidation this reanalyzes no reached body at all.
    UnrelatedEdit,
}

/// One point on the module axis: `bodies` reached leaves spread across
/// `modules` files, all called directly from `main`.
///
/// `modules == 1` places every leaf in the root module, so the generated program
/// is shaped like the existing single-file completion corpus. It is the only
/// configuration that can run both discovery protocols, because a program with
/// no imports is the only one whose observation ledger closes without a demand
/// round; that makes it the control point where the two protocols are compared
/// on an identical program.
#[derive(Clone, Copy)]
pub(crate) struct Corpus {
    bodies: usize,
    modules: usize,
}

impl Corpus {
    pub(crate) fn new(bodies: usize, modules: usize) -> Self {
        assert!(bodies >= 2, "the corpus needs at least one leaf plus main");
        assert!(modules >= 1, "the corpus needs at least the root module");
        assert!(
            modules <= bodies,
            "a module must own at least one leaf; {modules} modules cannot hold {bodies} bodies"
        );
        Self { bodies, modules }
    }

    /// Leaves owned by module `index`, as a contiguous block. Block ownership
    /// (rather than round-robin) keeps `f000` — the leaf the edit targets — in
    /// the root module at every module count, so a leaf edit always dirties
    /// exactly one source file and the edit's blast radius does not itself vary
    /// along the axis.
    fn leaves_for(&self, index: usize) -> std::ops::Range<usize> {
        let leaves = self.bodies - 1;
        let base = leaves / self.modules;
        let extra = leaves % self.modules;
        let start = index * base + index.min(extra);
        let len = base + usize::from(index < extra);
        start..start + len
    }

    fn module_path(index: usize) -> String {
        format!("/bench/m{index:03}.rue")
    }

    fn sources(&self, variant: Variant) -> BTreeMap<String, Arc<String>> {
        let mut sources = BTreeMap::new();
        let leaf_body = |leaf: usize| {
            if leaf == 0 && variant == Variant::LeafEdit {
                1
            } else {
                leaf
            }
        };
        // Module 0 is the root: it owns `main`, the unrelated `dead` body, and
        // its own share of the leaves.
        let mut root = String::new();
        for index in 1..self.modules {
            root.push_str(&format!(
                "const m{index:03} = @import(\"m{index:03}.rue\");\n"
            ));
        }
        for leaf in self.leaves_for(0) {
            root.push_str(&format!(
                "fn f{leaf:03}() -> i32 {{ {} }}\n",
                leaf_body(leaf)
            ));
        }
        root.push_str(&format!(
            "fn dead() -> i32 {{ {} }}\n",
            i32::from(variant == Variant::UnrelatedEdit)
        ));
        let calls = (0..self.modules)
            .flat_map(|index| {
                self.leaves_for(index).map(move |leaf| {
                    if index == 0 {
                        format!("f{leaf:03}()")
                    } else {
                        format!("m{index:03}.f{leaf:03}()")
                    }
                })
            })
            .collect::<Vec<_>>();
        root.push_str(&format!("fn main() -> i32 {{ {} }}\n", calls.join(" + ")));
        sources.insert(ROOT.to_owned(), Arc::new(root));

        for index in 1..self.modules {
            let mut module = String::new();
            for leaf in self.leaves_for(index) {
                module.push_str(&format!(
                    "pub fn f{leaf:03}() -> i32 {{ {} }}\n",
                    leaf_body(leaf)
                ));
            }
            sources.insert(Self::module_path(index), Arc::new(module));
        }
        sources
    }

    fn snapshot(&self, variant: Variant) -> SourceSnapshot {
        assemble(&self.sources(variant)).0
    }
}

/// Stable per-path physical identity. Discovery requires each file to carry a
/// distinct identity, and it must not shift between revisions or an unrelated
/// edit would look like a file replacement.
fn identity_for_path(path: &str) -> u64 {
    if path == ROOT {
        return 1;
    }
    let index: u64 = path
        .trim_start_matches("/bench/m")
        .trim_end_matches(".rue")
        .parse()
        .expect("generated module paths are /bench/mNNN.rue");
    index + 2
}

fn assemble(
    sources: &BTreeMap<String, Arc<String>>,
) -> (SourceSnapshot, ImportDiscoveryContext, AcceptedReadManifest) {
    let context = ImportDiscoveryContext::new(1, "/bench", None, "rue-1091-module-axis").unwrap();
    let root = sources.get(ROOT).unwrap().clone();
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        ROOT,
        ROOT,
        PhysicalFileIdentity::new(1091, 1),
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
                PhysicalFileIdentity::new(1091, identity),
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

/// How a revision's import discovery is driven to a committed artifact.
///
/// The two checked-in workloads do not agree here either, and the difference is
/// not incidental: it decides whether the session observes one revision or a
/// chain of successor revisions per edit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Discovery {
    /// Stage the already-assembled snapshot and commit it, with no import-input
    /// request and no successor revisions. This is what the single-file
    /// completion corpus does (`close_completion_discovery`).
    Staged,
    /// Open a rooted import-input request and drain the demand frontier,
    /// publishing an observation batch per round. This is what the
    /// representative fixture does, and what the real driver does when it reads
    /// imports from a filesystem.
    Demand,
}

impl Discovery {
    fn name(self) -> &'static str {
        match self {
            Discovery::Staged => "staged",
            Discovery::Demand => "rooted_demand",
        }
    }
}

/// Stage an already-complete snapshot without opening an import-input request.
///
/// Every module is present in the assembled snapshot, so the demand frontier
/// would be empty; this protocol simply never asks. It is the multi-file
/// generalization of the completion corpus's helper.
fn close_staged_discovery(session: &mut CompilerSession, source: &SourceSnapshot) {
    if committed_import_discovery(session)
        .is_some_and(|artifact| artifact.source_revision() == source.source_revision())
    {
        return;
    }
    let sources = path_keyed_sources(source);
    let (discovered, context, accepted_reads) = assemble(&sources);
    assert_eq!(discovered.source_revision(), source.source_revision());
    session
        .stage_import_discovery(
            &discovered,
            context,
            accepted_reads.shared_slice(),
            ImportObservationLedger::default(),
        )
        .unwrap();
    session
        .close_import_discovery(ImportObservationLedger::default())
        .unwrap();
}

fn path_keyed_sources(source: &SourceSnapshot) -> BTreeMap<String, Arc<String>> {
    source
        .files()
        .map(|file| {
            let path = if file.path.starts_with('/') {
                file.path.to_owned()
            } else {
                format!("/bench/{}", file.path)
            };
            (path, Arc::new(file.source.to_owned()))
        })
        .collect()
}

/// Drive the rooted import-demand protocol to completion for `source`, matching
/// what the representative fixture does. The completion corpus's single-file
/// helper cannot be reused here: it assumes exactly one file and never issues a
/// demand frontier.
fn close_discovery(session: &mut CompilerSession, source: &SourceSnapshot) {
    if committed_import_discovery(session)
        .is_some_and(|artifact| artifact.source_revision() == source.source_revision())
    {
        return;
    }
    let sources = path_keyed_sources(source);
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
                            PhysicalFileIdentity::new(1091, identity),
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

fn close(discovery: Discovery, session: &mut CompilerSession, source: &SourceSnapshot) {
    match discovery {
        Discovery::Staged => close_staged_discovery(session, source),
        Discovery::Demand => close_discovery(session, source),
    }
}

fn measured_semantic(
    session: &mut CompilerSession,
    source: &SourceSnapshot,
    options: &CompileOptions,
    discovery: Discovery,
) -> (Value, Arc<SemanticView>) {
    let mut output = None;
    let value = measure(session, |session| {
        let update = session.update(source);
        let parse = update.unstable_metrics();
        update.into_result().unwrap();
        close(discovery, session, source);
        output = Some(session.semantic(options).unwrap());
        parse
    });
    (value, output.unwrap())
}

/// Prime a session on the base revision, apply `variant`, and report the
/// production counters for the warm request.
fn warm_edit(corpus: &Corpus, discovery: Discovery, variant: Variant, name: &str) -> Value {
    let options = CompileOptions::default();
    let base = corpus.snapshot(Variant::Base);
    let edited = corpus.snapshot(variant);
    let mut session = CompilerSession::new();
    measured_semantic(&mut session, &base, &options, discovery);
    let (value, output) = measured_semantic(&mut session, &edited, &options, discovery);
    let parity = assert_cold_reused_parity_with_discovery(
        &mut session,
        &output,
        &edited,
        &options,
        None,
        |session, source| close(discovery, session, source),
        |session, source| close(discovery, session, source),
    );
    named(name, with_parity(value, parity))
}

fn cold(corpus: &Corpus, discovery: Discovery) -> Value {
    let options = CompileOptions::default();
    let base = corpus.snapshot(Variant::Base);
    let mut session = CompilerSession::new();
    let (value, _) = measured_semantic(&mut session, &base, &options, discovery);
    named("cold", value)
}

/// One row of the axis: every scenario at a single `(bodies, modules)` point.
fn configuration(bodies: usize, modules: usize, discovery: Discovery) -> Value {
    let corpus = Corpus::new(bodies, modules);
    let scenarios = vec![
        cold(&corpus, discovery),
        warm_edit(&corpus, discovery, Variant::LeafEdit, "warm_leaf_body"),
        warm_edit(
            &corpus,
            discovery,
            Variant::UnrelatedEdit,
            "warm_unrelated_body",
        ),
    ];
    let cold_bodies = count(&scenarios[0], &["semantic_work", "bodies_attempted"]);
    assert_eq!(
        cold_bodies, bodies as u64,
        "the corpus must reach exactly {bodies} bodies at {modules} modules"
    );
    if discovery == Discovery::Staged {
        // The staged protocol is the one the N=128 completion scenarios use, and
        // exact reverse invalidation under it is the claim
        // `docs/notes/body-analysis-cfg-incrementality-audit.md` makes. Pin it
        // here so a regression in the behavior that claim rests on fails loudly.
        //
        // The rooted-demand rows are deliberately left unasserted: they are the
        // observation this workload exists to collect, and encoding today's
        // value would make a repair look like a failure.
        let leaf = &scenarios[1];
        assert_eq!(
            count(leaf, &["semantic_work", "body_analyses_computed"]),
            1,
            "a staged leaf edit reanalyzes exactly the edited body"
        );
        let unrelated = &scenarios[2];
        assert_eq!(
            count(unrelated, &["semantic_work", "body_analyses_computed"]),
            0,
            "a staged unrelated edit reanalyzes no reached body"
        );
    }
    json!({
        "bodies": bodies,
        "modules": modules,
        "discovery": discovery.name(),
        "scenarios": scenarios,
    })
}

pub(crate) fn run(bodies: usize, module_counts: &[usize]) -> Value {
    // `Staged` is only a legal protocol for a program with no imports: with any
    // import present the observation ledger is incomplete and the attempted
    // revision refuses to close. That is itself part of the finding — the
    // shortcut the single-file completion corpus takes is unavailable to every
    // multi-module program, including every real one.
    let configurations = module_counts
        .iter()
        .flat_map(|modules| {
            let protocols: &[Discovery] = if *modules == 1 {
                &[Discovery::Staged, Discovery::Demand]
            } else {
                &[Discovery::Demand]
            };
            protocols
                .iter()
                .map(move |discovery| configuration(bodies, *modules, *discovery))
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": 1,
        "workload": "completion_module_axis",
        "measurement_scope": "structural_counters_only_not_wall_time",
        "question": "which of module count and import-discovery protocol decides warm body locality",
        "configuration": {
            "bodies": bodies,
            "module_counts": module_counts,
            "call_graph": "direct: main calls every leaf once",
            "discovery_protocols": ["staged", "rooted_demand"],
        },
        "configurations": configurations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_blocks_partition_the_reached_bodies() {
        for modules in [1, 2, 3, 7, 16] {
            let corpus = Corpus::new(64, modules);
            let owned = (0..modules)
                .flat_map(|index| corpus.leaves_for(index))
                .collect::<Vec<_>>();
            assert_eq!(
                owned,
                (0..63).collect::<Vec<_>>(),
                "{modules} modules must partition the 63 leaves in order"
            );
        }
    }

    #[test]
    fn the_edited_leaf_stays_in_the_root_module_at_every_count() {
        for modules in [1, 2, 16, 63] {
            let corpus = Corpus::new(64, modules);
            assert!(
                corpus.leaves_for(0).contains(&0),
                "f000 must stay in the root module at {modules} modules so the \
                 edit dirties exactly one source at every point on the axis"
            );
        }
    }

    #[test]
    fn every_module_count_generates_the_same_reached_body_count() {
        for modules in [1, 2, 4] {
            let corpus = Corpus::new(16, modules);
            let sources = corpus.sources(Variant::Base);
            let bodies = sources
                .values()
                .map(|source| source.matches("fn f").count())
                .sum::<usize>();
            assert_eq!(bodies, 15, "{modules} modules must still declare 15 leaves");
            assert_eq!(sources.len(), modules, "one file per module");
        }
    }

    #[test]
    fn only_the_edited_body_text_changes() {
        let corpus = Corpus::new(16, 4);
        let base = corpus.sources(Variant::Base);
        let leaf = corpus.sources(Variant::LeafEdit);
        let changed = base
            .iter()
            .filter(|(path, source)| leaf.get(*path) != Some(source))
            .count();
        assert_eq!(
            changed, 1,
            "a leaf edit changes exactly one module's source"
        );
    }
}
