use anyhow::{Context, Result};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

/// Comprehensive test report with results and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestReport {
    /// Metadata about the test run
    pub metadata: TestRunMetadata,

    /// Summary statistics
    pub summary: TestSummary,

    /// Individual test results
    pub results: Vec<TestResultEntry>,

    /// Performance metrics
    pub performance: PerformanceMetrics,
}

/// Metadata about when and how tests were run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRunMetadata {
    /// Timestamp when test run started
    pub start_time: u64,

    /// Timestamp when test run completed
    pub end_time: Option<u64>,

    /// Command line arguments used
    pub command_line: Vec<String>,

    /// Version of the test runner
    pub runner_version: String,

    /// Version of the rue compiler
    pub compiler_version: Option<String>,

    /// Environment information
    pub environment: HashMap<String, String>,
}

/// Summary statistics for the test run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSummary {
    /// Total number of tests discovered
    pub total: usize,

    /// Number of tests that passed
    pub passed: usize,

    /// Number of tests that failed
    pub failed: usize,

    /// Number of tests that were skipped
    pub skipped: usize,

    /// Total duration in milliseconds
    pub duration_ms: u64,
}

/// Result for an individual test
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResultEntry {
    /// Path to the test file
    pub path: String,

    /// Result of the test
    pub result: TestOutcome,

    /// Time taken to run this test in milliseconds
    pub duration_ms: u64,

    /// Test directives found in the file
    pub directives: Vec<String>,

    /// Specification references
    pub spec_references: Vec<String>,

    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Outcome of running a test
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "details")]
pub enum TestOutcome {
    /// Test passed successfully
    Pass,

    /// Test failed with a reason
    Fail { reason: String },

    /// Test was skipped with a reason
    Skip { reason: String },
}

/// Performance metrics for the test run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    /// Time spent discovering tests
    pub discovery_ms: u64,

    /// Time spent executing tests
    pub execution_ms: u64,

    /// Time spent on snapshot comparisons
    pub snapshot_ms: u64,

    /// Memory usage statistics
    pub memory: MemoryUsage,
}

/// Memory usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryUsage {
    /// Peak memory usage in bytes
    pub peak_bytes: u64,

    /// Average memory usage in bytes
    pub average_bytes: u64,
}

