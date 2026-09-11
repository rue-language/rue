//! Rust-owned validation entry point for daemon client performance records.

use std::path::PathBuf;

use rue_perf_schema::{
    DaemonInvocationRecord, DaemonPerformanceReport, project_test_execution_output,
    validate_daemon_invocation_record, validate_daemon_performance_report,
};

pub(crate) fn run() -> Result<u8, String> {
    let mut input = None;
    let mut args = std::env::args().skip(3);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--input" => {
                input = Some(PathBuf::from(
                    args.next()
                        .ok_or("daemon-performance validate requires --input <path>")?,
                ));
            }
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }
    let input = input.ok_or("daemon-performance validate requires --input <path>")?;
    let text = std::fs::read_to_string(&input)
        .map_err(|error| format!("could not read {}: {error}", input.display()))?;
    if let Ok(report) = serde_json::from_str::<DaemonPerformanceReport>(&text) {
        let errors = validate_daemon_performance_report(&report);
        return finish(errors);
    }
    let record = serde_json::from_str::<DaemonInvocationRecord>(&text).map_err(|error| {
        format!("daemon performance input is neither a report nor sidecar: {error}")
    })?;
    finish(validate_daemon_invocation_record(&record))
}

pub(crate) fn run_test_output() -> Result<u8, String> {
    let mut input = None;
    let mut args = std::env::args().skip(3);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--input" => {
                input = Some(PathBuf::from(
                    args.next()
                        .ok_or("daemon-performance test-output requires --input <path>")?,
                ));
            }
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }
    let input = input.ok_or("daemon-performance test-output requires --input <path>")?;
    let bytes = std::fs::read(&input)
        .map_err(|error| format!("could not read {}: {error}", input.display()))?;
    let projection = project_test_execution_output(&bytes)?;
    println!(
        "{}",
        serde_json::to_string(&projection)
            .map_err(|error| format!("could not encode test projection: {error}"))?
    );
    Ok(super::exit::OK)
}

fn finish(errors: Vec<String>) -> Result<u8, String> {
    if errors.is_empty() {
        Ok(super::exit::OK)
    } else {
        for error in errors {
            eprintln!("rue-bench daemon-performance: {error}");
        }
        Ok(super::exit::NOT_APPENDABLE)
    }
}
