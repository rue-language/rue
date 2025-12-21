//! Traceability report generator for Rue specification.
//!
//! Generates a report showing which spec paragraphs are covered by tests
//! and which remain uncovered.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// A paragraph from the specification.
#[derive(Debug, Clone)]
pub struct SpecParagraph {
    /// Paragraph ID (e.g., "3.1:5")
    pub id: String,
    /// Category (e.g., "legality-rule", "dynamic-semantics")
    pub category: String,
    /// The text content of the paragraph
    pub text: String,
    /// Source file where this paragraph is defined
    pub source_file: String,
}

/// A test case that references spec paragraphs.
#[derive(Debug, Clone)]
pub struct TestReference {
    /// Full test name (e.g., "lexical.comments::line_comment_after_code")
    pub test_name: String,
    /// Source file where this test is defined
    pub source_file: String,
}

/// Traceability report data.
#[derive(Debug)]
pub struct TraceabilityReport {
    /// All spec paragraphs, keyed by paragraph ID
    pub paragraphs: BTreeMap<String, SpecParagraph>,
    /// Tests covering each paragraph ID
    pub coverage: BTreeMap<String, Vec<TestReference>>,
    /// Test references that don't match any paragraph
    pub orphan_references: Vec<(String, String)>, // (test_name, invalid_ref)
}

impl TraceabilityReport {
    /// Check if a paragraph is informative (doesn't require test coverage).
    fn is_informative(para: &SpecParagraph) -> bool {
        para.category.contains("informative")
    }

    /// Count of normative paragraphs (require test coverage).
    pub fn normative_count(&self) -> usize {
        self.paragraphs
            .values()
            .filter(|p| !Self::is_informative(p))
            .count()
    }

    /// Count of covered normative paragraphs (have at least one test).
    pub fn normative_covered_count(&self) -> usize {
        self.paragraphs
            .values()
            .filter(|p| {
                !Self::is_informative(p)
                    && self
                        .coverage
                        .get(&p.id)
                        .map(|tests| !tests.is_empty())
                        .unwrap_or(false)
            })
            .count()
    }

    /// Count of uncovered normative paragraphs.
    pub fn normative_uncovered_count(&self) -> usize {
        self.normative_count() - self.normative_covered_count()
    }

