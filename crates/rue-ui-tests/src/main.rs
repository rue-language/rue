//! UI tests for Rue compiler features.
//!
//! These tests verify compiler behavior that is not part of the language specification,
//! such as warnings, diagnostics quality, and compiler flags.

use libtest_mimic::{Arguments, Failed, Trial};
use rue_test_runner::{Case, find_rue_binary, load_test_files, run_test_case};
use std::path::{Path, PathBuf};

/// Find the cases directory for UI tests.
fn find_cases_dir() -> PathBuf {
    std::env::var("RUE_UI_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let possible_paths = [
                "crates/rue-ui-tests/cases",
                "cases",
                "../rue-ui-tests/cases",
            ];
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            Path::new("cases").to_path_buf()
        })
}

/// Wrapper to convert TestResult to libtest_mimic's Failed type.
fn run_case_wrapper(case: &Case, rue_binary: &Path) -> Result<(), Failed> {
    run_test_case(case, rue_binary).map_err(|e| e.into())
}

fn main() {
    let args = Arguments::from_args();

    // Find the rue binary
    let rue_binary = find_rue_binary();

    // Find the cases directory
    let cases_dir = find_cases_dir();

    // Load all test files
    let test_files = load_test_files(&cases_dir);

    // Convert to trials
    let tests: Vec<Trial> = test_files
        .into_iter()
        .flat_map(|(_, test_file)| {
            let section_id = test_file.section.id.clone();
            let rue_binary = rue_binary.clone();

            test_file.case.into_iter().map(move |case| {
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
        eprintln!("Warning: No UI test cases found in {}", cases_dir.display());
        eprintln!("Make sure test files exist and have the correct format.");
    }

    libtest_mimic::run(&args, tests).exit();
}
