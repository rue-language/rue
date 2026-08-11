//! Lower-frequency scale-curve collection for maintained Rue programs.
//!
//! This is intentionally a peer *reporting mode* of the ADR-0067 runner, not a
//! second compilation path. It launches the same compiler binary, consumes the
//! same `--benchmark-json` phase partition, and uses the same raw sample type.

use std::path::{Path, PathBuf};

use rue_perf_schema::{
    Band, SCALING_REPORT_SCHEMA_VERSION, ScalingIdentity, ScalingManifest, ScalingObservation,
    ScalingRegime, ScalingReport, Summary, WorkloadShape, canonical_json, content_address,
};

use crate::measure::{SampleRequest, measure_fresh_compile};

const COMPILER_WORK_SAMPLES_PER_WORKLOAD: u32 = 2;

struct Options {
    manifest: PathBuf,
    compiler: PathBuf,
    commit: String,
    repo_root: PathBuf,
    std_root: Option<PathBuf>,
    output: PathBuf,
    workdir: Option<PathBuf>,
}

pub fn run() -> Result<(), String> {
    let options = parse_args()?;
    let text = std::fs::read_to_string(&options.manifest)
        .map_err(|error| format!("could not read {}: {error}", options.manifest.display()))?;
    let manifest = ScalingManifest::parse(&text)?;

    let holder;
    let workdir = match &options.workdir {
        Some(directory) => {
            std::fs::create_dir_all(directory)
                .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
            directory.as_path()
        }
        None => {
            holder = tempfile::tempdir()
                .map_err(|error| format!("could not create a work directory: {error}"))?;
            holder.path()
        }
    };

    let started_at = crate::utc_timestamp();
    let mut compiler_work_args = manifest.args.clone();
    compiler_work_args.extend(["--jobs".to_string(), "1".to_string()]);
    let mut target: Option<String> = None;
    let mut observations = Vec::with_capacity(manifest.workloads.len());
    for workload in &manifest.workloads {
        let source = options.repo_root.join(&workload.source);
        if !source.is_file() {
            return Err(format!(
                "scaling workload {:?} does not exist at {}",
                workload.id,
                source.display()
            ));
        }
        let output = workdir.join(format!("{}-out", workload.id));
        let mut shape: Option<WorkloadShape> = None;
        let mut work = None;
        let mut samples = Vec::with_capacity(manifest.samples as usize);
        for sample_index in 0..manifest.samples {
            eprintln!(
                "rue-bench: scaling {} sample {}/{}",
                workload.id,
                sample_index + 1,
                manifest.samples
            );
            let request = SampleRequest {
                compiler: &options.compiler,
                source: &source,
                args: &manifest.args,
                output: output.clone(),
                std_root: options.std_root.as_deref(),
                batch_size: 1,
                workload: &workload.id,
                sample_index,
            };
            let measured = measure_fresh_compile(&request).map_err(|detail| {
                format!(
                    "scaling workload {:?} sample {} failed: {detail}",
                    workload.id, sample_index
                )
            })?;
            if !measured.sample.phases.holds() {
                return Err(format!(
                    "scaling workload {:?} sample {} violated phase accounting: root={} attributed={}",
                    workload.id,
                    sample_index,
                    measured.sample.phases.compiler_root_ns,
                    measured.sample.phases.attributed_ns()
                ));
            }
            match &shape {
                None => shape = Some(measured.shape.clone()),
                Some(expected) if expected != &measured.shape => {
                    return Err(format!(
                        "scaling workload {:?} changed shape between samples: {expected:?} then {:?}",
                        workload.id, measured.shape
                    ));
                }
                Some(_) => {}
            }
            match &target {
                None => target = Some(measured.target.clone()),
                Some(expected) if expected != &measured.target => {
                    return Err(format!(
                        "compiler target changed between samples: {expected:?} then {:?}",
                        measured.target
                    ));
                }
                Some(_) => {}
            }
            if measured.compiler_build_profile != manifest.compiler_build_profile {
                return Err(format!(
                    "scaling workload {:?} requires compiler build profile {:?}, but the compiler reported {:?}",
                    workload.id, manifest.compiler_build_profile, measured.compiler_build_profile,
                ));
            }
            samples.push(measured.sample);
        }
        // Keep the structural probes after the published timing samples so
        // adding deterministic counters cannot warm workload files before the
        // established timing regime observes them.
        for probe_index in 0..COMPILER_WORK_SAMPLES_PER_WORKLOAD {
            eprintln!(
                "rue-bench: scaling {} deterministic-work probe {}/{}",
                workload.id,
                probe_index + 1,
                COMPILER_WORK_SAMPLES_PER_WORKLOAD
            );
            let request = SampleRequest {
                compiler: &options.compiler,
                source: &source,
                args: &compiler_work_args,
                output: output.clone(),
                std_root: options.std_root.as_deref(),
                batch_size: 1,
                workload: &workload.id,
                sample_index: probe_index,
            };
            let measured = measure_fresh_compile(&request).map_err(|detail| {
                format!(
                    "scaling workload {:?} deterministic-work probe {} failed: {detail}",
                    workload.id, probe_index
                )
            })?;
            match &shape {
                None => shape = Some(measured.shape.clone()),
                Some(expected) if expected != &measured.shape => {
                    return Err(format!(
                        "scaling workload {:?} changed shape between deterministic-work probes: {expected:?} then {:?}",
                        workload.id, measured.shape
                    ));
                }
                Some(_) => {}
            }
            match &target {
                None => target = Some(measured.target.clone()),
                Some(expected) if expected != &measured.target => {
                    return Err(format!(
                        "compiler target changed between samples: {expected:?} then {:?}",
                        measured.target
                    ));
                }
                Some(_) => {}
            }
            if measured.compiler_build_profile != manifest.compiler_build_profile {
                return Err(format!(
                    "scaling workload {:?} requires compiler build profile {:?}, but the compiler reported {:?}",
                    workload.id, manifest.compiler_build_profile, measured.compiler_build_profile,
                ));
            }
            observe_compiler_work(&mut work, measured.compiler_work, &workload.id)?;
        }
        observations.push(ScalingObservation {
            workload: workload.id.clone(),
            source: workload.source.clone(),
            question: workload.question.clone(),
            shape: shape.expect("the manifest requires at least two samples"),
            work: work.expect("the manifest requires at least two samples"),
            samples,
        });
    }

    let report = ScalingReport {
        schema_version: SCALING_REPORT_SCHEMA_VERSION,
        identity: ScalingIdentity {
            manifest_revision: manifest.revision,
            commit: options.commit,
            started_at,
            finished_at: crate::utc_timestamp(),
            target: target.unwrap_or_else(|| "unknown".to_string()),
            environment: crate::environment::fingerprint(),
        },
        regime: ScalingRegime {
            compiler_state: "fresh_process_compile".to_string(),
            os_page_cache: "uncontrolled".to_string(),
            program_runtime_executed: false,
            samples_per_workload: manifest.samples,
            compiler_args: manifest.args,
            compiler_build_profile: manifest.compiler_build_profile,
            compiler_work_samples_per_workload: COMPILER_WORK_SAMPLES_PER_WORKLOAD,
            compiler_work_args,
        },
        workloads: observations,
    };
    write_report(&options.output, &report)?;
    Ok(())
}

