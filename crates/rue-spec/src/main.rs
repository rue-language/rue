use libtest_mimic::{Arguments, Conclusion, Failed, Trial};
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

/// Wrapper to convert TestResult to libtest_mimic's Failed type.
fn run_case_wrapper(case: &Case, rue_binary: &Path) -> Result<(), Failed> {
    run_test_case(case, rue_binary).map_err(|e| e.into())
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

    let args = Arguments::from_args();

    // Find the rue binary
    let rue_binary = find_rue_binary();

    // Find the cases directory
    let cases_dir = find_cases_dir();

    // Load all test files
    let specs = load_test_files(&cases_dir);

    // Separate stable and preview tests
    let mut stable_tests: Vec<Trial> = Vec::new();
    let mut preview_tests: Vec<Trial> = Vec::new();

    for (_, spec) in specs {
        let section_id = spec.section.id.clone();

        for case in spec.case {
            let test_name = format!("{}::{}", section_id, case.name);
            let skip = case.skip;
            let is_preview = case.preview.is_some();
            let rue_binary = rue_binary.clone();

            let mut trial = Trial::test(test_name, move || run_case_wrapper(&case, &rue_binary));

            if skip {
                trial = trial.with_ignored_flag(true);
            }

            if is_preview {
                preview_tests.push(trial);
            } else {
                stable_tests.push(trial);
            }
        }
    }

    if stable_tests.is_empty() && preview_tests.is_empty() {
        eprintln!("Warning: No test cases found in {}", cases_dir.display());
        eprintln!("Make sure spec files exist and have the correct format.");
    }

    // Run stable tests first - these must all pass
    let stable_conclusion = if !stable_tests.is_empty() {
        println!("\n=== Stable Tests ===\n");
        libtest_mimic::run(&args, stable_tests)
    } else {
        Conclusion {
            num_filtered_out: 0,
            num_passed: 0,
            num_failed: 0,
            num_ignored: 0,
            num_measured: 0,
        }
    };

    // Run preview tests - these are allowed to fail
    let preview_conclusion = if !preview_tests.is_empty() {
        println!("\n=== Preview Tests ===\n");
        libtest_mimic::run(&args, preview_tests)
    } else {
        Conclusion {
            num_filtered_out: 0,
            num_passed: 0,
            num_failed: 0,
            num_ignored: 0,
            num_measured: 0,
        }
    };

    // Print summary
    println!("\n=== Summary ===\n");
    println!(
        "Stable:  {} passed, {} failed",
        stable_conclusion.num_passed, stable_conclusion.num_failed
    );

    let preview_total = preview_conclusion.num_passed + preview_conclusion.num_failed;
    if preview_total > 0 {
        let percent = (preview_conclusion.num_passed as f64 / preview_total as f64) * 100.0;
        println!(
            "Preview: {} passed, {} failed ({:.0}%)",
            preview_conclusion.num_passed, preview_conclusion.num_failed, percent
        );
    }

    // Exit with error only if stable tests failed
    // Preview test failures are allowed
    if stable_conclusion.num_failed > 0 {
        println!("\nResult: FAILED (stable tests failed)");
        std::process::exit(1);
    } else {
        println!("\nResult: PASSED");
        std::process::exit(0);
    }
}
