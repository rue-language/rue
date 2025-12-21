use libtest_mimic::{Arguments, Failed, Trial};
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

    // Convert to trials
    let tests: Vec<Trial> = specs
        .into_iter()
        .flat_map(|(_, spec)| {
            let section_id = spec.section.id.clone();
            let rue_binary = rue_binary.clone();

            spec.case.into_iter().map(move |case| {
                let test_name = format!("{}::{}", section_id, case.name);
                let skip = case.skip;
                let rue_binary = rue_binary.clone();

                let mut trial =
                    Trial::test(test_name, move || run_case_wrapper(&case, &rue_binary));

                if skip {
                    trial = trial.with_ignored_flag(true);
                }

                trial
            })
        })
        .collect();

    if tests.is_empty() {
        eprintln!("Warning: No test cases found in {}", cases_dir.display());
        eprintln!("Make sure spec files exist and have the correct format.");
    }

    libtest_mimic::run(&args, tests).exit();
}
