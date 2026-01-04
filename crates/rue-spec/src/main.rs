//! Specification test runner for the Rue programming language.
//!
//! This binary runs the specification test suite and generates traceability reports.
//! It serves two purposes:
//!
//! 1. **Test Runner**: Execute specification tests from TOML files in `crates/rue-spec/cases/`
//! 2. **Traceability**: Verify that all normative specification paragraphs have test coverage
//!
//! # Usage
//!
//! ## Running Tests
//!
//! ```bash
//! # Run all specification tests
//! ./buck2 run //crates/rue-spec:rue-spec
//!
//! # Filter tests by pattern
//! ./buck2 run //crates/rue-spec:rue-spec -- "arithmetic"
//! ```
//!
//! ## Traceability Reports
//!
//! ```bash
//! # Generate a coverage summary
//! ./buck2 run //crates/rue-spec:rue-spec -- --traceability
//!
//! # Generate a detailed traceability matrix
//! ./buck2 run //crates/rue-spec:rue-spec -- --traceability --detailed
//! ```
//!
//! # Environment Variables
//!
//! - `RUE_SPEC_DIR` - Path to specification markdown files (default: `docs/spec/src`)
//! - `RUE_SPEC_CASES` - Path to test case TOML files (default: `crates/rue-spec/cases`)
//! - `RUE_BINARY` - Path to the rue compiler binary

use libtest2_mimic::{Harness, RunContext, RunError, Trial};
use rue_test_runner::{
    Case, find_dir, find_rue_binary, load_test_files, run_test_case, should_skip_for_platform,
};
use std::path::Path;

mod traceability;

/// Possible paths for the spec directory.
const SPEC_DIR_PATHS: &[&str] = &["docs/spec/src", "../docs/spec/src", "../../docs/spec/src"];

/// Possible paths for the cases directory.
const CASES_DIR_PATHS: &[&str] = &["crates/rue-spec/cases", "cases", "../rue-spec/cases"];

/// Run the traceability report.
fn run_traceability(detailed: bool) {
    let spec_dir = find_dir("RUE_SPEC_DIR", SPEC_DIR_PATHS, "docs/spec/src");
    let cases_dir = find_dir("RUE_SPEC_CASES", CASES_DIR_PATHS, "cases");

    if !spec_dir.exists() {
        eprintln!("Error: Spec directory not found: {}", spec_dir.display());
        eprintln!("Set RUE_SPEC_DIR environment variable or run from project root.");
        std::process::exit(1);
    }

    if !cases_dir.exists() {
        eprintln!("Error: Cases directory not found: {}", cases_dir.display());
        eprintln!("Set RUE_SPEC_CASES environment variable or run from project root.");
        std::process::exit(1);
    }

    let report = traceability::generate_report(&spec_dir, &cases_dir);

    if detailed {
        report.print_detailed();
    } else {
        report.print_summary();
    }

    // Exit with error if there are uncovered normative paragraphs or orphan references
    // Informative paragraphs don't require test coverage
    if report.normative_uncovered_count() > 0 || !report.orphan_references.is_empty() {
        std::process::exit(1);
    }
}

/// Wrapper to convert TestResult to libtest2_mimic's RunError type.
fn run_case_wrapper(
    case: &Case,
    rue_binary: &Path,
    skip: bool,
    ctx: RunContext<'_>,
) -> Result<(), RunError> {
    if skip {
        return ctx.ignore_for("marked as skip");
    }
    if let Some(reason) = should_skip_for_platform(&case.only_on) {
        return ctx.ignore_for(reason);
    }
    run_test_case(case, rue_binary).map_err(|e| RunError::fail(e.to_string()))
}

/// Wrapper for preview tests - reports failures but marks them as ignored to avoid failing the build.
fn run_preview_case_wrapper(
    case: &Case,
    rue_binary: &Path,
    skip: bool,
    ctx: RunContext<'_>,
) -> Result<(), RunError> {
    if skip {
        return ctx.ignore_for("marked as skip");
    }
    if let Some(reason) = should_skip_for_platform(&case.only_on) {
        return ctx.ignore_for(reason);
    }
    match run_test_case(case, rue_binary) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Report the failure but mark as ignored so it doesn't fail the suite
            ctx.ignore_for(format!("preview test failed (allowed): {}", e))
        }
    }
}

fn main() {
    // Check for traceability flag before parsing libtest args
    let raw_args: Vec<String> = std::env::args().collect();

    if raw_args.iter().any(|a| a == "--traceability") {
        let detailed = raw_args.iter().any(|a| a == "--detailed");
        run_traceability(detailed);
        return;
    }

    if raw_args.iter().any(|a| a == "--help-traceability") {
        println!("Traceability Report Options:");
        println!();
        println!("  --traceability     Generate spec coverage report");
        println!("  --detailed         Show detailed traceability matrix");
        println!();
        println!("Environment Variables:");
        println!("  RUE_SPEC_DIR       Path to spec markdown files (default: docs/spec/src)");
        println!("  RUE_SPEC_CASES     Path to test case files (default: crates/rue-spec/cases)");
        return;
    }

    // Find the rue binary
    let rue_binary = find_rue_binary();

    // Find the cases directory
    let cases_dir = find_dir("RUE_SPEC_CASES", CASES_DIR_PATHS, "cases");

    // Load all test files
    let specs = load_test_files(&cases_dir);

    // Build test trials, separating stable and preview tests
    // Pre-allocate based on total case count across all specs
    let total_cases: usize = specs.iter().map(|(_, s)| s.case.len()).sum();
    let mut tests: Vec<Trial> = Vec::with_capacity(total_cases);

    for (_, spec) in specs {
        let section_id = spec.section.id.clone();

        for case in spec.case {
            let test_name = format!("{}::{}", section_id, case.name);
            let skip = case.skip;
            let is_preview = case.preview.is_some();
            let preview_should_pass = case.preview_should_pass;
            let rue_binary = rue_binary.clone();

            // Preview tests that should pass use the normal wrapper (fail on error).
            // Preview tests without preview_should_pass use the lenient wrapper (allow failure).
            // Non-preview tests always use the normal wrapper.
            let trial = if is_preview && !preview_should_pass {
                // Preview tests that are allowed to fail
                Trial::test(test_name, move |ctx| {
                    run_preview_case_wrapper(&case, &rue_binary, skip, ctx)
                })
            } else {
                // Stable tests and preview tests that should pass fail normally
                Trial::test(test_name, move |ctx| {
                    run_case_wrapper(&case, &rue_binary, skip, ctx)
                })
            };

            tests.push(trial);
        }
    }

    if tests.is_empty() {
        eprintln!("Warning: No test cases found in {}", cases_dir.display());
        eprintln!("Make sure spec files exist and have the correct format.");
    }

    // Run all tests
    //
    // Preview tests without `preview_should_pass` are allowed to fail -
    // failures are marked as "ignored" so they don't break the build.
    //
    // Preview tests with `preview_should_pass = true` fail normally,
    // providing real test output for implemented portions of preview features.
    Harness::with_env().discover(tests).main();
}
