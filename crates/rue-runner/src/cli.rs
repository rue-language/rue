use camino::Utf8PathBuf;
use clap::Parser;

/// Rue test runner - comprehensive testing framework for Rue programs
#[derive(Parser, Debug)]
#[command(name = "rue-runner")]
#[command(about = "Test runner for the Rue programming language")]
#[command(version)]
pub struct Args {
    /// Paths to search for test files (.rue files)
    #[arg(short = 't', long = "test-paths", default_values = ["tests", "examples"])]
    pub test_paths: Vec<Utf8PathBuf>,

    /// Path to the rue compiler binary
    #[arg(short = 'r', long = "rue-binary", default_value = "rue")]
    pub rue_binary: Utf8PathBuf,

    /// Path to the normative specification file
    #[arg(short = 's', long = "spec-file", default_value = "spec/norms.toml")]
    pub spec_file: Utf8PathBuf,

    /// Directory for golden snapshots
    #[arg(long = "snapshot-dir", default_value = "tests/snapshots")]
    pub snapshot_dir: Utf8PathBuf,

    /// Output file for JSON test report
    #[arg(long = "report-file")]
    pub report_file: Option<Utf8PathBuf>,

    /// Update golden snapshots instead of comparing
    #[arg(long = "update-snapshots")]
    pub update_snapshots: bool,

    /// Continue running tests even if some fail
    #[arg(long = "ignore-failures")]
    pub ignore_failures: bool,

    /// Filter tests by regex pattern (matches file paths)
    #[arg(long = "filter")]
    pub filter: Option<String>,

    /// Verbose output
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
}
