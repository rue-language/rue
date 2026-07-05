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

use rue_test_runner::collect_files_by_ext;
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
}

/// Normative paragraphs that currently have no *running* test, tracked as known
/// gaps pending compiler-feature implementation.
///
/// Each entry is `(paragraph_id, reason)`. These are rules whose only spec tests
/// are `skip = true` because the behavior they pin isn't implemented yet — the
/// tests are written and ready to un-skip the moment the feature lands. Before
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
    (
        "2.5:10",
        "'@directive must precede item' error not implemented \
         (lexical/builtins.toml::directive_must_precede_item, skipped)",
    ),
    (
        "2.5:21",
        "@allow(unreachable_code) suppression not implemented \
         (lexical/builtins.toml::allow_unreachable_code, skipped)",
    ),
];

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

    /// Whether the traceability gate should fail: any *unexpected* uncovered
    /// normative rule, any stale allowlist entry, any orphan reference, or any
    /// duplicate rule id.
    pub fn gate_failing(&self) -> bool {
        !self.unexpected_uncovered_normative_paragraphs().is_empty()
            || !self.stale_known_uncovered().is_empty()
            || !self.orphan_references.is_empty()
            || !self.duplicate_rule_ids.is_empty()
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

/// Extract a quoted shortcode argument (`name = "value"`), tolerating
/// whitespace around `=`. Zola accepts `id = "x"` and renders it identically
/// to `id="x"`, so the checker must too: a spaced `cat` used to silently
/// demote a rule to informative, and a spaced `id` made the rule invisible —
/// both dropped coverage enforcement without a report.
fn find_shortcode_arg(line: &str, name: &str) -> Option<String> {
    let mut search_from = 0;
    while let Some(rel) = line[search_from..].find(name) {
        let pos = search_from + rel;
        let after = line[pos + name.len()..].trim_start();
        if let Some(rest) = after.strip_prefix('=') {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_string());
                }
            }
        }
        search_from = pos + name.len();
    }
    None
}

/// Parse a spec marker from a line.
/// Format: {{ rule(id="X.Y:Z") }} or {{ rule(id="X.Y:Z", cat="category") }} (Zola shortcode)
/// Category can be: normative, informative, syntax, example
/// Default category (no cat) is informative.
/// Returns (id, category) if found.
fn parse_spec_comment(line: &str) -> Option<(String, String)> {
    let line = line.trim();

    // Zola shortcode format: {{ rule(id="X.Y:Z") }} or {{ rule(id="X.Y:Z", cat="category") }}
    if !(line.starts_with("{{") && line.contains("rule(")) {
        return None;
    }
    let id = find_shortcode_arg(line, "id")?;
    // Validate that the ID contains a colon (required format: X.Y:Z)
    if !id.contains(':') {
        return None;
    }
    let category = find_shortcode_arg(line, "cat").unwrap_or_else(|| "informative".to_string());
    Some((id, category))
}

/// Check if a line is a spec marker (Zola shortcode format).
fn is_spec_marker(line: &str) -> bool {
    line.starts_with("{{") && line.contains("rule(")
}

