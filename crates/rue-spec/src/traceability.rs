//! Traceability report generator for the Rue language specification.
//!
//! This module provides tools to analyze the relationship between the Rue language
//! specification (in `docs/spec/src/`) and the test suite (in `crates/rue-spec/cases/`).
//! It ensures that all normative specification requirements have corresponding tests.
//!
//! # Overview
//!
//! The traceability system works by:
//! 1. Parsing specification paragraphs from markdown files (marked with Zola shortcodes)
//! 2. Parsing test cases from TOML files (with `spec = [...]` references)
//! 3. Generating a coverage report showing which paragraphs are tested
//!
//! # Specification Format
//!
//! Specification paragraphs are marked using Zola shortcodes:
//!
//! ```markdown
//! {{ rule(id="3.1:5", cat="normative") }}
//! The `i32` type represents a 32-bit signed integer.
//! ```
//!
//! # Test Case Format
//!
//! Test cases reference specification paragraphs using the `spec` field:
//!
//! ```toml
//! [[case]]
//! name = "i32_literal"
//! spec = ["3.1:5"]
//! source = "fn main() -> i32 { 42 }"
//! exit_code = 42
//! ```
//!
//! # Usage
//!
//! The main entry point is [`generate_report`], which produces a [`TraceabilityReport`]:
//!
//! ```ignore
//! use std::path::Path;
//! use rue_spec::traceability::generate_report;
//!
//! let report = generate_report(
//!     Path::new("docs/spec/src"),
//!     Path::new("crates/rue-spec/cases"),
//! );
//!
//! // Print a summary to stdout
//! report.print_summary();
//!
//! // Check if all normative paragraphs are covered
//! if report.normative_uncovered_count() > 0 {
//!     eprintln!("Missing test coverage!");
//! }
//! ```

use rue_test_runner::{
    CI_EXECUTED_TARGETS, PlatformResponsibility, classify_platform_responsibility, discover_files,
    load_test_files, runs_on_required_ci,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Truncate a string to at most `max_chars` characters, appending "..." if truncated.
/// This is safe for UTF-8 strings as it counts characters, not bytes.
fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        // Take max_chars - 3 characters to leave room for "..."
        let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

/// A paragraph from the Rue language specification.
///
/// Each paragraph in the specification is identified by a unique ID in the format
/// `chapter.section:paragraph` (e.g., "3.1:5" for chapter 3, section 1, paragraph 5).
/// Paragraphs are categorized to distinguish normative requirements from informative
/// content.
///
/// # Categories
///
/// - `normative` - General normative rules that require test coverage
/// - `legality-rule` - Compile-time requirements (normative)
/// - `dynamic-semantics` - Runtime behavior (normative)
/// - `syntax` - Grammar rules (normative)
/// - `undefined-behavior` - UB conditions (normative)
/// - `example` - Code examples (informative)
/// - `informative` - Explanatory text (informative, default)
#[derive(Debug, Clone)]
pub struct SpecParagraph {
    /// Paragraph ID in the format `chapter.section:paragraph` (e.g., "3.1:5").
    pub id: String,
    /// Category of the paragraph (e.g., "legality-rule", "dynamic-semantics").
    /// Normative categories require test coverage.
    pub category: String,
    /// Nearest enclosing Markdown heading, used as the stable rule title.
    pub title: String,
    /// Path relative to the specification source root.
    pub source_path: String,
    /// The text content of the paragraph (first non-empty line after the marker).
    pub text: String,
}

/// A reference from a test case to specification paragraphs.
///
/// This struct tracks which tests cover which specification paragraphs,
/// enabling traceability between the test suite and the language specification.
#[derive(Debug, Clone)]
pub struct TestReference {
    /// Full test name in the format `section::case_name`
    /// (e.g., "lexical.comments::line_comment_after_code").
    pub test_name: String,
    /// Lines of Rue source the case compiles. Used by the focused-case gate:
    /// a normative rule whose only evidence is a large program is coverage on
    /// paper, but nothing in the failure output points at the rule.
    pub source_lines: usize,
}

/// A behavior-asserting spec case whose citations are all non-normative.
///
/// Informative and example paragraphs are useful documentation, but they do
/// not establish a language requirement. Keeping this finding in the report
/// makes a case which claims executable behavior without a normative anchor
/// visible to both humans and machine consumers of the traceability report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonNormativeCase {
    /// Full test name in the format `section::case_name`.
    pub test_name: String,
    /// Every cited paragraph and its parsed category, in citation order.
    pub citations: Vec<(String, String)>,
}

/// A complete traceability report linking specification paragraphs to tests.
///
/// This report provides:
/// - A list of all specification paragraphs (both normative and informative)
/// - Coverage information showing which tests reference each paragraph
/// - Detection of orphan references (tests that reference non-existent paragraphs)
///
/// The report distinguishes between normative paragraphs (which require test coverage)
/// and informative paragraphs (which do not). Use [`TraceabilityReport::normative_coverage_percentage`]
/// to check the coverage of normative paragraphs specifically.
///
/// # Example
///
/// ```ignore
/// let report = generate_report(&spec_dir, &cases_dir);
/// if report.normative_uncovered_count() > 0 {
///     report.print_summary();
/// }
/// ```
#[derive(Debug)]
pub struct TraceabilityReport {
    /// All specification paragraphs, keyed by paragraph ID (e.g., "3.1:5").
    pub paragraphs: BTreeMap<String, SpecParagraph>,
    /// Tests covering each paragraph ID. Empty vectors indicate uncovered paragraphs.
    pub coverage: BTreeMap<String, Vec<TestReference>>,
    /// Test references that don't match any existing paragraph.
    /// Each entry is a tuple of (test_name, invalid_reference_id).
    pub orphan_references: Vec<(String, String)>,
    /// Rule ids defined more than once in the spec. Duplicates fail the gate:
    /// with last-writer-wins parsing, a later duplicate can silently replace a
    /// normative rule (erasing its coverage requirement) — as nearly happened
    /// with 3.7:49 on 2026-07-04.
    pub duplicate_rule_ids: Vec<String>,
    /// How many cases each lane is responsible for executing.
    pub responsibility_census: ResponsibilityCensus,
    /// Cases excluded from coverage because their `only_on` scope names no
    /// platform any required CI lane executes. Each entry is
    /// `(test_name, only_on)`. Reported so the exclusion is visible rather than
    /// silent; a rule left uncovered by it fails the gate the ordinary way.
    pub platform_unreachable_cases: Vec<(String, Vec<String>)>,
    /// Behavior-asserting cases whose citations are all informative/example. These
    /// fail the gate unless repaired with a normative citation.
    pub non_normative_cases: Vec<NonNormativeCase>,
}

/// The headline figures of a [`TraceabilityReport`], for machine consumers.
///
/// Deliberately carries the two sides of every ratio rather than a percentage:
/// a consumer that receives only "how many normative rules exist" can do
/// nothing but assert they are all covered, which is what the homepage did
/// before RUE-1261 and why it reported 100% while three rules were uncovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceabilitySummary {
    /// Paragraphs in a normative category — those that require coverage.
    pub normative_total: usize,
    /// Normative paragraphs with at least one test.
    pub normative_covered: usize,
    /// Normative paragraphs with no test.
    pub normative_uncovered: usize,
    /// Of the uncovered, how many are allowlisted with a written reason.
    pub known_uncovered: usize,
    /// Every paragraph, normative and informative.
    pub paragraphs_total: usize,
    /// Every covered paragraph, normative and informative. Against
    /// `paragraphs_total` this is diluted by informative prose that is not
    /// meant to be traced, so it is not a quality measure — publish the
    /// normative ratio instead.
    pub paragraphs_covered: usize,
    /// Executable cases after parameter expansion.
    pub cases: usize,
    /// Target-independent cases, owned by the Linux-complete lane.
    pub cases_semantic: usize,
    /// Cases that build and run for the executing host's target.
    pub cases_native: usize,
    /// Cases pinning architecture-specific output for a declared target.
    pub cases_backend: usize,
    /// Cases no required CI lane can execute, excluded from coverage.
    pub platform_unreachable_cases: usize,
    /// Per-category totals, keyed by category name.
    pub categories: BTreeMap<String, CategoryCoverage>,
}

/// One category's share of [`TraceabilitySummary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategoryCoverage {
    pub total: usize,
    pub covered: usize,
    /// Whether this category requires coverage.
    pub normative: bool,
}

/// How the corpus divides across the lanes that execute it.
///
/// Reported so the platform split is a visible fact of every traceability run
/// rather than an implicit property of the workflow file.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResponsibilityCensus {
    /// Target-independent cases, owned by the Linux-complete lane.
    pub semantic: usize,
    /// Cases that build and run a program for the executing host's target.
    pub native: usize,
    /// Cases pinning architecture-specific output for a declared `target`.
    pub backend: usize,
}

impl ResponsibilityCensus {
    fn record(&mut self, responsibility: PlatformResponsibility) {
        match responsibility {
            PlatformResponsibility::Semantic => self.semantic += 1,
            PlatformResponsibility::Native => self.native += 1,
            PlatformResponsibility::Backend => self.backend += 1,
        }
    }
}

/// The largest program a spec case may use and still count as *focused*
/// evidence for a normative rule.
///
/// A rule whose only coverage is a large multi-feature program is covered on
/// paper only: when it regresses, the failure names the program rather than the
/// rule, and the case usually fails for one of the dozen other rules it also
/// touches first. Large programs remain valuable as integration and slow-tier
/// coverage — they just cannot be a normative rule's sole evidence (RUE-1161).
/// The largest case in the corpus today is 28 lines.
pub const FOCUSED_CASE_MAX_SOURCE_LINES: usize = 40;

/// Normative paragraphs that currently have no *running* test, tracked as known
/// gaps — either pending compiler-feature implementation, or not reachable by a
/// test at all.
///
/// Each entry is `(paragraph_id, reason)`. Most are rules whose only spec tests
/// are `skip = true` because the behavior they pin isn't implemented yet — the
/// tests are written and ready to un-skip the moment the feature lands. A few
/// pin something no test can positively exercise: undefined behavior, a
/// boundary that needs a harness the spec corpus cannot build, or a limit whose
/// counterexample is too large to construct. Both kinds belong here for the
/// same reason — the gate stays green while the report names the gap instead of
/// counting it as covered. Neither kind is a place to park a rule that is
/// merely inconvenient to test. Before
/// RUE-132, such a skipped test silently *counted* as coverage, so the report
/// falsely claimed 100%. Now skipped tests don't count, and these genuine gaps
/// are listed here explicitly: the gate stays green while the report tells the
/// truth (the rules are shown as known-uncovered, not as covered).
///
/// **Maintenance:** when a feature ships and its test un-skips, the rule regains
/// real coverage and its entry here becomes stale — the gate will fail and tell
/// you to remove it (see [`TraceabilityReport::stale_known_uncovered`]). This
/// mirrors the `known_bug` xfail convention: an exemption that starts passing
/// must be retired so it converts back into an enforced check.
pub const KNOWN_UNCOVERED_NORMATIVE: &[(&str, &str)] = &[
    // ADR-0064 P4 (RUE-1058) foreign-boundary rules. These are normative but not
    // coverable by the standalone spec corpus: the abort proof needs a C caller
    // linked to a Rue export (a harness only the preview-gated `c_ffi` CLI suite
    // provides), and the other two describe undefined / programmer-responsibility
    // behavior that is not positively testable.
    // C.3:1's counterexample is a source file one byte under 4 GiB. Constructing
    // it is not a feature gap — the check is implemented and rejects with E1401
    // — but a 4 GiB fixture cannot live in the corpus or run in a CI lane.
    (
        "C.3:1",
        "Maximum source-file length (4,294,967,295 bytes, rejected with E1401 \
         before lexing): positively testing the rejection needs a source file of \
         that size, which cannot be committed to the corpus or generated within a \
         test lane's time and disk budget. The sibling limit C.3:2 (file count) \
         has the same shape. The check itself is implemented and runs on every \
         source the compiler accepts.",
    ),
    (
        "9.3:2",
        "Abort-at-boundary for a trapping `pub extern \"C\" fn` export: verified by \
         execution in the preview-gated c_ffi CLI suite \
         (crates/rue-cli-tests, `*_trapping_export_aborts_at_boundary`), which links \
         a C caller to a Rue export — a setup the standalone spec corpus cannot build.",
    ),
    (
        "9.3:3",
        "Reverse-direction undefined behavior (a foreign exception or `longjmp` \
         crossing a Rue frame): undefined behavior is not positively testable; it is \
         documented as the mirror of the abort-at-boundary rule (9.3:2).",
    ),
    (
        "9.3:4",
        "Ownership/linear-move across the C boundary: a by-value linear or \
         destructor-bearing crossing is a hard error (not FFI-safe), so the \
         move-without-destructor rule governs `@raw`/`@raw_mut` pointer escapes under \
         ADR-0028 programmer responsibility, which is not positively testable.",
    ),
    // The test-body `?` rules (ADR-0083 §1). Every one of them is about a test
    // body, and 6.7:9 is the reason this corpus cannot reach one: a spec case is
    // an executable request, and an executable request never analyzes, lowers,
    // or runs a test body. Observing these rules needs a *test* request and a
    // dispatched process, which is what the rue-compiler suites below build.
    (
        "6.7:13",
        "`?` legality inside a test body: a spec case is an executable request, \
         which by 6.7:9 never analyzes a test body, so this corpus cannot observe \
         acceptance or rejection there at all. Covered by the rue-compiler session \
         tests in `crates/rue-compiler/src/test_body_try_tests.rs`, which lower the \
         same programs under `RootSelection::Tests`: `?` on a trusted `Option` and \
         on a trusted `Result` analyzes, a `()`-returning helper the test calls \
         still reports E0503/E0505, and a same-shape lookalike still reports \
         E0504.",
    ),
    (
        "6.7:14",
        "The failure arm's dynamic semantics: reaching them needs a linked test \
         image and a dispatched process, which the spec corpus does not build. \
         Covered end to end by \
         `crates/rue-compiler/src/test_image_tests.rs::platform_native_a_failing_test_body_question_reports_and_traps` \
         (exit 101, `panic: unhandled error` on stderr, one `unhandled_error` frame \
         naming the start of the `?` operand) and \
         `..._a_succeeding_test_body_question_completes_normally`.",
    ),
    (
        "6.7:15",
        "The rendered payload: the rendering is carried on the run's failure \
         channel, which only a dispatched test process writes. Covered by \
         `crates/rue-compiler/src/test_image_tests.rs::platform_native_the_reported_payload_is_the_rendered_error`, \
         which pins one shape per rule — `None`, a unit variant, a payload variant \
         carrying a negative integer and a byte string, and a struct — and by the \
         session test that proves one printer serves every site on an error type.",
    ),
    (
        "6.7:16",
        "Destructors skipped on the failing path: an absence, observable only by \
         running a test whose live local has a `drop fn` and seeing its work not \
         happen. Covered by \
         `crates/rue-compiler/src/test_image_tests.rs::platform_native_a_failing_question_skips_a_live_local_destructor`.",
    ),
];

