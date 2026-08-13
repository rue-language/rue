//! Lower-frequency scale-curve collection for maintained Rue programs.
//!
//! This is intentionally a peer *reporting mode* of the ADR-0067 runner, not a
//! second compilation path. It launches the same compiler binary, consumes the
//! same `--benchmark-json` phase partition, and uses the same raw sample type.

use std::path::{Path, PathBuf};

use rue_perf_schema::{
    Band, BuildBoundaryPolicy, SCALING_REPORT_SCHEMA_VERSION, ScalingIdentity, ScalingManifest,
    ScalingObservation, ScalingRegime, ScalingReport, Summary, WorkerSetting, WorkloadShape,
    canonical_json, content_address,
};

use crate::measure::{SampleRequest, measure_fresh_compile};

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
    let mut target: Option<String> = None;
    let mut observations = Vec::with_capacity(manifest.workloads.len() * manifest.workers.len());
    for workload in &manifest.workloads {
        let source = options.repo_root.join(&workload.source);
        if !source.is_file() {
            return Err(format!(
                "scaling workload {:?} does not exist at {}",
                workload.id,
                source.display()
            ));
        }
        let mut shape: Option<WorkloadShape> = None;
        let mut reference_work = None;
        let mut output_identity: Option<(String, u64)> = None;
        for worker_setting in &manifest.workers {
            let policy = BuildBoundaryPolicy::fresh_source_to_native_v1(*worker_setting);
            let mut args = manifest.args.clone();
            args.extend(policy.canonical_compiler_args());
            let output = workdir.join(format!("{}-{:?}-out", workload.id, worker_setting));
            let mut resolved_workers = None;
            let mut samples = Vec::with_capacity(manifest.samples as usize);
            for sample_index in 0..manifest.samples {
                eprintln!(
                    "rue-bench: scaling {} {:?} sample {}/{}",
                    workload.id,
                    worker_setting,
                    sample_index + 1,
                    manifest.samples
                );
                let request = SampleRequest {
                    compiler: &options.compiler,
                    source: &source,
                    args: &args,
                    target: Some(&manifest.target),
                    output: output.clone(),
                    std_root: options.std_root.as_deref(),
                    batch_size: 1,
                    workload: &workload.id,
                    sample_index,
                    boundary_policy: Some(&policy),
                };
                let measured = measure_fresh_compile(&request).map_err(|detail| {
                    format!(
                        "scaling workload {:?} {:?} sample {} failed: {detail}",
                        workload.id, worker_setting, sample_index
                    )
                })?;
                if !measured.sample.phases.holds() {
                    return Err(format!(
                        "scaling workload {:?} {:?} sample {} violated phase accounting: root={} attributed={}",
                        workload.id,
                        worker_setting,
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
                        workload.id,
                        manifest.compiler_build_profile,
                        measured.compiler_build_profile,
                    ));
                }
                let evidence = measured
                    .sample
                    .boundary_evidence
                    .first()
                    .expect("the boundary policy requires exactly one process proof");
                let current_resolved = evidence.compiler.configuration.resolved_workers;
                match resolved_workers {
                    None => resolved_workers = Some(current_resolved),
                    Some(expected) if expected == current_resolved => {}
                    Some(expected) => {
                        return Err(format!(
                            "worker row {worker_setting:?} resolved inconsistently: {expected} then {current_resolved}"
                        ));
                    }
                }
                let current_output = (
                    evidence.runner.output_sha256.clone(),
                    evidence.compiler.emitted_output_size_bytes,
                );
                match &output_identity {
                    None => output_identity = Some(current_output),
                    Some(expected) if expected == &current_output => {}
                    Some(_) => {
                        return Err(format!(
                            "scaling workload {:?} produced different output across worker rows",
                            workload.id
                        ));
                    }
                }
                if *worker_setting == WorkerSetting::One {
                    observe_compiler_work(
                        &mut reference_work,
                        measured.compiler_work,
                        &workload.id,
                    )?;
                }
                samples.push(measured.sample);
            }
            observations.push(ScalingObservation {
                workload: workload.id.clone(),
                source: workload.source.clone(),
                question: workload.question.clone(),
                worker_setting: *worker_setting,
                resolved_workers: resolved_workers.expect("the manifest requires samples"),
                shape: shape
                    .clone()
                    .expect("the manifest requires at least two samples"),
                work: reference_work
                    .expect("the reference worker matrix starts with the one-worker row"),
                samples,
            });
        }
    }

    let report = ScalingReport {
        schema_version: SCALING_REPORT_SCHEMA_VERSION,
        identity: ScalingIdentity {
            manifest_revision: manifest.revision,
            commit: options.commit,
            started_at,
            finished_at: crate::utc_timestamp(),
            target: target.unwrap_or_else(|| manifest.target.clone()),
            environment: crate::environment::fingerprint(),
        },
        regime: ScalingRegime {
            compiler_state: "fresh_process_compile".to_string(),
            os_page_cache: "uncontrolled".to_string(),
            program_runtime_executed: false,
            samples_per_workload: manifest.samples,
            compiler_args: manifest.args,
            compiler_build_profile: manifest.compiler_build_profile,
            boundary: manifest.boundary,
            workers: manifest.workers,
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
        "- Commit: `{}`\n- Fixture revision: {}\n- Target: `{}`\n- Compiler build profile: `{}`\n- Boundary: `{:?}`\n- Worker matrix: `{:?}`\n- Machine: {} ({} cores, {} bytes memory)\n- Runner: `{}` / `{}` ({})\n- Regime: {} sequential fresh compiler processes per workload and worker row; OS page-cache state is uncontrolled.\n- Structural work: one-worker samples must agree exactly; schedule-dependent parallel work remains raw per-process evidence.\n- Runtime separation: compiled programs were not executed.\n\n",
        report.identity.commit,
        report.identity.manifest_revision,
        report.identity.target,
        report.regime.compiler_build_profile,
        report.regime.boundary,
        report.regime.workers,
        report.identity.environment.cpu_model,
        report.identity.environment.core_count,
        report.identity.environment.memory_bytes,
        report.identity.environment.runner_label,
        report.identity.environment.runner_image,
        report.identity.environment.runner_image_version,
        report.regime.samples_per_workload,
    ));
    out.push_str("Times and memory are median ± median absolute deviation (MAD). A changed shape id marks comparisons with earlier artifacts as advisory.\n\n");
    out.push_str("| workload / workers | shape id | files/modules | functions | bytes/lines/tokens | process ms | compiler ms | peak MiB | output KiB |\n");
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
            observation_label(observation),
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

    out.push_str("\n## Worker scaling and critical path\n\n");
    out.push_str("Utilization divides summed query-worker active time by compiler-root time and the compiler's resolved worker count. Ready wait is summed across dependency-ready items, so the mean and maximum are the directly comparable latency signals. Body columns show total/max milliseconds from bounded compiler histograms. Semantic prerequisite and analysis columns are adjacent, non-overlapping lexical intervals inside each body transaction. The five top-level CFG breakdown columns are non-overlapping lexical intervals inside each successful CFG body; projection, dependency collection, and prerequisite queries further partition the domain/prerequisite interval. CFG total remains the inclusive query duration and can be slightly larger because it also contains timing publication and outer query bookkeeping. The rooted-acquisition envelope is inclusive: it contains the semantic attempt used to discover a trusted-toolchain park, is not an exclusive phase, and must not be added to semantic time or read as filesystem cost.\n\n");
    out.push_str("| workload / workers | utilization | active ms | ready mean/max ms | longest chain | rooted acquisition envelope ms | semantic prerequisites total/max ms | semantic analysis total/max ms | CFG input total/max ms | local epoch total/max ms | domain/prereq total/max ms | domain projection total/max ms | prerequisite collection total/max ms | prerequisite queries total/max ms | CFG builder total/max ms | publication total/max ms | CFG total/max ms | CFG opt total/max ms | joins declined/total | donated permits |\n");
    out.push_str(
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
    );
    for observation in &report.workloads {
        let evidence = observation.samples.iter().map(|sample| {
            sample
                .boundary_evidence
                .first()
                .map(|evidence| evidence.critical_path.clone())
                .unwrap_or_default()
        });
        let utilization = summarize(evidence.clone().zip(&observation.samples).map(
            |(evidence, sample)| {
                let capacity = sample
                    .phases
                    .compiler_root_ns
                    .saturating_mul(u64::from(observation.resolved_workers));
                if capacity == 0 {
                    0
                } else {
                    evidence.query_worker_active_ns.saturating_mul(10_000) / capacity
                }
            },
        ));
        let active = summarize(evidence.clone().map(|e| e.query_worker_active_ns));
        let ready_mean = summarize(evidence.clone().map(|e| {
            if e.ready_items == 0 {
                0
            } else {
                e.ready_wait_ns / e.ready_items
            }
        }));
        let ready_max = summarize(evidence.clone().map(|e| e.max_ready_wait_ns));
        let chain = summarize(evidence.clone().map(|e| e.longest_query_dependency_chain));
        let toolchain = summarize(evidence.clone().map(|e| e.toolchain_acquisition_ns));
        let semantic_prerequisite_total = summarize(
            evidence
                .clone()
                .map(|e| e.semantic_prerequisite_bodies.total_ns),
        );
        let semantic_prerequisite_max = summarize(
            evidence
                .clone()
                .map(|e| e.semantic_prerequisite_bodies.max_ns),
        );
        let semantic_total = summarize(evidence.clone().map(|e| e.semantic_bodies.total_ns));
        let semantic_max = summarize(evidence.clone().map(|e| e.semantic_bodies.max_ns));
        let input_total = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_input_preparation_bodies.total_ns),
        );
        let input_max = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_input_preparation_bodies.max_ns),
        );
        let materialization_total = summarize(
            evidence
                .clone()
                .map(|e| e.semantic_materialization_bodies.total_ns),
        );
        let materialization_max = summarize(
            evidence
                .clone()
                .map(|e| e.semantic_materialization_bodies.max_ns),
        );
        let domain_total = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_domain_prerequisite_bodies.total_ns),
        );
        let domain_max = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_domain_prerequisite_bodies.max_ns),
        );
        let projection_total = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_domain_projection_bodies.total_ns),
        );
        let projection_max = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_domain_projection_bodies.max_ns),
        );
        let collection_total = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_prerequisite_collection_bodies.total_ns),
        );
        let collection_max = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_prerequisite_collection_bodies.max_ns),
        );
        let prerequisite_query_total = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_prerequisite_query_bodies.total_ns),
        );
        let prerequisite_query_max = summarize(
            evidence
                .clone()
                .map(|e| e.cfg_prerequisite_query_bodies.max_ns),
        );
        let builder_total = summarize(evidence.clone().map(|e| e.cfg_builder_bodies.total_ns));
        let builder_max = summarize(evidence.clone().map(|e| e.cfg_builder_bodies.max_ns));
        let publication_total =
            summarize(evidence.clone().map(|e| e.cfg_publication_bodies.total_ns));
        let publication_max = summarize(evidence.clone().map(|e| e.cfg_publication_bodies.max_ns));
        let cfg_total = summarize(evidence.clone().map(|e| e.cfg_construction_bodies.total_ns));
        let cfg_max = summarize(evidence.clone().map(|e| e.cfg_construction_bodies.max_ns));
        let opt_total = summarize(evidence.clone().map(|e| e.cfg_optimization_bodies.total_ns));
        let opt_max = summarize(evidence.clone().map(|e| e.cfg_optimization_bodies.max_ns));
        let joins = summarize(evidence.clone().map(|e| e.joins));
        let declined = summarize(evidence.clone().map(|e| e.declined_joins));
        let donated = summarize(evidence.map(|e| e.donated_permits));
        out.push_str(&format!(
            "| {} | {:.1}% | {:.2} | {:.3}/{:.3} | {} | {:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {:.2}/{:.2} | {}/{} | {} |\n",
            observation_label(observation),
            utilization.median as f64 / 100.0,
            active.median as f64 / 1_000_000.0,
            ready_mean.median as f64 / 1_000_000.0,
            ready_max.median as f64 / 1_000_000.0,
            chain.median,
            toolchain.median as f64 / 1_000_000.0,
            semantic_prerequisite_total.median as f64 / 1_000_000.0,
            semantic_prerequisite_max.median as f64 / 1_000_000.0,
            semantic_total.median as f64 / 1_000_000.0,
            semantic_max.median as f64 / 1_000_000.0,
            input_total.median as f64 / 1_000_000.0,
            input_max.median as f64 / 1_000_000.0,
            materialization_total.median as f64 / 1_000_000.0,
            materialization_max.median as f64 / 1_000_000.0,
            domain_total.median as f64 / 1_000_000.0,
            domain_max.median as f64 / 1_000_000.0,
            projection_total.median as f64 / 1_000_000.0,
            projection_max.median as f64 / 1_000_000.0,
            collection_total.median as f64 / 1_000_000.0,
            collection_max.median as f64 / 1_000_000.0,
            prerequisite_query_total.median as f64 / 1_000_000.0,
            prerequisite_query_max.median as f64 / 1_000_000.0,
            builder_total.median as f64 / 1_000_000.0,
            builder_max.median as f64 / 1_000_000.0,
            publication_total.median as f64 / 1_000_000.0,
            publication_max.median as f64 / 1_000_000.0,
            cfg_total.median as f64 / 1_000_000.0,
            cfg_max.median as f64 / 1_000_000.0,
            opt_total.median as f64 / 1_000_000.0,
            opt_max.median as f64 / 1_000_000.0,
            declined.median,
            joins.median,
            donated.median,
        ));
    }

    out.push_str("\n## Deterministic query work\n\n");
    out.push_str("Counts are exact for one fresh compiler process and must agree across the measured one-worker samples. Parallel scheduling outcomes remain available in each raw boundary proof but are not collapsed into an allegedly deterministic count. Request outcomes expose all query traffic before validation detail: claims start computations, reuses return compatible retained or task-local terminals, and joins share in-flight work. Declined joins are included in joins and identify wait-graph avoidance.\n\n");
    out.push_str("| workload | claims | reuses | joins declined/total | body completions | publications red/green | cancellations/cycles |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
        let runtime = observation.work.query_runtime;
        out.push_str(&format!(
            "| {} | {} | {} | {}/{} | {} | {}/{} | {}/{} |\n",
            observation.workload,
            runtime.claims,
            runtime.reuses,
            runtime.declined_joins,
            runtime.joins,
            runtime.body_completions,
            runtime.red_publications,
            runtime.green_publications,
            runtime.cancellations,
            runtime.cycles,
        ));
    }

    out.push_str("\nRegistry logical/index distinguishes requested exact-node resolutions from accesses to the shared incarnation index. `nodes/token` exposes validation amplification independently of clock noise.\n\n");
    out.push_str("| workload | traversals | input/dependency observations | memo hit/miss | registry logical/index | endorsements hit/probe | terminal leases duplicate/total | demands reuse/compute/join/total | retention scan entries | nodes/token |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
        let runtime = observation.work.query_runtime;
        let validation = runtime.validation;
        let nodes_per_token = if observation.shape.tokens == 0 {
            0.0
        } else {
            validation.node_visits as f64 / observation.shape.tokens as f64
        };
        out.push_str(&format!(
            "| {} | {} | {}/{} | {}/{} | {}/{} | {}/{} | {}/{} | {}/{}/{}/{} | {} | {:.2} |\n",
            observation.workload,
            validation.traversals,
            validation.input_observations,
            validation.dependency_observations,
            validation.memo_hits,
            validation.memo_misses,
            validation.registry_probes,
            validation.registry_index_lookups,
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

    out.push_str("\n## Semantic provider observations\n\n");
    out.push_str("Counts are exact snapshots of the provider operations already performed by production body analysis; the scaling probe adds no provider lookup or materialization work. Lookup and candidate counts expose demand at the semantic boundary, while exact fact-family reads and durable materializations separate repeated observation from body-local representation work.\n\n");
    out.push_str("| workload | name/import lookups | method/operator candidates | declaration facts total (identity/signature/type/const) |\n");
    out.push_str("| --- | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
        let work = observation.work.semantic_provider;
        out.push_str(&format!(
            "| {} | {}/{} | {}/{} | {} ({}/{}/{}/{}) |\n",
            observation.workload,
            work.name_lookups,
            work.import_lookups,
            work.method_candidates,
            work.operator_candidates,
            work.declaration_facts,
            work.identity_facts,
            work.signature_facts,
            work.type_facts,
            work.const_facts,
        ));
    }
    out.push_str("\n| workload | durable materializations total (shared/owned; const/nominal/function/method) | anonymous facts | producer facts | toolchain facts |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
        let work = observation.work.semantic_provider;
        out.push_str(&format!(
            "| {} | {} ({}/{}; {}/{}/{}/{}) | {} | {} | {} |\n",
            observation.workload,
            work.materializations,
            work.shared_payload_materializations,
            work.owned_payload_materializations,
            work.const_materializations,
            work.nominal_materializations,
            work.function_materializations,
            work.method_materializations,
            work.anonymous_facts,
            work.producer_facts,
            work.toolchain_facts,
        ));
    }

    out.push_str("\n## Semantic reachability scheduling\n\n");
    out.push_str("Counts are exact for database-owned body reachability. Width buckets count non-empty dependency-ready logical frontiers; multi-permit runtimes execute bounded windows as structured batches while a single-permit runtime executes the same windows inline. Transaction counts distinguish ready-frontier prefetch from fallback coordinator demand; `keys/batch` exposes available scheduling breadth independently of clock time.\n\n");
    out.push_str("| workload | scans | scan keys | batches | scheduled keys | keys/batch | transactions prefetched/serial | width 1 | width 2–3 | width 4–7 | width 8+ |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
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

    out.push_str("\n## CFG materialization preparation\n\n");
    out.push_str("Counts are exact for construction of the request-local lookup index and selection of body-local fact closures. `selections/build` exposes how broadly one immutable index is shared without conflating lookup preparation with the exact per-body selection that must remain.\n\n");
    out.push_str("| workload | index builds | declarations scanned | anonymous nominals scanned | type nodes scanned | fact selections | selections/build |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
        let work = observation.work.cfg_materialization;
        let selections_per_build = if work.index_builds == 0 {
            0.0
        } else {
            work.fact_selections as f64 / work.index_builds as f64
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {:.2} |\n",
            observation.workload,
            work.index_builds,
            work.declarations_scanned,
            work.anonymous_nominals_scanned,
            work.type_nodes_scanned,
            work.fact_selections,
            selections_per_build,
        ));
    }

    out.push_str("\nSelected body-local semantic inputs expose the aggregate size of the fresh per-body epochs created from those closures. `inputs/selection` is the mean number of selected root facts, not a transitive type-node count.\n\n");
    out.push_str("| workload | declarations | anonymous nominals | callables | nominal metadata | modules | builtin nominals | required types | inputs/selection |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
        let work = observation.work.cfg_materialization;
        let selected_inputs = work
            .declarations_selected
            .saturating_add(work.anonymous_nominals_selected)
            .saturating_add(work.callables_selected)
            .saturating_add(work.nominal_metadata_selected)
            .saturating_add(work.modules_selected)
            .saturating_add(work.builtin_nominals_selected)
            .saturating_add(work.required_types_selected);
        let inputs_per_selection = if work.fact_selections == 0 {
            0.0
        } else {
            selected_inputs as f64 / work.fact_selections as f64
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {:.2} |\n",
            observation.workload,
            work.declarations_selected,
            work.anonymous_nominals_selected,
            work.callables_selected,
            work.nominal_metadata_selected,
            work.modules_selected,
            work.builtin_nominals_selected,
            work.required_types_selected,
            inputs_per_selection,
        ));
    }

    out.push_str("\n## CFG prerequisite work\n\n");
    out.push_str("Counts are exact for stable types reached from body-local CFG domains and the unique registered prerequisite requests issued before AIR-to-CFG construction. `requests/type` exposes query traffic independently of elapsed time. Drop-glue terminals transitively observe their exact type-fact terminals; direct type-fact requests remain separate here so duplicate parent edges stay visible.\n\n");
    out.push_str("| workload | stable types scanned | layout requests | type-fact requests | drop-glue requests | requests/type |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
        let work = observation.work.cfg_prerequisites;
        let requests = work
            .layout_requests
            .saturating_add(work.type_fact_requests)
            .saturating_add(work.drop_glue_requests);
        let requests_per_type = if work.stable_types_scanned == 0 {
            0.0
        } else {
            requests as f64 / work.stable_types_scanned as f64
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.2} |\n",
            observation.workload,
            work.stable_types_scanned,
            work.layout_requests,
            work.type_fact_requests,
            work.drop_glue_requests,
            requests_per_type,
        ));
    }

    out.push_str("\n## CFG retained-charge bookkeeping\n\n");
    out.push_str("Counts are exact for logical retained-charge walks over body-local symbol tables at publication. They expose memory-policy bookkeeping independently of semantic construction and clock noise.\n\n");
    out.push_str("| workload | interner scans | entries scanned | UTF-8 bytes scanned |\n");
    out.push_str("| --- | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
        let work = observation.work.cfg_retained_charge;
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            observation.workload,
            work.interner_scans,
            work.interner_entries_scanned,
            work.interner_utf8_bytes_scanned,
        ));
    }

    out.push_str("\n## Query display identities\n\n");
    out.push_str("Counts and UTF-8 key bytes are exact for identities the compiler actually formatted. Structured-wait values count only labels rendered for a detected wait cycle; registering an acyclic edge is free of display formatting. Shared family names are excluded. `bytes/token` exposes presentation-only bookkeeping growth independently of clock noise.\n\n");
    out.push_str("| workload | memo nodes count/bytes | structured waits count/bytes | abort fallbacks count/bytes | bytes/token |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: |\n");
    for observation in reference_observations(report) {
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
        out.push_str(&format!("| {}", observation_label(observation)));
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
    for observation in reference_observations(report) {
        out.push_str(&format!(
            "- `{}` (`{}`): {}\n",
            observation.workload, observation.source, observation.question
        ));
    }
    out
}