    /// Get uncovered normative paragraph IDs.
    pub fn uncovered_normative_paragraphs(&self) -> Vec<&String> {
        self.paragraphs
            .iter()
            .filter(|(_, para)| {
                !Self::is_informative(para)
                    && self
                        .coverage
                        .get(&para.id)
                        .map(|tests| tests.is_empty())
                        .unwrap_or(true)
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Coverage percentage for normative paragraphs.
    pub fn normative_coverage_percentage(&self) -> f64 {
        let total = self.normative_count();
        if total == 0 {
            100.0
        } else {
            (self.normative_covered_count() as f64 / total as f64) * 100.0
        }
    }

    /// Count of covered paragraphs (have at least one test).
    pub fn covered_count(&self) -> usize {
        self.coverage
            .iter()
            .filter(|(id, tests)| self.paragraphs.contains_key(*id) && !tests.is_empty())
            .count()
    }

    /// Count of uncovered paragraphs.
    pub fn uncovered_count(&self) -> usize {
        self.paragraphs.len() - self.covered_count()
    }

    /// Get uncovered paragraph IDs.
    pub fn uncovered_paragraphs(&self) -> Vec<&String> {
        self.paragraphs
            .keys()
            .filter(|id| {
                self.coverage
                    .get(*id)
                    .map(|tests| tests.is_empty())
                    .unwrap_or(true)
            })
            .collect()
    }

    /// Coverage percentage.
    pub fn coverage_percentage(&self) -> f64 {
        if self.paragraphs.is_empty() {
            100.0
        } else {
            (self.covered_count() as f64 / self.paragraphs.len() as f64) * 100.0
        }
    }

    /// Print a summary report.
    pub fn print_summary(&self) {
        println!("=== Rue Specification Traceability Report ===\n");

        // Normative coverage stats (what matters for pass/fail)
        let normative_total = self.normative_count();
        let normative_covered = self.normative_covered_count();
        let normative_uncovered = self.normative_uncovered_count();
        let normative_pct = self.normative_coverage_percentage();

        println!(
            "Normative Coverage: {:.1}% ({}/{} paragraphs)",
            normative_pct, normative_covered, normative_total
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
            let marker = if category.contains("informative") {
                " (informative)"
            } else {
                ""
            };
            println!(
                "  {:20} {:.1}% ({}/{}){}",
                category, pct, covered, total, marker
            );
        }
        println!();

        // Uncovered normative paragraphs (what needs to be fixed)
        let uncovered_normative = self.uncovered_normative_paragraphs();
        if !uncovered_normative.is_empty() {
            println!("Uncovered normative paragraphs ({}):", normative_uncovered);
            for id in uncovered_normative {
                if let Some(para) = self.paragraphs.get(id) {
                    // Truncate text to 60 chars
                    let text = if para.text.len() > 60 {
                        format!("{}...", &para.text[..57])
                    } else {
                        para.text.clone()
                    };
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
    }

    /// Print detailed report showing all paragraphs and their tests.
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
                let text = if para.text.len() > 50 {
                    format!("{}...", &para.text[..47])
                } else {
                    para.text.clone()
                };

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
/// Format: r[X.Y.Z] or r[X.Y.Z#category] at the start of a line.
/// Category can be: normative, informative, syntax, example
/// Default category (no #) is informative.
/// Returns (id, category) if found.
fn parse_spec_comment(line: &str) -> Option<(String, String)> {
    let line = line.trim();

    // Format: r[X.Y.Z] or r[X.Y.Z#category] at start of line
    if line.starts_with("r[") && line.ends_with(']') {
        let inner = line.trim_start_matches("r[").trim_end_matches(']');

        if inner.is_empty() {
            return None;
        }

        // Split on # to get ID and optional category
        let (id, category) = if let Some(hash_pos) = inner.find('#') {
            let id_part = &inner[..hash_pos];
            let cat_part = &inner[hash_pos + 1..];
            (id_part.to_string(), cat_part.to_string())
        } else {
            (inner.to_string(), "informative".to_string())
        };

        if id.is_empty() {
            return None;
        }

        // Convert from dot notation (X.Y.Z) to colon notation (X.Y:Z) for compatibility
        // The last dot becomes a colon
        let colon_id = if let Some(last_dot) = id.rfind('.') {
            format!("{}:{}", &id[..last_dot], &id[last_dot + 1..])
        } else {
            id
        };

        return Some((colon_id, category));
    }

    None
}

/// Parse spec paragraphs from a markdown file.
fn parse_spec_file(path: &Path, paragraphs: &mut BTreeMap<String, SpecParagraph>) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let lines: Vec<&str> = content.lines().collect();
    let source_file = path.to_string_lossy().to_string();

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
                    || next_line.starts_with("r[")
                    || next_line.starts_with('#')
                {
                    break;
                }
                text = next_line.to_string();
                break;
            }

            paragraphs.insert(
                id.clone(),
                SpecParagraph {
                    id,
                    category,
                    text,
                    source_file: source_file.clone(),
                },
            );
        }
    }
}

/// Recursively collect all files with the given extension from a directory.
fn collect_files_by_ext(dir: &Path, ext: &str, files: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_files_by_ext(&path, ext, files);
            } else if path.extension().is_some_and(|e| e == ext) {
                files.push(path);
            }
        }
    }
}

/// Parse all spec paragraphs from the spec directory.
pub fn parse_spec_paragraphs(spec_dir: &Path) -> BTreeMap<String, SpecParagraph> {
    let mut paragraphs = BTreeMap::new();

    let mut md_files = Vec::new();
    collect_files_by_ext(spec_dir, "md", &mut md_files);

    for path in md_files {
        parse_spec_file(&path, &mut paragraphs);
    }

    paragraphs
}

/// Information about a test file.
pub struct TestFile {
    pub section_id: String,
    pub cases: Vec<TestCase>,
    pub source_file: String,
}

/// Information about a test case.
pub struct TestCase {
    pub name: String,
    pub spec_refs: Vec<String>,
}