fn format_non_normative_case(case: &NonNormativeCase) -> String {
    let citations = case
        .citations
        .iter()
        .map(|(id, category)| format!("{id} [{category}]"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} cites only {}", case.test_name, citations)
}

impl TraceabilityReport {
    /// Whether `id` is on the [`KNOWN_UNCOVERED_NORMATIVE`] allowlist.
    fn is_known_uncovered(id: &str) -> bool {
        KNOWN_UNCOVERED_NORMATIVE.iter().any(|(k, _)| *k == id)
    }

    /// Check if a paragraph is normative (requires test coverage).
    /// Normative categories: normative, legality-rule, dynamic-semantics, syntax, undefined-behavior
    fn is_normative(para: &SpecParagraph) -> bool {
        matches!(
            para.category.as_str(),
            "normative" | "legality-rule" | "dynamic-semantics" | "syntax" | "undefined-behavior"
        )
    }

    /// Returns the total count of normative paragraphs in the specification.
    ///
    /// Normative paragraphs are those that define required behavior and must have
    /// test coverage. This includes categories: `normative`, `legality-rule`,
    /// `dynamic-semantics`, `syntax`, and `undefined-behavior`.
    pub fn normative_count(&self) -> usize {
        self.paragraphs
            .values()
            .filter(|p| Self::is_normative(p))
            .count()
    }

    /// Returns the count of normative paragraphs that have at least one test.
    pub fn normative_covered_count(&self) -> usize {
        self.paragraphs
            .values()
            .filter(|p| {
                Self::is_normative(p)
                    && self
                        .coverage
                        .get(&p.id)
                        .map(|tests| !tests.is_empty())
                        .unwrap_or(false)
            })
            .count()
    }

    /// Returns the count of normative paragraphs that have no tests.
    ///
    /// This is the primary metric for determining if the test suite is complete.
    /// A value greater than zero indicates missing test coverage.
    pub fn normative_uncovered_count(&self) -> usize {
        self.normative_count() - self.normative_covered_count()
    }

    /// Returns the IDs of normative paragraphs that have no tests.
    ///
    /// Use this to identify which specification requirements still need test coverage.
    pub fn uncovered_normative_paragraphs(&self) -> Vec<&String> {
        self.paragraphs
            .iter()
            .filter(|(_, para)| {
                Self::is_normative(para)
                    && self
                        .coverage
                        .get(&para.id)
                        .map(|tests| tests.is_empty())
                        .unwrap_or(true)
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Uncovered normative paragraphs that are **not** on the
    /// [`KNOWN_UNCOVERED_NORMATIVE`] allowlist.
    ///
    /// This is the set the traceability gate fails on: a genuinely-uncovered
    /// normative rule that isn't a tracked, known gap. Allowlisted rules are
    /// reported separately (see [`Self::print_summary`]) but don't fail the gate.
    pub fn unexpected_uncovered_normative_paragraphs(&self) -> Vec<&String> {
        self.uncovered_normative_paragraphs()
            .into_iter()
            .filter(|id| !Self::is_known_uncovered(id))
            .collect()
    }

    /// Stale [`KNOWN_UNCOVERED_NORMATIVE`] entries: allowlisted IDs that are no
    /// longer legitimately-uncovered normative rules — either the paragraph now
    /// has a running test (the feature shipped; un-skip happened) or the ID no
    /// longer names a normative paragraph at all.
    ///
    /// A non-empty result fails the gate, demanding the stale entry be removed —
    /// so the allowlist can never silently mask a rule that regained coverage.
    pub fn stale_known_uncovered(&self) -> Vec<&'static str> {
        KNOWN_UNCOVERED_NORMATIVE
            .iter()
            .filter(|(id, _)| {
                let is_uncovered_normative =
                    self.paragraphs.get(*id).is_some_and(Self::is_normative)
                        && self
                            .coverage
                            .get(*id)
                            .map(|tests| tests.is_empty())
                            .unwrap_or(true);
                !is_uncovered_normative
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Normative paragraphs whose every covering case exceeds
    /// [`FOCUSED_CASE_MAX_SOURCE_LINES`].
    ///
    /// Such a rule has coverage only through a large program. Each entry is
    /// `(paragraph_id, smallest_covering_case, its_source_lines)`.
    pub fn unfocused_normative_coverage(&self) -> Vec<(&String, &str, usize)> {
        self.paragraphs
            .values()
            .filter(|para| Self::is_normative(para))
            .filter_map(|para| {
                let tests = self.coverage.get(&para.id)?;
                let smallest = tests.iter().min_by_key(|test| test.source_lines)?;
                (smallest.source_lines > FOCUSED_CASE_MAX_SOURCE_LINES).then_some((
                    &para.id,
                    smallest.test_name.as_str(),
                    smallest.source_lines,
                ))
            })
            .collect()
    }

    /// Whether the traceability gate should fail: any *unexpected* uncovered
    /// normative rule, stale allowlist entry, orphan reference,
    /// duplicate rule id, unfocused normative coverage, or behavior-asserting case
    /// whose citations are all non-normative.
    pub fn gate_failing(&self) -> bool {
        !self.unexpected_uncovered_normative_paragraphs().is_empty()
            || !self.stale_known_uncovered().is_empty()
            || !self.orphan_references.is_empty()
            || !self.duplicate_rule_ids.is_empty()
            || !self.unfocused_normative_coverage().is_empty()
            || !self.non_normative_cases.is_empty()
    }

    /// Returns the coverage percentage for normative paragraphs (0.0 to 100.0).
    ///
    /// Returns 100.0 if there are no normative paragraphs.
    pub fn normative_coverage_percentage(&self) -> f64 {
        let total = self.normative_count();
        if total == 0 {
            100.0
        } else {
            (self.normative_covered_count() as f64 / total as f64) * 100.0
        }
    }

    /// Returns the count of all paragraphs (normative and informative) that have at least one test.
    pub fn covered_count(&self) -> usize {
        self.coverage
            .iter()
            .filter(|(id, tests)| self.paragraphs.contains_key(*id) && !tests.is_empty())
            .count()
    }

    /// Returns the overall coverage percentage for all paragraphs (0.0 to 100.0).
    ///
    /// This includes both normative and informative paragraphs. For the metric
    /// that matters for test suite completeness, use [`Self::normative_coverage_percentage`].
    pub fn coverage_percentage(&self) -> f64 {
        if self.paragraphs.is_empty() {
            100.0
        } else {
            (self.covered_count() as f64 / self.paragraphs.len() as f64) * 100.0
        }
    }

    /// The report's headline figures, for machine consumers.
    ///
    /// Exists so the website's status board reads the same computation the
    /// traceability gate does. The homepage previously carried a hand-written
    /// count of `rule(cat=…)` markers, which was a second implementation of
    /// this and drifted from it (RUE-1261).
    pub fn summary(&self) -> TraceabilitySummary {
        let mut categories: BTreeMap<String, CategoryCoverage> = BTreeMap::new();
        for para in self.paragraphs.values() {
            let entry = categories
                .entry(para.category.clone())
                .or_insert(CategoryCoverage {
                    total: 0,
                    covered: 0,
                    normative: Self::is_normative(para),
                });
            entry.total += 1;
            if self
                .coverage
                .get(&para.id)
                .map(|tests| !tests.is_empty())
                .unwrap_or(false)
            {
                entry.covered += 1;
            }
        }

        TraceabilitySummary {
            normative_total: self.normative_count(),
            normative_covered: self.normative_covered_count(),
            normative_uncovered: self.normative_uncovered_count(),
            // Uncovered normative paragraphs that are allowlisted with a
            // written reason, as distinct from ones nobody has looked at.
            known_uncovered: self
                .uncovered_normative_paragraphs()
                .iter()
                .filter(|id| Self::is_known_uncovered(id))
                .count(),
            paragraphs_total: self.paragraphs.len(),
            paragraphs_covered: self.covered_count(),
            // Executable cases, after the runner's parameter expansion — the
            // number of cases that actually run, not the number authored.
            cases: self.responsibility_census.semantic
                + self.responsibility_census.native
                + self.responsibility_census.backend,
            cases_semantic: self.responsibility_census.semantic,
            cases_native: self.responsibility_census.native,
            cases_backend: self.responsibility_census.backend,
            platform_unreachable_cases: self.platform_unreachable_cases.len(),
            categories,
        }
    }

    /// Print [`Self::summary`] as JSON on one line.
    ///
    /// Hand-emitted rather than serde-derived so the spec crate keeps its
    /// three dependencies. Every value is an integer or a bool, and every key
    /// is either a fixed literal or a category name — an identifier from a
    /// closed set — so there is nothing here that needs escaping.
    pub fn print_summary_json(&self) {
        let summary = self.summary();
        let categories: Vec<String> = summary
            .categories
            .iter()
            .map(|(name, coverage)| {
                format!(
                    "\"{}\":{{\"total\":{},\"covered\":{},\"normative\":{}}}",
                    name, coverage.total, coverage.covered, coverage.normative
                )
            })
            .collect();
        println!(
            concat!(
                "{{\"normative_total\":{},\"normative_covered\":{},\"normative_uncovered\":{},",
                "\"known_uncovered\":{},\"paragraphs_total\":{},\"paragraphs_covered\":{},",
                "\"cases\":{},\"cases_semantic\":{},\"cases_native\":{},\"cases_backend\":{},",
                "\"platform_unreachable_cases\":{},\"categories\":{{{}}}}}"
            ),
            summary.normative_total,
            summary.normative_covered,
            summary.normative_uncovered,
            summary.known_uncovered,
            summary.paragraphs_total,
            summary.paragraphs_covered,
            summary.cases,
            summary.cases_semantic,
            summary.cases_native,
            summary.cases_backend,
            summary.platform_unreachable_cases,
            categories.join(",")
        );
    }

    /// Prints a summary report to stdout.
    ///
    /// The summary includes:
    /// - Overall normative and total coverage percentages
    /// - Coverage breakdown by paragraph category
    /// - List of uncovered normative paragraphs (if any)
    /// - List of orphan references (if any)
    pub fn print_summary(&self) {
        println!("=== Rue Specification Traceability Report ===\n");

        // Normative coverage stats (what matters for pass/fail)
        let normative_total = self.normative_count();
        let normative_covered = self.normative_covered_count();
        let normative_pct = self.normative_coverage_percentage();

        println!(
            "Normative Coverage: {:.1}% ({}/{} paragraphs, {} uncovered)",
            normative_pct,
            normative_covered,
            normative_total,
            self.normative_uncovered_count()
        );

        // Overall stats (informative)
        let total = self.paragraphs.len();
        let covered = self.covered_count();
        let informative_count = total - normative_total;
        println!(
            "Total Coverage: {:.1}% ({}/{} paragraphs, {} informative)",
            self.coverage_percentage(),
            covered,
            total,
            informative_count
        );
        println!();

        // Count by category
        let mut by_category: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for para in self.paragraphs.values() {
            let entry = by_category.entry(&para.category).or_insert((0, 0));
            entry.0 += 1;
            if self
                .coverage
                .get(&para.id)
                .map(|t| !t.is_empty())
                .unwrap_or(false)
            {
                entry.1 += 1;
            }
        }

        println!("Coverage by category:");
        for (category, (total, covered)) in &by_category {
            let pct = if *total > 0 {
                (*covered as f64 / *total as f64) * 100.0
            } else {
                100.0
            };
            let is_normative = matches!(
                *category,
                "normative"
                    | "legality-rule"
                    | "dynamic-semantics"
                    | "syntax"
                    | "undefined-behavior"
            );
            let marker = if is_normative { "" } else { " (informative)" };
            println!(
                "  {:20} {:.1}% ({}/{}){}",
                category, pct, covered, total, marker
            );
        }
        println!();

        // Known-gap normative paragraphs: uncovered, but explicitly allowlisted
        // as pending a compiler feature (their only tests are skipped). Reported
        // honestly but not gate-failing.
        let known_gaps: Vec<&String> = self
            .uncovered_normative_paragraphs()
            .into_iter()
            .filter(|id| Self::is_known_uncovered(id))
            .collect();
        if !known_gaps.is_empty() {
            println!(
                "Known-uncovered normative paragraphs ({}, pending implementation):",
                known_gaps.len()
            );
            for id in known_gaps {
                let reason = KNOWN_UNCOVERED_NORMATIVE
                    .iter()
                    .find(|(k, _)| k == id)
                    .map(|(_, r)| *r)
                    .unwrap_or("");
                if let Some(para) = self.paragraphs.get(id) {
                    println!("  {} [{}]: {}", id, para.category, reason);
                }
            }
            println!();
        }

        // Stale allowlist entries: an allowlisted rule that regained coverage (or
        // no longer exists) must be de-listed — this fails the gate.
        let stale = self.stale_known_uncovered();
        if !stale.is_empty() {
            println!("Stale KNOWN_UNCOVERED_NORMATIVE entries ({}):", stale.len());
            for id in stale {
                println!(
                    "  {} is now covered (or not a normative paragraph) — remove it \
                     from KNOWN_UNCOVERED_NORMATIVE",
                    id
                );
            }
            println!();
        }

        // Unexpected uncovered normative paragraphs (what needs to be fixed).
        let uncovered_normative = self.unexpected_uncovered_normative_paragraphs();
        if !uncovered_normative.is_empty() {
            println!(
                "Uncovered normative paragraphs ({}):",
                uncovered_normative.len()
            );
            for id in uncovered_normative {
                if let Some(para) = self.paragraphs.get(id) {
                    let text = truncate_with_ellipsis(&para.text, 60);
                    println!("  {} [{}]: {}", id, para.category, text);
                }
            }
            println!();
        }

        // Orphan references
        if !self.orphan_references.is_empty() {
            println!(
                "Invalid spec references ({}):",
                self.orphan_references.len()
            );
            for (test_name, ref_id) in &self.orphan_references {
                println!("  {} references non-existent '{}'", test_name, ref_id);
            }
            println!();
        }

        if !self.non_normative_cases.is_empty() {
            println!(
                "Behavior-asserting cases with only non-normative citations ({}):",
                self.non_normative_cases.len()
            );
            for case in &self.non_normative_cases {
                println!("  {}", format_non_normative_case(case));
            }
            println!();
        }

        // Which lane owns executing each part of the corpus.
        let census = self.responsibility_census;
        println!(
            "Platform responsibility: {} semantic (Linux-complete lane), {} native \
             (host execution), {} backend (declared target)",
            census.semantic, census.native, census.backend
        );
        println!();

        // Cases whose platform scope names no required CI lane. They still run
        // for a developer on that host, but nothing in CI executes them, so
        // they cannot stand as evidence that a rule holds.
        if !self.platform_unreachable_cases.is_empty() {
            println!(
                "Cases not executed by any required CI lane ({}, not counted as coverage):",
                self.platform_unreachable_cases.len()
            );
            for (test_name, only_on) in &self.platform_unreachable_cases {
                println!(
                    "  {} is only_on {:?}; required lanes run {:?}",
                    test_name, only_on, CI_EXECUTED_TARGETS
                );
            }
            println!();
        }

        // Normative rules whose only evidence is a large program.
        let unfocused = self.unfocused_normative_coverage();
        if !unfocused.is_empty() {
            println!(
                "Normative paragraphs covered only through large programs ({}):",
                unfocused.len()
            );
            for (id, test_name, lines) in unfocused {
                println!(
                    "  {} smallest covering case '{}' is {} lines (budget {}) — add a \
                     focused case; the large program stays as integration coverage",
                    id, test_name, lines, FOCUSED_CASE_MAX_SOURCE_LINES
                );
            }
            println!();
        }

        // Duplicate rule ids (each silently replaced an earlier paragraph)
        if !self.duplicate_rule_ids.is_empty() {
            println!(
                "Duplicate rule ids ({}) — renumber so each id appears once:",
                self.duplicate_rule_ids.len()
            );
            for dup in &self.duplicate_rule_ids {
                println!("  {}", dup);
            }
            println!();
        }
    }

    /// Prints a detailed traceability matrix to stdout.
    ///
    /// The detailed report shows every paragraph grouped by chapter, with:
    /// - Coverage status (✓ for covered, ⚠ for uncovered)
    /// - Paragraph ID and category
    /// - Truncated paragraph text
    /// - List of tests covering each paragraph
    ///
    /// Ends with the same summary as [`Self::print_summary`].
    pub fn print_detailed(&self) {
        println!("=== Rue Specification Traceability Matrix ===\n");

        // Group paragraphs by chapter
        let mut by_chapter: BTreeMap<String, Vec<&SpecParagraph>> = BTreeMap::new();
        for para in self.paragraphs.values() {
            let chapter = para.id.split(':').next().unwrap_or(&para.id).to_string();
            by_chapter.entry(chapter).or_default().push(para);
        }

        for (chapter, paras) in &by_chapter {
            println!("Chapter {}", chapter);
            println!("{}", "-".repeat(40));

            for para in paras {
                let tests = self.coverage.get(&para.id);
                let test_count = tests.map(|t| t.len()).unwrap_or(0);

                let status = if test_count > 0 { "✓" } else { "⚠" };
                let text = truncate_with_ellipsis(&para.text, 50);

                println!("  {} {}  [{}]", status, para.id, para.category);
                println!("    {}", text);

                if let Some(tests) = tests {
                    for test in tests {
                        println!("      → {}", test.test_name);
                    }
                }
                println!();
            }
        }

        // Print summary at the end
        self.print_summary();
    }
}

/// Parse a spec marker from a line.
/// Format: {{ rule(id="X.Y:Z") }} or {{ rule(id="X.Y:Z", cat="category") }} (Zola shortcode)
/// Category can be: normative, informative, syntax, example
/// Default category (no cat) is informative — this is the spec's own rule
/// (1.3:2, restated by B.1:5), not a tooling convention: normativity is opted
/// into explicitly. Before RUE-1768, 1.3:2 said the opposite of B.1:5 and this
/// default silently picked the winner, so 239 uncategorised paragraphs sat
/// outside the coverage gate while the report claimed full coverage.
/// Returns (id, category) if found.
fn parse_spec_comment(line: &str) -> Result<Option<(String, String)>, String> {
    let line = line.trim();

    if !line
        .strip_prefix("{{")
        .is_some_and(|body| body.trim_start().starts_with("rule"))
    {
        return Ok(None);
    }
    let body = line
        .strip_prefix("{{")
        .and_then(|body| body.strip_suffix("}}"))
        .map(str::trim)
        .and_then(|body| body.strip_prefix("rule("))
        .and_then(|body| body.strip_suffix(')'))
        .ok_or_else(|| "malformed rule marker".to_string())?;

    let mut id = None;
    let mut category = None;
    for argument in body.split(',') {
        let (name, value) = argument
            .trim()
            .split_once('=')
            .ok_or_else(|| "malformed rule marker argument".to_string())?;
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .filter(|value| !value.contains('"'))
            .ok_or_else(|| {
                format!(
                    "rule marker argument `{}` must be a quoted string",
                    name.trim()
                )
            })?
            .to_string();
        let slot = match name.trim() {
            "id" => &mut id,
            "cat" => &mut category,
            other => return Err(format!("unknown rule marker argument `{other}`")),
        };
        if slot.replace(value).is_some() {
            return Err(format!("duplicate rule marker argument `{}`", name.trim()));
        }
    }

    let id = id.ok_or_else(|| "rule marker is missing `id`".to_string())?;
    let valid_id = id.split_once(':').is_some_and(|(section, paragraph)| {
        section.contains('.')
            && section
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric()))
            && !paragraph.is_empty()
            && paragraph.chars().all(|c| c.is_ascii_alphanumeric())
    });
    if !valid_id {
        return Err(format!("invalid rule id `{id}` (expected X.Y:Z)"));
    }
    let category = category.unwrap_or_else(|| "informative".to_string());
    const CATEGORIES: &[&str] = &[
        "normative",
        "legality-rule",
        "dynamic-semantics",
        "syntax",
        "undefined-behavior",
        "example",
        "informative",
    ];
    if !CATEGORIES.contains(&category.as_str()) {
        return Err(format!("unknown rule category `{category}`"));
    }
    Ok(Some((id, category)))
}

/// Check if a line is a spec marker (Zola shortcode format).
fn is_spec_marker(line: &str) -> bool {
    line.trim()
        .strip_prefix("{{")
        .is_some_and(|body| body.trim_start().starts_with("rule"))
}

/// A production declaration that must be kept identical between a normative
/// chapter grammar block and Appendix A.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GrammarSyncMarker {
    id: String,
    production: String,
    role: String,
    relation: String,
    symbol: Option<String>,
}

/// Parse the HTML-comment marker used for grammar consistency checks.
///
/// The marker is deliberately an HTML comment rather than a Zola shortcode:
/// it is metadata for the traceability gate and must not become a rendered
/// documentation feature. Its small, structured form also makes removing a
/// mirrored production fail closed instead of relying on a substring search.
fn parse_grammar_sync_marker(line: &str) -> Result<Option<GrammarSyncMarker>, String> {
    let line = line.trim();
    let Some(body) = line
        .strip_prefix("<!--")
        .and_then(|body| body.strip_suffix("-->"))
        .map(str::trim)
    else {
        return Ok(None);
    };
    if !body.starts_with("grammar-sync") {
        return Ok(None);
    }
    let body = body
        .strip_prefix("grammar-sync(")
        .and_then(|body| body.strip_suffix(')'))
        .ok_or_else(|| "malformed grammar-sync marker".to_string())?;

    let mut id = None;
    let mut production = None;
    let mut role = None;
    let mut relation = None;
    let mut symbol = None;
    for argument in body.split(',') {
        let (name, value) = argument
            .trim()
            .split_once('=')
            .ok_or_else(|| "malformed grammar-sync marker argument".to_string())?;
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .filter(|value| !value.contains('"'))
            .ok_or_else(|| {
                format!(
                    "grammar-sync marker argument `{}` must be a quoted string",
                    name.trim()
                )
            })?
            .to_string();
        let slot = match name.trim() {
            "id" => &mut id,
            "production" => &mut production,
            "role" => &mut role,
            "relation" => &mut relation,
            "symbol" => &mut symbol,
            other => return Err(format!("unknown grammar-sync marker argument `{other}`")),
        };
        if slot.replace(value).is_some() {
            return Err(format!(
                "duplicate grammar-sync marker argument `{}`",
                name.trim()
            ));
        }
    }

    let id = id.ok_or_else(|| "grammar-sync marker is missing `id`".to_string())?;
    let valid_id = id.split_once(':').is_some_and(|(section, paragraph)| {
        section.contains('.')
            && section
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric()))
            && !paragraph.is_empty()
            && paragraph.chars().all(|c| c.is_ascii_alphanumeric())
    });
    if !valid_id {
        return Err(format!(
            "invalid grammar-sync rule id `{id}` (expected X.Y:Z)"
        ));
    }
    let production =
        production.ok_or_else(|| "grammar-sync marker is missing `production`".to_string())?;
    if production.is_empty()
        || !production
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(format!("invalid grammar-sync production `{production}`"));
    }
    let role = role.ok_or_else(|| "grammar-sync marker is missing `role`".to_string())?;
    if !matches!(role.as_str(), "source" | "appendix") {
        return Err(format!("unknown grammar-sync marker role `{role}`"));
    }
    let relation = relation.unwrap_or_else(|| "exact".to_string());
    if !matches!(relation.as_str(), "exact" | "contains") {
        return Err(format!("unknown grammar-sync marker relation `{relation}`"));
    }
    if relation == "contains" && symbol.is_none() {
        return Err("grammar-sync `contains` marker is missing `symbol`".to_string());
    }
    if relation == "exact" && symbol.is_some() {
        return Err("grammar-sync `exact` marker cannot specify `symbol`".to_string());
    }
    if let Some(symbol) = &symbol
        && (symbol.is_empty()
            || !symbol
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    {
        return Err(format!("invalid grammar-sync symbol `{symbol}`"));
    }
    Ok(Some(GrammarSyncMarker {
        id,
        production,
        role,
        relation,
        symbol,
    }))
}

/// Normalize one EBNF production for comparison while preserving its syntax.
fn normalize_grammar_production(production: &str) -> String {
    production.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Find a marked production in the next EBNF fence after a marker.
fn parse_marked_grammar_production(
    lines: &[&str],
    marker_index: usize,
    production: &str,
    path: &Path,
) -> Result<String, String> {
    let mut fence = None;
    for (index, line) in lines.iter().enumerate().skip(marker_index + 1) {
        let trimmed = line.trim();
        if trimmed == "```ebnf" {
            fence = Some(index);
            break;
        }
        if trimmed.starts_with("```") {
            return Err(format!(
                "{}:{}: grammar-sync marker must precede an EBNF block",
                path.display(),
                marker_index + 1
            ));
        }
    }
    let fence = fence.ok_or_else(|| {
        format!(
            "{}:{}: grammar-sync marker has no following EBNF block",
            path.display(),
            marker_index + 1
        )
    })?;

    for (index, line) in lines.iter().enumerate().skip(fence + 1) {
        let trimmed = line.trim();
        if trimmed == "```" {
            break;
        }
        let Some((lhs, _)) = trimmed.split_once('=') else {
            continue;
        };
        if lhs.trim() != production {
            continue;
        }

        let mut declaration = trimmed.to_string();
        let mut next_index = index + 1;
        while !declaration.contains(';') {
            let next = lines.get(next_index).copied();
            let Some(next) = next else {
                break;
            };
            declaration.push(' ');
            declaration.push_str(next.trim());
            next_index += 1;
        }
        if declaration.contains(';') {
            return Ok(normalize_grammar_production(&declaration));
        }
    }

    Err(format!(
        "{}:{}: grammar-sync production `{production}` is missing from the following EBNF block",
        path.display(),
        marker_index + 1
    ))
}

#[derive(Debug)]
struct LocatedGrammarProduction {
    path: std::path::PathBuf,
    line: usize,
    declaration: String,
}

type GrammarSyncKey = (String, String, String, Option<String>);

fn grammar_sync_key(marker: &GrammarSyncMarker) -> GrammarSyncKey {
    (
        marker.id.clone(),
        marker.production.clone(),
        marker.relation.clone(),
        marker.symbol.clone(),
    )
}

fn grammar_declaration_contains_symbol(declaration: &str, symbol: &str) -> bool {
    declaration
        .split_once('=')
        .is_some_and(|(_, rhs)| rhs.split_whitespace().any(|token| token == symbol))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppendixGrammarProduction {
    name: String,
    line: usize,
    rhs: String,
}

/// Remove EBNF comments while retaining line breaks for diagnostics.
fn strip_ebnf_comments(input: &str, first_line: usize, path: &Path) -> Result<String, String> {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut quote = None;
    let mut special = false;
    let mut escaped = false;
    let mut line_number = first_line;
    while let Some(character) = chars.next() {
        if character == '\n' {
            line_number += 1;
        }
        if let Some(quote_character) = quote {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote_character {
                quote = None;
            }
        } else if special {
            output.push(character);
            if character == '?' {
                special = false;
            }
        } else if matches!(character, '"' | '\'') {
            output.push(character);
            quote = Some(character);
        } else if character == '?' {
            output.push(character);
            special = true;
        } else if character == '(' && chars.peek() == Some(&'*') {
            let comment_line = line_number;
            output.push(' ');
            chars.next();
            let mut closed = false;
            while let Some(comment_character) = chars.next() {
                if comment_character == '*' && chars.peek() == Some(&')') {
                    chars.next();
                    closed = true;
                    break;
                }
                if comment_character == '\n' {
                    output.push('\n');
                    line_number += 1;
                }
            }
            if !closed {
                return Err(format!(
                    "{}:{}: unterminated EBNF comment in Appendix A",
                    path.display(),
                    comment_line,
                ));
            }
        } else {
            output.push(character);
        }
    }
    Ok(output)
}

/// Split one Appendix EBNF fence into declarations.
///
/// This is deliberately a small, fail-closed reader rather than a claim to
/// implement all of EBNF. It understands the syntax used by Appendix A:
/// quoted terminals, `? special sequences ?`, comments, and semicolon-ended
/// productions. Anything else that is not a production is rejected so a
/// malformed appendix cannot silently pass the gate.
fn parse_appendix_grammar_fence(
    content: &str,
    first_line: usize,
    path: &Path,
) -> Result<Vec<AppendixGrammarProduction>, String> {
    let content = strip_ebnf_comments(content, first_line, path)?;
    let mut productions = Vec::new();
    let mut declaration_start = 0;
    let mut quote = None;
    let mut quote_start = 0;
    let mut special = false;
    let mut special_start = 0;
    let mut escaped = false;
    let mut delimiters = Vec::<(char, usize)>::new();

    for (index, character) in content.char_indices() {
        if let Some(quote_character) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote_character {
                quote = None;
            }
            continue;
        }
        if special {
            if character == '?' {
                special = false;
            }
            continue;
        }
        match character {
            '"' | '\'' => {
                quote = Some(character);
                quote_start = ebnf_line_for_offset(&content, index, first_line);
            }
            '?' => {
                special = true;
                special_start = ebnf_line_for_offset(&content, index, first_line);
            }
            '(' | '[' | '{' => {
                delimiters.push((character, ebnf_line_for_offset(&content, index, first_line)))
            }
            ')' | ']' | '}' => {
                let expected = match character {
                    ')' => '(',
                    ']' => '[',
                    '}' => '{',
                    _ => unreachable!(),
                };
                let Some((opening, opening_line)) = delimiters.pop() else {
                    return Err(format!(
                        "{}:{}: unexpected EBNF delimiter `{character}` in Appendix A",
                        path.display(),
                        ebnf_line_for_offset(&content, index, first_line),
                    ));
                };
                if opening != expected {
                    return Err(format!(
                        "{}:{}: mismatched EBNF delimiter `{character}` for `{opening}` opened at line {opening_line}",
                        path.display(),
                        ebnf_line_for_offset(&content, index, first_line),
                    ));
                }
            }
            ';' => {
                if let Some((opening, opening_line)) = delimiters.last() {
                    return Err(format!(
                        "{}:{}: production ends before EBNF delimiter `{opening}` opened at line {opening_line} is closed",
                        path.display(),
                        ebnf_line_for_offset(&content, index, first_line),
                    ));
                }
                let declaration = content[declaration_start..index].trim();
                if declaration.is_empty() {
                    declaration_start = index + character.len_utf8();
                    continue;
                }
                let Some(equal_index) = find_ebnf_equal(declaration) else {
                    return Err(format!(
                        "{}:{}: Appendix A contains a non-production EBNF declaration `{}`",
                        path.display(),
                        ebnf_line_for_offset(&content, declaration_start, first_line),
                        declaration
                    ));
                };
                let name = declaration[..equal_index].trim();
                if !is_ebnf_identifier(name) {
                    return Err(format!(
                        "{}:{}: Appendix A has invalid production name `{name}`",
                        path.display(),
                        ebnf_line_for_offset(&content, declaration_start, first_line),
                    ));
                }
                productions.push(AppendixGrammarProduction {
                    name: name.to_string(),
                    line: ebnf_line_for_offset(&content, declaration_start, first_line),
                    rhs: declaration[equal_index + 1..].trim().to_string(),
                });
                declaration_start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if quote.is_some() {
        return Err(format!(
            "{}:{}: unterminated quoted terminal in Appendix A",
            path.display(),
            quote_start,
        ));
    }
    if special {
        return Err(format!(
            "{}:{}: unterminated EBNF special sequence in Appendix A",
            path.display(),
            special_start,
        ));
    }
    if let Some((opening, opening_line)) = delimiters.last() {
        return Err(format!(
            "{}:{}: unterminated EBNF delimiter `{opening}` in Appendix A",
            path.display(),
            opening_line,
        ));
    }
    if !content[declaration_start..].trim().is_empty() {
        return Err(format!(
            "{}:{}: Appendix A has an unterminated EBNF production",
            path.display(),
            ebnf_line_for_offset(&content, declaration_start, first_line),
        ));
    }
    Ok(productions)
}

fn ebnf_line_for_offset(content: &str, offset: usize, first_line: usize) -> usize {
    let declaration = &content[offset..];
    let first_non_whitespace = declaration
        .find(|character: char| !character.is_whitespace())
        .unwrap_or(0);
    first_line
        + content[..offset + first_non_whitespace]
            .matches('\n')
            .count()
}

fn is_ebnf_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn find_ebnf_equal(value: &str) -> Option<usize> {
    let mut quote = None;
    let mut special = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if let Some(quote_character) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote_character {
                quote = None;
            }
        } else if special {
            if character == '?' {
                special = false;
            }
        } else {
            match character {
                '"' | '\'' => quote = Some(character),
                '?' => special = true,
                '=' => return Some(index),
                _ => {}
            }
        }
    }
    None
}

fn ebnf_references(rhs: &str) -> Result<Vec<String>, String> {
    let mut references = Vec::new();
    let mut chars = rhs.char_indices().peekable();
    let mut quote = None;
    let mut special = false;
    let mut escaped = false;
    while let Some((index, character)) = chars.next() {
        if let Some(quote_character) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote_character {
                quote = None;
            }
            continue;
        }
        if special {
            if character == '?' {
                special = false;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '?' => special = true,
            character if character.is_ascii_alphabetic() || character == '_' => {
                let start = index;
                let mut end = index + character.len_utf8();
                while let Some((next_index, next_character)) = chars.peek().copied() {
                    if next_character.is_ascii_alphanumeric() || next_character == '_' {
                        end = next_index + next_character.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                references.push(rhs[start..end].to_string());
            }
            _ => {}
        }
    }
    if quote.is_some() {
        return Err("unterminated quoted terminal in EBNF production".to_string());
    }
    if special {
        return Err("unterminated EBNF special sequence in production".to_string());
    }
    Ok(references)
}

/// Check that every unquoted Appendix A RHS identifier names a production.
fn validate_appendix_grammar(spec_dir: &Path) -> Result<(), String> {
    let appendix_path = spec_dir.join("appendices/A-grammar.md");
    let content = fs::read_to_string(&appendix_path).map_err(|error| {
        format!(
            "failed to read Appendix A grammar {}: {error}",
            appendix_path.display()
        )
    })?;
    let lines: Vec<&str> = content.lines().collect();
    let mut productions = Vec::new();
    let mut fence_count = 0;
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim() != "```ebnf" {
            index += 1;
            continue;
        }
        fence_count += 1;
        let first_line = index + 2;
        let end = lines[index + 1..]
            .iter()
            .position(|line| line.trim() == "```")
            .map(|offset| index + 1 + offset)
            .ok_or_else(|| {
                format!(
                    "{}:{}: unterminated EBNF fence in Appendix A",
                    appendix_path.display(),
                    index + 1
                )
            })?;
        let fence = lines[index + 1..end].join("\n");
        productions.extend(parse_appendix_grammar_fence(
            &fence,
            first_line,
            &appendix_path,
        )?);
        index = end + 1;
    }
    if fence_count == 0 {
        return Err(format!(
            "{}: Appendix A contains no EBNF fence",
            appendix_path.display()
        ));
    }
    if productions.is_empty() {
        return Err(format!(
            "{}: Appendix A contains no EBNF productions",
            appendix_path.display()
        ));
    }

    let mut definition_locations = BTreeMap::<String, Vec<usize>>::new();
    for production in &productions {
        definition_locations
            .entry(production.name.clone())
            .or_default()
            .push(production.line);
    }
    let duplicates = definition_locations
        .iter()
        .filter(|(_, locations)| locations.len() > 1)
        .map(|(name, locations)| {
            format!(
                "`{name}` at lines {}",
                locations
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>();
    if !duplicates.is_empty() {
        return Err(format!(
            "{}: Appendix A defines duplicate grammar productions: {}",
            appendix_path.display(),
            duplicates.join("; ")
        ));
    }

    let definitions: std::collections::BTreeSet<_> = productions
        .iter()
        .map(|production| production.name.as_str())
        .collect();
    let mut undefined = BTreeMap::<String, Vec<(String, usize)>>::new();
    for production in &productions {
        for reference in ebnf_references(&production.rhs).map_err(|error| {
            format!(
                "{}:{}: cannot parse Appendix A production `{}`: {error}",
                appendix_path.display(),
                production.line,
                production.name
            )
        })? {
            if !definitions.contains(reference.as_str()) {
                undefined
                    .entry(reference)
                    .or_default()
                    .push((production.name.clone(), production.line));
            }
        }
    }
    if undefined.is_empty() {
        return Ok(());
    }
    let details = undefined
        .into_iter()
        .map(|(symbol, origins)| {
            let origins = origins
                .into_iter()
                .map(|(production, line)| format!("{production} at line {line}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("`{symbol}` referenced by {origins}")
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!(
        "{}: Appendix A references undefined grammar symbols: {details}",
        appendix_path.display()
    ))
}

/// Verify every marked normative production has an exact Appendix A mirror.
///
/// This is intentionally part of the existing traceability gate. A marker in
/// the normative chapter is an obligation, so deleting the Appendix A marker
/// or changing either EBNF declaration fails the same canonical check that
/// already guards spec/test relationships.
fn validate_grammar_consistency(spec_dir: &Path) -> Result<(), String> {
    let mut sources: BTreeMap<GrammarSyncKey, Vec<LocatedGrammarProduction>> = BTreeMap::new();
    let mut appendices: BTreeMap<GrammarSyncKey, Vec<LocatedGrammarProduction>> = BTreeMap::new();

    let md_files = discover_files(spec_dir, "md").map_err(|error| {
        format!(
            "failed to discover specification files under {}: {error}",
            spec_dir.display()
        )
    })?;
    for path in md_files {
        let content = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read spec file {}: {error}", path.display()))?;
        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let Some(marker) = parse_grammar_sync_marker(line)
                .map_err(|error| format!("{}:{}: {error}", path.display(), index + 1))?
            else {
                continue;
            };
            let declaration = if marker.relation == "contains" && marker.role == "source" {
                String::new()
            } else {
                parse_marked_grammar_production(&lines, index, &marker.production, &path)?
            };
            let located = LocatedGrammarProduction {
                path: path.clone(),
                line: index + 1,
                declaration,
            };
            let key = grammar_sync_key(&marker);
            match marker.role.as_str() {
                "source" => sources.entry(key).or_default().push(located),
                "appendix" => appendices.entry(key).or_default().push(located),
                _ => unreachable!("parse_grammar_sync_marker validates roles"),
            }
        }
    }

    for (key, source_markers) in &sources {
        if source_markers.len() != 1 {
            return Err(format!(
                "grammar-sync {}:{} requires exactly one source marker, found {}",
                key.0,
                key.1,
                source_markers.len()
            ));
        }
        let source = &source_markers[0];
        let appendix_markers = appendices.get(key).ok_or_else(|| {
            format!(
                "grammar-sync {}:{} has no Appendix A mirror (source at {}:{})",
                key.0,
                key.1,
                source.path.display(),
                source.line
            )
        })?;
        if appendix_markers.len() != 1 {
            return Err(format!(
                "grammar-sync {}:{} requires exactly one Appendix A marker, found {}",
                key.0,
                key.1,
                appendix_markers.len()
            ));
        }
        let appendix = &appendix_markers[0];
        if !appendix
            .path
            .ends_with(Path::new("appendices/A-grammar.md"))
        {
            return Err(format!(
                "grammar-sync {}:{} appendix marker is not in Appendix A ({}:{})",
                key.0,
                key.1,
                appendix.path.display(),
                appendix.line
            ));
        }
        match key.2.as_str() {
            "exact" if source.declaration != appendix.declaration => {
                return Err(format!(
                    "grammar-sync {}:{} differs between {}:{} and {}:{}\n  source: {}\n  appendix: {}",
                    key.0,
                    key.1,
                    source.path.display(),
                    source.line,
                    appendix.path.display(),
                    appendix.line,
                    source.declaration,
                    appendix.declaration
                ));
            }
            "exact" => {}
            "contains" => {
                let symbol = key.3.as_deref().expect("contains markers require symbols");
                if !grammar_declaration_contains_symbol(&appendix.declaration, symbol) {
                    return Err(format!(
                        "grammar-sync {}:{} requires Appendix A production `{}` to contain `{symbol}` ({}:{})",
                        key.0,
                        key.1,
                        key.1,
                        appendix.path.display(),
                        appendix.line
                    ));
                }
            }
            _ => unreachable!("parse_grammar_sync_marker validates relations"),
        }
    }
    for key in appendices.keys() {
        if !sources.contains_key(key) {
            return Err(format!(
                "grammar-sync {}:{} has an Appendix A marker without a source marker",
                key.0, key.1
            ));
        }
    }
    validate_appendix_grammar(spec_dir)?;
    Ok(())
}

/// Parse spec paragraphs from a markdown file. A rule id that was already
/// registered (same file or an earlier one) is recorded in `duplicates` —
/// last-writer-wins insertion silently REPLACED the earlier paragraph before,
/// so an informative restatement authored after a normative rule erased the
/// coverage requirement without any report.
fn parse_spec_file(
    path: &Path,
    source_path: &str,
    paragraphs: &mut BTreeMap<String, SpecParagraph>,
    duplicates: &mut Vec<String>,
) -> Result<(), String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("failed to read spec file {}: {error}", path.display()))?;

    let lines: Vec<&str> = content.lines().collect();

    let mut title = String::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(heading) = line.trim().strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            if !heading.is_empty() {
                title = heading.to_string();
            }
        }
        let marker = parse_spec_comment(line)
            .map_err(|error| format!("{}:{}: {error}", path.display(), i + 1))?;
        if let Some((id, category)) = marker {
            // Get the next non-empty line as the paragraph text
            let mut text = String::new();
            for j in (i + 1)..lines.len() {
                let next_line = lines[j].trim();
                if next_line.is_empty() {
                    continue;
                }
                // Stop at code blocks, other spec markers, or headers
                if next_line.starts_with("```")
                    || is_spec_marker(next_line)
                    || next_line.starts_with('#')
                {
                    break;
                }
                text = next_line.to_string();
                break;
            }

            if let Some(prev) = paragraphs.insert(
                id.clone(),
                SpecParagraph {
                    id: id.clone(),
                    category: category.clone(),
                    title: title.clone(),
                    source_path: source_path.to_string(),
                    text,
                },
            ) {
                duplicates.push(format!(
                    "{} (categories '{}' and '{}'; second occurrence in {})",
                    id,
                    prev.category,
                    category,
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// Parses all specification paragraphs from markdown files in a directory.
///
/// Recursively searches for `.md` files and extracts paragraphs marked with the
/// Zola shortcode format: `{{ rule(id="X.Y:Z", cat="category") }}`.
///
/// # Arguments
///
/// * `spec_dir` - Path to the specification source directory (e.g., `docs/spec/src`)
///
/// # Returns
///
/// A map of paragraph IDs to [`SpecParagraph`] structs, sorted by ID, plus a
/// list of DUPLICATE rule ids (an id defined more than once across the spec).
/// Duplicates fail the traceability gate: with last-writer-wins insertion, a
/// duplicate can silently replace a normative rule with an informative one.
pub fn parse_spec_paragraphs(
    spec_dir: &Path,
) -> Result<(BTreeMap<String, SpecParagraph>, Vec<String>), String> {
    let mut paragraphs = BTreeMap::new();
    let mut duplicates = Vec::new();

    let md_files = discover_files(spec_dir, "md").map_err(|error| {
        format!(
            "failed to discover specification files under {}: {error}",
            spec_dir.display()
        )
    })?;

    for path in md_files {
        let source_path = path
            .strip_prefix(spec_dir)
            .map_err(|_| format!("{} is outside {}", path.display(), spec_dir.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        parse_spec_file(&path, &source_path, &mut paragraphs, &mut duplicates)?;
    }

    Ok((paragraphs, duplicates))
}

/// Whether a case carries a runner assertion that needs a specification anchor.
/// The runner owns the grouped execution and golden-IR field predicates; the
/// remaining compile/diagnostic assertions are explicit here. `compile_only`
/// is included because it asserts compile success even though it does not run
/// the produced program. Adding a new assertion requires updating the runner's
/// corresponding predicate and this traceability adapter together.
fn asserts_executable_behavior(case: &rue_test_runner::Case) -> bool {
    case.has_execution_assertions()
        || case.has_golden_ir_assertions()
        || case.compile_fail
        || !case.error_contains.is_empty()
        || case.expected_error.is_some()
        || case.expected_error_code.is_some()
}

/// Generates a complete traceability report linking specification to tests.
///
/// This is the main entry point for the traceability system. It:
/// 1. Parses all specification paragraphs from markdown files
/// 2. Parses all test cases from TOML files
/// 3. Builds a coverage map showing which tests cover which paragraphs
/// 4. Detects orphan references (tests referencing non-existent paragraphs)
///
/// # Arguments
///
/// * `spec_dir` - Path to the specification source directory (e.g., `docs/spec/src`)
/// * `cases_dir` - Path to the test cases directory (e.g., `crates/rue-spec/cases`)
///
/// # Returns
///
/// A [`TraceabilityReport`] that can be used to print summaries, check coverage,
/// or programmatically analyze the relationship between spec and tests.
///
/// # Example
///
/// ```ignore
/// let report = generate_report(Path::new("docs/spec/src"), Path::new("crates/rue-spec/cases"));
/// report.print_summary();
/// ```
pub fn generate_report(spec_dir: &Path, cases_dir: &Path) -> Result<TraceabilityReport, String> {
    validate_grammar_consistency(spec_dir)?;

    // Parse spec paragraphs
    let (paragraphs, duplicate_rule_ids) = parse_spec_paragraphs(spec_dir)?;

    // Use the runner's typed deserialization and parameter expansion so the
    // traceability view cannot accept metadata the executable suite rejects.
    let test_files = load_test_files(cases_dir)?;

    // Build coverage map
    let mut coverage: BTreeMap<String, Vec<TestReference>> = BTreeMap::new();
    let mut orphan_references = Vec::new();

    // Initialize coverage map with all paragraph IDs
    for id in paragraphs.keys() {
        coverage.insert(id.clone(), Vec::new());
    }

    // Process test files
    let mut platform_unreachable_cases = Vec::new();
    let mut responsibility_census = ResponsibilityCensus::default();
    let mut non_normative_cases = Vec::new();
    for (_, test_file) in &test_files {
        for case in &test_file.case {
            let test_name = format!("{}::{}", test_file.section.id, case.name);
            // `load_test_files` has already rejected ambiguous cases, so every
            // case here classifies.
            if let Ok(responsibility) = classify_platform_responsibility(case) {
                responsibility_census.record(responsibility);
            }
            let reachable = runs_on_required_ci(&case.only_on);
            if !reachable && !case.spec.is_empty() {
                platform_unreachable_cases.push((test_name.clone(), case.only_on.clone()));
            }
            let counts_as_coverage =
                !case.skip && (case.preview.is_none() || case.preview_should_pass) && reachable;

            // Citation integrity applies even when a case is skipped, preview-
            // allowed-to-fail, or unreachable on required CI. Those cases do
            // not contribute coverage, but their behavior claims must not be
            // allowed to hide behind the coverage filters.
            if asserts_executable_behavior(case) && !case.spec.is_empty() {
                let citations = case
                    .spec
                    .iter()
                    .map(|id| {
                        (
                            id.clone(),
                            paragraphs
                                .get(id)
                                .map(|paragraph| paragraph.category.clone())
                                .unwrap_or_else(|| "orphan".to_string()),
                        )
                    })
                    .collect::<Vec<_>>();
                if citations.iter().all(|(id, _)| {
                    paragraphs
                        .get(id)
                        .is_none_or(|paragraph| !TraceabilityReport::is_normative(paragraph))
                }) {
                    non_normative_cases.push(NonNormativeCase {
                        test_name: test_name.clone(),
                        citations,
                    });
                }
            }

            for spec_ref in &case.spec {
                if let Some(tests) = coverage.get_mut(spec_ref) {
                    // A skipped or preview-allowed-to-fail case never exercises
                    // the rule, so it must not mark the paragraph as covered
                    // (RUE-132). The same holds along the platform axis: a case
                    // scoped to a host no required lane runs never executes in
                    // CI (RUE-1161). Their references are still valid, so they
                    // are not orphans — they simply contribute no coverage.
                    if counts_as_coverage {
                        tests.push(TestReference {
                            test_name: test_name.clone(),
                            source_lines: case.source.lines().count(),
                        });
                    }
                } else {
                    orphan_references.push((test_name.clone(), spec_ref.clone()));
                }
            }
        }
    }

    Ok(TraceabilityReport {
        paragraphs,
        coverage,
        orphan_references,
        duplicate_rule_ids,
        responsibility_census,
        platform_unreachable_cases,
        non_normative_cases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_appendix(spec_dir: &Path) {
        fs::create_dir(spec_dir.join("appendices")).unwrap();
        fs::write(
            spec_dir.join("appendices/A-grammar.md"),
            "```ebnf\nstart = token ;\ntoken = \"token\" ;\n```",
        )
        .unwrap();
    }

    #[test]
    fn appendix_grammar_accepts_the_checked_in_grammar() {
        validate_appendix_grammar(Path::new("docs/spec/src")).unwrap();
    }

    #[test]
    fn appendix_grammar_reports_all_undefined_symbols_in_order() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        fs::write(
            spec_dir.path().join("appendices/A-grammar.md"),
            r#"```ebnf
start = missing_b | "missing_in_a_string" | missing_a ;
other = missing_b | missing_c ;
```"#,
        )
        .unwrap();

        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(
            error.contains("`missing_a` referenced by start at line 2"),
            "{error}"
        );
        assert!(
            error.contains("`missing_b` referenced by start at line 2, other at line 3"),
            "{error}"
        );
        assert!(
            error.contains("`missing_c` referenced by other at line 3"),
            "{error}"
        );
        assert!(error.find("missing_a").unwrap() < error.find("missing_b").unwrap());
        assert!(error.find("missing_b").unwrap() < error.find("missing_c").unwrap());
    }

    #[test]
    fn appendix_grammar_ignores_terminals_special_sequences_and_comments() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        fs::write(
            spec_dir.path().join("appendices/A-grammar.md"),
            r#"```ebnf
(* prose_reference and missing_in_comment are not symbols *)
start = "missing_in_a_string (* not a comment *)" | ? prose_reference and missing_in_special ? | helper ;
helper = "keyword" ;
```"#,
        )
        .unwrap();

        assert!(validate_appendix_grammar(spec_dir.path()).is_ok());
    }

    #[test]
    fn appendix_grammar_fails_closed_for_missing_fences_and_productions() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        let appendix = spec_dir.path().join("appendices/A-grammar.md");
        fs::write(&appendix, "No grammar here").unwrap();
        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(error.contains("no EBNF fence"), "{error}");

        fs::write(&appendix, "```ebnf\n(* only a comment *)\n```").unwrap();
        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(error.contains("no EBNF productions"), "{error}");
    }

    #[test]
    fn appendix_grammar_accepts_a_line_comment_at_physical_eof() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        // Deliberately omit the final newline. The lexer accepts a comment at
        // physical EOF, so the grammar models the line terminator as optional.
        fs::write(
            spec_dir.path().join("appendices/A-grammar.md"),
            "```ebnf\nany_char_except_newline = ? any character except '\\n' or '\\r' ? ;\nnewline = \"\\r\\n\" | \"\\n\" | \"\\r\" ;\nline_comment = \"//\" { any_char_except_newline } [ newline ] ;\n```",
        )
        .unwrap();

        assert!(validate_appendix_grammar(spec_dir.path()).is_ok());
    }

    #[test]
    fn appendix_grammar_rejects_duplicate_and_mismatched_definitions() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        let appendix = spec_dir.path().join("appendices/A-grammar.md");
        fs::write(
            &appendix,
            "```ebnf\nstart = token ;\n```\n\n```ebnf\nstart = ( token ] ;\n```",
        )
        .unwrap();
        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(error.contains("mismatched EBNF delimiter"), "{error}");

        fs::write(&appendix, "```ebnf\nstart = ( token ;\n```").unwrap();
        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(
            error.contains("production ends before EBNF delimiter `(` opened at line 2"),
            "{error}"
        );

        fs::write(
            &appendix,
            "```ebnf\nstart = token ;\n```\n\n```ebnf\nstart = \"other\" ;\n```",
        )
        .unwrap();
        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(error.contains("duplicate grammar productions"), "{error}");
        assert!(error.contains("lines 2, 6"), "{error}");
    }

    #[test]
    fn appendix_grammar_does_not_balance_delimiters_across_productions() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        let appendix = spec_dir.path().join("appendices/A-grammar.md");
        fs::write(
            &appendix,
            "```ebnf\nxbroken = ( ybroken ;\nybroken = ) xbroken ;\n```",
        )
        .unwrap();
        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(
            error.contains("production ends before EBNF delimiter `(` opened at line 2"),
            "{error}"
        );

        fs::write(
            &appendix,
            "```ebnf\nmultiline = (\n    token\n) ;\ntoken = \"token\" ;\n```",
        )
        .unwrap();
        assert!(validate_appendix_grammar(spec_dir.path()).is_ok());
    }

    #[test]
    fn appendix_grammar_reports_unterminated_construct_start_lines() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        fs::write(
            spec_dir.path().join("appendices/A-grammar.md"),
            "```ebnf\n\nstart = \"unterminated ;\n```",
        )
        .unwrap();
        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(
            error.contains(":3: unterminated quoted terminal"),
            "{error}"
        );

        fs::write(
            spec_dir.path().join("appendices/A-grammar.md"),
            "```ebnf\n\nstart = ? prose without a closing marker\n```",
        )
        .unwrap();
        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(
            error.contains(":3: unterminated EBNF special sequence"),
            "{error}"
        );

        fs::write(
            spec_dir.path().join("appendices/A-grammar.md"),
            "```ebnf\n\n(* comment without a closing marker\n```",
        )
        .unwrap();
        let error = validate_appendix_grammar(spec_dir.path()).unwrap_err();
        assert!(error.contains(":3: unterminated EBNF comment"), "{error}");
    }

    #[test]
    fn lexical_grammar_sync_pairs_reject_a_source_appendix_drift() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        let markers = r#"
<!-- grammar-sync(id="2.1:6", production="STRING", role="source", relation="contains", symbol="string_char") -->
<!-- grammar-sync(id="2.1:6", production="string_char", role="source") -->
<!-- grammar-sync(id="2.2:1", production="any_char_except_newline", role="source") -->
<!-- grammar-sync(id="2.2:1", production="newline", role="source") -->
<!-- grammar-sync(id="2.2:1", production="line_comment", role="source") -->
```ebnf
STRING = '"' { string_char } '"' ;
string_char = "x" ;
any_char_except_newline = "x" ;
newline = "n" ;
line_comment = "//" { any_char_except_newline } [ newline ] ;
```
"#;
        let appendix = markers.replace("role=\"source\"", "role=\"appendix\"");
        fs::write(spec_dir.path().join("source.md"), markers).unwrap();
        fs::write(spec_dir.path().join("appendices/A-grammar.md"), &appendix).unwrap();
        assert!(validate_grammar_consistency(spec_dir.path()).is_ok());

        fs::write(
            spec_dir.path().join("appendices/A-grammar.md"),
            appendix.replace("string_char = \"x\"", "string_char = \"drift\""),
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(
            error.contains("grammar-sync 2.1:6:string_char differs"),
            "{error}"
        );
    }

    #[test]
    fn grammar_sync_markers_parse_and_reject_unknown_roles() {
        let marker = parse_grammar_sync_marker(
            "<!-- grammar-sync(id=\"2.1:26\", production=\"byte_literal\", role=\"source\") -->",
        )
        .unwrap()
        .unwrap();
        assert_eq!(marker.id, "2.1:26");
        assert_eq!(marker.production, "byte_literal");
        assert_eq!(marker.role, "source");
        assert_eq!(marker.relation, "exact");
        assert_eq!(marker.symbol, None);
        let derivation = parse_grammar_sync_marker(
            "<!-- grammar-sync(id=\"2.1:26\", production=\"INTEGER\", role=\"source\", relation=\"contains\", symbol=\"byte_literal\") -->",
        )
        .unwrap()
        .unwrap();
        assert_eq!(derivation.relation, "contains");
        assert_eq!(derivation.symbol.as_deref(), Some("byte_literal"));
        assert!(
            parse_grammar_sync_marker(
                "<!-- grammar-sync(id=\"2.1:26\", production=\"byte_literal\", role=\"other\") -->"
            )
            .is_err()
        );
    }

    #[test]
    fn grammar_sync_requires_an_exact_appendix_mirror() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        let source = r#"
<!-- grammar-sync(id="2.1:26", production="byte_literal", role="source") -->
```ebnf
byte_literal = "b'" byte_char "'" ;
byte_char = "x" ;
```
"#;
        let appendix = r#"
<!-- grammar-sync(id="2.1:26", production="byte_literal", role="appendix") -->
```ebnf
byte_literal = "b'" byte_char "'" ;
byte_char = "x" ;
```
"#;
        fs::write(spec_dir.path().join("source.md"), source).unwrap();
        let appendix_path = spec_dir.path().join("appendices/A-grammar.md");
        fs::write(&appendix_path, appendix).unwrap();
        assert!(validate_grammar_consistency(spec_dir.path()).is_ok());

        fs::write(
            &appendix_path,
            appendix.replace("byte_char", "escape_sequence"),
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(error.contains("differs"), "{error}");
    }

    #[test]
    fn grammar_sync_rejects_a_missing_appendix_marker() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::write(
            spec_dir.path().join("source.md"),
            r#"
<!-- grammar-sync(id="2.1:26", production="byte_literal", role="source") -->
```ebnf
byte_literal = "b'" byte_char "'" ;
```
"#,
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(error.contains("no Appendix A mirror"), "{error}");
    }

    #[test]
    fn grammar_sync_requires_integer_to_derive_byte_literals() {
        let spec_dir = tempfile::tempdir().unwrap();
        fs::create_dir(spec_dir.path().join("appendices")).unwrap();
        fs::write(
            spec_dir.path().join("source.md"),
            r#"
<!-- grammar-sync(id="2.1:26", production="INTEGER", role="source", relation="contains", symbol="byte_literal") -->
"#,
        )
        .unwrap();
        let appendix = spec_dir.path().join("appendices/A-grammar.md");
        fs::write(
            &appendix,
            r#"
<!-- grammar-sync(id="2.1:26", production="INTEGER", role="appendix", relation="contains", symbol="byte_literal") -->
```ebnf
INTEGER = byte_literal | dec_literal ;
byte_literal = "b" ;
dec_literal = "d" ;
```
"#,
        )
        .unwrap();
        assert!(validate_grammar_consistency(spec_dir.path()).is_ok());

        fs::write(
            &appendix,
            r#"
<!-- grammar-sync(id="2.1:26", production="INTEGER", role="appendix", relation="contains", symbol="byte_literal") -->
```ebnf
INTEGER = dec_literal ;
byte_literal = "b" ;
dec_literal = "d" ;
```
"#,
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(error.contains("contain `byte_literal`"), "{error}");
    }

    #[test]
    fn grammar_sync_keeps_c_ffi_items_and_productions_reachable() {
        let spec_dir = tempfile::tempdir().unwrap();
        let appendix_dir = spec_dir.path().join("appendices");
        fs::create_dir(&appendix_dir).unwrap();
        let source = r#"
<!-- grammar-sync(id="9.3:1a", production="item", role="source", relation="contains", symbol="extern_block") -->
<!-- grammar-sync(id="9.3:1a", production="item", role="source", relation="contains", symbol="extern_export") -->
<!-- grammar-sync(id="9.3:1a", production="extern_block", role="source") -->
<!-- grammar-sync(id="9.3:1a", production="extern_fn", role="source") -->
<!-- grammar-sync(id="9.3:1a", production="extern_result", role="source") -->
<!-- grammar-sync(id="9.3:1a", production="extern_export", role="source") -->
```ebnf
item          = function | extern_block | extern_export | struct_def ;
extern_block  = "extern" STRING "{" { extern_fn } "}" ;
extern_fn     = "fn" IDENT "(" [ params ] ")" [ extern_result ] ";" ;
extern_result = "->" type ;
extern_export = "pub" "extern" STRING [ "unchecked" ] "fn" IDENT "(" [ params ] ")" [ result ] "{" block "}" ;
```
"#;
        let appendix = r#"
<!-- grammar-sync(id="9.3:1a", production="item", role="appendix", relation="contains", symbol="extern_block") -->
<!-- grammar-sync(id="9.3:1a", production="item", role="appendix", relation="contains", symbol="extern_export") -->
<!-- grammar-sync(id="9.3:1a", production="extern_block", role="appendix") -->
<!-- grammar-sync(id="9.3:1a", production="extern_fn", role="appendix") -->
<!-- grammar-sync(id="9.3:1a", production="extern_result", role="appendix") -->
<!-- grammar-sync(id="9.3:1a", production="extern_export", role="appendix") -->
```ebnf
item          = function | extern_block | extern_export | struct_def ;
extern_block  = "extern" STRING "{" { extern_fn } "}" ;
extern_fn     = "fn" IDENT "(" [ params ] ")" [ extern_result ] ";" ;
extern_result = "->" type ;
extern_export = "pub" "extern" STRING [ "unchecked" ] "fn" IDENT "(" [ params ] ")" [ result ] "{" block "}" ;
function      = "function" ;
struct_def    = "struct" ;
STRING        = "string" ;
IDENT         = "identifier" ;
params        = "params" ;
type          = "type" ;
result        = "result" ;
block         = "block" ;
```
"#;
        fs::write(spec_dir.path().join("foreign-boundary.md"), source).unwrap();
        let appendix_path = appendix_dir.join("A-grammar.md");
        fs::write(&appendix_path, appendix).unwrap();
        assert!(validate_grammar_consistency(spec_dir.path()).is_ok());

        fs::write(
            &appendix_path,
            appendix.replace(
                "item          = function | extern_block | extern_export | struct_def ;",
                "item          = function | extern_block | struct_def ;",
            ),
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(error.contains("contain `extern_export`"), "{error}");

        fs::write(
            &appendix_path,
            appendix.replace(
                "extern_block  = \"extern\" STRING \"{\" { extern_fn } \"}\" ;",
                "extern_block  = \"extern\" STRING \"{\" { extern_fn } \"}\" \"broken\" ;",
            ),
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(
            error.contains("extern_block") && error.contains("differs"),
            "{error}"
        );

        fs::write(
            &appendix_path,
            appendix.replace(
                "extern_fn     = \"fn\" IDENT \"(\" [ params ] \")\" [ extern_result ] \";\" ;",
                "extern_fn     = \"fn\" IDENT \"(\" [ params ] \")\" [ extern_result ] \"broken\" ;",
            ),
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(
            error.contains("extern_fn") && error.contains("differs"),
            "{error}"
        );

        fs::write(
            &appendix_path,
            appendix.replace(
                "extern_result = \"->\" type ;",
                "extern_result = \"=>\" type ;",
            ),
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(
            error.contains("extern_result") && error.contains("differs"),
            "{error}"
        );

        fs::write(
            &appendix_path,
            appendix.replace(
                "extern_export = \"pub\" \"extern\" STRING [ \"unchecked\" ] \"fn\" IDENT \"(\" [ params ] \")\" [ result ] \"{\" block \"}\" ;",
                "extern_export = \"pub\" \"extern\" STRING [ \"unchecked\" ] \"fn\" IDENT \"(\" [ params ] \")\" [ result ] \"{\" block \"}\" \"broken\" ;",
            ),
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(
            error.contains("extern_export") && error.contains("differs"),
            "{error}"
        );
    }

    #[test]
    fn grammar_sync_keeps_yield_expr_reachable_and_exact() {
        let spec_dir = tempfile::tempdir().unwrap();
        let appendix_dir = spec_dir.path().join("appendices");
        fs::create_dir(&appendix_dir).unwrap();
        let source = r#"
<!-- grammar-sync(id="6.6:2", production="primary", role="source", relation="contains", symbol="yield_expr") -->
<!-- grammar-sync(id="6.6:2", production="yield_expr", role="source") -->
```ebnf
primary    = expression | yield_expr ;
yield_expr = "yield" expression ;
```
"#;
        let appendix = r#"
<!-- grammar-sync(id="6.6:2", production="primary", role="appendix", relation="contains", symbol="yield_expr") -->
<!-- grammar-sync(id="6.6:2", production="yield_expr", role="appendix") -->
```ebnf
primary    = expression | yield_expr ;
yield_expr = "yield" expression ;
expression = "value" ;
```
"#;
        fs::write(spec_dir.path().join("borrow-accessors.md"), source).unwrap();
        let appendix_path = appendix_dir.join("A-grammar.md");
        fs::write(&appendix_path, appendix).unwrap();
        assert!(validate_grammar_consistency(spec_dir.path()).is_ok());

        fs::write(
            &appendix_path,
            appendix.replace(
                "primary    = expression | yield_expr ;",
                "primary    = expression ;",
            ),
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(error.contains("contain `yield_expr`"), "{error}");

        fs::write(
            &appendix_path,
            appendix.replace(
                "yield_expr = \"yield\" expression ;",
                "yield_expr = \"yield\" expression \"drift\" ;",
            ),
        )
        .unwrap();
        let error = validate_grammar_consistency(spec_dir.path()).unwrap_err();
        assert!(
            error.contains("yield_expr") && error.contains("differs"),
            "{error}"
        );
    }

    #[test]
    fn test_parse_spec_comment() {
        // Simple shortcode without category defaults to informative
        let (id, cat) = parse_spec_comment("{{ rule(id=\"3.1:1\") }}")
            .unwrap()
            .unwrap();
        assert_eq!(id, "3.1:1");
        assert_eq!(cat, "informative");

        // Shortcode with explicit normative category
        let (id, cat) = parse_spec_comment("{{ rule(id=\"4.2:3\", cat=\"normative\") }}")
            .unwrap()
            .unwrap();
        assert_eq!(id, "4.2:3");
        assert_eq!(cat, "normative");

        // Shortcode with explicit syntax category
        let (id, cat) = parse_spec_comment("{{ rule(id=\"2.1:1\", cat=\"syntax\") }}")
            .unwrap()
            .unwrap();
        assert_eq!(id, "2.1:1");
        assert_eq!(cat, "syntax");

        // Invalid: no colon in ID
        assert!(parse_spec_comment("{{ rule(id=\"3.1.1\") }}").is_err());

        // Invalid formats
        assert!(parse_spec_comment("not a spec comment").unwrap().is_none());
        assert!(parse_spec_comment("<!-- not spec -->").unwrap().is_none());
        assert!(
            parse_spec_comment("{{ note(text=\"rule\") }}")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn malformed_markers_and_unknown_categories_are_rejected() {
        assert!(parse_spec_comment("{{ rule(id=\"1.1:1\" }}").is_err());
        assert!(parse_spec_comment("{{ rule(cat=\"normative\") }}").is_err());
        assert!(
            parse_spec_comment("{{ rule(id=\"1.1:1\", cat=\"typo\") }}")
                .unwrap_err()
                .contains("unknown rule category")
        );
    }

    #[test]
    fn malformed_test_file_fails_report_even_with_valid_sibling() {
        let spec_dir = tempfile::tempdir().unwrap();
        write_test_appendix(spec_dir.path());
        fs::write(
            spec_dir.path().join("spec.md"),
            "{{ rule(id=\"1.1:1\", cat=\"normative\") }}\nRule.",
        )
        .unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(
            cases_dir.path().join("valid.toml"),
            r#"
[section]
id = "test.section"
name = "Test"
[[case]]
name = "valid"
source = "fn main() -> i32 { 0 }"
spec = ["1.1:1"]
exit_code = 0
"#,
        )
        .unwrap();
        fs::write(cases_dir.path().join("malformed.toml"), "case = [").unwrap();

        let error = generate_report(spec_dir.path(), cases_dir.path()).unwrap_err();
        assert!(error.contains("malformed.toml"), "{error}");
    }

    #[test]
    fn traceability_uses_runner_parameter_expansion() {
        let spec_dir = tempfile::tempdir().unwrap();
        write_test_appendix(spec_dir.path());
        fs::write(
            spec_dir.path().join("spec.md"),
            concat!(
                "{{ rule(id=\"1.1:1\", cat=\"normative\") }}\nBase.\n",
                "{{ rule(id=\"1.1:2\", cat=\"normative\") }}\nExtra.\n"
            ),
        )
        .unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(
            cases_dir.path().join("params.toml"),
            r#"
[section]
id = "test.section"
name = "Test"
[[case]]
name = "case_{kind}"
source = "fn main() -> i32 { 0 }"
spec = ["1.1:1"]
exit_code = 0
params = [{ kind = "expanded", spec_extra = ["1.1:2"] }]
"#,
        )
        .unwrap();

        let report = generate_report(spec_dir.path(), cases_dir.path()).unwrap();
        assert_eq!(
            report.coverage["1.1:1"][0].test_name,
            "test.section::case_expanded"
        );
        assert_eq!(
            report.coverage["1.1:2"][0].test_name,
            "test.section::case_expanded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_spec_file_fails_report() {
        use std::os::unix::fs::PermissionsExt;

        let spec_dir = tempfile::tempdir().unwrap();
        let spec = spec_dir.path().join("unreadable.md");
        fs::write(&spec, "{{ rule(id=\"1.1:1\") }}\nRule.").unwrap();
        fs::set_permissions(&spec, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&spec).is_ok() {
            // Mode bits are not enforced for this user (root / CAP_DAC_OVERRIDE),
            // so the unreadable-file premise is vacuous here. Skip, don't fail.
            fs::set_permissions(&spec, fs::Permissions::from_mode(0o600)).unwrap();
            eprintln!("skipped: permission bits not enforced for this user");
            return;
        }
        let cases_dir = tempfile::tempdir().unwrap();
        let result = generate_report(spec_dir.path(), cases_dir.path());
        fs::set_permissions(&spec, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_spec_file() {
        let content = r#"
+++
title = "Test"
+++

# Test

{{ rule(id="3.1:1", cat="normative") }}
This is a test paragraph.

{{ rule(id="3.1:2", cat="normative") }}
Another paragraph.
"#;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.md");
        fs::write(&file_path, content).unwrap();

        let mut paragraphs = BTreeMap::new();
        let mut duplicates = Vec::new();
        parse_spec_file(&file_path, "test.md", &mut paragraphs, &mut duplicates).unwrap();
        assert!(duplicates.is_empty());

        assert_eq!(paragraphs.len(), 2);
        assert!(paragraphs.contains_key("3.1:1"));
        assert!(paragraphs.contains_key("3.1:2"));
        assert_eq!(paragraphs["3.1:1"].category, "normative");
        assert_eq!(paragraphs["3.1:2"].category, "normative");
        assert_eq!(paragraphs["3.1:1"].title, "Test");
        assert_eq!(paragraphs["3.1:1"].source_path, "test.md");
        assert_eq!(paragraphs["3.1:1"].text, "This is a test paragraph.");
    }

    #[test]
    fn test_default_category_is_informative() {
        // Rules without explicit category default to informative
        let (id, cat) = parse_spec_comment("{{ rule(id=\"1.1:1\") }}")
            .unwrap()
            .unwrap();
        assert_eq!(id, "1.1:1");
        assert_eq!(cat, "informative");
    }

    #[test]
    fn test_explicit_example_category() {
        // Paragraphs can be explicitly marked as examples
        let content = r#"
{{ rule(id="3.1:5", cat="example") }}
```rue
fn main() { }
```
"#;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.md");
        fs::write(&file_path, content).unwrap();

        let mut paragraphs = BTreeMap::new();
        let mut duplicates = Vec::new();
        parse_spec_file(&file_path, "test.md", &mut paragraphs, &mut duplicates).unwrap();
        assert!(duplicates.is_empty());

        assert_eq!(paragraphs.len(), 1);
        assert!(paragraphs.contains_key("3.1:5"));
        assert_eq!(paragraphs["3.1:5"].category, "example");
        assert_eq!(paragraphs["3.1:5"].text, "");
    }

    #[test]
    fn test_coverage_calculation() {
        let mut paragraphs = BTreeMap::new();
        paragraphs.insert(
            "1.1:1".to_string(),
            SpecParagraph {
                id: "1.1:1".to_string(),
                category: "legality-rule".to_string(),
                title: "Test".to_string(),
                source_path: "test.md".to_string(),
                text: "Test".to_string(),
            },
        );
        paragraphs.insert(
            "1.1:2".to_string(),
            SpecParagraph {
                id: "1.1:2".to_string(),
                category: "legality-rule".to_string(),
                title: "Test".to_string(),
                source_path: "test.md".to_string(),
                text: "Test 2".to_string(),
            },
        );

        let mut coverage = BTreeMap::new();
        coverage.insert(
            "1.1:1".to_string(),
            vec![TestReference {
                test_name: "test::case1".to_string(),
                source_lines: 1,
            }],
        );
        coverage.insert("1.1:2".to_string(), vec![]);

        let report = TraceabilityReport {
            paragraphs,
            coverage,
            orphan_references: vec![],
            duplicate_rule_ids: vec![],
            responsibility_census: ResponsibilityCensus::default(),
            platform_unreachable_cases: vec![],
            non_normative_cases: vec![],
        };

        assert_eq!(report.covered_count(), 1);
        // One of the two paragraphs is uncovered.
        assert_eq!(report.paragraphs.len() - report.covered_count(), 1);
        assert_eq!(report.coverage_percentage(), 50.0);
    }

    #[test]
    fn executable_cases_with_only_non_normative_citations_fail_with_categories() {
        let spec_dir = tempfile::tempdir().unwrap();
        write_test_appendix(spec_dir.path());
        fs::write(
            spec_dir.path().join("spec.md"),
            concat!(
                "{{ rule(id=\"1.1:1\", cat=\"informative\") }}\nExplanation.\n",
                "{{ rule(id=\"1.1:2\", cat=\"example\") }}\nExample.\n",
            ),
        )
        .unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(
            cases_dir.path().join("cases.toml"),
            r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "informative_only"
spec = ["1.1:1", "1.1:2"]
source = "fn main() -> i32 { 0 }"
exit_code = 0
"#,
        )
        .unwrap();

        let report = generate_report(spec_dir.path(), cases_dir.path()).unwrap();
        assert_eq!(
            &report.non_normative_cases,
            &[NonNormativeCase {
                test_name: "test.section::informative_only".to_string(),
                citations: vec![
                    ("1.1:1".to_string(), "informative".to_string()),
                    ("1.1:2".to_string(), "example".to_string()),
                ],
            }]
        );
        assert!(report.gate_failing());
    }

    #[test]
    fn non_normative_citation_check_includes_non_coverage_cases_and_assertion_kinds() {
        let spec_dir = tempfile::tempdir().unwrap();
        write_test_appendix(spec_dir.path());
        fs::write(
            spec_dir.path().join("spec.md"),
            concat!(
                "{{ rule(id=\"1.1:1\", cat=\"informative\") }}\nInformative.\n",
                "{{ rule(id=\"1.1:2\", cat=\"example\") }}\nExample.\n",
                "{{ rule(id=\"1.1:3\", cat=\"informative\") }}\nInformative.\n",
                "{{ rule(id=\"1.1:4\", cat=\"informative\") }}\nInformative.\n",
                "{{ rule(id=\"1.1:5\", cat=\"informative\") }}\nInformative.\n",
                "{{ rule(id=\"1.1:6\", cat=\"informative\") }}\nInformative.\n",
                "{{ rule(id=\"1.1:7\", cat=\"informative\") }}\nInformative.\n",
                "{{ rule(id=\"1.1:8\", cat=\"informative\") }}\nInformative.\n",
            ),
        )
        .unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(
            cases_dir.path().join("cases.toml"),
            r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "normal"
spec = ["1.1:1", "1.1:2"]
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "skipped"
spec = ["1.1:3"]
skip = true
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "preview_may_fail"
spec = ["1.1:4"]
preview = "floats"
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "preview_must_pass"
spec = ["1.1:5"]
preview = "floats"
preview_should_pass = true
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "unreachable"
spec = ["1.1:6"]
only_on = ["x86-64-macos"]
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "golden"
spec = ["1.1:7"]
source = "fn main() -> i32 { 0 }"
expected_ast = "program"

[[case]]
name = "compile_fail"
spec = ["1.1:8"]
source = "fn main() -> i32 { true }"
compile_fail = true
error_contains = "type mismatch"

[[case]]
name = "source_only"
spec = ["1.1:1"]
source = "fn main() -> i32 { 0 }"
"#,
        )
        .unwrap();

        let report = generate_report(spec_dir.path(), cases_dir.path()).unwrap();
        let mut names = report
            .non_normative_cases
            .iter()
            .map(|case| case.test_name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "test.section::compile_fail",
                "test.section::golden",
                "test.section::normal",
                "test.section::preview_may_fail",
                "test.section::preview_must_pass",
                "test.section::skipped",
                "test.section::unreachable",
            ]
        );
        assert!(!names.contains(&"test.section::source_only"));
        assert!(report.gate_failing());
        assert_eq!(
            format_non_normative_case(
                report
                    .non_normative_cases
                    .iter()
                    .find(|case| case.test_name == "test.section::normal")
                    .unwrap()
            ),
            "test.section::normal cites only 1.1:1 [informative], 1.1:2 [example]"
        );
    }

    #[test]
    fn test_skipped_and_preview_tests_do_not_count_as_coverage() {
        // Four paragraphs, each referenced by exactly one test: a normal test, a
        // skipped test, a preview test allowed to fail, and a preview test
        // required to pass. Only the normal and the required-to-pass tests
        // should count as coverage (RUE-132).
        let spec = r#"
{{ rule(id="1.1:1", cat="normative") }}
Covered by a normal test.

{{ rule(id="1.1:2", cat="normative") }}
Referenced only by a skipped test.

{{ rule(id="1.1:3", cat="normative") }}
Referenced only by a preview test allowed to fail.

{{ rule(id="1.1:4", cat="normative") }}
Referenced by a preview test required to pass.
"#;
        let cases = r#"
[section]
id = "t.section"
name = "T"

[[case]]
name = "normal"
spec = ["1.1:1"]
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "skipped"
spec = ["1.1:2"]
skip = true
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "preview_may_fail"
spec = ["1.1:3"]
preview = "floats"
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "preview_must_pass"
spec = ["1.1:4"]
preview = "floats"
preview_should_pass = true
source = "fn main() -> i32 { 0 }"
exit_code = 0
"#;
        let spec_dir = tempfile::tempdir().unwrap();
        write_test_appendix(spec_dir.path());
        fs::write(spec_dir.path().join("s.md"), spec).unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(cases_dir.path().join("c.toml"), cases).unwrap();

        let report = generate_report(spec_dir.path(), cases_dir.path()).unwrap();

        // Only 1.1:1 and 1.1:4 are exercised by a running, must-pass test.
        assert!(!report.coverage["1.1:1"].is_empty(), "normal test covers");
        assert!(
            report.coverage["1.1:2"].is_empty(),
            "skipped test must not count as coverage"
        );
        assert!(
            report.coverage["1.1:3"].is_empty(),
            "preview-allowed-to-fail test must not count as coverage"
        );
        assert!(
            !report.coverage["1.1:4"].is_empty(),
            "preview-must-pass test covers"
        );
        assert_eq!(report.normative_covered_count(), 2);
        assert_eq!(report.normative_uncovered_count(), 2);
        // The skipped/preview references are valid, so they are not orphans.
        assert!(report.orphan_references.is_empty());
    }

    #[test]
    fn platform_unreachable_cases_do_not_count_as_coverage() {
        // Two rules, each covered by exactly one platform-scoped case: one
        // scoped to a host a required lane runs, one scoped only to Intel
        // macOS, which nothing in required CI executes (RUE-1161).
        let spec = r#"
{{ rule(id="1.1:1", cat="normative") }}
Covered by a case a required lane runs.

{{ rule(id="1.1:2", cat="normative") }}
Referenced only by a case no required lane runs.
"#;
        let cases = r#"
[section]
id = "t.section"
name = "T"

[[case]]
name = "on_a_required_lane"
spec = ["1.1:1"]
only_on = ["aarch64-macos"]
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "on_no_lane"
spec = ["1.1:2"]
only_on = ["x86-64-macos"]
source = "fn main() -> i32 { 0 }"
exit_code = 0
"#;
        let spec_dir = tempfile::tempdir().unwrap();
        write_test_appendix(spec_dir.path());
        fs::write(spec_dir.path().join("s.md"), spec).unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(cases_dir.path().join("c.toml"), cases).unwrap();

        let report = generate_report(spec_dir.path(), cases_dir.path()).unwrap();

        assert!(!report.coverage["1.1:1"].is_empty(), "native lane covers");
        assert!(
            report.coverage["1.1:2"].is_empty(),
            "a case no required lane executes must not count as coverage"
        );
        assert_eq!(
            report.platform_unreachable_cases,
            vec![(
                "t.section::on_no_lane".to_string(),
                vec!["x86-64-macos".to_string()]
            )]
        );
        // The reference is valid, so it is reported as unreachable, not orphaned.
        assert!(report.orphan_references.is_empty());
    }

    #[test]
    fn large_programs_cannot_be_a_normative_rules_only_coverage() {
        let spec = r#"
{{ rule(id="1.1:1", cat="normative") }}
Covered only by a large program.

{{ rule(id="1.1:2", cat="normative") }}
Covered by the same large program and a focused case.
"#;
        let body = "    let _x: i32 = 0;\n".repeat(FOCUSED_CASE_MAX_SOURCE_LINES + 1);
        let cases = format!(
            r#"
[section]
id = "t.section"
name = "T"

[[case]]
name = "large_program"
spec = ["1.1:1", "1.1:2"]
source = """
fn main() -> i32 {{
{body}    0
}}
"""
exit_code = 0

[[case]]
name = "focused"
spec = ["1.1:2"]
source = "fn main() -> i32 {{ 0 }}"
exit_code = 0
"#
        );
        let spec_dir = tempfile::tempdir().unwrap();
        write_test_appendix(spec_dir.path());
        fs::write(spec_dir.path().join("s.md"), spec).unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(cases_dir.path().join("c.toml"), cases).unwrap();

        let report = generate_report(spec_dir.path(), cases_dir.path()).unwrap();

        let unfocused = report.unfocused_normative_coverage();
        assert_eq!(unfocused.len(), 1, "got {unfocused:?}");
        assert_eq!(unfocused[0].0, "1.1:1");
        assert_eq!(unfocused[0].1, "t.section::large_program");
        assert!(report.gate_failing());
    }

    #[test]
    fn test_known_uncovered_allowlist_gates_correctly() {
        // Build a report seeding EVERY allowlisted rule as an uncovered
        // normative paragraph (mirroring production, where they all exist), plus
        // any extra `(id, covered)` paragraphs the caller wants.
        fn report_with(extra: &[(&str, bool)]) -> TraceabilityReport {
            let mut paragraphs = BTreeMap::new();
            let mut coverage = BTreeMap::new();
            let mut add = |id: &str, covered: bool| {
                paragraphs.insert(
                    id.to_string(),
                    SpecParagraph {
                        id: id.to_string(),
                        category: "normative".to_string(),
                        title: "Test".to_string(),
                        source_path: "test.md".to_string(),
                        text: "t".to_string(),
                    },
                );
                let tests = if covered {
                    vec![TestReference {
                        test_name: "t::c".to_string(),
                        source_lines: 1,
                    }]
                } else {
                    vec![]
                };
                coverage.insert(id.to_string(), tests);
            };
            for (id, _) in KNOWN_UNCOVERED_NORMATIVE {
                add(id, false);
            }
            for (id, covered) in extra {
                add(id, *covered);
            }
            TraceabilityReport {
                paragraphs,
                coverage,
                orphan_references: vec![],
                duplicate_rule_ids: vec![],
                responsibility_census: ResponsibilityCensus::default(),
                platform_unreachable_cases: vec![],
                non_normative_cases: vec![],
            }
        }

        // All allowlisted rules uncovered, nothing else: reported, but the gate
        // passes and nothing is stale.
        let r = report_with(&[]);
        assert!(r.unexpected_uncovered_normative_paragraphs().is_empty());
        assert!(r.stale_known_uncovered().is_empty());
        assert!(!r.gate_failing());

        // A non-allowlisted uncovered normative rule fails the gate.
        let r = report_with(&[("99.9:1", false)]);
        assert_eq!(r.unexpected_uncovered_normative_paragraphs().len(), 1);
        assert!(r.gate_failing());

        // If there are any allowlisted rules, a covered one is stale and fails
        // the gate. The allowlist may legitimately be empty when all written
        // skipped normative cases have been implemented.
        if let Some((allow_id, _)) = KNOWN_UNCOVERED_NORMATIVE.first() {
            let r = report_with(&[(allow_id, true)]);
            assert_eq!(r.stale_known_uncovered(), vec![*allow_id]);
            assert!(r.gate_failing());
        }
    }

    #[test]
    fn test_truncate_with_ellipsis_ascii() {
        // Short string - no truncation
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");

        // Exact length - no truncation
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");

        // Needs truncation
        assert_eq!(truncate_with_ellipsis("hello world", 8), "hello...");
    }

    #[test]
    fn test_truncate_with_ellipsis_utf8() {
        // Japanese characters (3 bytes each in UTF-8)
        let japanese = "こんにちは世界"; // 7 characters

        // No truncation needed
        assert_eq!(truncate_with_ellipsis(japanese, 10), japanese);

        // Truncate at character boundary (not byte boundary)
        let truncated = truncate_with_ellipsis(japanese, 6);
        assert_eq!(truncated, "こんに..."); // 3 chars + "..."

        // Mixed ASCII and UTF-8: "Hello世界" is 7 characters
        let mixed = "Hello世界";
        assert_eq!(truncate_with_ellipsis(mixed, 10), mixed);
        assert_eq!(truncate_with_ellipsis(mixed, 7), mixed); // Exactly 7 chars, no truncation
        assert_eq!(truncate_with_ellipsis(mixed, 6), "Hel..."); // 6 chars means 3 content + "..."
    }

    #[test]
    fn test_truncate_with_ellipsis_emoji() {
        // Emoji are multi-byte
        let emoji = "🎉🎊🎁🎈";
        assert_eq!(truncate_with_ellipsis(emoji, 10), emoji);
        assert_eq!(truncate_with_ellipsis(emoji, 4), emoji);
        assert_eq!(truncate_with_ellipsis(emoji, 3), "...");
    }
}
