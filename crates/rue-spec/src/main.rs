use libtest2_mimic::{Harness, RunContext, RunError, Trial};
use rue_test_runner::{Case, find_rue_binary, load_test_files, run_test_case};
use std::path::{Path, PathBuf};

mod traceability;

/// Find the spec directory.
fn find_spec_dir() -> PathBuf {
    std::env::var("RUE_SPEC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let possible_paths = ["docs/spec/src", "../docs/spec/src", "../../docs/spec/src"];
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            Path::new("docs/spec/src").to_path_buf()
        })
}

/// Find the cases directory.
fn find_cases_dir() -> PathBuf {
    std::env::var("RUE_SPEC_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let possible_paths = ["crates/rue-spec/cases", "cases", "../rue-spec/cases"];
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            Path::new("cases").to_path_buf()
        })
}

/// Run the traceability report.
fn run_traceability(detailed: bool) {
    let spec_dir = find_spec_dir();
    let cases_dir = find_cases_dir();

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
    let cases_dir = find_cases_dir();

    // Load all test files
    let specs = load_test_files(&cases_dir);

    // Build test trials, separating stable and preview tests
    let mut tests: Vec<Trial> = Vec::new();

    for (_, spec) in specs {
        let section_id = spec.section.id.clone();

        for case in spec.case {
            let test_name = format!("{}::{}", section_id, case.name);
            let skip = case.skip;
            let is_preview = case.preview.is_some();
            let rue_binary = rue_binary.clone();

            let trial = if is_preview {
                // Preview tests use the wrapper that tracks but doesn't fail
                Trial::test(test_name, move |ctx| {
                    run_preview_case_wrapper(&case, &rue_binary, skip, ctx)
                })
            } else {
                // Stable tests fail normally
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
    // Note: libtest2-mimic's Harness::main() calls std::process::exit(),
    // so we can't run code after it. Preview test summary is printed via
    // a custom drop handler or we accept the limitation.
    //
    // Preview tests that fail are marked as "ignored" with a reason,
    // so they won't cause the overall test run to fail.
    Harness::with_env().discover(tests).main();
}
