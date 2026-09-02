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
//! # Filter tests by pattern (matches `section::case` test names)
//! ./buck2 run //crates/rue-spec:rue-spec -- "arithmetic"
//!
//! # Filter by specification section or paragraph
//! ./buck2 run //crates/rue-spec:rue-spec -- 4.2
//! ./buck2 run //crates/rue-spec:rue-spec -- --spec 4.2:5
//! ```
//!
//! An argument shaped like a specification ID (`4.2`, `4.3a`, `4.2:5`), or one
//! passed with `--spec`, selects the cases citing it rather than being matched
//! against test names. A selector that matches no case is an error, not an
//! empty pass.
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
//! - `RUE_PLATFORM_CASE_SELECTION=native` - register only declaratively
//!   platform-scoped cases applicable to the current host
//!
//! Cases that intentionally exercise the repository standard library opt in
//! with `real_std = true`. Other cases remain isolated from `RUE_STD_PATH`.

use libtest2_mimic::{Harness, RunContext, RunError, Trial};
use rue_test_runner::{
    Case, ExpectedFailureOutcome, PlatformCaseSelection, TestResult, classify_expected_failure,
    find_dir, find_rue_binary, load_test_files, run_test_case, should_skip_for_platform,
    validate_nonempty_case_corpus,
};
use std::path::Path;

mod machine_index;
mod traceability;

/// Possible paths for the spec directory.
const SPEC_DIR_PATHS: &[&str] = &["docs/spec/src", "../docs/spec/src", "../../docs/spec/src"];

/// Possible paths for the cases directory.
const CASES_DIR_PATHS: &[&str] = &["crates/rue-spec/cases", "cases", "../rue-spec/cases"];

/// Run the traceability report.
///
/// `json` prints only the machine-readable summary and skips the gate's exit
/// code: the website build asks for the numbers, and a coverage gap is the
/// traceability gate's business to fail, not the site build's.
fn run_traceability(detailed: bool, json: bool) {
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

    let report = traceability::generate_report(&spec_dir, &cases_dir).unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(1);
    });

    if json {
        report.print_summary_json();
        return;
    }

    if detailed {
        report.print_detailed();
    } else {
        report.print_summary();
    }

    // Exit with error if the gate is failing: an *unexpected* uncovered
    // normative paragraph (one not on the known-gap allowlist), a stale
    // allowlist entry, an orphan reference, or a behavior-asserting case whose
    // citations are all non-normative. Rules whose only tests are
    // skipped/preview-allowed-to-fail no longer count as coverage (RUE-132);
    // the ones tracked in KNOWN_UNCOVERED_NORMATIVE are reported but don't fail
    // the gate. Informative paragraphs never require coverage.
    if report.gate_failing() {
        std::process::exit(1);
    }
}

fn run_machine_index(check: bool) {
    let spec_dir = find_dir("RUE_SPEC_DIR", SPEC_DIR_PATHS, "docs/spec/src");
    let cases_dir = find_dir("RUE_SPEC_CASES", CASES_DIR_PATHS, "cases");
    let bytes = machine_index::generate(&spec_dir, &cases_dir).unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(1);
    });
    if check {
        let reproduced = machine_index::generate(&spec_dir, &cases_dir).unwrap_or_else(|error| {
            eprintln!("error: failed to reproduce machine index: {error}");
            std::process::exit(1);
        });
        if bytes != reproduced {
            eprintln!("error: machine index bytes are not reproducible");
            std::process::exit(1);
        }
        return;
    }
    use std::io::Write;
    std::io::stdout().write_all(&bytes).unwrap_or_else(|error| {
        eprintln!("error: failed to write machine index: {error}");
        std::process::exit(1);
    });
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

/// libtest flags that consume the argument after them. A paragraph-shaped
/// value belonging to one of these is that flag's value, not a selector.
const VALUE_TAKING_FLAGS: &[&str] = &[
    "--skip",
    "--format",
    "--test-threads",
    "--logfile",
    "--color",
    "-Z",
];