fn observe_compiler_work(
    expected: &mut Option<rue_perf_schema::CompilerWork>,
    observed: rue_perf_schema::CompilerWork,
    workload: &str,
) -> Result<(), String> {
    match expected {
        None => *expected = Some(observed),
        Some(expected) if *expected != observed => {
            return Err(format!(
                "scaling workload {workload:?} changed deterministic compiler work between samples: {expected:?} then {observed:?}"
            ));
        }
        Some(_) => {}
    }
    Ok(())
}

fn parse_args() -> Result<Options, String> {
    let mut manifest = None;
    let mut compiler = None;
    let mut commit = None;
    let mut output = None;
    let mut repo_root = None;
    let mut std_root = None;
    let mut workdir = None;
    let mut args = std::env::args().skip(2);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value()?)),
            "--compiler" => compiler = Some(PathBuf::from(value()?)),
            "--commit" => commit = Some(value()?),
            "--out" => output = Some(PathBuf::from(value()?)),
            "--repo-root" => repo_root = Some(PathBuf::from(value()?)),
            "--std-root" => std_root = Some(PathBuf::from(value()?)),
            "--workdir" => workdir = Some(PathBuf::from(value()?)),
            other => return Err(format!("unrecognized scaling argument {other:?}")),
        }
    }
    let commit = commit.ok_or("scaling requires --commit <revision>")?;
    if commit.is_empty() {
        return Err("--commit must not be empty".to_string());
    }
    let repo_root = repo_root.unwrap_or_else(|| PathBuf::from("."));
    let std_root = std_root.or_else(|| {
        let candidate = repo_root.join("std");
        candidate.is_dir().then_some(candidate)
    });
    Ok(Options {
        manifest: manifest.ok_or("scaling requires --manifest <path>")?,
        compiler: compiler.ok_or("scaling requires --compiler <path>")?,
        commit,
        output: output.ok_or("scaling requires --out <path>")?,
        repo_root,
        std_root,
        workdir,
    })
}