/// Parse test files and extract spec references.
pub fn parse_test_files(cases_dir: &Path) -> Vec<TestFile> {
    let mut test_files = Vec::new();

    let mut toml_files = Vec::new();
    collect_files_by_ext(cases_dir, "toml", &mut toml_files);

    for path in toml_files {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Parse the TOML file
        let table: toml::Table = match toml::from_str(&content) {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Get section ID
        let section_id = table
            .get("section")
            .and_then(|s| s.get("id"))
            .and_then(|id| id.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Get cases
        let mut cases = Vec::new();
        if let Some(case_array) = table.get("case").and_then(|c| c.as_array()) {
            for case in case_array {
                let name = case
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unnamed")
                    .to_string();

                let spec_refs: Vec<String> = case
                    .get("spec")
                    .and_then(|s| s.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                cases.push(TestCase { name, spec_refs });
            }
        }

        test_files.push(TestFile {
            section_id,
            cases,
            source_file: path.to_string_lossy().to_string(),
        });
    }

    test_files
}

/// Generate a traceability report.
pub fn generate_report(spec_dir: &Path, cases_dir: &Path) -> TraceabilityReport {
    // Parse spec paragraphs
    let paragraphs = parse_spec_paragraphs(spec_dir);

    // Parse test files
    let test_files = parse_test_files(cases_dir);

    // Build coverage map
    let mut coverage: BTreeMap<String, Vec<TestReference>> = BTreeMap::new();
    let mut orphan_references = Vec::new();

    // Initialize coverage map with all paragraph IDs
    for id in paragraphs.keys() {
        coverage.insert(id.clone(), Vec::new());
    }

    // Track which references are valid
    let valid_ids: BTreeSet<_> = paragraphs.keys().cloned().collect();

    // Process test files
    for test_file in &test_files {
        for case in &test_file.cases {
            let test_name = format!("{}::{}", test_file.section_id, case.name);

            for spec_ref in &case.spec_refs {
                if valid_ids.contains(spec_ref) {
                    coverage.get_mut(spec_ref).unwrap().push(TestReference {
                        test_name: test_name.clone(),
                        source_file: test_file.source_file.clone(),
                    });
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_spec_comment() {
        // Simple rule ID without category defaults to informative
        let (id, cat) = parse_spec_comment("r[3.1.1]").unwrap();
        assert_eq!(id, "3.1:1");
        assert_eq!(cat, "informative");

        // Rule with explicit normative category
        let (id, cat) = parse_spec_comment("r[4.2.3#normative]").unwrap();
        assert_eq!(id, "4.2:3");
        assert_eq!(cat, "normative");

        // Rule with explicit syntax category
        let (id, cat) = parse_spec_comment("r[2.1.1#syntax]").unwrap();
        assert_eq!(id, "2.1:1");
        assert_eq!(cat, "syntax");

        // Invalid formats
        assert!(parse_spec_comment("not a spec comment").is_none());
        assert!(parse_spec_comment("<!-- not spec -->").is_none());
        assert!(parse_spec_comment("r[").is_none()); // Incomplete
        assert!(parse_spec_comment("r[]").is_none()); // Empty
    }

    #[test]
    fn test_parse_spec_file() {
        let content = r#"
# Test

r[3.1.1#normative]
This is a test paragraph.

r[3.1.2#normative]
Another paragraph.
"#;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.md");
        fs::write(&file_path, content).unwrap();

        let mut paragraphs = BTreeMap::new();
        parse_spec_file(&file_path, &mut paragraphs);

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
        let (id, cat) = parse_spec_comment("r[1.1.1]").unwrap();
        assert_eq!(id, "1.1:1");
        assert_eq!(cat, "informative");
    }

    #[test]
    fn test_explicit_example_category() {
        // Paragraphs can be explicitly marked as examples
        let content = r#"
r[3.1.5#example]
```rue
fn main() { }
```
"#;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.md");
        fs::write(&file_path, content).unwrap();

        let mut paragraphs = BTreeMap::new();
        parse_spec_file(&file_path, &mut paragraphs);

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
                source_file: "test.md".to_string(),
            },
        );
        paragraphs.insert(
            "1.1:2".to_string(),
            SpecParagraph {
                id: "1.1:2".to_string(),
                category: "legality-rule".to_string(),
                text: "Test 2".to_string(),
                source_file: "test.md".to_string(),
            },
        );

        let mut coverage = BTreeMap::new();
        coverage.insert(
            "1.1:1".to_string(),
            vec![TestReference {
                test_name: "test::case1".to_string(),
                source_file: "test.toml".to_string(),
            }],
        );
        coverage.insert("1.1:2".to_string(), vec![]);

        let report = TraceabilityReport {
            paragraphs,
            coverage,
            orphan_references: vec![],
        };

        assert_eq!(report.covered_count(), 1);
        assert_eq!(report.uncovered_count(), 1);
        assert_eq!(report.coverage_percentage(), 50.0);
    }
}