/// Whether `argument` has the shape of a specification selector: a section
/// (`4.2`, `4.3a`) or a single paragraph (`4.2:5`, `4.3:3a`). Paragraph
/// components use the traceability ID grammar's ASCII-alphanumeric alphabet,
/// with a leading digit required to keep malformed values out of libtest.
///
/// libtest filters match *test names* (`section.id::case_name`), which never
/// contain paragraph IDs — so before RUE-1161 the documented
/// `scripts/rue spec 4.2` silently selected zero cases and reported a pass.
fn is_spec_selector(argument: &str) -> bool {
    let (section, paragraph) = match argument.split_once(':') {
        Some((section, paragraph)) => (section, Some(paragraph)),
        None => (argument, None),
    };
    let Some((chapter, subsection)) = section.split_once('.') else {
        return false;
    };
    let chapter_ok = !chapter.is_empty() && chapter.bytes().all(|b| b.is_ascii_digit());
    let subsection_ok = !subsection.is_empty()
        && subsection
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        && subsection.starts_with(|c: char| c.is_ascii_digit());
    let paragraph_ok = paragraph.is_none_or(|p| {
        !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_alphanumeric())
            && p.starts_with(|c: char| c.is_ascii_digit())
    });
    chapter_ok && subsection_ok && paragraph_ok
}

/// Whether a case citing `spec_ids` is selected by `selector`.
///
/// A bare section selector (`4.2`) matches every paragraph in that section; a
/// full paragraph selector (`4.2:5`) matches only that paragraph.
fn selector_matches(selector: &str, spec_ids: &[String]) -> bool {
    if selector.contains(':') {
        return spec_ids.iter().any(|id| id == selector);
    }
    spec_ids.iter().any(|id| {
        id.split_once(':')
            .is_some_and(|(section, _)| section == selector)
    })
}

/// Split raw argv into specification selectors and the arguments the libtest
/// harness should still parse.
fn partition_spec_selectors(raw_args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut selectors = Vec::new();
    let mut harness_args = Vec::new();
    let mut arguments = raw_args.iter();

    if let Some(program) = arguments.next() {
        harness_args.push(program.clone());
    }

    let mut previous: Option<&str> = None;
    let mut expecting_selector = false;
    for argument in arguments {
        if expecting_selector {
            selectors.push(argument.clone());
            expecting_selector = false;
            previous = None;
            continue;
        }
        if argument == "--spec" {
            expecting_selector = true;
            continue;
        }
        if let Some(value) = argument.strip_prefix("--spec=") {
            selectors.push(value.to_string());
            previous = None;
            continue;
        }
        let is_flag_value = previous.is_some_and(|flag| VALUE_TAKING_FLAGS.contains(&flag));
        if !is_flag_value && is_spec_selector(argument) {
            selectors.push(argument.clone());
            previous = None;
            continue;
        }
        previous = Some(argument.as_str());
        harness_args.push(argument.clone());
    }

    (selectors, harness_args)
}

#[derive(Debug, PartialEq, Eq)]
enum PreviewDisposition {
    Ignore(String),
    Fail(String),
}

fn preview_disposition(result: TestResult) -> PreviewDisposition {
    match classify_expected_failure(result) {
        ExpectedFailureOutcome::ExpectedFailure(error) => {
            PreviewDisposition::Ignore(format!("preview test failed (allowed): {}", error))
        }
        ExpectedFailureOutcome::FatalFailure(error) => PreviewDisposition::Fail(error.to_string()),
        ExpectedFailureOutcome::UnexpectedPass => PreviewDisposition::Fail(
            "preview test PASSED without preview_should_pass = true. Add that marker so this \
             assertion becomes required coverage."
                .to_string(),
        ),
    }
}

/// Wrapper giving preview cases xfail semantics without hiding fatal failures.
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
    match preview_disposition(run_test_case(case, rue_binary)) {
        PreviewDisposition::Ignore(reason) => ctx.ignore_for(reason),
        PreviewDisposition::Fail(reason) => Err(RunError::fail(reason)),
    }
}

