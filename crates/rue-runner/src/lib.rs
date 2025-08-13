// Test runner library for Rue programming language
//
// This crate provides comprehensive testing functionality including:
// - Test discovery from .rue files
// - Directive parsing from test comments
// - Normative specification validation
// - Test execution for various test types
// - Golden snapshot management
// - JSON report generation

use anyhow::Result;
use tracing::{error, info};

pub mod cli;
pub mod directives;
pub mod discover;
pub mod exec;
pub mod report;
pub mod snapshot;
pub mod spec;

pub use cli::Args;
pub use directives::{TestDirective, TestKind, TestSpec, build_test_spec};
pub use discover::TestDiscoverer;
pub use exec::TestExecutor;
pub use report::TestReport;
pub use snapshot::SnapshotManager;
pub use spec::SpecLoader;

/// Main entry point for running tests
pub fn run(args: Args) -> Result<()> {
    // Load specification
    let spec_loader = SpecLoader::new(&args.spec_file)?;

    // Discover tests
    let discoverer = TestDiscoverer::new(&args.test_paths);
    let tests = discoverer.discover()?;

    let total_tests = tests.len();
    info!("Found {} test files", total_tests);

    // Apply filter if specified
    let filtered_tests = if let Some(filter) = &args.filter {
        let regex = regex::Regex::new(filter)?;
        tests
            .into_iter()
            .filter(|test| regex.is_match(test.path.as_ref()))
            .collect()
    } else {
        tests
    };

    if filtered_tests.len() != total_tests {
        info!("Filtered to {} test files", filtered_tests.len());
    }

    // Execute tests
    let executor = TestExecutor::new(&args.rue_binary, &spec_loader)?;
    let snapshot_manager = if args.update_snapshots {
        SnapshotManager::with_update_mode(&args.snapshot_dir)?
    } else {
        SnapshotManager::new(&args.snapshot_dir)?
    };

    let mut report = TestReport::new();

    for test in filtered_tests {
        match executor.execute_test(&test, &snapshot_manager)? {
            exec::TestResult::Pass => {
                report.add_pass(&test.path);
                info!("PASS: {}", test.path);
            }
            exec::TestResult::Fail(reason) => {
                report.add_fail(&test.path, &reason);
                error!("FAIL: {}: {}", test.path, reason);
            }
            exec::TestResult::Skip(reason) => {
                report.add_skip(&test.path, &reason);
                info!("SKIP: {}: {}", test.path, reason);
            }
        }
    }

    // Generate report
    if let Some(report_file) = &args.report_file {
        report.write_json(report_file)?;
    }

    // Print summary
    if args.verbose {
        report.print_detailed();
    } else {
        report.print_summary();
    }

    if report.has_failures() && !args.ignore_failures {
        std::process::exit(1);
    }

    Ok(())
}
