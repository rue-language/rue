//! Reproducible in-process invalidation workload for `CanonicalFrontendSession`.

use std::{collections::HashMap, env, process, sync::Arc, time::Instant};

use rue_compiler::{
    CanonicalFrontendSession, CanonicalFrontendSessionWork, CompileOptions, ParsedModulesWork,
    SourceMetadata, SourceSnapshot,
};
use rue_span::FileId;
use serde_json::{Value, json};

const DEFAULT_MODULES: usize = 128;
const DEFAULT_WARMUP: usize = 3;
const DEFAULT_ITERATIONS: usize = 10;

#[derive(Clone, Copy)]
struct Config {
    modules: usize,
    warmup: usize,
    iterations: usize,
}

#[derive(Clone, Copy, Default)]
struct QueryCounts {
    merge_executions: usize,
    merge_reuses: usize,
    rir_executions: usize,
    rir_reuses: usize,
    semantic_executions: usize,
    semantic_reuses: usize,
    definition_executions: usize,
    definition_reuses: usize,
    downstream_invalidations: usize,
    semantic_entries_invalidated: usize,
    definition_entries_invalidated: usize,
}

impl QueryCounts {
    fn from(work: &CanonicalFrontendSessionWork) -> Self {
        Self {
            merge_executions: work.merge.executions,
            merge_reuses: work.merge.reuses,
            rir_executions: work.rir.executions,
            rir_reuses: work.rir.reuses,
            semantic_executions: work.semantic.executions,
            semantic_reuses: work.semantic.reuses,
            definition_executions: work.definitions.executions,
            definition_reuses: work.definitions.reuses,
            downstream_invalidations: work.downstream_invalidations,
            semantic_entries_invalidated: work.semantic_entries_invalidated,
            definition_entries_invalidated: work.definition_entries_invalidated,
        }
    }

    fn delta(self, before: Self) -> Self {
        Self {
            merge_executions: self.merge_executions - before.merge_executions,
            merge_reuses: self.merge_reuses - before.merge_reuses,
            rir_executions: self.rir_executions - before.rir_executions,
            rir_reuses: self.rir_reuses - before.rir_reuses,
            semantic_executions: self.semantic_executions - before.semantic_executions,
            semantic_reuses: self.semantic_reuses - before.semantic_reuses,
            definition_executions: self.definition_executions - before.definition_executions,
            definition_reuses: self.definition_reuses - before.definition_reuses,
            downstream_invalidations: self.downstream_invalidations
                - before.downstream_invalidations,
            semantic_entries_invalidated: self.semantic_entries_invalidated
                - before.semantic_entries_invalidated,
            definition_entries_invalidated: self.definition_entries_invalidated
                - before.definition_entries_invalidated,
        }
    }

    fn json(self) -> Value {
        json!({
            "merge_executions": self.merge_executions,
            "merge_reuses": self.merge_reuses,
            "rir_executions": self.rir_executions,
            "rir_reuses": self.rir_reuses,
            "semantic_executions": self.semantic_executions,
            "semantic_reuses": self.semantic_reuses,
            "definition_executions": self.definition_executions,
            "definition_reuses": self.definition_reuses,
            "downstream_invalidations": self.downstream_invalidations,
            "semantic_entries_invalidated": self.semantic_entries_invalidated,
            "definition_entries_invalidated": self.definition_entries_invalidated,
        })
    }
}

struct Fixture {
    modules: usize,
    base: SourceSnapshot,
    leaf_edit: SourceSnapshot,
    identity_edit: SourceSnapshot,
    syntax_error: SourceSnapshot,
}

impl Fixture {
    fn new(modules: usize) -> Self {
        assert!(modules >= 2, "workload requires at least two modules");
        Self {
            modules,
            base: snapshot(modules, Variant::Base),
            leaf_edit: snapshot(modules, Variant::LeafEdit),
            identity_edit: snapshot(modules, Variant::IdentityEdit),
            syntax_error: snapshot(modules, Variant::SyntaxError),
        }
    }
}

#[derive(Clone, Copy)]
enum Variant {
    Base,
    LeafEdit,
    IdentityEdit,
    SyntaxError,
}