fn main() {
    // Check for traceability flag before parsing libtest args
    let raw_args: Vec<String> = std::env::args().collect();

    if raw_args
        .iter()
        .any(|a| matches!(a.as_str(), "--machine-index" | "--check-machine-index"))
    {
        run_machine_index(raw_args.iter().any(|a| a == "--check-machine-index"));
        return;
    }

    if raw_args.iter().any(|a| a == "--traceability") {
        let detailed = raw_args.iter().any(|a| a == "--detailed");
        let json = raw_args.iter().any(|a| a == "--json");
        run_traceability(detailed, json);
        return;
    }

    if raw_args.iter().any(|a| a == "--help-traceability") {
        println!("Traceability Report Options:");
        println!();
        println!("  --traceability     Generate spec coverage report");
        println!("  --detailed         Show detailed traceability matrix");
        println!("  --json             Print the headline figures as JSON and exit 0");
        println!();
        println!("Environment Variables:");
        println!("  RUE_SPEC_DIR       Path to spec markdown files (default: docs/spec/src)");
        println!("  RUE_SPEC_CASES     Path to test case files (default: crates/rue-spec/cases)");
        return;
    }

    let (spec_selectors, harness_args) = partition_spec_selectors(&raw_args);

    let platform_selection = PlatformCaseSelection::from_env().unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(2);
    });

    // Find the rue binary
    let rue_binary = find_rue_binary();

    // Find the cases directory
    let cases_dir = find_dir("RUE_SPEC_CASES", CASES_DIR_PATHS, "cases");

    // Load all test files
    let specs = load_test_files(&cases_dir).unwrap_or_else(|error| {
        eprintln!("error: {error}");
        std::process::exit(1);
    });

    // Build test trials, separating stable and preview tests
    // Pre-allocate based on total case count across all specs
    let total_cases: usize = specs.iter().map(|(_, s)| s.case.len()).sum();
    if let Err(error) = validate_nonempty_case_corpus(&cases_dir, total_cases, "spec") {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
    let mut tests: Vec<Trial> = Vec::with_capacity(total_cases);

    for (_, spec) in specs {
        let section_id = spec.section.id.clone();

        for case in spec.case {
            if !platform_selection.includes(&case.only_on) {
                continue;
            }
            if !spec_selectors.is_empty()
                && !spec_selectors
                    .iter()
                    .any(|selector| selector_matches(selector, &case.spec))
            {
                continue;
            }
            let test_name = format!("{}::{}", section_id, case.name);
            let skip = case.skip;
            let is_preview = case.preview.is_some();
            let preview_should_pass = case.preview_should_pass;
            let rue_binary = rue_binary.clone();

            // Preview tests that should pass use the normal wrapper (fail on error).
            // Other preview tests use xfail semantics: an ordinary failure is
            // ignored, while a fatal failure or unexpected pass fails.
            // Non-preview tests always use the normal wrapper.
            let trial = if is_preview && !preview_should_pass {
                // Preview tests expected to fail until their marker is updated.
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
        if spec_selectors.is_empty() {
            eprintln!(
                "error: platform case selection {platform_selection:?} selected no spec cases on {}",
                rue_test_runner::get_host_target()
            );
        } else {
            // A filter that matches nothing must not report a pass: that is how
            // a mistyped paragraph ID turns into false evidence that a rule is
            // exercised (RUE-1161).
            eprintln!(
                "error: no spec case cites {} (selection {platform_selection:?} on {})",
                spec_selectors.join(", "),
                rue_test_runner::get_host_target()
            );
        }
        std::process::exit(1);
    }

    // Run all tests
    //
    // Preview tests without `preview_should_pass` are expected to fail -
    // ordinary failures are ignored, but fatal failures and XPASS fail.
    //
    // Preview tests with `preview_should_pass = true` fail normally,
    // providing real test output for implemented portions of preview features.
    Harness::with_args(harness_args).discover(tests).main();
}

#[cfg(test)]
mod runner_tests {
    use super::*;
    use rue_test_runner::TestFailure;

    fn selectors(args: &[&str]) -> Vec<String> {
        let raw: Vec<String> = std::iter::once("rue-spec")
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect();
        partition_spec_selectors(&raw).0
    }

    fn harness_args(args: &[&str]) -> Vec<String> {
        let raw: Vec<String> = std::iter::once("rue-spec")
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect();
        partition_spec_selectors(&raw).1
    }

    #[test]
    fn spec_selector_shape_matches_section_and_paragraph_ids() {
        assert!(is_spec_selector("4.2"));
        assert!(is_spec_selector("4.3a"));
        assert!(is_spec_selector("4.2:5"));
        assert!(is_spec_selector("11.10:123"));
        assert!(is_spec_selector("4.3:3a"));
        assert!(is_spec_selector("4.3:30ABC"));

        assert!(!is_spec_selector("arithmetic"));
        assert!(!is_spec_selector("expressions.arithmetic"));
        assert!(!is_spec_selector("--quiet"));
        assert!(!is_spec_selector("4."));
        assert!(!is_spec_selector("4.2:"));
        assert!(!is_spec_selector("4.2:x"));
        assert!(!is_spec_selector("4.2:3_"));
        assert!(!is_spec_selector("4.2:3-"));
        assert!(!is_spec_selector("4.2:3:4"));
        assert!(!is_spec_selector("4.2:3a:"));
    }

    #[test]
    fn paragraph_selectors_are_split_out_of_the_harness_arguments() {
        assert_eq!(selectors(&["--quiet", "4.2"]), vec!["4.2".to_string()]);
        assert_eq!(
            harness_args(&["--quiet", "4.2"]),
            vec!["rue-spec".to_string(), "--quiet".to_string()]
        );

        assert_eq!(selectors(&["--spec", "4.2:5"]), vec!["4.2:5".to_string()]);
        assert_eq!(selectors(&["--spec=4.2:5"]), vec!["4.2:5".to_string()]);
        assert_eq!(selectors(&["4.3:3a"]), vec!["4.3:3a".to_string()]);
        assert_eq!(harness_args(&["4.3:3a"]), vec!["rue-spec".to_string()]);

        // A name filter still reaches libtest untouched.
        assert!(selectors(&["arithmetic"]).is_empty());
        assert_eq!(
            harness_args(&["arithmetic"]),
            vec!["rue-spec".to_string(), "arithmetic".to_string()]
        );

        // A paragraph-shaped value belonging to a flag stays that flag's value.
        assert!(selectors(&["--skip", "4.2"]).is_empty());
        assert_eq!(
            harness_args(&["--skip", "4.2"]),
            vec![
                "rue-spec".to_string(),
                "--skip".to_string(),
                "4.2".to_string()
            ]
        );
    }

    #[test]
    fn section_selectors_match_every_paragraph_in_the_section() {
        let cited = vec!["4.2:5".to_string(), "9.1:2".to_string()];

        assert!(selector_matches("4.2", &cited));
        assert!(selector_matches("9.1", &cited));
        assert!(selector_matches("4.2:5", &cited));

        assert!(!selector_matches("4.2:6", &cited));
        assert!(!selector_matches("4.20", &cited));
        assert!(!selector_matches("4.1", &cited));
        assert!(!selector_matches("4.2", &[]));
    }

    #[test]
    fn preview_disposition_rejects_xpass_and_fatal_failure() {
        let xpass = preview_disposition(Ok(()));
        assert!(matches!(
            xpass,
            PreviewDisposition::Fail(message) if message.contains("preview_should_pass")
        ));

        let ordinary = preview_disposition(Err(TestFailure::assertion("not implemented")));
        assert!(matches!(ordinary, PreviewDisposition::Ignore(_)));

        let fatal = preview_disposition(Err(TestFailure::fatal("compiler timed out")));
        assert_eq!(
            fatal,
            PreviewDisposition::Fail("compiler timed out".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn preview_disposition_rejects_fake_compiler_panic() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary fake compiler directory");
        let binary = directory.path().join("rue");
        std::fs::write(
            &binary,
            "#!/bin/sh\nprintf 'panicked at fake preview compiler' >&2\nexit 101\n",
        )
        .expect("write fake compiler");
        let mut permissions = std::fs::metadata(&binary)
            .expect("fake compiler metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).expect("make fake compiler executable");

        let case = Case {
            name: "preview_panic".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            preview: Some("test_infra".to_string()),
            ..Default::default()
        };
        let disposition = preview_disposition(run_test_case(&case, &binary));

        assert!(matches!(
            disposition,
            PreviewDisposition::Fail(message) if message.contains("INTERNAL COMPILER ERROR")
        ));
    }
}