fn write_report(path: &Path, report: &ScalingReport) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let raw = canonical_json(report)
        .map_err(|error| format!("could not serialize the scaling report: {error}"))?;
    std::fs::write(path, raw)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    let markdown_path = path.with_extension("md");
    std::fs::write(&markdown_path, render(report))
        .map_err(|error| format!("could not write {}: {error}", markdown_path.display()))?;
    let address = content_address(report).unwrap_or_else(|_| "unaddressable".to_string());
    eprintln!(
        "rue-bench: wrote {} and {} ({address})",
        path.display(),
        markdown_path.display()
    );
    Ok(())
}

fn render(report: &ScalingReport) -> String {
    let mut out = String::new();
    out.push_str("# Rue compiler scaling report\n\n");
    out.push_str(&format!(
        "- Commit: `{}`\n- Fixture revision: {}\n- Target: `{}`\n- Compiler build profile: `{}`\n- Machine: {} ({} cores, {} bytes memory)\n- Runner: `{}` / `{}` ({})\n- Regime: {} sequential fresh compiler processes per workload; OS page-cache state is uncontrolled.\n- Structural work: {} single-worker compiler-work probes per workload with arguments `{:?}`; probe timings are not published.\n- Runtime separation: compiled programs were not executed.\n\n",
        report.identity.commit,
        report.identity.manifest_revision,
        report.identity.target,
        report.regime.compiler_build_profile,
        report.identity.environment.cpu_model,
        report.identity.environment.core_count,
        report.identity.environment.memory_bytes,
        report.identity.environment.runner_label,
        report.identity.environment.runner_image,
        report.identity.environment.runner_image_version,
        report.regime.samples_per_workload,
        report.regime.compiler_work_samples_per_workload,
        report.regime.compiler_work_args,
    ));
    out.push_str("Times and memory are median ± median absolute deviation (MAD). A changed shape id marks comparisons with earlier artifacts as advisory.\n\n");
    out.push_str("| workload | shape id | files/modules | functions | bytes/lines/tokens | process ms | compiler ms | peak MiB | output KiB |\n");
    out.push_str("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in &report.workloads {
        let process = summarize(
            observation
                .samples
                .iter()
                .map(|sample| sample.process_elapsed_ns),
        );
        let compiler = summarize(
            observation
                .samples
                .iter()
                .map(|sample| sample.phases.compiler_root_ns),
        );
        let memory = summarize(
            observation
                .samples
                .iter()
                .map(|sample| sample.peak_memory_bytes),
        );
        let output = summarize(
            observation
                .samples
                .iter()
                .map(|sample| sample.output_binary_bytes),
        );
        let shape_id = content_address(&(&observation.source, &observation.shape))
            .map(|hash| hash[..12].to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        out.push_str(&format!(
            "| {} | `{}` | {}/{} | {} | {}/{}/{} | {:.2} ± {:.2} | {:.2} ± {:.2} | {:.1} ± {:.1} | {:.1} ± {:.1} |\n",
            observation.workload,
            shape_id,
            observation.shape.files,
            observation.shape.modules,
            observation.shape.functions,
            observation.shape.bytes,
            observation.shape.lines,
            observation.shape.tokens,
            process.median as f64 / 1_000_000.0,
            process.mad as f64 / 1_000_000.0,
            compiler.median as f64 / 1_000_000.0,
            compiler.mad as f64 / 1_000_000.0,
            memory.median as f64 / (1024.0 * 1024.0),
            memory.mad as f64 / (1024.0 * 1024.0),
            output.median as f64 / 1024.0,
            output.mad as f64 / 1024.0,
        ));
    }

    out.push_str("\n## Deterministic query work\n\n");
    out.push_str("Counts are exact for one fresh compiler process and must agree across the fixed single-worker structural probes. `nodes/token` exposes validation amplification independently of clock noise.\n\n");
    out.push_str("| workload | traversals | input/dependency observations | memo hit/miss | endorsements hit/probe | terminal leases duplicate/total | demands reuse/compute/join/total | retention scan entries | nodes/token |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in &report.workloads {
        let runtime = observation.work.query_runtime;
        let validation = runtime.validation;
        let nodes_per_token = if observation.shape.tokens == 0 {
            0.0
        } else {
            validation.node_visits as f64 / observation.shape.tokens as f64
        };
        out.push_str(&format!(
            "| {} | {} | {}/{} | {}/{} | {}/{} | {}/{} | {}/{}/{}/{} | {} | {:.2} |\n",
            observation.workload,
            validation.traversals,
            validation.input_observations,
            validation.dependency_observations,
            validation.memo_hits,
            validation.memo_misses,
            validation.endorsement_hits,
            validation.endorsement_probes,
            validation.duplicate_terminal_lease_observations,
            validation.terminal_lease_observations,
            validation.demand_reuses,
            validation.demand_computes,
            validation.demand_joins,
            validation.demands,
            runtime.retention_scan_entries,
            nodes_per_token,
        ));
    }

    out.push_str("\n## Semantic reachability scheduling\n\n");
    out.push_str("Counts are exact for database-owned body reachability. Width buckets count non-empty frontiers submitted to structured batch scheduling; transaction counts distinguish structured prefetch from serial coordinator demand; `keys/batch` exposes available scheduling breadth independently of clock time.\n\n");
    out.push_str("| workload | scans | scan keys | batches | scheduled keys | keys/batch | transactions prefetched/serial | width 1 | width 2–3 | width 4–7 | width 8+ |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in &report.workloads {
        let work = observation.work.semantic_reachability;
        let keys_per_batch = if work.frontier_batches == 0 {
            0.0
        } else {
            work.frontier_keys as f64 / work.frontier_batches as f64
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.2} | {}/{} | {} | {} | {} | {} |\n",
            observation.workload,
            work.frontier_scans,
            work.frontier_scan_keys,
            work.frontier_batches,
            work.frontier_keys,
            keys_per_batch,
            work.transactions_prefetched,
            work.transactions_serial,
            work.frontier_width_one,
            work.frontier_width_two_to_three,
            work.frontier_width_four_to_seven,
            work.frontier_width_eight_or_more,
        ));
    }

    out.push_str("\n## Query display identities\n\n");
    out.push_str("Counts and UTF-8 key bytes are exact for identities the compiler actually formatted. Structured-wait values count only labels rendered for a detected wait cycle; registering an acyclic edge is free of display formatting. Shared family names are excluded. `bytes/token` exposes presentation-only bookkeeping growth independently of clock noise.\n\n");
    out.push_str("| workload | memo nodes count/bytes | structured waits count/bytes | abort fallbacks count/bytes | bytes/token |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: |\n");
    for observation in &report.workloads {
        let identities = observation.work.query_runtime.display_identities;
        let total_bytes = identities
            .memo_node_bytes
            .saturating_add(identities.structured_wait_bytes)
            .saturating_add(identities.abort_fallback_bytes);
        let bytes_per_token = if observation.shape.tokens == 0 {
            0.0
        } else {
            total_bytes as f64 / observation.shape.tokens as f64
        };
        out.push_str(&format!(
            "| {} | {}/{} | {}/{} | {}/{} | {:.2} |\n",
            observation.workload,
            identities.memo_node_materializations,
            identities.memo_node_bytes,
            identities.structured_wait_materializations,
            identities.structured_wait_bytes,
            identities.abort_fallback_materializations,
            identities.abort_fallback_bytes,
            bytes_per_token,
        ));
    }

    out.push_str("\n## Additive compiler-root phase medians\n\n");
    out.push_str("All values are milliseconds. Bands are mutually exclusive and sum to compiler-root time per raw sample.\n\n");
    out.push_str("| workload | source/parse | program | semantic | CFG/opt | backend | object | linking | mixed | unattributed |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in &report.workloads {
        out.push_str(&format!("| {}", observation.workload));
        for band in Band::all() {
            let summary = summarize(
                observation
                    .samples
                    .iter()
                    .map(|sample| sample.phases.band_ns(band)),
            );
            out.push_str(&format!(" | {:.2}", summary.median as f64 / 1_000_000.0));
        }
        out.push_str(" |\n");
    }

    out.push_str("\n## Fixture intent\n\n");
    for observation in &report.workloads {
        out.push_str(&format!(
            "- `{}` (`{}`): {}\n",
            observation.workload, observation.source, observation.question
        ));
    }
    out
}