fn snapshot(modules: usize, variant: Variant) -> SourceSnapshot {
    let changed = modules - 1;
    let physical = (0..modules)
        .map(|index| (FileId::new(index as u32), format!("/bench/m{index:03}.rue")))
        .collect::<HashMap<_, _>>();
    let logical = (0..modules)
        .map(|index| {
            let name = if matches!(variant, Variant::IdentityEdit) && index == changed {
                format!("renamed{index:03}.rue")
            } else {
                format!("m{index:03}.rue")
            };
            (FileId::new(index as u32), name)
        })
        .collect::<HashMap<_, _>>();
    let metadata = SourceMetadata::new(FileId::new(0), physical, logical).unwrap();
    let sources = (0..modules)
        .map(|index| {
            let source = if index == 0 {
                "fn main() -> i32 { 0 }".to_string()
            } else if index == changed && matches!(variant, Variant::SyntaxError) {
                format!("fn leaf{index}( {{")
            } else {
                let value = usize::from(
                    index == changed
                        && matches!(variant, Variant::LeafEdit | Variant::IdentityEdit),
                );
                format!("fn leaf{index}() -> i32 {{ {value} }}")
            };
            (FileId::new(index as u32), Arc::new(source))
        })
        .collect();
    SourceSnapshot::new(metadata, sources).unwrap()
}

fn parse_json(work: ParsedModulesWork) -> Value {
    json!({
        "lexer_invocations": work.syntax.lexer_invocations,
        "parser_invocations": work.syntax.parser_invocations,
        "lexed_bytes": work.syntax.lexed_bytes,
        "tokens": work.syntax.tokens,
        "modules_considered": work.modules_considered,
        "modules_reused": work.modules_reused,
        "modules_rebound": work.modules_rebound,
        "modules_reparsed": work.modules_reparsed,
        "source_text_clones": work.source_text_clones,
        "source_bytes_rehashed": work.source_bytes_rehashed,
    })
}

fn semantic_work_json(work: &CanonicalFrontendSessionWork, from: usize) -> Value {
    let records = &work.semantic_records[from..];
    json!({
        "bind_invocations": records.iter().map(|record| record.work.binding.bind_invocations).sum::<usize>(),
        "body_free_function_lookups": records.iter().map(|record| record.work.body_analysis.free_function_record_lookups).sum::<usize>(),
        "manifest_build_invocations": records.iter().map(|record| record.work.manifest.build_invocations).sum::<usize>(),
    })
}

fn definition_work_json(work: &CanonicalFrontendSessionWork, from: usize) -> Value {
    let records = &work.definition_records[from..];
    json!({
        "bind_invocations": records.iter().map(|record| record.binding.bind_invocations).sum::<usize>(),
        "manifest_build_invocations": records.iter().map(|record| record.manifest.build_invocations).sum::<usize>(),
        "manifest_bindings_visited": records.iter().map(|record| record.issuance.manifest_bindings_visited).sum::<usize>(),
        "ids_issued": records.iter().map(|record| record.issuance.ids_issued).sum::<usize>(),
    })
}

fn measure<F>(session: &mut CanonicalFrontendSession, operation: F) -> Value
where
    F: FnOnce(&mut CanonicalFrontendSession) -> ParsedModulesWork,
{
    let before = QueryCounts::from(session.work());
    let semantic_records = session.work().semantic_records.len();
    let definition_records = session.work().definition_records.len();
    let started = Instant::now();
    let parse = operation(session);
    let elapsed = started.elapsed();
    let work = session.work();
    json!({
        "wall_time_ns": elapsed.as_nanos(),
        "parse": parse_json(parse),
        "queries": QueryCounts::from(work).delta(before).json(),
        "semantic_work": semantic_work_json(work, semantic_records),
        "definition_work": definition_work_json(work, definition_records),
    })
}