/// Parse spec paragraphs from a markdown file. A rule id that was already
/// registered (same file or an earlier one) is recorded in `duplicates` —
/// last-writer-wins insertion silently REPLACED the earlier paragraph before,
/// so an informative restatement authored after a normative rule erased the
/// coverage requirement without any report.
fn parse_spec_file(
    path: &Path,
    paragraphs: &mut BTreeMap<String, SpecParagraph>,
    duplicates: &mut Vec<String>,
) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading spec file {}: {}", path.display(), e);
            return;
        }
    };

    let lines: Vec<&str> = content.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        if let Some((id, category)) = parse_spec_comment(line) {
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
pub fn parse_spec_paragraphs(spec_dir: &Path) -> (BTreeMap<String, SpecParagraph>, Vec<String>) {
    let mut paragraphs = BTreeMap::new();
    let mut duplicates = Vec::new();

    let mut md_files = Vec::new();
    collect_files_by_ext(spec_dir, "md", &mut md_files);

    for path in md_files {
        parse_spec_file(&path, &mut paragraphs, &mut duplicates);
    }

    (paragraphs, duplicates)
}

/// A parsed test specification file.
///
/// Each TOML file in the test cases directory represents a section of tests.
/// This struct holds the parsed metadata and test cases from one such file.
pub struct TestFile {
    /// The section identifier from the TOML file (e.g., "expressions.arithmetic").
    pub section_id: String,
    /// All test cases defined in this file.
    pub cases: Vec<TestCase>,
}

/// A single test case with its specification references.
///
/// For parameterized tests, this represents one expanded instance with
/// any `spec_extra` references merged into the base `spec` references.
pub struct TestCase {
    /// The test case name (expanded with parameter values for parameterized tests).
    pub name: String,
    /// Specification paragraph IDs that this test covers (e.g., ["3.1:5", "3.1:6"]).
    pub spec_refs: Vec<String>,
    /// Whether this case actually *runs* and so can satisfy coverage.
    ///
    /// A rule's behavior is only exercised by a test that runs and is required
    /// to pass. A case does **not** count as coverage if it is `skip = true`, or
    /// if it is a preview-feature test that is allowed to fail (`preview` set
    /// without `preview_should_pass`). Such a case's spec references still count
    /// for orphan detection — a dangling reference is broken whether or not the
    /// case runs — but they do not mark a normative paragraph as covered
    /// (RUE-132).
    pub counts_as_coverage: bool,
}

/// Parses test files and extracts specification references.
///
/// Recursively searches for `.toml` files in the cases directory and parses
/// each one to extract test cases and their spec references.
///
/// # Parameterized Tests
///
/// For parameterized tests (those with a `params` array), this function expands
/// each parameter set into a separate [`TestCase`], substituting placeholders
/// in the test name and merging any `spec_extra` references with the base `spec` array.
///
/// # Arguments
///
/// * `cases_dir` - Path to the test cases directory (e.g., `crates/rue-spec/cases`)
///
/// # Returns
///
/// A list of [`TestFile`] structs, one per TOML file found.
pub fn parse_test_files(cases_dir: &Path) -> Vec<TestFile> {
    let mut test_files = Vec::new();

    let mut toml_files = Vec::new();
    collect_files_by_ext(cases_dir, "toml", &mut toml_files);

    for path in toml_files {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error reading test file {}: {}", path.display(), e);
                continue;
            }
        };

        // Parse the TOML file
        let table: toml::Table = match toml::from_str(&content) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Error parsing test file {}: {}", path.display(), e);
                continue;
            }
        };

        // Get section ID
        let section_id = table
            .get("section")
            .and_then(|s| s.get("id"))
            .and_then(|id| id.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Get cases
        let case_array_opt = table.get("case").and_then(|c| c.as_array());
        let mut cases = Vec::new();
        if let Some(case_array) = case_array_opt {
            for case in case_array {
                let base_name = case
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unnamed")
                    .to_string();

                let base_spec_refs: Vec<String> = case
                    .get("spec")
                    .and_then(|s| s.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                // A case only counts as coverage if it actually runs and is
                // required to pass (RUE-132): a `skip = true` case never runs,
                // and a preview-feature test without `preview_should_pass` is
                // allowed to fail, so neither exercises the rule it references.
                let base_skip = case.get("skip").and_then(|s| s.as_bool()).unwrap_or(false);
                let is_preview = case
                    .get("preview")
                    .and_then(|p| p.as_str())
                    .is_some_and(|s| !s.is_empty());
                let preview_should_pass = case
                    .get("preview_should_pass")
                    .and_then(|p| p.as_bool())
                    .unwrap_or(false);
                let preview_allowed_to_fail = is_preview && !preview_should_pass;

                // Check if this is a parameterized test
                if let Some(params_array) = case.get("params").and_then(|p| p.as_array()) {
                    // Expand parameterized test - each param set becomes a test case
                    for param_set in params_array {
                        let param_table = match param_set.as_table() {
                            Some(t) => t,
                            None => continue,
                        };

                        // Substitute placeholders in name
                        let mut expanded_name = base_name.clone();
                        for (key, value) in param_table {
                            let placeholder = format!("{{{}}}", key);
                            let replacement = match value {
                                toml::Value::String(s) => s.clone(),
                                toml::Value::Integer(i) => i.to_string(),
                                other => other.to_string(),
                            };
                            expanded_name = expanded_name.replace(&placeholder, &replacement);
                        }

                        // Merge base spec refs with spec_extra from params
                        let mut spec_refs = base_spec_refs.clone();
                        if let Some(spec_extra) = param_table.get("spec_extra") {
                            if let Some(arr) = spec_extra.as_array() {
                                for item in arr {
                                    if let Some(s) = item.as_str() {
                                        spec_refs.push(s.to_string());
                                    }
                                }
                            }
                        }

                        // A per-param `skip = true` override also disqualifies
                        // just that instance from counting as coverage.
                        let param_skip = param_table
                            .get("skip")
                            .and_then(|s| s.as_bool())
                            .unwrap_or(false);
                        // Per-param `preview`/`preview_should_pass` overrides
                        // mirror the runner's expand_case: an instance the
                        // runner allows to fail must not count as coverage,
                        // even when the BASE case has no preview (RUE-132).
                        let param_is_preview = param_table
                            .get("preview")
                            .and_then(|p| p.as_str())
                            .map(|s| !s.is_empty());
                        let param_should_pass = param_table
                            .get("preview_should_pass")
                            .and_then(|p| p.as_bool());
                        let effective_preview = param_is_preview.unwrap_or(is_preview);
                        let effective_should_pass =
                            param_should_pass.unwrap_or(preview_should_pass);
                        let param_preview_allowed_to_fail =
                            effective_preview && !effective_should_pass;
                        let counts_as_coverage =
                            !base_skip && !param_skip && !param_preview_allowed_to_fail;

                        cases.push(TestCase {
                            name: expanded_name,
                            spec_refs,
                            counts_as_coverage,
                        });
                    }
                } else {
                    // Non-parameterized test
                    cases.push(TestCase {
                        name: base_name,
                        spec_refs: base_spec_refs,
                        counts_as_coverage: !base_skip && !preview_allowed_to_fail,
                    });
                }
            }
        }

        test_files.push(TestFile { section_id, cases });
    }

    test_files
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
pub fn generate_report(spec_dir: &Path, cases_dir: &Path) -> TraceabilityReport {
    // Parse spec paragraphs
    let (paragraphs, duplicate_rule_ids) = parse_spec_paragraphs(spec_dir);

    // Parse test files
    let test_files = parse_test_files(cases_dir);

    // Build coverage map
    let mut coverage: BTreeMap<String, Vec<TestReference>> = BTreeMap::new();
    let mut orphan_references = Vec::new();

    // Initialize coverage map with all paragraph IDs
    for id in paragraphs.keys() {
        coverage.insert(id.clone(), Vec::new());
    }

    // Process test files
    for test_file in &test_files {
        for case in &test_file.cases {
            let test_name = format!("{}::{}", test_file.section_id, case.name);

            for spec_ref in &case.spec_refs {
                if let Some(tests) = coverage.get_mut(spec_ref) {
                    // A skipped or preview-allowed-to-fail case never exercises
                    // the rule, so it must not mark the paragraph as covered
                    // (RUE-132). Its reference is still valid, so it is not an
                    // orphan — it simply doesn't contribute coverage.
                    if case.counts_as_coverage {
                        tests.push(TestReference {
                            test_name: test_name.clone(),
                        });
                    }
                } else {
                    orphan_references.push((test_name.clone(), spec_ref.clone()));
                }
            }
        }
    }

    TraceabilityReport {
        paragraphs,
        coverage,
        orphan_references,
        duplicate_rule_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_spec_comment() {
        // Simple shortcode without category defaults to informative
        let (id, cat) = parse_spec_comment("{{ rule(id=\"3.1:1\") }}").unwrap();
        assert_eq!(id, "3.1:1");
        assert_eq!(cat, "informative");

        // Shortcode with explicit normative category
        let (id, cat) = parse_spec_comment("{{ rule(id=\"4.2:3\", cat=\"normative\") }}").unwrap();
        assert_eq!(id, "4.2:3");
        assert_eq!(cat, "normative");

        // Shortcode with explicit syntax category
        let (id, cat) = parse_spec_comment("{{ rule(id=\"2.1:1\", cat=\"syntax\") }}").unwrap();
        assert_eq!(id, "2.1:1");
        assert_eq!(cat, "syntax");

        // Invalid: no colon in ID
        assert!(parse_spec_comment("{{ rule(id=\"3.1.1\") }}").is_none());

        // Invalid formats
        assert!(parse_spec_comment("not a spec comment").is_none());
        assert!(parse_spec_comment("<!-- not spec -->").is_none());
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
        parse_spec_file(&file_path, &mut paragraphs, &mut duplicates);
        assert!(duplicates.is_empty());

        assert_eq!(paragraphs.len(), 2);
        assert!(paragraphs.contains_key("3.1:1"));
        assert!(paragraphs.contains_key("3.1:2"));
        assert_eq!(paragraphs["3.1:1"].category, "normative");
        assert_eq!(paragraphs["3.1:2"].category, "normative");
        assert_eq!(paragraphs["3.1:1"].text, "This is a test paragraph.");
    }

    #[test]
    fn test_default_category_is_informative() {
        // Rules without explicit category default to informative
        let (id, cat) = parse_spec_comment("{{ rule(id=\"1.1:1\") }}").unwrap();
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
        parse_spec_file(&file_path, &mut paragraphs, &mut duplicates);
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
                text: "Test".to_string(),
            },
        );
        paragraphs.insert(
            "1.1:2".to_string(),
            SpecParagraph {
                id: "1.1:2".to_string(),
                category: "legality-rule".to_string(),
                text: "Test 2".to_string(),
            },
        );

        let mut coverage = BTreeMap::new();
        coverage.insert(
            "1.1:1".to_string(),
            vec![TestReference {
                test_name: "test::case1".to_string(),
            }],
        );
        coverage.insert("1.1:2".to_string(), vec![]);

        let report = TraceabilityReport {
            paragraphs,
            coverage,
            orphan_references: vec![],
            duplicate_rule_ids: vec![],
        };

        assert_eq!(report.covered_count(), 1);
        // One of the two paragraphs is uncovered.
        assert_eq!(report.paragraphs.len() - report.covered_count(), 1);
        assert_eq!(report.coverage_percentage(), 50.0);
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
preview = "some_feature"
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "preview_must_pass"
spec = ["1.1:4"]
preview = "some_feature"
preview_should_pass = true
source = "fn main() -> i32 { 0 }"
exit_code = 0
"#;
        let spec_dir = tempfile::tempdir().unwrap();
        fs::write(spec_dir.path().join("s.md"), spec).unwrap();
        let cases_dir = tempfile::tempdir().unwrap();
        fs::write(cases_dir.path().join("c.toml"), cases).unwrap();

        let report = generate_report(spec_dir.path(), cases_dir.path());

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
                        text: "t".to_string(),
                    },
                );
                let tests = if covered {
                    vec![TestReference {
                        test_name: "t::c".to_string(),
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

        // An allowlisted rule that is now COVERED is stale and fails the gate.
        let allow_id = KNOWN_UNCOVERED_NORMATIVE[0].0;
        let r = report_with(&[(allow_id, true)]);
        assert_eq!(r.stale_known_uncovered(), vec![allow_id]);
        assert!(r.gate_failing());
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