fn summarize(values: impl Iterator<Item = u64>) -> Summary {
    let values: Vec<u64> = values.collect();
    Summary::of(&values).expect("a scaling observation always has samples")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rue_perf_schema::{EnvironmentFingerprint, Phase, PhaseAccounting, Sample};

    use super::*;

    fn report() -> ScalingReport {
        let phase_ns = Phase::ALL.into_iter().map(|phase| (phase, 10)).collect();
        let sample = Sample {
            batch_size: 1,
            process_elapsed_ns: 100,
            peak_memory_bytes: 1024,
            output_binary_bytes: 512,
            phases: PhaseAccounting {
                phase_ns,
                mixed_parallel_ns: 10,
                unattributed_ns: 20,
                compiler_root_ns: 100,
            },
        };
        ScalingReport {
            schema_version: SCALING_REPORT_SCHEMA_VERSION,
            identity: ScalingIdentity {
                manifest_revision: 1,
                commit: "abc".to_string(),
                started_at: "2026-01-01T00:00:00Z".to_string(),
                finished_at: "2026-01-01T00:00:01Z".to_string(),
                target: "test-target".to_string(),
                environment: EnvironmentFingerprint {
                    runner_label: "local".to_string(),
                    runner_image: "local".to_string(),
                    runner_image_version: "unknown".to_string(),
                    cpu_model: "test cpu".to_string(),
                    core_count: 4,
                    memory_bytes: 4096,
                    kernel_version: "test".to_string(),
                    os_version: "test".to_string(),
                    architecture: "test".to_string(),
                },
            },
            regime: ScalingRegime {
                compiler_state: "fresh_process_compile".to_string(),
                os_page_cache: "uncontrolled".to_string(),
                program_runtime_executed: false,
                samples_per_workload: 2,
                compiler_args: Vec::new(),
                compiler_build_profile: "release_thin_lto".to_string(),
                compiler_work_samples_per_workload: 2,
                compiler_work_args: vec!["--jobs".to_string(), "1".to_string()],
            },
            workloads: vec![ScalingObservation {
                workload: "probe".to_string(),
                source: "probe.rue".to_string(),
                question: "test".to_string(),
                shape: WorkloadShape {
                    files: 1,
                    modules: 1,
                    bytes: 10,
                    lines: 1,
                    tokens: 5,
                    functions: 1,
                },
                work: rue_perf_schema::CompilerWork {
                    semantic_reachability: rue_perf_schema::SemanticReachabilityWork {
                        frontier_scans: 4,
                        frontier_scan_keys: 7,
                        frontier_batches: 2,
                        frontier_keys: 7,
                        frontier_width_one: 1,
                        frontier_width_eight_or_more: 1,
                        transactions_prefetched: 7,
                        transactions_serial: 0,
                        ..Default::default()
                    },
                    query_runtime: rue_perf_schema::QueryRuntimeWork {
                        validation: rue_perf_schema::ValidationWork {
                            traversals: 3,
                            node_visits: 7,
                            memo_hits: 5,
                            memo_misses: 2,
                            ..Default::default()
                        },
                        display_identities: rue_perf_schema::DisplayIdentityWork {
                            memo_node_materializations: 3,
                            memo_node_bytes: 21,
                            structured_wait_materializations: 2,
                            structured_wait_bytes: 14,
                            ..Default::default()
                        },
                        retention_enforcements: 1,
                        retention_scan_entries: 4,
                    },
                },
                samples: vec![sample.clone(), sample],
            }],
        }
    }

    #[test]
    fn markdown_states_the_regime_and_runtime_separation() {
        let rendered = render(&report());
        assert!(rendered.contains("OS page-cache state is uncontrolled"));
        assert!(rendered.contains("Compiler build profile: `release_thin_lto`"));
        assert!(rendered.contains("2 single-worker compiler-work probes"));
        assert!(rendered.contains("compiled programs were not executed"));
        assert!(rendered.contains("median absolute deviation"));
        assert!(rendered.contains("shape id"));
        assert!(rendered.contains("Deterministic query work"));
        assert!(rendered.contains("nodes/token"));
        assert!(rendered.contains("Semantic reachability scheduling"));
        assert!(rendered.contains("keys/batch"));
        assert!(rendered.contains("Query display identities"));
        assert!(rendered.contains("bytes/token"));
    }

    #[test]
    fn raw_report_round_trips_without_unknown_derived_fields() {
        let report = report();
        let encoded = canonical_json(&report).unwrap();
        let decoded: ScalingReport = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, report);
    }

    #[test]
    fn phase_fixture_is_additive() {
        let report = report();
        assert!(report.workloads[0].samples[0].phases.holds());
        let phase_map: BTreeMap<_, _> = report.workloads[0].samples[0].phases.phase_ns.clone();
        assert_eq!(phase_map.len(), Phase::ALL.len());
    }

    #[test]
    fn changed_deterministic_work_rejects_a_scaling_workload() {
        let mut expected = None;
        let first = rue_perf_schema::CompilerWork::default();
        observe_compiler_work(&mut expected, first, "probe").unwrap();
        observe_compiler_work(&mut expected, first, "probe").unwrap();

        let changed = rue_perf_schema::CompilerWork {
            query_runtime: rue_perf_schema::QueryRuntimeWork {
                retention_scan_entries: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let error = observe_compiler_work(&mut expected, changed, "probe").unwrap_err();
        assert!(
            error.contains("changed deterministic compiler work"),
            "{error}"
        );
    }
}