fn run_iteration(fixture: &Fixture) -> Vec<Value> {
    let options = CompileOptions::default();
    let mut session = CanonicalFrontendSession::new();
    let mut scenarios = Vec::new();

    scenarios.push(named(
        "cold",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.base).work();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "exact_noop",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.base).work();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "leaf_body_edit",
        measure(&mut session, |session| {
            let update = session.update(&fixture.leaf_edit);
            assert!(update.downstream_invalidated());
            let parse = update.work();
            update.into_result().unwrap();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "module_identity_change",
        measure(&mut session, |session| {
            let update = session.update(&fixture.identity_edit);
            assert!(update.downstream_invalidated());
            let parse = update.work();
            update.into_result().unwrap();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "failed_syntax_edit",
        measure(&mut session, |session| {
            let update = session.update(&fixture.syntax_error);
            assert!(update.result().is_err());
            let parse = update.work();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "syntax_recovery",
        measure(&mut session, |session| {
            let parse = session.update(&fixture.identity_edit).work();
            session.merge().unwrap();
            session.rir().unwrap();
            session.semantic(&options).unwrap();
            parse
        }),
    ));

    let mut stable = CanonicalFrontendSession::new();
    scenarios.push(named(
        "stable_definitions_cold",
        measure(&mut stable, |session| {
            let parse = session.update(&fixture.base).work();
            session.stable_definitions(&options).unwrap();
            parse
        }),
    ));
    scenarios.push(named(
        "stable_definitions_reuse",
        measure(&mut stable, |session| {
            session.stable_definitions(&options).unwrap();
            ParsedModulesWork::default()
        }),
    ));
    assert_structure(&scenarios, fixture.modules);
    scenarios
}

fn named(name: &str, mut value: Value) -> Value {
    value["name"] = json!(name);
    value
}

fn count(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .fold(value, |value, key| &value[*key])
        .as_u64()
        .unwrap()
}

fn assert_structure(scenarios: &[Value], modules: usize) {
    let get = |name: &str| {
        scenarios
            .iter()
            .find(|value| value["name"] == name)
            .unwrap()
    };
    let cold = get("cold");
    assert_eq!(count(cold, &["parse", "modules_reparsed"]), modules as u64);
    assert_eq!(count(cold, &["parse", "lexer_invocations"]), modules as u64);
    assert_eq!(
        count(cold, &["parse", "parser_invocations"]),
        modules as u64
    );
    assert_query_executions(cold, 1, 1, 1, 0);
    assert_semantic_work(cold, 1, 1, 0);

    let noop = get("exact_noop");
    assert_eq!(count(noop, &["parse", "modules_reused"]), modules as u64);
    assert_reuse_parse_is_all_zero(noop);
    assert_query_executions(noop, 0, 0, 0, 0);
    assert_eq!(count(noop, &["queries", "merge_reuses"]), 1);
    // The explicit RIR query reuses once, then semantic validation asks for it again.
    assert_eq!(count(noop, &["queries", "rir_reuses"]), 2);
    assert_eq!(count(noop, &["queries", "semantic_reuses"]), 1);

    let leaf = get("leaf_body_edit");
    assert_eq!(
        count(leaf, &["parse", "modules_reused"]),
        (modules - 1) as u64
    );
    assert_eq!(count(leaf, &["parse", "modules_reparsed"]), 1);
    assert_eq!(count(leaf, &["parse", "lexer_invocations"]), 1);
    assert_eq!(count(leaf, &["parse", "parser_invocations"]), 1);
    assert_query_executions(leaf, 1, 1, 1, 0);
    assert_eq!(count(leaf, &["queries", "downstream_invalidations"]), 1);

    let identity = get("module_identity_change");
    assert_eq!(count(identity, &["parse", "modules_rebound"]), 1);
    assert_reuse_parse_is_all_zero(identity);
    assert_query_executions(identity, 1, 1, 1, 0);
    assert_eq!(count(identity, &["queries", "downstream_invalidations"]), 1);

    let failed = get("failed_syntax_edit");
    assert_eq!(count(failed, &["parse", "modules_reparsed"]), 1);
    assert_eq!(count(failed, &["parse", "lexer_invocations"]), 1);
    assert_eq!(count(failed, &["parse", "parser_invocations"]), 1);
    assert_query_executions(failed, 0, 0, 0, 0);
    assert_eq!(count(failed, &["queries", "merge_reuses"]), 1);
    assert_eq!(count(failed, &["queries", "rir_reuses"]), 2);
    assert_eq!(count(failed, &["queries", "downstream_invalidations"]), 0);
    assert_eq!(count(failed, &["queries", "semantic_reuses"]), 1);

    let recovery = get("syntax_recovery");
    assert_eq!(
        count(recovery, &["parse", "modules_reused"]),
        modules as u64
    );
    assert_reuse_parse_is_all_zero(recovery);
    assert_query_executions(recovery, 0, 0, 0, 0);
    assert_eq!(count(recovery, &["queries", "semantic_reuses"]), 1);

    let stable_cold = get("stable_definitions_cold");
    assert_query_executions(stable_cold, 1, 1, 1, 1);
    assert_semantic_work(stable_cold, 1, 1, 0);
    assert_eq!(
        count(stable_cold, &["definition_work", "bind_invocations"]),
        1
    );
    assert_eq!(
        count(
            stable_cold,
            &["definition_work", "manifest_build_invocations"]
        ),
        1
    );
    let stable_reuse = get("stable_definitions_reuse");
    assert_query_executions(stable_reuse, 0, 0, 0, 0);
    assert_semantic_work(stable_reuse, 0, 0, 0);
    assert_eq!(
        count(stable_reuse, &["definition_work", "bind_invocations"]),
        0
    );
    assert_eq!(
        count(
            stable_reuse,
            &["definition_work", "manifest_build_invocations"]
        ),
        0
    );
    assert_eq!(count(stable_reuse, &["queries", "definition_reuses"]), 1);
}

fn assert_reuse_parse_is_all_zero(scenario: &Value) {
    assert_eq!(count(scenario, &["parse", "lexer_invocations"]), 0);
    assert_eq!(count(scenario, &["parse", "parser_invocations"]), 0);
    assert_eq!(count(scenario, &["parse", "source_text_clones"]), 0);
    assert_eq!(count(scenario, &["parse", "source_bytes_rehashed"]), 0);
}

fn assert_query_executions(
    scenario: &Value,
    merge: u64,
    rir: u64,
    semantic: u64,
    definitions: u64,
) {
    assert_eq!(count(scenario, &["queries", "merge_executions"]), merge);
    assert_eq!(count(scenario, &["queries", "rir_executions"]), rir);
    assert_eq!(
        count(scenario, &["queries", "semantic_executions"]),
        semantic
    );
    assert_eq!(
        count(scenario, &["queries", "definition_executions"]),
        definitions
    );
}

fn assert_semantic_work(scenario: &Value, binds: u64, bodies: u64, manifests: u64) {
    assert_eq!(
        count(scenario, &["semantic_work", "bind_invocations"]),
        binds
    );
    assert_eq!(
        count(scenario, &["semantic_work", "body_free_function_lookups"]),
        bodies
    );
    assert_eq!(
        count(scenario, &["semantic_work", "manifest_build_invocations"]),
        manifests
    );
}

fn parse_config() -> Config {
    let mut config = Config {
        modules: DEFAULT_MODULES,
        warmup: DEFAULT_WARMUP,
        iterations: DEFAULT_ITERATIONS,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| usage(&format!("missing value for {arg}")));
        let value = value
            .parse::<usize>()
            .unwrap_or_else(|_| usage(&format!("invalid value for {arg}")));
        match arg.as_str() {
            "--modules" if value >= 2 => config.modules = value,
            "--warmup" => config.warmup = value,
            "--iterations" if value > 0 => config.iterations = value,
            _ => usage(&format!("unknown option or invalid value: {arg}")),
        }
    }
    config
}

fn usage(message: &str) -> ! {
    eprintln!("error: {message}");
    eprintln!("usage: rue-compiler-session-bench [--modules N] [--warmup N] [--iterations N]");
    process::exit(2)
}

fn main() {
    let config = parse_config();
    let fixture = Fixture::new(config.modules);
    for _ in 0..config.warmup {
        run_iteration(&fixture);
    }
    let iterations = (0..config.iterations)
        .map(|_| run_iteration(&fixture))
        .collect::<Vec<_>>();
    println!(
        "{}",
        json!({
            "schema_version": 1,
            "workload": "canonical_frontend_session_invalidation",
            "configuration": {
                "modules": config.modules,
                "warmup_iterations": config.warmup,
                "measured_iterations": config.iterations,
                "filesystem_discovery_during_updates": false,
                "backend_or_link": false,
            },
            "iterations": iterations,
        })
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_workload_is_a_structural_smoke_test() {
        let scenarios = run_iteration(&Fixture::new(4));
        assert_eq!(scenarios.len(), 8);
        assert!(
            scenarios
                .iter()
                .all(|scenario| scenario["wall_time_ns"].is_u64())
        );
    }
}