impl TestReport {
    /// Create a new test report
    pub fn new() -> Self {
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            metadata: TestRunMetadata {
                start_time,
                end_time: None,
                command_line: std::env::args().collect(),
                runner_version: env!("CARGO_PKG_VERSION").to_string(),
                compiler_version: None,
                environment: Self::collect_environment(),
            },
            summary: TestSummary {
                total: 0,
                passed: 0,
                failed: 0,
                skipped: 0,
                duration_ms: 0,
            },
            results: Vec::new(),
            performance: PerformanceMetrics {
                discovery_ms: 0,
                execution_ms: 0,
                snapshot_ms: 0,
                memory: MemoryUsage {
                    peak_bytes: 0,
                    average_bytes: 0,
                },
            },
        }
    }

    /// Add a passing test result
    pub fn add_pass(&mut self, path: &Utf8Path) {
        self.add_result(path, TestOutcome::Pass, Vec::new(), Vec::new());
    }

    /// Add a failing test result
    pub fn add_fail(&mut self, path: &Utf8Path, reason: &str) {
        self.add_result(
            path,
            TestOutcome::Fail {
                reason: reason.to_string(),
            },
            Vec::new(),
            Vec::new(),
        );
    }

    /// Add a skipped test result
    pub fn add_skip(&mut self, path: &Utf8Path, reason: &str) {
        self.add_result(
            path,
            TestOutcome::Skip {
                reason: reason.to_string(),
            },
            Vec::new(),
            Vec::new(),
        );
    }

    /// Add a test result with full details
    pub fn add_result(
        &mut self,
        path: &Utf8Path,
        outcome: TestOutcome,
        directives: Vec<String>,
        spec_references: Vec<String>,
    ) {
        let entry = TestResultEntry {
            path: path.to_string(),
            result: outcome.clone(),
            duration_ms: 0, // Will be updated when timing info is available
            directives,
            spec_references,
            metadata: HashMap::new(),
        };

        match outcome {
            TestOutcome::Pass => self.summary.passed += 1,
            TestOutcome::Fail { .. } => self.summary.failed += 1,
            TestOutcome::Skip { .. } => self.summary.skipped += 1,
        }

        self.summary.total += 1;
        self.results.push(entry);
    }

    /// Check if there are any test failures
    pub fn has_failures(&self) -> bool {
        self.summary.failed > 0
    }

    /// Finalize the report (set end time, calculate durations, etc.)
    pub fn finalize(&mut self) {
        self.metadata.end_time = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );

        if let Some(end_time) = self.metadata.end_time {
            self.summary.duration_ms = (end_time - self.metadata.start_time) * 1000;
        }
    }

    /// Write the report as JSON to a file
    pub fn write_json(&mut self, path: &Utf8Path) -> Result<()> {
        self.finalize();

        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize test report to JSON")?;

        fs::write(path, json)
            .with_context(|| format!("Failed to write test report to: {}", path))?;

        tracing::info!("Test report written to: {}", path);
        Ok(())
    }

    /// Print a summary to stdout
    pub fn print_summary(&self) {
        println!("\nTest Results Summary:");
        println!("====================");
        println!("Total:   {}", self.summary.total);
        println!("Passed:  {}", self.summary.passed);
        println!("Failed:  {}", self.summary.failed);
        println!("Skipped: {}", self.summary.skipped);

        if self.summary.total > 0 {
            let pass_rate = (self.summary.passed as f64 / self.summary.total as f64) * 100.0;
            println!("Pass rate: {:.1}%", pass_rate);
        }

        println!("Duration: {}ms", self.summary.duration_ms);

        if self.summary.failed > 0 {
            println!("\nFailures:");
            for result in &self.results {
                if let TestOutcome::Fail { reason } = &result.result {
                    println!("  {} - {}", result.path, reason);
                }
            }
        }
    }

    /// Print a detailed report to stdout
    pub fn print_detailed(&self) {
        self.print_summary();

        println!("\nDetailed Results:");
        println!("================");

        for result in &self.results {
            let status = match &result.result {
                TestOutcome::Pass => "PASS",
                TestOutcome::Fail { .. } => "FAIL",
                TestOutcome::Skip { .. } => "SKIP",
            };

            println!("{}: {}", status, result.path);

            if !result.directives.is_empty() {
                println!("  Directives: {}", result.directives.join(", "));
            }

            if !result.spec_references.is_empty() {
                println!("  Specs: {}", result.spec_references.join(", "));
            }

            match &result.result {
                TestOutcome::Fail { reason } => {
                    println!("  Reason: {}", reason);
                }
                TestOutcome::Skip { reason } => {
                    println!("  Reason: {}", reason);
                }
                TestOutcome::Pass => {}
            }

            println!("  Duration: {}ms", result.duration_ms);
            println!();
        }
    }

    /// Get tests grouped by specification reference
    pub fn tests_by_spec(&self) -> HashMap<String, Vec<&TestResultEntry>> {
        let mut by_spec = HashMap::new();

        for result in &self.results {
            if result.spec_references.is_empty() {
                by_spec
                    .entry("unspecified".to_string())
                    .or_insert_with(Vec::new)
                    .push(result);
            } else {
                for spec_ref in &result.spec_references {
                    by_spec
                        .entry(spec_ref.clone())
                        .or_insert_with(Vec::new)
                        .push(result);
                }
            }
        }

        by_spec
    }

    /// Get pass rate for a specific specification
    pub fn spec_pass_rate(&self, spec_ref: &str) -> Option<f64> {
        let tests_for_spec: Vec<_> = self
            .results
            .iter()
            .filter(|r| r.spec_references.contains(&spec_ref.to_string()))
            .collect();

        if tests_for_spec.is_empty() {
            return None;
        }

        let passed = tests_for_spec
            .iter()
            .filter(|r| matches!(r.result, TestOutcome::Pass))
            .count();

        Some((passed as f64 / tests_for_spec.len() as f64) * 100.0)
    }

    fn collect_environment() -> HashMap<String, String> {
        let mut env = HashMap::new();

        // Collect relevant environment variables
        let vars = ["RUST_LOG", "CARGO_MANIFEST_DIR", "TERM", "USER"];
        for var in &vars {
            if let Ok(value) = std::env::var(var) {
                env.insert(var.to_string(), value);
            }
        }

        // Add system information
        env.insert("os".to_string(), std::env::consts::OS.to_string());
        env.insert("arch".to_string(), std::env::consts::ARCH.to_string());

        env
    }
}

impl Default for TestReport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::NamedTempFile;

    #[test]
    fn test_new_report() {
        let report = TestReport::new();

        assert_eq!(report.summary.total, 0);
        assert_eq!(report.summary.passed, 0);
        assert_eq!(report.summary.failed, 0);
        assert_eq!(report.summary.skipped, 0);
        assert!(!report.has_failures());
    }

    #[test]
    fn test_add_results() {
        let mut report = TestReport::new();

        report.add_pass(&Utf8PathBuf::from("test1.rue"));
        report.add_fail(&Utf8PathBuf::from("test2.rue"), "Type error");
        report.add_skip(&Utf8PathBuf::from("test3.rue"), "Missing dependency");

        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.passed, 1);
        assert_eq!(report.summary.failed, 1);
        assert_eq!(report.summary.skipped, 1);
        assert!(report.has_failures());
    }

    #[test]
    fn test_json_serialization() {
        let mut report = TestReport::new();

        report.add_pass(&Utf8PathBuf::from("test.rue"));

        let temp_file = NamedTempFile::new().unwrap();
        let temp_path = Utf8PathBuf::try_from(temp_file.path().to_path_buf()).unwrap();

        report.write_json(&temp_path).unwrap();

        let content = fs::read_to_string(&temp_path).unwrap();
        let parsed: TestReport = serde_json::from_str(&content).unwrap();

        assert_eq!(parsed.summary.total, 1);
        assert_eq!(parsed.summary.passed, 1);
    }

    #[test]
    fn test_spec_grouping() {
        let mut report = TestReport::new();

        report.add_result(
            &Utf8PathBuf::from("test1.rue"),
            TestOutcome::Pass,
            vec!["compile-pass".to_string()],
            vec!["type-system.inference".to_string()],
        );

        report.add_result(
            &Utf8PathBuf::from("test2.rue"),
            TestOutcome::Pass,
            vec!["run-pass".to_string()],
            vec!["type-system.inference".to_string()],
        );

        let by_spec = report.tests_by_spec();
        assert_eq!(by_spec.get("type-system.inference").unwrap().len(), 2);

        let pass_rate = report.spec_pass_rate("type-system.inference").unwrap();
        assert_eq!(pass_rate, 100.0);
    }
}