fn reference_observations(report: &ScalingReport) -> impl Iterator<Item = &ScalingObservation> {
    report
        .workloads
        .iter()
        .filter(|observation| observation.worker_setting == WorkerSetting::One)
}

fn observation_label(observation: &ScalingObservation) -> String {
    format!(
        "{} / {:?}→{}",
        observation.workload, observation.worker_setting, observation.resolved_workers
    )
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
            boundary_evidence: Vec::new(),
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
                boundary: rue_perf_schema::BuildBoundary::FreshSourceToNativeV1,
                workers: WorkerSetting::REFERENCE_MATRIX.to_vec(),
            },
            workloads: vec![ScalingObservation {
                workload: "probe".to_string(),
                source: "probe.rue".to_string(),
                question: "test".to_string(),
                worker_setting: WorkerSetting::One,
                resolved_workers: 1,
                shape: WorkloadShape {
                    files: 1,
                    modules: 1,
                    bytes: 10,
                    lines: 1,
                    tokens: 5,
                    functions: 1,
                },
                work: rue_perf_schema::CompilerWork {
                    semantic_provider: rue_perf_schema::SemanticProviderWork {
                        name_lookups: 2,
                        import_lookups: 3,
                        method_candidates: 5,
                        operator_candidates: 7,
                        declaration_facts: 60,
                        identity_facts: 11,
                        signature_facts: 13,
                        type_facts: 17,
                        const_facts: 19,
                        materializations: 23,
                        shared_payload_materializations: 3,
                        owned_payload_materializations: 20,
                        const_materializations: 2,
                        nominal_materializations: 3,
                        function_materializations: 11,
                        method_materializations: 7,
                        anonymous_facts: 29,
                        producer_facts: 31,
                        toolchain_facts: 37,
                    },
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
                    cfg_materialization: rue_perf_schema::CfgMaterializationWork {
                        index_builds: 1,
                        declarations_scanned: 3,
                        anonymous_nominals_scanned: 1,
                        type_nodes_scanned: 7,
                        fact_selections: 4,
                        declarations_selected: 11,
                        anonymous_nominals_selected: 13,
                        callables_selected: 17,
                        nominal_metadata_selected: 18,
                        modules_selected: 19,
                        builtin_nominals_selected: 23,
                        required_types_selected: 29,
                    },
                    cfg_prerequisites: rue_perf_schema::CfgPrerequisiteWork {
                        stable_types_scanned: 31,
                        layout_requests: 37,
                        type_fact_requests: 41,
                        drop_glue_requests: 41,
                    },
                    cfg_retained_charge: rue_perf_schema::CfgRetainedChargeWork {
                        interner_scans: 4,
                        interner_entries_scanned: 31,
                        interner_utf8_bytes_scanned: 127,
                    },
                    query_runtime: rue_perf_schema::QueryRuntimeWork {
                        claims: 41,
                        reuses: 43,
                        joins: 47,
                        declined_joins: 2,
                        body_completions: 53,
                        red_publications: 59,
                        green_publications: 61,
                        cancellations: 67,
                        cycles: 71,
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
        assert!(rendered.contains("one-worker samples must agree exactly"));
        assert!(rendered.contains("compiled programs were not executed"));
        assert!(rendered.contains("median absolute deviation"));
        assert!(rendered.contains("shape id"));
        assert!(rendered.contains("Deterministic query work"));
        assert!(rendered.contains("Worker scaling and critical path"));
        assert!(rendered.contains("domain projection total/max ms"));
        assert!(rendered.contains("prerequisite queries total/max ms"));
        assert!(rendered.contains("CFG prerequisite work"));
        assert!(rendered.contains("requests/type"));
        assert!(rendered.contains("rooted acquisition envelope ms"));
        assert!(rendered.contains("must not be added to semantic time"));
        assert!(rendered.contains("joins declined/total"));
        assert!(rendered.contains("| probe | 41 | 43 | 2/47 | 53 | 59/61 | 67/71 |"));
        assert!(rendered.contains("nodes/token"));
        assert!(rendered.contains("Semantic provider observations"));
        assert!(rendered.contains("durable materializations"));
        assert!(rendered.contains("23 (3/20; 2/3/11/7)"));
        assert!(rendered.contains("Semantic reachability scheduling"));
        assert!(rendered.contains("keys/batch"));
        assert!(rendered.contains("CFG materialization preparation"));
        assert!(rendered.contains("selections/build"));
        assert!(rendered.contains("CFG retained-charge bookkeeping"));
        assert!(rendered.contains("UTF-8 bytes scanned"));
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
