//! Shared test runner infrastructure for Rue compiler tests.
//!
//! This crate provides common functionality for running compiler tests,
//! including test case parsing, execution, and output comparison.

pub mod pipe_drain;

use pipe_drain::{PIPE_DRAIN_FINISH_TIMEOUT, spawn_pipe_drain};
use rue_error::{PreviewFeature, error_code_metadata};
use rue_target::Target;
use serde::{Deserialize, Deserializer};

/// Default timeout for test execution in milliseconds (10 seconds).
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// Exit code used by the Rue runtime for runtime errors (division by zero, overflow, etc.).
///
/// This matches the convention used by Rust's test harness and the Rue runtime.
/// When a Rue program encounters a runtime error, it exits with this code.
pub const RUNTIME_ERROR_EXIT_CODE: i32 = 101;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The coordinates of one slice in a sharded test corpus (RUE-1116).
///
/// This type validates and exposes the `INDEX/COUNT` execution contract.
/// Corpus-specific code owns the assignment policy; the CLI harness uses
/// measured case weights rather than assuming equal-cost cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardSelector {
    index: u64,
    count: u64,
}

/// Error parsing a `RUE_*_TEST_SHARD` specification.
#[derive(Debug, PartialEq, Eq)]
pub enum ShardSpecError {
    /// The value was not of the form `INDEX/COUNT`.
    Malformed(String),
    /// `COUNT` was zero (nothing to partition into).
    ZeroCount,
    /// `INDEX` was not strictly less than `COUNT`.
    IndexOutOfRange { index: u64, count: u64 },
}

impl std::fmt::Display for ShardSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShardSpecError::Malformed(value) => write!(
                f,
                "expected shard spec of the form INDEX/COUNT (0-based), got {value:?}"
            ),
            ShardSpecError::ZeroCount => write!(f, "shard COUNT must be non-zero"),
            ShardSpecError::IndexOutOfRange { index, count } => write!(
                f,
                "shard INDEX {index} out of range for COUNT {count} (expected 0..{count})"
            ),
        }
    }
}

impl std::error::Error for ShardSpecError {}

impl ShardSelector {
    /// Parse an optional `INDEX/COUNT` spec (0-based index). A `None` or blank
    /// spec yields `Ok(None)` — the default "run the whole corpus" behavior —
    /// so an unset environment variable is never an error.
    pub fn parse(spec: Option<&str>) -> Result<Option<ShardSelector>, ShardSpecError> {
        let spec = match spec.map(str::trim) {
            None | Some("") => return Ok(None),
            Some(spec) => spec,
        };
        let malformed = || ShardSpecError::Malformed(spec.to_string());
        let (index, count) = spec.split_once('/').ok_or_else(malformed)?;
        let index: u64 = index.trim().parse().map_err(|_| malformed())?;
        let count: u64 = count.trim().parse().map_err(|_| malformed())?;
        if count == 0 {
            return Err(ShardSpecError::ZeroCount);
        }
        if index >= count {
            return Err(ShardSpecError::IndexOutOfRange { index, count });
        }
        Ok(Some(ShardSelector { index, count }))
    }

    /// Parse the spec from environment variable `var`. Unset or blank yields
    /// `Ok(None)`.
    pub fn from_env(var: &str) -> Result<Option<ShardSelector>, ShardSpecError> {
        match std::env::var(var) {
            Ok(value) => ShardSelector::parse(Some(&value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(ShardSpecError::Malformed("<non-unicode>".to_string()))
            }
        }
    }

    /// The 0-based shard index.
    pub fn index(&self) -> u64 {
        self.index
    }

    /// The total number of shards.
    pub fn count(&self) -> u64 {
        self.count
    }
}

#[cfg(test)]
mod shard_selector_tests {
    use super::*;

    #[test]
    fn unset_or_blank_yields_no_selector() {
        assert_eq!(ShardSelector::parse(None), Ok(None));
        assert_eq!(ShardSelector::parse(Some("")), Ok(None));
        assert_eq!(ShardSelector::parse(Some("   ")), Ok(None));
    }

    #[test]
    fn accepts_well_formed_spec() {
        let selector = ShardSelector::parse(Some("2/4")).unwrap().unwrap();
        assert_eq!(selector.index(), 2);
        assert_eq!(selector.count(), 4);
        // Surrounding whitespace is tolerated.
        assert_eq!(
            ShardSelector::parse(Some(" 0 / 3 "))
                .unwrap()
                .unwrap()
                .count(),
            3
        );
    }

    #[test]
    fn rejects_malformed_and_out_of_range() {
        assert!(matches!(
            ShardSelector::parse(Some("1")),
            Err(ShardSpecError::Malformed(_))
        ));
        assert!(matches!(
            ShardSelector::parse(Some("a/4")),
            Err(ShardSpecError::Malformed(_))
        ));
        assert_eq!(
            ShardSelector::parse(Some("0/0")),
            Err(ShardSpecError::ZeroCount)
        );
        assert_eq!(
            ShardSelector::parse(Some("4/4")),
            Err(ShardSpecError::IndexOutOfRange { index: 4, count: 4 })
        );
    }
}

/// A section header in a test file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub id: String,
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub description: String,
    /// Optional reference to spec chapter (e.g., "3.1")
    #[allow(dead_code)]
    #[serde(default)]
    pub spec_chapter: Option<String>,
}

/// A parameter set for parameterized tests.
///
/// Each parameter set generates one test instance. Parameters can:
/// - Provide values for `{placeholder}` substitution in string fields
/// - Override case fields like `exit_code`, `compile_fail`, etc.
/// - Add extra spec references via `spec_extra`
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ParamSet {
    /// All parameter values as a flat map.
    /// Special keys: `exit_code`, `compile_fail`, `skip`, `spec_extra`, etc.
    /// Other keys are used for `{key}` substitution in templates.
    /// String values may reference other parameters; dependencies are resolved
    /// deterministically before any case field is expanded.
    #[serde(flatten)]
    pub values: HashMap<String, toml::Value>,
}

/// Wrapper type for `error_contains` that can be either a single string or an array.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ErrorContains(pub Vec<String>);

impl ErrorContains {
    /// Returns true if there are no expected error substrings.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns an iterator over the expected error substrings.
    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.0.iter()
    }
}

impl<'de> Deserialize<'de> for ErrorContains {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{self, Visitor};

        struct ErrorContainsVisitor;

        impl<'de> Visitor<'de> for ErrorContainsVisitor {
            type Value = ErrorContains;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or array of strings")
            }

            fn visit_str<E>(self, value: &str) -> Result<ErrorContains, E>
            where
                E: de::Error,
            {
                Ok(ErrorContains(vec![value.to_string()]))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<ErrorContains, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<String>()? {
                    values.push(value);
                }
                Ok(ErrorContains(values))
            }
        }

        deserializer.deserialize_any(ErrorContainsVisitor)
    }
}

/// A single test case.
///
/// # Golden-IR assertions vs execution assertions
///
/// Golden-IR assertions (`expected_tokens`, `expected_ast`, `expected_rir`,
/// `expected_air`, `expected_cfg`, `expected_mir`, `expected_lowering`,
/// `expected_liveness`, `expected_regalloc`, `expected_asm`,
/// `expected_stackframe`, and `expected_abi`) are checked by running
/// `rue --emit <stage>` and comparing the dump. Execution assertions
/// (`exit_code`, `expected_stdout`, `runtime_error`, warning checks, ...)
/// are checked by compiling and running the program.
///
/// A case may combine both: ALL of its assertions are verified — the golden
/// comparisons run first, then the program is compiled and executed for the
/// execution assertions. (Previously the execution assertions were silently
/// skipped when a golden assertion was present; see RUE-132.)
/// `compile_fail` cannot be combined with golden-IR assertions, since a
/// program that fails to compile has no IR to dump.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub name: String,
    /// Human-readable explanation of what this case pins and why. Not used by
    /// the runner; it exists so case files can document intent inline.
    #[allow(dead_code)]
    #[serde(default)]
    pub description: Option<String>,
    pub source: String,
    /// Expected exit code (for successful compilation)
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// If true, compilation should fail
    #[serde(default)]
    pub compile_fail: bool,
    /// If true, only compile (don't run) - useful for infinite loops
    #[serde(default)]
    pub compile_only: bool,
    /// Substring(s) that should appear in the error message.
    /// Can be a single string or an array of strings.
    #[serde(default)]
    pub error_contains: ErrorContains,
    /// Expected exact error output (golden test)
    #[serde(default)]
    pub expected_error: Option<String>,
    /// Expected canonical code of the single emitted compile error.
    #[serde(default)]
    pub expected_error_code: Option<String>,
    /// Expected tokens dump (golden test)
    #[serde(default)]
    pub expected_tokens: Option<String>,
    /// Expected AST dump (golden test)
    #[serde(default)]
    pub expected_ast: Option<String>,
    /// Expected RIR dump (golden test)
    #[serde(default)]
    pub expected_rir: Option<String>,
    /// Expected AIR dump (golden test)
    #[serde(default)]
    pub expected_air: Option<String>,
    /// Expected MIR dump (golden test)
    #[serde(default)]
    pub expected_mir: Option<String>,
    /// Expected lowering dump (golden test)
    #[serde(default)]
    pub expected_lowering: Option<String>,
    /// Expected liveness dump (golden test)
    #[serde(default)]
    pub expected_liveness: Option<String>,
    /// Expected register allocation dump (golden test)
    #[serde(default)]
    pub expected_regalloc: Option<String>,
    /// Expected assembly dump (golden test)
    #[serde(default)]
    pub expected_asm: Option<String>,
    /// Expected stack frame dump (golden test)
    #[serde(default)]
    pub expected_stackframe: Option<String>,
    /// Expected calling-convention/placement report (golden test)
    #[serde(default)]
    pub expected_abi: Option<String>,
    /// Expected CFG dump (golden test)
    #[serde(default)]
    pub expected_cfg: Option<String>,
    /// Expected runtime error message (program compiles but fails at runtime)
    #[serde(default)]
    pub runtime_error: Option<String>,
    /// Expected exit code for runtime errors (defaults to [`RUNTIME_ERROR_EXIT_CODE`])
    #[serde(default)]
    pub runtime_exit_code: Option<i32>,
    /// Skip this test
    #[serde(default)]
    pub skip: bool,
    /// Substrings that should appear in warning messages
    #[serde(default)]
    pub warning_contains: Option<Vec<String>>,
    /// Expected number of warnings
    #[serde(default)]
    pub expected_warning_count: Option<usize>,
    /// If true, verify no warnings were emitted
    #[serde(default)]
    pub no_warnings: bool,
    /// Spec paragraph references (e.g., ["3.1:1", "3.1:2"])
    #[allow(dead_code)]
    #[serde(default)]
    pub spec: Vec<String>,
    /// Expected stdout output after successful execution (e.g., from @dbg calls)
    #[serde(default)]
    pub expected_stdout: Option<String>,
    /// Preview feature required to run this test (e.g., "mutable_strings").
    /// Tests with this field are compiled with `--preview <feature>` and
    /// are allowed to fail without failing the overall test suite,
    /// unless `preview_should_pass` is true.
    #[serde(default)]
    pub preview: Option<String>,
    /// If true, this preview test should pass and will fail the suite if it doesn't.
    /// Use this to mark preview tests that are expected to work after implementation.
    /// This provides real test output for implemented portions of preview features.
    #[serde(default)]
    pub preview_should_pass: bool,
    /// If true, compile this case with the repository's real standard library.
    /// The default remains an isolated environment with `RUE_STD_PATH` removed.
    #[serde(default)]
    pub real_std: bool,
    /// Target architecture (e.g., "x86-64-linux", "aarch64-macos").
    /// When specified, the compiler is invoked with `--target <target>`.
    /// Required for target-specific golden tests; optional for other test types.
    #[serde(default)]
    pub target: Option<String>,
    /// Optimization level (0, 1, 2, or 3).
    /// When specified, the compiler is invoked with `-O<level>`.
    /// Defaults to 0 (no optimization) if not specified.
    #[serde(default)]
    pub opt_level: Option<u8>,
    /// Timeout for test execution in milliseconds.
    /// Defaults to [`DEFAULT_TIMEOUT_MS`] if not specified.
    /// If the test exceeds this timeout, it will be killed and marked as failed.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Input to provide to the program's stdin during execution.
    /// This is useful for testing programs that read from stdin (e.g., @read_line).
    /// The input is piped to the program before execution starts.
    #[serde(default)]
    pub stdin: Option<String>,
    /// Expected stderr output (substring match).
    /// For runtime errors, use `runtime_error` instead. This field is for
    /// checking stderr content in successful runs (e.g., panic messages).
    #[serde(default)]
    pub stderr_contains: Option<String>,
    /// Parameter sets for generating multiple test instances from a template.
    /// When present, this case is expanded into multiple cases, one per param set.
    /// Template placeholders like `{type}` in `source` and `name` are substituted.
    #[serde(default)]
    pub params: Vec<ParamSet>,
    /// Auxiliary source files for multi-file tests (for module imports).
    /// Each entry maps a relative filename to its source content.
    /// Example: `{ "math.rue" = "pub fn add(a: i32, b: i32) -> i32 { a + b }" }`
    #[serde(default)]
    pub aux_files: HashMap<String, String>,
    /// List of target triples on which this test should run.
    /// If specified, the test is skipped on hosts that don't match any of the targets.
    /// Example: `only_on = ["x86-64-linux", "aarch64-linux"]`
    /// If not specified, the test runs on all platforms.
    #[serde(default)]
    pub only_on: Vec<String>,
}

impl Case {
    /// Whether this case carries any golden-IR assertions (checked against
    /// `rue --emit <stage>` output).
    pub fn has_golden_ir_assertions(&self) -> bool {
        self.expected_tokens.is_some()
            || self.expected_ast.is_some()
            || self.expected_rir.is_some()
            || self.expected_air.is_some()
            || self.expected_cfg.is_some()
            || self.expected_mir.is_some()
            || self.expected_lowering.is_some()
            || self.expected_liveness.is_some()
            || self.expected_regalloc.is_some()
            || self.expected_asm.is_some()
            || self.expected_stackframe.is_some()
            || self.expected_abi.is_some()
    }

    /// Whether this case carries any golden-IR assertion whose output depends
    /// on backend lowering or target-specific code generation.
    pub fn has_target_specific_golden_ir_assertions(&self) -> bool {
        self.expected_mir.is_some()
            || self.expected_lowering.is_some()
            || self.expected_liveness.is_some()
            || self.expected_regalloc.is_some()
            || self.expected_asm.is_some()
            || self.expected_stackframe.is_some()
            || self.expected_abi.is_some()
    }

    /// Whether verifying this case requires *running* the produced binary, as
    /// opposed to only building it.
    ///
    /// `compile_only` stops at the executable, and the warning assertions are
    /// compile-time. Only these assertions need a host that can execute the
    /// case's target, which is what makes cross-compilation to a foreign
    /// architecture legal for some cases and impossible for others.
    pub fn requires_program_execution(&self) -> bool {
        !self.compile_only
            && (self.exit_code.is_some()
                || self.expected_stdout.is_some()
                || self.runtime_error.is_some()
                || self.runtime_exit_code.is_some()
                || self.stdin.is_some()
                || self.stderr_contains.is_some())
    }

    /// The names of the backend-specific golden assertions this case sets, in
    /// declaration order. Used to name the offending fields when a case pins
    /// architecture-specific output without declaring its `target`.
    pub fn target_specific_golden_ir_fields(&self) -> Vec<&'static str> {
        [
            ("expected_mir", self.expected_mir.is_some()),
            ("expected_lowering", self.expected_lowering.is_some()),
            ("expected_liveness", self.expected_liveness.is_some()),
            ("expected_regalloc", self.expected_regalloc.is_some()),
            ("expected_asm", self.expected_asm.is_some()),
            ("expected_stackframe", self.expected_stackframe.is_some()),
            ("expected_abi", self.expected_abi.is_some()),
        ]
        .into_iter()
        .filter_map(|(field, present)| present.then_some(field))
        .collect()
    }

    /// Whether this case carries assertions that require actually compiling
    /// the program to a binary (and, unless `compile_only`, running it).
    ///
    /// Used so a case combining golden-IR assertions with execution
    /// assertions verifies BOTH, instead of silently skipping the execution
    /// half (RUE-132).
    pub fn has_execution_assertions(&self) -> bool {
        self.exit_code.is_some()
            || self.expected_stdout.is_some()
            || self.runtime_error.is_some()
            || self.runtime_exit_code.is_some()
            || self.stdin.is_some()
            || self.stderr_contains.is_some()
            || self.compile_only
            || self.no_warnings
            || self.expected_warning_count.is_some()
            || self.warning_contains.is_some()
    }
}

/// A test file containing a section and its cases.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestFile {
    pub section: Section,
    #[serde(default)]
    pub case: Vec<Case>,
}

/// A test failure with a class that expected-failure wrappers cannot erase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFailure {
    message: String,
    fatal: bool,
}

impl TestFailure {
    /// A normal expectation mismatch that an explicit xfail may absorb.
    pub fn assertion(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: false,
        }
    }

    /// An infrastructure failure, timeout, signal, panic, or compiler ICE.
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: true,
        }
    }

    /// Whether this failure must fail even an expected-failure test.
    pub fn is_fatal(&self) -> bool {
        self.fatal
    }

    /// Add context without losing the failure class.
    pub fn with_context(mut self, context: impl std::fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }
}

impl std::fmt::Display for TestFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TestFailure {}

impl std::ops::Deref for TestFailure {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

/// Result of running a test or one of its subprocesses.
pub type TestResult<T = ()> = Result<T, TestFailure>;

/// The three possible outcomes of running a case marked as expected to fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedFailureOutcome {
    UnexpectedPass,
    ExpectedFailure(TestFailure),
    FatalFailure(TestFailure),
}

/// Classify an expected-failure result without allowing fatal errors to hide.
pub fn classify_expected_failure(result: TestResult) -> ExpectedFailureOutcome {
    match result {
        Ok(()) => ExpectedFailureOutcome::UnexpectedPass,
        Err(error) if error.is_fatal() => ExpectedFailureOutcome::FatalFailure(error),
        Err(error) => ExpectedFailureOutcome::ExpectedFailure(error),
    }
}

/// Get the current host target triple in Rue's format.
///
/// Returns strings like "x86-64-linux", "aarch64-linux", "aarch64-macos".
pub fn get_host_target() -> &'static str {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        Target::X86_64Linux.name()
    }
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        Target::Aarch64Linux.name()
    }
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        Target::Aarch64Macos.name()
    }
    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    {
        "x86-64-macos"
    }
    #[cfg(not(any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
    )))]
    {
        "unknown"
    }
}

/// Every platform name `only_on`/`known_bug_on` may legally name — the same
/// set [`get_host_target`] can return. Case files are validated against this
/// list at load time: a typo'd platform name would otherwise make the case
/// silently run NOWHERE while still counting as spec coverage (the RUE-132
/// skipped-test-counts-as-coverage class, via the platform axis).
pub const KNOWN_TARGETS: &[&str] = &[
    Target::X86_64Linux.name(),
    Target::Aarch64Linux.name(),
    Target::Aarch64Macos.name(),
    // Host-only: Rue can run tests on Intel macOS, but does not currently have
    // an x86-64 macOS compiler target.
    "x86-64-macos",
];

/// Select which declarative platform cases a test harness registers.
///
/// Required native CI uses `native` to run every case whose `only_on` list
/// includes the current host, without duplicating the target-independent
/// corpus. Keeping this selection in the shared manifest layer means a newly
/// added platform-scoped case is picked up automatically.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlatformCaseSelection {
    #[default]
    All,
    Native,
}

impl PlatformCaseSelection {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("") | Some("all") => Ok(Self::All),
            Some("native") => Ok(Self::Native),
            Some(other) => Err(format!(
                "unknown RUE_PLATFORM_CASE_SELECTION {other:?} (expected all or native)"
            )),
        }
    }

    pub fn from_env() -> Result<Self, String> {
        match std::env::var("RUE_PLATFORM_CASE_SELECTION") {
            Ok(value) => Self::parse(Some(&value)),
            Err(std::env::VarError::NotPresent) => Self::parse(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn includes(self, only_on: &[String]) -> bool {
        match self {
            Self::All => true,
            Self::Native => !only_on.is_empty() && should_skip_for_platform(only_on).is_none(),
        }
    }
}

/// The platforms a required CI lane actually executes cases on.
///
/// This is the executable half of the platform responsibility matrix
/// (RUE-1160/RUE-1161): `x86-64-linux` runs the complete target-independent
/// corpus, and the two native lanes run every `only_on`-scoped case for their
/// host. `x86-64-macos` is in [`KNOWN_TARGETS`] because a developer can run the
/// suite on an Intel Mac, but no required lane does — a case scoped only to it
/// executes nowhere in CI, so it must not be credited as specification coverage.
///
/// `scripts/validate-ci-gate.py` keeps this list and `.github/workflows/ci.yml`
/// in lockstep, so adding a lane (or dropping one) cannot silently diverge from
/// what the harness believes CI covers.
pub const CI_EXECUTED_TARGETS: &[&str] = &["x86-64-linux", "aarch64-linux", "aarch64-macos"];

/// The architecture component of a platform name in [`KNOWN_TARGETS`], i.e.
/// everything before the trailing `-<os>` (`x86-64-linux` -> `x86-64`).
///
/// Returns `None` for a name that is not a known platform; callers reach this
/// only after [`validate_only_on_targets`] has accepted the name.
pub fn target_architecture(target: &str) -> Option<&str> {
    if !KNOWN_TARGETS.contains(&target) {
        return None;
    }
    target.rsplit_once('-').map(|(arch, _os)| arch)
}

/// Whether a case scoped by `only_on` executes on at least one platform that
/// required CI runs.
///
/// An empty `only_on` means the case is target-independent and runs everywhere,
/// including the Linux-complete lane.
pub fn runs_on_required_ci(only_on: &[String]) -> bool {
    only_on.is_empty()
        || only_on
            .iter()
            .any(|platform| CI_EXECUTED_TARGETS.contains(&platform.as_str()))
}

/// Which lane is responsible for executing a case (RUE-1161).
///
/// The classification is structural — derived from the assertions a case
/// actually makes — so it cannot drift from what the case does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformResponsibility {
    /// Target-independent: diagnostics, semantic assertions, and golden IR up
    /// to AIR/CFG. The Linux-complete lane owns these; running them again on
    /// every native host would be three copies of one result.
    Semantic,
    /// Compiles and runs a real program for the host's native target. The
    /// Linux-complete lane owns the unscoped ones; an `only_on` list hands the
    /// case to the matching native lane as well.
    Native,
    /// Pins architecture-specific output for an explicitly declared `target`.
    /// Cross-compiled emission is host-independent and stays on the
    /// Linux-complete lane; a backend case that also *executes* belongs to the
    /// native lane for its architecture.
    Backend,
}

/// A case whose platform responsibility cannot be determined from its metadata.
///
/// Each variant describes a declaration that leaves it genuinely undecidable
/// which architecture a case is about, or that asks a host to execute a program
/// built for a different one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformResponsibilityAmbiguity {
    /// Backend-specific golden output with no declared `target`: the expected
    /// text is one architecture's, but nothing says which, so the case silently
    /// pins whatever the compiler's default target happens to be.
    UndeclaredArchitecture { fields: String },
    /// A `target` whose architecture differs from a host in `only_on`. The case
    /// claims two architectures at once.
    ScopeArchitectureMismatch { target: String, platform: String },
    /// A `target` combined with assertions that run the program, and no
    /// `only_on` scope: the case builds for one architecture and then asks
    /// whichever host is running the suite to execute the result.
    UnscopedForeignExecution { target: String },
}

/// A [`PlatformResponsibilityAmbiguity`] located in a specific case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformResponsibilityError {
    /// The name of the offending test case.
    pub test_name: String,
    /// The section ID the test belongs to.
    pub section_id: String,
    /// What makes the case's platform responsibility undecidable.
    pub ambiguity: PlatformResponsibilityAmbiguity,
}

impl std::fmt::Display for PlatformResponsibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "test '{}::{}' ", self.section_id, self.test_name)?;
        match &self.ambiguity {
            PlatformResponsibilityAmbiguity::UndeclaredArchitecture { fields } => write!(
                f,
                "asserts backend-specific golden output ({fields}) without declaring \
                 `target`. That output belongs to one architecture; add \
                 `target = \"<arch>-<os>\"` so the case declares which."
            ),
            PlatformResponsibilityAmbiguity::ScopeArchitectureMismatch { target, platform } => {
                write!(
                    f,
                    "declares `target = \"{target}\"` but is scoped to `only_on` platform \
                     '{platform}' of a different architecture. Scope the case to hosts of \
                     its own architecture, or drop the mismatched entry."
                )
            }
            PlatformResponsibilityAmbiguity::UnscopedForeignExecution { target } => write!(
                f,
                "declares `target = \"{target}\"` and asserts runtime behavior, but no \
                 `only_on` scope. Whichever host runs the suite would be asked to execute a \
                 program built for {target}. Add `only_on` listing the hosts of that \
                 architecture, or use `compile_only` and keep it a cross-compilation case."
            ),
        }
    }
}

impl std::error::Error for PlatformResponsibilityError {}

/// Classify which lane owns executing `case`, or explain why its platform
/// responsibility is ambiguous.
pub fn classify_platform_responsibility(
    case: &Case,
) -> Result<PlatformResponsibility, PlatformResponsibilityAmbiguity> {
    let executes = case.has_execution_assertions();

    if case.has_target_specific_golden_ir_assertions() && case.target.is_none() {
        return Err(PlatformResponsibilityAmbiguity::UndeclaredArchitecture {
            fields: case.target_specific_golden_ir_fields().join(", "),
        });
    }

    let Some(target) = case.target.as_deref() else {
        return Ok(if executes {
            PlatformResponsibility::Native
        } else {
            PlatformResponsibility::Semantic
        });
    };

    // An unknown `target` is the compiler driver's error to report, not a
    // responsibility ambiguity; classification stays silent about it.
    if let Some(target_arch) = target_architecture(target) {
        for platform in &case.only_on {
            if target_architecture(platform).is_some_and(|arch| arch != target_arch) {
                return Err(PlatformResponsibilityAmbiguity::ScopeArchitectureMismatch {
                    target: target.to_string(),
                    platform: platform.clone(),
                });
            }
        }
    }

    // Cross-compiling and stopping at the executable is host-independent; only
    // running the result needs a host of the declared architecture.
    if case.requires_program_execution() && case.only_on.is_empty() {
        return Err(PlatformResponsibilityAmbiguity::UnscopedForeignExecution {
            target: target.to_string(),
        });
    }

    Ok(PlatformResponsibility::Backend)
}

/// Validate that every case in a file declares an unambiguous platform
/// responsibility. Returns one error per offending case.
pub fn validate_platform_responsibility(test_file: &TestFile) -> Vec<PlatformResponsibilityError> {
    test_file
        .case
        .iter()
        .filter_map(|case| {
            classify_platform_responsibility(case)
                .err()
                .map(|ambiguity| PlatformResponsibilityError {
                    test_name: case.name.clone(),
                    section_id: test_file.section.id.clone(),
                    ambiguity,
                })
        })
        .collect()
}

/// Check if a test should be skipped based on `only_on` restrictions.
///
/// Returns `Some(reason)` if the test should be skipped, `None` if it should run.
pub fn should_skip_for_platform(only_on: &[String]) -> Option<String> {
    if only_on.is_empty() {
        return None;
    }

    let host = get_host_target();
    if only_on.iter().any(|target| target == host) {
        None
    } else {
        Some(format!(
            "test only runs on {:?}, current host is {}",
            only_on, host
        ))
    }
}

/// Convert a TOML value to a string for template substitution.
fn toml_value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        // Arrays and tables are stringified as TOML
        other => other.to_string(),
    }
}

/// Substitute `{key}` placeholders in a string with values from the param set.
fn substitute_placeholders(template: &str, params: &HashMap<String, toml::Value>) -> String {
    let mut keys = params.keys().collect::<Vec<_>>();
    keys.sort_unstable();
    let mut result = String::with_capacity(template.len());
    let mut cursor = 0;
    while cursor < template.len() {
        let Some((start, key, placeholder_len)) = find_next_placeholder(template, cursor, &keys)
        else {
            result.push_str(&template[cursor..]);
            break;
        };
        result.push_str(&template[cursor..start]);
        result.push_str(&toml_value_to_string(&params[key]));
        cursor = start + placeholder_len;
    }
    result
}

/// Find the earliest exact known `{key}` token after `cursor`. Sorting the
/// candidate keys makes equal-position matches deterministic, while searching
/// for complete tokens avoids interpreting ordinary surrounding braces as
/// placeholders.
fn find_next_placeholder<'a>(
    template: &str,
    cursor: usize,
    keys: &[&'a String],
) -> Option<(usize, &'a str, usize)> {
    keys.iter()
        .filter_map(|key| {
            let placeholder = format!("{{{key}}}");
            template[cursor..]
                .find(&placeholder)
                .map(|offset| (cursor + offset, key.as_str(), placeholder.len()))
        })
        .min_by_key(|(start, _, _)| *start)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParamPlaceholderError {
    Unknown { key: String, referenced_by: String },
    Cycle { path: Vec<String> },
}

impl std::fmt::Display for ParamPlaceholderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { key, referenced_by } => write!(
                f,
                "parameter '{referenced_by}' references unknown placeholder '{{{key}}}'"
            ),
            Self::Cycle { path } => write!(
                f,
                "parameter placeholder cycle: {}",
                path.iter()
                    .map(|key| format!("{{{key}}}"))
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
        }
    }
}

fn is_placeholder_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|first| {
        (first == '_' || first.is_ascii_alphabetic())
            && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
    })
}

fn resolve_template(
    template: &str,
    referenced_by: &str,
    params: &HashMap<String, toml::Value>,
    resolved: &mut HashMap<String, toml::Value>,
    active: &mut Vec<String>,
) -> Result<String, ParamPlaceholderError> {
    let mut scan = 0;
    while let Some(start_offset) = template[scan..].find('{') {
        let start = scan + start_offset;
        let Some(end_offset) = template[start + 1..].find(['{', '}']) else {
            break;
        };
        if template.as_bytes()[start + 1 + end_offset] != b'}' {
            scan = start + 1;
            continue;
        }
        let reference = &template[start + 1..start + 1 + end_offset];
        if is_placeholder_key(reference) && !params.contains_key(reference) {
            return Err(ParamPlaceholderError::Unknown {
                key: reference.to_string(),
                referenced_by: referenced_by.to_string(),
            });
        }
        scan = start + 2 + end_offset;
    }

    let mut keys = params.keys().collect::<Vec<_>>();
    keys.sort_unstable();
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0;
    while cursor < template.len() {
        let Some((start, reference, placeholder_len)) =
            find_next_placeholder(template, cursor, &keys)
        else {
            output.push_str(&template[cursor..]);
            break;
        };
        output.push_str(&template[cursor..start]);
        let reference_value = resolve_param_value(reference, params, resolved, active)?;
        output.push_str(&toml_value_to_string(&reference_value));
        cursor = start + placeholder_len;
    }
    Ok(output)
}

fn resolve_nested_value(
    value: toml::Value,
    params: &HashMap<String, toml::Value>,
    resolved: &mut HashMap<String, toml::Value>,
    active: &mut Vec<String>,
) -> Result<toml::Value, ParamPlaceholderError> {
    match value {
        toml::Value::String(template) => Ok(toml::Value::String(resolve_template(
            &template,
            "nested parameter value",
            params,
            resolved,
            active,
        )?)),
        toml::Value::Array(values) => Ok(toml::Value::Array(
            values
                .into_iter()
                .map(|value| resolve_nested_value(value, params, resolved, active))
                .collect::<Result<_, _>>()?,
        )),
        toml::Value::Table(values) => Ok(toml::Value::Table(
            values
                .into_iter()
                .map(|(name, value)| {
                    Ok((name, resolve_nested_value(value, params, resolved, active)?))
                })
                .collect::<Result<_, ParamPlaceholderError>>()?,
        )),
        other => Ok(other),
    }
}

fn resolve_param_value(
    key: &str,
    params: &HashMap<String, toml::Value>,
    resolved: &mut HashMap<String, toml::Value>,
    active: &mut Vec<String>,
) -> Result<toml::Value, ParamPlaceholderError> {
    if let Some(value) = resolved.get(key) {
        return Ok(value.clone());
    }
    if let Some(position) = active.iter().position(|active_key| active_key == key) {
        let mut path = active[position..].to_vec();
        path.push(key.to_string());
        return Err(ParamPlaceholderError::Cycle { path });
    }
    let value = params
        .get(key)
        .expect("resolving a parameter that was already checked")
        .clone();
    active.push(key.to_string());
    let value = match value {
        toml::Value::String(template) => {
            toml::Value::String(resolve_template(&template, key, params, resolved, active)?)
        }
        other => resolve_nested_value(other, params, resolved, active)?,
    };
    active.pop();
    resolved.insert(key.to_string(), value.clone());
    Ok(value)
}

fn resolve_param_values(
    params: &HashMap<String, toml::Value>,
) -> Result<HashMap<String, toml::Value>, ParamPlaceholderError> {
    let mut resolved = HashMap::new();
    let mut keys = params.keys().collect::<Vec<_>>();
    keys.sort_unstable();
    for key in keys {
        resolve_param_value(key, params, &mut resolved, &mut Vec::new())?;
    }
    Ok(resolved)
}

fn substitute_optional_string(
    value: &Option<String>,
    params: &HashMap<String, toml::Value>,
) -> Option<String> {
    value
        .as_ref()
        .map(|value| substitute_placeholders(value, params))
}

fn substitute_string_vec(values: &[String], params: &HashMap<String, toml::Value>) -> Vec<String> {
    values
        .iter()
        .map(|value| substitute_placeholders(value, params))
        .collect()
}

fn substitute_optional_string_vec(
    values: &Option<Vec<String>>,
    params: &HashMap<String, toml::Value>,
) -> Option<Vec<String>> {
    values
        .as_ref()
        .map(|values| substitute_string_vec(values, params))
}

fn substitute_string_map(
    values: &HashMap<String, String>,
    params: &HashMap<String, toml::Value>,
) -> HashMap<String, String> {
    values
        .iter()
        .map(|(key, value)| {
            (
                substitute_placeholders(key, params),
                substitute_placeholders(value, params),
            )
        })
        .collect()
}

const PARAM_OVERRIDE_KEYS: &[&str] = &[
    "exit_code",
    "compile_fail",
    "compile_only",
    "skip",
    "runtime_exit_code",
    "no_warnings",
    "opt_level",
    "target",
    "preview",
    "preview_should_pass",
    "timeout_ms",
    "error_contains",
    "expected_error",
    "expected_error_code",
    "expected_tokens",
    "expected_ast",
    "expected_rir",
    "expected_air",
    "expected_cfg",
    "expected_mir",
    "expected_lowering",
    "expected_liveness",
    "expected_regalloc",
    "expected_asm",
    "expected_stackframe",
    "expected_abi",
    "warning_contains",
    "expected_warning_count",
    "spec_extra",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct InvalidParamOverrideError {
    param_index: usize,
    key: String,
    expected: &'static str,
    actual: String,
}

impl std::fmt::Display for InvalidParamOverrideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "parameter set #{} override '{}' must be {}, got {}",
            self.param_index, self.key, self.expected, self.actual
        )
    }
}

fn is_string_array(value: &toml::Value) -> bool {
    matches!(value, toml::Value::Array(values) if values.iter().all(toml::Value::is_str))
}

fn invalid_param_override_expectation(key: &str, value: &toml::Value) -> Option<&'static str> {
    let valid = match key {
        "exit_code" | "runtime_exit_code" => value
            .as_integer()
            .is_some_and(|value| i32::try_from(value).is_ok()),
        "compile_fail" | "compile_only" | "skip" | "no_warnings" | "preview_should_pass" => {
            value.is_bool()
        }
        "opt_level" => value
            .as_integer()
            .is_some_and(|value| (0..=3).contains(&value)),
        "target"
        | "preview"
        | "expected_error"
        | "expected_error_code"
        | "expected_tokens"
        | "expected_ast"
        | "expected_rir"
        | "expected_air"
        | "expected_cfg"
        | "expected_mir"
        | "expected_lowering"
        | "expected_liveness"
        | "expected_regalloc"
        | "expected_asm"
        | "expected_stackframe"
        | "expected_abi" => value.is_str(),
        "timeout_ms" => value
            .as_integer()
            .is_some_and(|value| u64::try_from(value).is_ok()),
        "error_contains" | "warning_contains" => value.is_str() || is_string_array(value),
        "expected_warning_count" => value
            .as_integer()
            .is_some_and(|value| usize::try_from(value).is_ok()),
        "spec_extra" => is_string_array(value),
        _ => unreachable!("every reserved parameter override key must have a value schema"),
    };

    if valid {
        return None;
    }

    Some(match key {
        "exit_code" | "runtime_exit_code" => "an integer in the i32 range",
        "compile_fail" | "compile_only" | "skip" | "no_warnings" | "preview_should_pass" => {
            "a boolean"
        }
        "opt_level" => "an integer from 0 through 3",
        "target"
        | "preview"
        | "expected_error"
        | "expected_error_code"
        | "expected_tokens"
        | "expected_ast"
        | "expected_rir"
        | "expected_air"
        | "expected_cfg"
        | "expected_mir"
        | "expected_lowering"
        | "expected_liveness"
        | "expected_regalloc"
        | "expected_asm"
        | "expected_stackframe"
        | "expected_abi" => "a string",
        "timeout_ms" | "expected_warning_count" => "a non-negative integer",
        "error_contains" | "warning_contains" => "a string or an array of strings",
        "spec_extra" => "an array of strings",
        _ => unreachable!("all parameter override keys are handled above"),
    })
}

fn invalid_param_overrides(case: &Case) -> Vec<InvalidParamOverrideError> {
    let mut errors = Vec::new();
    for (param_index, param_set) in case.params.iter().enumerate() {
        for (key, value) in &param_set.values {
            if !PARAM_OVERRIDE_KEYS.contains(&key.as_str()) {
                continue;
            }
            if let Some(expected) = invalid_param_override_expectation(key, value) {
                errors.push(InvalidParamOverrideError {
                    param_index: param_index + 1,
                    key: key.clone(),
                    expected,
                    actual: format!("{value:?}"),
                });
            }
        }
    }
    errors.sort_by(|a, b| (a.param_index, &a.key).cmp(&(b.param_index, &b.key)));
    errors
}

fn contains_placeholder(template: &str, key: &str) -> bool {
    template.contains(&format!("{{{key}}}"))
}

fn value_contains_placeholder(value: &toml::Value, key: &str) -> bool {
    match value {
        toml::Value::String(value) => contains_placeholder(value, key),
        toml::Value::Array(values) => values
            .iter()
            .any(|value| value_contains_placeholder(value, key)),
        toml::Value::Table(values) => values
            .values()
            .any(|value| value_contains_placeholder(value, key)),
        _ => false,
    }
}

fn case_contains_placeholder(case: &Case, key: &str) -> bool {
    contains_placeholder(&case.name, key)
        || case
            .description
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || contains_placeholder(&case.source, key)
        || case
            .error_contains
            .iter()
            .any(|value| contains_placeholder(value, key))
        || case
            .expected_error
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_error_code
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_tokens
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_ast
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_rir
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_air
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_mir
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_lowering
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_liveness
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_regalloc
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_asm
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_stackframe
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_abi
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .expected_cfg
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .runtime_error
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .warning_contains
            .as_ref()
            .is_some_and(|values| values.iter().any(|value| contains_placeholder(value, key)))
        || case
            .spec
            .iter()
            .any(|value| contains_placeholder(value, key))
        || case
            .expected_stdout
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .preview
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .target
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .stdin
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case
            .stderr_contains
            .as_ref()
            .is_some_and(|value| contains_placeholder(value, key))
        || case.aux_files.iter().any(|(path, source)| {
            contains_placeholder(path, key) || contains_placeholder(source, key)
        })
        || case
            .only_on
            .iter()
            .any(|value| contains_placeholder(value, key))
}

fn param_values_contain_placeholder(param_set: &ParamSet, key: &str) -> bool {
    param_set
        .values
        .values()
        .any(|value| value_contains_placeholder(value, key))
}

fn is_valid_param_key(case: &Case, param_set: &ParamSet, key: &str) -> bool {
    if PARAM_OVERRIDE_KEYS.contains(&key) {
        return true;
    }

    case_contains_placeholder(case, key) || param_values_contain_placeholder(param_set, key)
}

fn unknown_param_keys(case: &Case) -> Vec<&str> {
    let mut unknown_keys = Vec::new();

    for param_set in &case.params {
        for key in param_set.values.keys() {
            if is_valid_param_key(case, param_set, key) {
                continue;
            }
            unknown_keys.push(key.as_str());
        }
    }

    unknown_keys.sort_unstable();
    unknown_keys.dedup();
    unknown_keys
}

fn validate_param_keys(case: &Case) {
    let unknown_keys = unknown_param_keys(case);
    if unknown_keys.is_empty() {
        return;
    }

    panic!(
        "test '{}' has params key(s) that are neither field overrides nor referenced \
         placeholders: {}. Parameter keys must be one of the reserved override keys \
         ({}) or appear as a {{key}} placeholder in a substituted field.",
        case.name,
        unknown_keys.join(", "),
        PARAM_OVERRIDE_KEYS.join(", ")
    );
}

fn validate_param_overrides(case: &Case) {
    let errors = invalid_param_overrides(case);
    if errors.is_empty() {
        return;
    }

    let details = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    panic!(
        "test '{}' has invalid parameter override value(s): {}",
        case.name, details
    );
}

/// Expand a single case with params into multiple concrete cases.
/// If the case has no params, returns the case unchanged (in a vec).
pub fn expand_case(case: Case) -> Vec<Case> {
    if case.params.is_empty() {
        return vec![case];
    }

    validate_param_keys(&case);
    validate_param_overrides(&case);

    case.params
        .iter()
        .map(|param_set| {
            let params = resolve_param_values(&param_set.values).unwrap_or_else(|error| {
                panic!(
                    "test '{}' has invalid parameter placeholders: {error}",
                    case.name
                )
            });
            let mut expanded = Case {
                // Substitute placeholders in string fields
                name: substitute_placeholders(&case.name, &params),
                description: substitute_optional_string(&case.description, &params),
                source: substitute_placeholders(&case.source, &params),
                error_contains: ErrorContains(
                    case.error_contains
                        .iter()
                        .map(|s| substitute_placeholders(s, &params))
                        .collect(),
                ),
                expected_error: substitute_optional_string(&case.expected_error, &params),
                expected_error_code: substitute_optional_string(&case.expected_error_code, &params),
                expected_tokens: substitute_optional_string(&case.expected_tokens, &params),
                expected_ast: substitute_optional_string(&case.expected_ast, &params),
                expected_rir: substitute_optional_string(&case.expected_rir, &params),
                expected_air: substitute_optional_string(&case.expected_air, &params),
                expected_mir: substitute_optional_string(&case.expected_mir, &params),
                expected_lowering: substitute_optional_string(&case.expected_lowering, &params),
                expected_liveness: substitute_optional_string(&case.expected_liveness, &params),
                expected_regalloc: substitute_optional_string(&case.expected_regalloc, &params),
                expected_asm: substitute_optional_string(&case.expected_asm, &params),
                expected_stackframe: substitute_optional_string(&case.expected_stackframe, &params),
                expected_abi: substitute_optional_string(&case.expected_abi, &params),
                expected_cfg: substitute_optional_string(&case.expected_cfg, &params),
                runtime_error: substitute_optional_string(&case.runtime_error, &params),
                warning_contains: substitute_optional_string_vec(&case.warning_contains, &params),
                spec: substitute_string_vec(&case.spec, &params),
                expected_stdout: substitute_optional_string(&case.expected_stdout, &params),
                preview: substitute_optional_string(&case.preview, &params),
                target: substitute_optional_string(&case.target, &params),
                stdin: substitute_optional_string(&case.stdin, &params),
                stderr_contains: substitute_optional_string(&case.stderr_contains, &params),
                aux_files: substitute_string_map(&case.aux_files, &params),
                only_on: substitute_string_vec(&case.only_on, &params),

                // Copy non-template fields with potential overrides
                exit_code: case.exit_code,
                compile_fail: case.compile_fail,
                compile_only: case.compile_only,
                runtime_exit_code: case.runtime_exit_code,
                skip: case.skip,
                expected_warning_count: case.expected_warning_count,
                no_warnings: case.no_warnings,
                preview_should_pass: case.preview_should_pass,
                real_std: case.real_std,
                opt_level: case.opt_level,
                timeout_ms: case.timeout_ms,

                // Clear params on expanded case
                params: vec![],
            };

            // Apply field overrides from params
            if let Some(value) = params.get("exit_code") {
                expanded.exit_code = Some(
                    i32::try_from(value.as_integer().expect("validated integer override"))
                        .expect("validated i32 override"),
                );
            }
            if let Some(value) = params.get("compile_fail") {
                expanded.compile_fail = value.as_bool().expect("validated boolean override");
            }
            if let Some(value) = params.get("compile_only") {
                expanded.compile_only = value.as_bool().expect("validated boolean override");
            }
            if let Some(value) = params.get("skip") {
                expanded.skip = value.as_bool().expect("validated boolean override");
            }
            if let Some(value) = params.get("runtime_exit_code") {
                expanded.runtime_exit_code = Some(
                    i32::try_from(value.as_integer().expect("validated integer override"))
                        .expect("validated i32 override"),
                );
            }
            if let Some(value) = params.get("no_warnings") {
                expanded.no_warnings = value.as_bool().expect("validated boolean override");
            }
            if let Some(value) = params.get("opt_level") {
                expanded.opt_level = Some(
                    u8::try_from(value.as_integer().expect("validated integer override"))
                        .expect("validated u8 override"),
                );
            }
            if let Some(value) = params.get("target") {
                expanded.target = Some(
                    value
                        .as_str()
                        .expect("validated string override")
                        .to_string(),
                );
            }
            if let Some(value) = params.get("preview") {
                expanded.preview = Some(
                    value
                        .as_str()
                        .expect("validated string override")
                        .to_string(),
                );
            }
            if let Some(value) = params.get("preview_should_pass") {
                expanded.preview_should_pass = value.as_bool().expect("validated boolean override");
            }
            if let Some(value) = params.get("timeout_ms") {
                expanded.timeout_ms = Some(
                    u64::try_from(value.as_integer().expect("validated integer override"))
                        .expect("validated u64 override"),
                );
            }
            // A per-param diagnostic assertion lets a
            // parameterized case give each variant its own diagnostic assertion
            // (e.g. a mix of failing and succeeding variants). Without honoring
            // it here, the override was silently dropped and never checked — the
            // same vacuous-pass hole RUE-132 closes elsewhere. Accepts a string
            // or an array of strings, matching the case-level field.
            if let Some(value) = params.get("error_contains") {
                match value {
                    toml::Value::String(s) => {
                        expanded.error_contains =
                            ErrorContains(vec![substitute_placeholders(s, &params)]);
                    }
                    toml::Value::Array(arr) => {
                        expanded.error_contains = ErrorContains(
                            arr.iter()
                                .map(|v| v.as_str().expect("validated string array override"))
                                .map(|s| substitute_placeholders(s, &params))
                                .collect(),
                        );
                    }
                    _ => unreachable!("validated error_contains override"),
                }
            }
            if let Some(value) = params.get("expected_error") {
                if let Some(s) = value.as_str() {
                    expanded.expected_error = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_error_code") {
                if let Some(s) = value.as_str() {
                    expanded.expected_error_code = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_tokens") {
                if let Some(s) = value.as_str() {
                    expanded.expected_tokens = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_ast") {
                if let Some(s) = value.as_str() {
                    expanded.expected_ast = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_rir") {
                if let Some(s) = value.as_str() {
                    expanded.expected_rir = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_air") {
                if let Some(s) = value.as_str() {
                    expanded.expected_air = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_cfg") {
                if let Some(s) = value.as_str() {
                    expanded.expected_cfg = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_mir") {
                if let Some(s) = value.as_str() {
                    expanded.expected_mir = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_lowering") {
                if let Some(s) = value.as_str() {
                    expanded.expected_lowering = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_liveness") {
                if let Some(s) = value.as_str() {
                    expanded.expected_liveness = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_regalloc") {
                if let Some(s) = value.as_str() {
                    expanded.expected_regalloc = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_asm") {
                if let Some(s) = value.as_str() {
                    expanded.expected_asm = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_abi") {
                if let Some(s) = value.as_str() {
                    expanded.expected_abi = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("expected_stackframe") {
                if let Some(s) = value.as_str() {
                    expanded.expected_stackframe = Some(substitute_placeholders(s, &params));
                }
            }
            if let Some(value) = params.get("warning_contains") {
                match value {
                    toml::Value::String(s) => {
                        expanded.warning_contains = Some(vec![substitute_placeholders(s, &params)]);
                    }
                    toml::Value::Array(arr) => {
                        expanded.warning_contains = Some(
                            arr.iter()
                                .map(|v| v.as_str().expect("validated string array override"))
                                .map(|s| substitute_placeholders(s, &params))
                                .collect(),
                        );
                    }
                    _ => unreachable!("validated warning_contains override"),
                }
            }
            if let Some(value) = params.get("expected_warning_count") {
                expanded.expected_warning_count = Some(
                    usize::try_from(value.as_integer().expect("validated integer override"))
                        .expect("validated usize override"),
                );
            }

            // Merge spec_extra into spec
            if let Some(value) = params.get("spec_extra") {
                for item in value.as_array().expect("validated array override") {
                    let item = item.as_str().expect("validated string array override");
                    expanded.spec.push(substitute_placeholders(item, &params));
                }
            }

            expanded
        })
        .collect()
}

/// Expand all parameterized cases in a test file.
pub fn expand_test_file(mut test_file: TestFile) -> TestFile {
    let expanded_cases: Vec<Case> = test_file.case.drain(..).flat_map(expand_case).collect();
    test_file.case = expanded_cases;
    test_file
}

/// The source location of one expanded test identity.
///
/// Test harnesses use the identity as the key for libtest2's scheduler. Keep
/// the source and case name alongside it so duplicate identities can be
/// reported before any trials are constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestNameOrigin {
    /// The name passed to libtest2 (for example, `section::case`).
    pub name: String,
    /// The test file that declared the case.
    pub source: String,
    /// The expanded case name within the source file.
    pub case: String,
}

/// Reject duplicate names in a complete, already-expanded test corpus.
///
/// This is intentionally shared by all harnesses. Filtering by tier, platform,
/// selector, or shard must happen only after this check: otherwise a duplicate
/// can hide in an unselected slice and still corrupt concurrent scheduling when
/// a later invocation selects it.
pub fn validate_unique_test_names<I>(origins: I) -> Result<(), String>
where
    I: IntoIterator<Item = TestNameOrigin>,
{
    let mut by_name: BTreeMap<String, Vec<TestNameOrigin>> = BTreeMap::new();
    for origin in origins {
        by_name.entry(origin.name.clone()).or_default().push(origin);
    }

    let duplicates = by_name
        .into_iter()
        .filter(|(_, origins)| origins.len() > 1)
        .collect::<Vec<_>>();
    if duplicates.is_empty() {
        return Ok(());
    }

    let mut details = Vec::new();
    for (name, mut origins) in duplicates {
        origins.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.case.cmp(&right.case))
        });
        let locations = origins
            .into_iter()
            .map(|origin| format!("{} (case '{}')", origin.source, origin.case))
            .collect::<Vec<_>>();
        details.push(format!(
            "duplicate test name '{}': {}",
            name,
            locations.join("; ")
        ));
    }

    Err(format!(
        "{} duplicate test name(s) found:\n  - {}",
        details.len(),
        details.join("\n  - ")
    ))
}

/// An error indicating an unknown preview feature name was used in a test.
#[derive(Debug, Clone)]
pub struct UnknownPreviewFeatureError {
    /// The invalid feature name found in the test.
    pub feature_name: String,
    /// The name of the test case using this feature.
    pub test_name: String,
    /// The section ID the test belongs to.
    pub section_id: String,
}

impl std::fmt::Display for UnknownPreviewFeatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown preview feature '{}' in test '{}::{}'; valid features are: {}",
            self.feature_name,
            self.section_id,
            self.test_name,
            PreviewFeature::all_names()
        )
    }
}

impl std::error::Error for UnknownPreviewFeatureError {}

/// Validate all preview feature names in a test file.
///
/// Returns a list of errors for any unknown preview feature names.
/// An empty list means all preview features are valid (or no preview features are used).
pub fn validate_preview_features(test_file: &TestFile) -> Vec<UnknownPreviewFeatureError> {
    let mut errors = Vec::new();

    for case in &test_file.case {
        if let Some(ref feature_name) = case.preview {
            // Try to parse as a valid PreviewFeature
            if feature_name.parse::<PreviewFeature>().is_err() {
                errors.push(UnknownPreviewFeatureError {
                    feature_name: feature_name.clone(),
                    test_name: case.name.clone(),
                    section_id: test_file.section.id.clone(),
                });
            }
        }
    }

    errors
}

/// An error indicating a case's `only_on` list names an unknown platform.
///
/// An unknown name can never equal the host, so the case would be skipped on
/// EVERY platform — silently, while its spec references still count as
/// coverage in the traceability gate. Reject at load time like unknown
/// preview-feature names.
#[derive(Debug, Clone)]
pub struct UnknownPlatformError {
    /// The unrecognized platform name.
    pub platform: String,
    /// The name of the offending test case.
    pub test_name: String,
    /// The section ID the test belongs to.
    pub section_id: String,
}

impl std::fmt::Display for UnknownPlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "test '{}' in section '{}' has unknown only_on platform '{}' (known: {})",
            self.test_name,
            self.section_id,
            self.platform,
            KNOWN_TARGETS.join(", ")
        )
    }
}

impl std::error::Error for UnknownPlatformError {}

/// Validate all `only_on` platform names in a test file against
/// [`KNOWN_TARGETS`].
pub fn validate_only_on_targets(test_file: &TestFile) -> Vec<UnknownPlatformError> {
    let mut errors = Vec::new();
    for case in &test_file.case {
        for platform in &case.only_on {
            if !KNOWN_TARGETS.contains(&platform.as_str()) {
                errors.push(UnknownPlatformError {
                    platform: platform.clone(),
                    test_name: case.name.clone(),
                    section_id: test_file.section.id.clone(),
                });
            }
        }
    }
    errors
}

/// An error indicating a case declares a compile-error assertion without
/// `compile_fail = true`.
///
/// Those assertions are only ever checked inside the `compile_fail` branch of
/// [`run_test_case`]; on a case expected to compile they would silently never
/// run, turning a typo'd expectation into a vacuous pass. Rejecting the
/// combination at load time is cleaner than half-honoring it (RUE-132).
#[derive(Debug, Clone)]
pub struct StrayErrorAssertionError {
    /// The name of the offending test case.
    pub test_name: String,
    /// The section ID the test belongs to.
    pub section_id: String,
    /// Which compile-error assertion fields were set.
    pub fields: String,
}

impl std::fmt::Display for StrayErrorAssertionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "test '{}::{}' sets {} but is not `compile_fail` — that assertion is \
             only checked when compilation is expected to fail, so it would never \
             run. Add `compile_fail = true`, or (for a runtime failure) use \
             `runtime_error`, or remove the field.",
            self.section_id, self.test_name, self.fields
        )
    }
}

impl std::error::Error for StrayErrorAssertionError {}

/// Validate that no case carries a compile-error assertion without
/// `compile_fail`. Returns one error per offending case.
pub fn validate_error_assertions(test_file: &TestFile) -> Vec<StrayErrorAssertionError> {
    let mut errors = Vec::new();
    for case in &test_file.case {
        if case.compile_fail {
            continue;
        }
        let fields = [
            (!case.error_contains.is_empty()).then_some("`error_contains`"),
            case.expected_error.is_some().then_some("`expected_error`"),
            case.expected_error_code
                .is_some()
                .then_some("`expected_error_code`"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if fields.is_empty() {
            continue;
        }
        errors.push(StrayErrorAssertionError {
            test_name: case.name.clone(),
            section_id: test_file.section.id.clone(),
            fields: fields.join(", "),
        });
    }
    errors
}

/// An error indicating a `compile_fail` case declares no error assertion at all
/// (`error_contains`, `expected_error`, or `expected_error_code`).
///
/// Without an assertion, such a case passes on *any* rejection — a diagnostic
/// for an unrelated reason, or (before ICE detection) even a compiler crash —
/// so it verifies nothing about *why* the program is rejected. Requiring at
/// least one assertion at load time forces every `compile_fail` case to pin the
/// specific error it is testing (RUE-132).
#[derive(Debug, Clone)]
pub struct BareCompileFailError {
    /// The name of the offending test case.
    pub test_name: String,
    /// The section ID the test belongs to.
    pub section_id: String,
}

impl std::fmt::Display for BareCompileFailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "test '{}::{}' is `compile_fail` but declares no `error_contains`, \
             `expected_error`, or `expected_error_code` — it would pass on ANY rejection, verifying nothing \
             about why the program is rejected. Add an assertion pinning the \
             specific error (e.g. `expected_error_code = \"E0206\"`).",
            self.section_id, self.test_name
        )
    }
}

impl std::error::Error for BareCompileFailError {}

/// Validate that every `compile_fail` case declares at least one error
/// assertion. Returns one error per offending case.
///
/// This is the inverse of [`validate_error_assertions`]: that check rejects a
/// compile-error assertion on a case that is *not* `compile_fail`; this one
/// rejects a `compile_fail` case that carries *no* such assertion.
pub fn validate_compile_fail_assertions(test_file: &TestFile) -> Vec<BareCompileFailError> {
    let mut errors = Vec::new();
    for case in &test_file.case {
        if !case.compile_fail {
            continue;
        }
        if !case.error_contains.is_empty()
            || case.expected_error.is_some()
            || case.expected_error_code.is_some()
        {
            continue;
        }
        errors.push(BareCompileFailError {
            test_name: case.name.clone(),
            section_id: test_file.section.id.clone(),
        });
    }
    errors
}

/// An expected diagnostic code that is not in the compiler-owned inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownExpectedErrorCodeError {
    pub test_name: String,
    pub section_id: String,
    pub code: String,
}

impl std::fmt::Display for UnknownExpectedErrorCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "test '{}::{}' declares unknown `expected_error_code` {:?}",
            self.section_id, self.test_name, self.code
        )
    }
}

impl std::error::Error for UnknownExpectedErrorCodeError {}

/// Validate typed diagnostic declarations against the canonical compiler
/// inventory. This runs after parameter expansion, so placeholders and
/// per-parameter overrides cannot bypass validation.
pub fn validate_expected_error_codes(test_file: &TestFile) -> Vec<UnknownExpectedErrorCodeError> {
    let known_codes = error_code_metadata()
        .iter()
        .map(|metadata| metadata.code.to_string())
        .collect::<BTreeSet<_>>();
    test_file
        .case
        .iter()
        .filter_map(|case| {
            let code = case.expected_error_code.as_ref()?;
            (!known_codes.contains(code)).then_some(UnknownExpectedErrorCodeError {
                test_name: case.name.clone(),
                section_id: test_file.section.id.clone(),
                code: code.clone(),
            })
        })
        .collect()
}

/// An error indicating a `compile_fail` case declares an `exit_code`.
///
/// `exit_code` describes the compiled program's runtime status. A
/// `compile_fail` case never produces or runs that program, so the assertion
/// would be silently ignored.
#[derive(Debug, Clone)]
pub struct CompileFailExitCodeError {
    /// The name of the offending test case.
    pub test_name: String,
    /// The section ID the test belongs to.
    pub section_id: String,
}

impl std::fmt::Display for CompileFailExitCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            concat!(
                "test '{}::{}' is `compile_fail` but also declares `exit_code` — ",
                "no program runs for a compile failure, so that assertion would be ignored. ",
                "Remove `exit_code`."
            ),
            self.section_id, self.test_name
        )
    }
}

impl std::error::Error for CompileFailExitCodeError {}

/// Validate that no `compile_fail` case declares a runtime `exit_code`.
pub fn validate_compile_fail_exit_codes(test_file: &TestFile) -> Vec<CompileFailExitCodeError> {
    test_file
        .case
        .iter()
        .filter(|case| case.compile_fail && case.exit_code.is_some())
        .map(|case| CompileFailExitCodeError {
            test_name: case.name.clone(),
            section_id: test_file.section.id.clone(),
        })
        .collect()
}

/// An error indicating that a compile-only case carries assertions or input
/// that are consumed only by the produced-program phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileOnlyRuntimeAssertionError {
    pub test_name: String,
    pub section_id: String,
    pub fields: String,
}

impl std::fmt::Display for CompileOnlyRuntimeAssertionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "test '{}::{}' is `compile_only` but declares produced-program field(s) {} — \
             compile-only cases cannot use runtime assertions or input; remove those fields",
            self.section_id, self.test_name, self.fields
        )
    }
}

impl std::error::Error for CompileOnlyRuntimeAssertionError {}

/// Validate that `compile_only` cases do not carry produced-program-only
/// assertions or input. The field order is part of the stable diagnostic.
pub fn validate_compile_only_runtime_assertions(
    test_file: &TestFile,
) -> Vec<CompileOnlyRuntimeAssertionError> {
    const RUNTIME_FIELDS: [(&str, fn(&Case) -> bool); 6] = [
        ("exit_code", |case| case.exit_code.is_some()),
        ("expected_stdout", |case| case.expected_stdout.is_some()),
        ("runtime_error", |case| case.runtime_error.is_some()),
        ("runtime_exit_code", |case| case.runtime_exit_code.is_some()),
        ("stdin", |case| case.stdin.is_some()),
        ("stderr_contains", |case| case.stderr_contains.is_some()),
    ];
    test_file
        .case
        .iter()
        .filter(|case| case.compile_only)
        .filter_map(|case| {
            let fields = RUNTIME_FIELDS
                .iter()
                .filter_map(|(field, present)| present(case).then_some(format!("`{field}`")))
                .collect::<Vec<_>>();
            (!fields.is_empty()).then_some(CompileOnlyRuntimeAssertionError {
                test_name: case.name.clone(),
                section_id: test_file.section.id.clone(),
                fields: fields.join(", "),
            })
        })
        .collect()
}

/// An error indicating a substring assertion whose empty value would match
/// every output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyContainsAssertionError {
    pub test_name: String,
    pub section_id: String,
    pub fields: String,
}

impl std::fmt::Display for EmptyContainsAssertionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "test '{}::{}' declares empty substring assertion(s) {} — empty \
             contains assertions match every output; use a non-empty substring or remove the field",
            self.section_id, self.test_name, self.fields
        )
    }
}

impl std::error::Error for EmptyContainsAssertionError {}

/// Validate every shared-harness substring assertion, retaining empty arrays
/// as valid but rejecting empty string entries. Field order is deterministic.
pub fn validate_empty_contains_assertions(
    test_file: &TestFile,
) -> Vec<EmptyContainsAssertionError> {
    test_file
        .case
        .iter()
        .filter_map(|case| {
            let mut fields = Vec::new();
            for (index, value) in case.error_contains.iter().enumerate() {
                if value.is_empty() {
                    fields.push(format!("`error_contains[{index}]`"));
                }
            }
            if let Some(values) = &case.warning_contains {
                for (index, value) in values.iter().enumerate() {
                    if value.is_empty() {
                        fields.push(format!("`warning_contains[{index}]`"));
                    }
                }
            }
            if case.stderr_contains.as_deref() == Some("") {
                fields.push("`stderr_contains`".to_string());
            }
            (!fields.is_empty()).then_some(EmptyContainsAssertionError {
                test_name: case.name.clone(),
                section_id: test_file.section.id.clone(),
                fields: fields.join(", "),
            })
        })
        .collect()
}

/// Validate that an explicitly configured case corpus contains at least one
/// case before command-line filtering is applied.
///
/// This deliberately checks the loaded corpus, not the number of selected
/// trials: a user filter that matches zero cases may remain successful, while a
/// missing or empty configured corpus must fail.
pub fn validate_nonempty_case_corpus(
    cases_dir: &Path,
    case_count: usize,
    corpus_name: &str,
) -> Result<(), String> {
    if case_count > 0 {
        return Ok(());
    }

    Err(format!(
        "no {corpus_name} test cases found in {}",
        cases_dir.display()
    ))
}

/// Recursively discover files with the given extension.
///
/// The result is sorted by path. Every filesystem error is returned: callers
/// must never run a partial corpus merely because one directory entry could
/// not be inspected.
pub fn discover_files(dir: &Path, ext: &str) -> std::io::Result<Vec<PathBuf>> {
    fn visit(
        dir: &Path,
        ext: &str,
        files: &mut Vec<PathBuf>,
        visited_dirs: &mut BTreeSet<PathBuf>,
    ) -> std::io::Result<()> {
        let canonical_dir = fs::canonicalize(dir).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("failed to resolve directory {}: {error}", dir.display()),
            )
        })?;
        if !visited_dirs.insert(canonical_dir) {
            return Ok(());
        }
        let read_dir = fs::read_dir(dir).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("failed to read directory {}: {error}", dir.display()),
            )
        })?;
        let mut entries = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("failed to read an entry in {}: {error}", dir.display()),
                )
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("failed to inspect {}: {error}", path.display()),
                )
            })?;
            entries.push((path, file_type));
        }
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        for (path, file_type) in entries {
            if file_type.is_dir() {
                visit(&path, ext, files, visited_dirs)?;
            } else if file_type.is_symlink() {
                let target = fs::metadata(&path).map_err(|error| {
                    std::io::Error::new(
                        error.kind(),
                        format!(
                            "failed to inspect symlink target {}: {error}",
                            path.display()
                        ),
                    )
                })?;
                if target.is_dir() {
                    visit(&path, ext, files, visited_dirs)?;
                } else if target.is_file()
                    && path.extension().is_some_and(|candidate| candidate == ext)
                {
                    files.push(path);
                }
            } else if file_type.is_file()
                && path.extension().is_some_and(|candidate| candidate == ext)
            {
                files.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    let mut visited_dirs = BTreeSet::new();
    visit(dir, ext, &mut files, &mut visited_dirs)?;
    Ok(files)
}

/// Load all test files from a directory (including subdirectories).
///
/// This function validates test metadata and returns every discovery, read,
/// parse, or validation failure instead of silently running a partial corpus.
pub fn load_test_files(cases_dir: &Path) -> Result<Vec<(String, TestFile)>, String> {
    let mut specs = Vec::new();
    let mut test_name_origins = Vec::new();
    let mut preview_errors: Vec<UnknownPreviewFeatureError> = Vec::new();
    let mut platform_errors: Vec<UnknownPlatformError> = Vec::new();
    let mut stray_error_assertions: Vec<StrayErrorAssertionError> = Vec::new();
    let mut unknown_expected_error_codes: Vec<UnknownExpectedErrorCodeError> = Vec::new();
    let mut bare_compile_fail: Vec<BareCompileFailError> = Vec::new();
    let mut compile_fail_exit_codes: Vec<CompileFailExitCodeError> = Vec::new();
    let mut compile_only_runtime_assertions: Vec<CompileOnlyRuntimeAssertionError> = Vec::new();
    let mut empty_contains_assertions: Vec<EmptyContainsAssertionError> = Vec::new();
    let mut platform_responsibility: Vec<PlatformResponsibilityError> = Vec::new();

    let toml_files = discover_files(cases_dir, "toml").map_err(|error| {
        format!(
            "failed to discover test files under {}: {error}",
            cases_dir.display()
        )
    })?;

    let mut load_errors: Vec<String> = Vec::new();
    for path in toml_files {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                load_errors.push(format!("{}: {}", path.display(), e));
                continue;
            }
        };

        match toml::from_str::<TestFile>(&content) {
            Ok(spec) => {
                // Expand any parameterized test cases
                let spec = expand_test_file(spec);

                // Validate preview feature names
                preview_errors.extend(validate_preview_features(&spec));

                // Validate only_on platform names (a typo would silently skip
                // the case on every host while still counting as coverage)
                platform_errors.extend(validate_only_on_targets(&spec));

                // Reject compile-error assertions on cases that aren't compile_fail
                stray_error_assertions.extend(validate_error_assertions(&spec));

                // Typed diagnostic evidence must name a compiler-owned code.
                // This is deliberately after expansion so every concrete
                // parameter variant is checked.
                unknown_expected_error_codes.extend(validate_expected_error_codes(&spec));

                // Reject `compile_fail` cases that carry no error assertion at all
                bare_compile_fail.extend(validate_compile_fail_assertions(&spec));

                // Reject runtime exit-code assertions on cases that never
                // produce or execute a program.
                compile_fail_exit_codes.extend(validate_compile_fail_exit_codes(&spec));

                compile_only_runtime_assertions
                    .extend(validate_compile_only_runtime_assertions(&spec));
                empty_contains_assertions.extend(validate_empty_contains_assertions(&spec));

                // Reject cases whose platform responsibility is ambiguous: an
                // architecture-specific expectation with no declared target, or
                // a declared target a scoped host cannot execute (RUE-1161).
                platform_responsibility.extend(validate_platform_responsibility(&spec));

                test_name_origins.extend(spec.case.iter().map(|case| TestNameOrigin {
                    name: format!("{}::{}", spec.section.id, case.name),
                    source: path.display().to_string(),
                    case: case.name.clone(),
                }));

                // Build a relative path from cases_dir to create the identifier
                // e.g., "expressions/match" for "cases/expressions/match.toml"
                let relative = path
                    .strip_prefix(cases_dir)
                    .unwrap_or(&path)
                    .with_extension("");
                let identifier = relative
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                specs.push((identifier, spec));
            }
            Err(e) => {
                load_errors.push(format!("{}: {}", path.display(), e));
            }
        }
    }

    // A case file that fails to load must fail the suite: silently skipping it
    // would silently remove every test it contains. (RUE-132)
    if !load_errors.is_empty() {
        return Err(format!(
            "{} test file(s) failed to load:\n  - {}",
            load_errors.len(),
            load_errors.join("\n  - ")
        ));
    }

    validate_unique_test_names(test_name_origins)?;

    // Report all preview feature errors and fail if any were found
    if !preview_errors.is_empty() {
        return Err(format!(
            "{} test(s) use unknown preview feature names:\n  - {}",
            preview_errors.len(),
            preview_errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    // Report unknown only_on platform names and fail if any were found
    if !platform_errors.is_empty() {
        return Err(format!(
            "{} case(s) use unknown only_on platform names:\n  - {}",
            platform_errors.len(),
            platform_errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    // Report stray compile-error assertions and fail if any were found
    if !stray_error_assertions.is_empty() {
        return Err(format!(
            "{} case(s) set a compile-error assertion without `compile_fail`:\n  - {}",
            stray_error_assertions.len(),
            stray_error_assertions
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    if !unknown_expected_error_codes.is_empty() {
        return Err(format!(
            "{} case(s) declare unknown expected diagnostic codes:\n  - {}",
            unknown_expected_error_codes.len(),
            unknown_expected_error_codes
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    // Report bare compile_fail cases and fail if any were found
    if !bare_compile_fail.is_empty() {
        return Err(format!(
            "{} `compile_fail` case(s) have no error assertion:\n  - {}",
            bare_compile_fail.len(),
            bare_compile_fail
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    // Report cases with an undecidable platform responsibility.
    if !platform_responsibility.is_empty() {
        return Err(format!(
            "{} case(s) have an ambiguous platform responsibility:\n  - {}",
            platform_responsibility.len(),
            platform_responsibility
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    // Report runtime exit-code assertions that compile-fail cases cannot check.
    if !compile_fail_exit_codes.is_empty() {
        return Err(format!(
            "{} `compile_fail` case(s) declare an ignored `exit_code`:\n  - {}",
            compile_fail_exit_codes.len(),
            compile_fail_exit_codes
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    if !compile_only_runtime_assertions.is_empty() {
        return Err(format!(
            "{} `compile_only` case(s) declare produced-program fields:\n  - {}",
            compile_only_runtime_assertions.len(),
            compile_only_runtime_assertions
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    if !empty_contains_assertions.is_empty() {
        return Err(format!(
            "{} case(s) declare empty substring assertions:\n  - {}",
            empty_contains_assertions.len(),
            empty_contains_assertions
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    // Sort by identifier for deterministic ordering
    specs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(specs)
}

/// Normalize a string for golden test comparison.
/// This trims trailing whitespace from each line and ensures consistent line endings.
pub fn normalize_golden(s: &str) -> String {
    s.lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Strip the single boundary newline that a TOML `"""` multi-line block adds
/// around authored content: at most one leading and at most one trailing
/// newline (`\r\n` or `\n`).
///
/// This is the *only* leniency applied when comparing a spec case's
/// `expected_stdout` against a program's actual stdout — the compare is
/// otherwise byte-exact (matching the CLI runner in rue-cli-tests). Unlike
/// [`normalize_golden`], it does NOT trim per-line trailing whitespace or
/// internal blank lines, so a stdout-formatting regression (e.g. a stray
/// trailing space or an extra blank line) is still caught. It exists purely so
/// the readable `"""` authoring convention — closing delimiter on its own line,
/// which forces a trailing newline — doesn't force every expectation to be
/// written on one crowded line. Reserve [`normalize_golden`] for `--emit` IR
/// golden dumps, which are inherently multi-line and formatting-insensitive.
pub fn strip_block_boundary_newlines(s: &str) -> &str {
    let s = s
        .strip_prefix("\r\n")
        .or_else(|| s.strip_prefix('\n'))
        .unwrap_or(s);
    s.strip_suffix("\r\n")
        .or_else(|| s.strip_suffix('\n'))
        .unwrap_or(s)
}

/// Normalize error output for golden test comparison.
/// Replaces the temp file path with a placeholder "<source>".
pub fn normalize_error_output(s: &str, source_path: &Path) -> String {
    let path_str = source_path.to_string_lossy();
    let normalized = s.replace(path_str.as_ref(), "<source>");
    normalize_golden(&normalized)
}

/// Strip the emit header (e.g., "=== RIR ===" or "=== MIR (aarch64-macos) ===") from the output.
pub fn strip_emit_header(output: &str, stage: &str) -> String {
    // Match headers like "=== MIR ===" or "=== MIR (x86-64-linux) ===" or "=== MIR (aarch64-macos) ==="
    let prefix = format!("=== {} ", stage);
    let exact = format!("=== {} ===", stage);
    output
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            // Filter out both "=== STAGE ===" and "=== STAGE (target) ==="
            trimmed != exact && !(trimmed.starts_with(&prefix) && trimmed.ends_with("==="))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compare actual output against expected golden output.
pub fn check_golden(actual: &str, expected: &str, label: &str) -> TestResult {
    let actual_normalized = normalize_golden(actual);
    let expected_normalized = normalize_golden(expected);

    if actual_normalized != expected_normalized {
        return Err(TestFailure::assertion(format!(
            "{} mismatch:\n--- expected ---\n{}\n--- actual ---\n{}\n",
            label, expected_normalized, actual_normalized
        )));
    }
    Ok(())
}

/// Map emit stage flag to the header name used in the compiler output.
/// For example, "rir" -> "RIR", "tokens" -> "Tokens"
///
fn stage_to_header_name(stage: &str) -> &'static str {
    match stage {
        "tokens" => "Tokens",
        "ast" => "AST",
        "rir" => "RIR",
        "air" => "AIR",
        "cfg" => "CFG",
        "mir" => "MIR",
        "lowering" => "Instruction Selection",
        "liveness" => "Liveness Analysis",
        "regalloc" => "Register Allocation",
        "asm" => "Assembly",
        "stackframe" => "Stack Frame",
        "abi" => "ABI",
        _ => panic!("Unknown stage: {}", stage),
    }
}

/// Run a golden test for a specific IR stage.
///
/// This helper runs `rue --emit <stage>` on the source file and compares
/// the output against the expected golden output.
fn run_golden_ir_test(
    rue_binary: &Path,
    source_path: &Path,
    stage: &str,
    expected: &str,
    build_command: impl Fn(&Path) -> Command,
    timeout: Duration,
) -> TestResult {
    // Run the emit under the same timeout as the rest of the case, so a compiler
    // that hangs while dumping an IR fails this one case instead of wedging the
    // whole suite (RUE-132). `run_with_timeout` surfaces a hang as a distinct
    // [`TIMEOUT_PREFIX`] error.
    let mut cmd = build_command(rue_binary);
    cmd.arg("--emit").arg(stage).arg(source_path);
    let output = run_with_timeout(cmd, timeout, None)
        .map_err(|error| error.with_context(format!("Failed to run rue --emit {stage}")))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(error) = ice_message(&output.status, &stderr) {
        return Err(error.with_context(format!("rue --emit {stage}")));
    }

    if !output.status.success() {
        return Err(TestFailure::assertion(format!(
            "rue --emit {} failed:\n{}",
            stage, stderr
        )));
    }

    let actual = String::from_utf8_lossy(&output.stdout);
    // Strip the "=== STAGE ===" or "=== STAGE (target) ===" header for golden comparison
    let header_name = stage_to_header_name(stage);
    let actual = strip_emit_header(&actual, header_name);
    check_golden(&actual, expected, header_name)
}

/// Marker prefix identifying a timeout failure. A timed-out run is a distinct
/// failure class (like an ICE): the process ran past its wall-clock budget and
/// was killed, rather than producing a wrong-but-finite result. Both this
/// harness and the CLI harness surface it via this prefix so an infinite loop
/// in a test program is reported and skipped past, never hanging the suite.
pub const TIMEOUT_PREFIX: &str = "TIMEOUT:";

/// Build a compiler command without ambient settings that can change a case.
///
/// Harness-specific, explicit environment entries may be applied after this
/// function returns. Removing these variables first keeps the default case
/// environment deterministic while preserving intentional overrides.
pub fn compiler_command(binary: &Path) -> Command {
    let mut command = Command::new(binary);
    command.env_remove("RUE_STD_PATH");
    command.env_remove("RUST_LOG");
    command
}

/// Put the spawned child in its own process group so a timeout can kill the
/// whole group, not just the direct child. A compiled test program spawning
/// nothing is the common case, but killing the group is the safe default: any
/// grandchildren it forked are torn down too, so nothing survives to keep a
/// pipe open and wedge the harness.
#[cfg(unix)]
pub fn configure_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // process_group(0) makes the child a new group leader whose pgid == its pid.
    cmd.process_group(0);
}

#[cfg(not(unix))]
pub fn configure_process_group(_cmd: &mut Command) {}

/// Kill the timed-out child and everything in its process group, then reap it.
#[cfg(unix)]
pub fn kill_process_group(child: &mut std::process::Child) {
    // The child leads its own group (see `configure_process_group`), so a
    // negative pid targets every process in that group with SIGKILL.
    let pid = child.id() as i32;
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill(); // belt-and-suspenders in case the group send raced
    let _ = child.wait(); // reap the zombie
}

#[cfg(not(unix))]
pub fn kill_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Run a command with a timeout and optional stdin input.
///
/// This function spawns a child process (in its own process group), drains its
/// stdout and stderr on dedicated reader threads, feeds it stdin (if provided)
/// on its own writer thread, and polls for completion, killing the whole
/// process group if it exceeds the specified timeout.
///
/// # Why the reader/writer threads (RUE-338 deadlock class)
///
/// Draining stdout AND stderr **concurrently**, starting immediately after
/// spawn, is essential: if the pipes were read only after the child exits, a
/// program that writes more than the OS pipe capacity (~64KB on Linux) would
/// block forever in `write()`, `try_wait` would never report an exit, and the
/// timeout would manufacture a false failure. For the same reason stdin is
/// written on its own thread — a large input can't block the drain, and the
/// drain can't block the stdin write. Mirrors the oracle-diff fuzzer's
/// `run_with_timeout` (RUE-338).
///
/// # Arguments
/// * `cmd` - The command to run (already configured with arguments)
/// * `timeout` - Maximum duration to wait for the process to complete
/// * `stdin_input` - Optional input to write to the process's stdin
///
/// # Returns
/// * `Ok(Output)` - The process output (stdout, stderr, exit status)
/// * `Err(TestFailure)` - Fatal failure if the process timed out or could not run.
///   A timeout error is prefixed with [`TIMEOUT_PREFIX`] so callers can report
///   it as a distinct failure class.
pub fn run_with_timeout(
    cmd: Command,
    timeout: Duration,
    stdin_input: Option<&str>,
) -> TestResult<Output> {
    run_with_timeout_impl(cmd, timeout, stdin_input, None)
}

/// Run a command with a timeout while retaining at most `output_limit` bytes
/// from each of stdout and stderr.
///
/// Unlike checking [`Output`] after [`run_with_timeout`] returns, this limit is
/// enforced by the pipe-drain threads as bytes arrive. Once either stream
/// exceeds the limit, further bytes are discarded, the process group is
/// killed, and the function returns a fatal failure identifying the stream.
/// The limit applies independently to stdout and stderr.
pub fn run_with_timeout_and_output_limit(
    cmd: Command,
    timeout: Duration,
    stdin_input: Option<&str>,
    output_limit: usize,
) -> TestResult<Output> {
    run_with_timeout_impl(cmd, timeout, stdin_input, Some(output_limit))
}

fn run_with_timeout_impl(
    mut cmd: Command,
    timeout: Duration,
    stdin_input: Option<&str>,
    output_limit: Option<usize>,
) -> TestResult<Output> {
    configure_process_group(&mut cmd);
    let mut child = cmd
        .stdin(if stdin_input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| TestFailure::fatal(format!("Failed to spawn process: {}", e)))?;

    // Drain stdout and stderr on their own threads, started right after spawn so
    // neither pipe can fill and wedge the child (see the RUE-338 note above).
    // The drains send chunks over channels instead of being joined directly:
    // if a descendant process inherits a pipe fd and keeps it open after the
    // direct child exits, a reader thread can block forever waiting for EOF.
    let mut stdout_drain = spawn_pipe_drain(child.stdout.take(), output_limit);
    let mut stderr_drain = spawn_pipe_drain(child.stderr.take(), output_limit);

    // Feed stdin on its own thread so a large input can't block the drain (and
    // vice versa). A program may exit without reading all of its input; a broken
    // pipe here is not a test failure, so errors are ignored (matching the CLI
    // harness). Dropping the pipe when the closure ends closes it, signaling EOF.
    let stdin_writer = child.stdin.take().map(|mut stdin| {
        let input = stdin_input.unwrap_or_default().to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        })
    });

    let start = Instant::now();

    loop {
        stdout_drain.poll();
        stderr_drain.poll();
        if stdout_drain.overflowed() || stderr_drain.overflowed() {
            kill_process_group(&mut child);
            let stream = match (stdout_drain.overflowed(), stderr_drain.overflowed()) {
                (true, true) => "stdout and stderr",
                (true, false) => "stdout",
                (false, true) => "stderr",
                (false, false) => unreachable!(),
            };
            return Err(TestFailure::fatal(format!(
                "OUTPUT LIMIT: {stream} exceeded the {}-byte capture limit (process group killed)",
                output_limit.expect("overflow requires an output limit")
            )));
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                // Process finished: collect any fully-drained output, but do
                // not wait forever if a descendant inherited a pipe fd.
                stdout_drain.finish(PIPE_DRAIN_FINISH_TIMEOUT);
                stderr_drain.finish(PIPE_DRAIN_FINISH_TIMEOUT);
                drop(stdin_writer);
                if stdout_drain.overflowed() || stderr_drain.overflowed() {
                    let stream = match (stdout_drain.overflowed(), stderr_drain.overflowed()) {
                        (true, true) => "stdout and stderr",
                        (true, false) => "stdout",
                        (false, true) => "stderr",
                        (false, false) => unreachable!(),
                    };
                    return Err(TestFailure::fatal(format!(
                        "OUTPUT LIMIT: {stream} exceeded the {}-byte capture limit",
                        output_limit.expect("overflow requires an output limit")
                    )));
                }
                return Ok(Output {
                    status,
                    stdout: stdout_drain.into_bytes(),
                    stderr: stderr_drain.into_bytes(),
                });
            }
            Ok(None) => {
                // Still running - check timeout
                if start.elapsed() > timeout {
                    // Kill the whole process group, then collect whatever the
                    // drain helpers already captured without waiting forever
                    // for EOF from an escaped descendant.
                    kill_process_group(&mut child);
                    stdout_drain.finish(PIPE_DRAIN_FINISH_TIMEOUT);
                    stderr_drain.finish(PIPE_DRAIN_FINISH_TIMEOUT);
                    drop(stdin_writer);
                    return Err(TestFailure::fatal(format!(
                        "{} test execution timed out after {} ms (process group killed)",
                        TIMEOUT_PREFIX,
                        timeout.as_millis()
                    )));
                }
                // Sleep briefly before polling again
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                return Err(TestFailure::fatal(format!(
                    "Failed to wait for process: {}",
                    e
                )));
            }
        }
    }
}

/// Detect an internal compiler error in a compiler invocation's outcome.
///
/// Returns a failure message when the compiler panicked (a Rust `panicked at`
/// backtrace or an `internal compiler error` diagnostic on stderr) or died by
/// signal (e.g. SIGABRT — no exit code on unix). An ICE must never satisfy a
/// `compile_fail` expectation: "the compiler rejected the program" and "the
/// compiler crashed" are different outcomes, and conflating them lets crashes
/// hide inside passing suites. Mirrors `ice_message` in rue-cli-tests.
/// Check a finished compiler process for signs of a panic / ICE.
/// This is the SINGLE shared implementation — rue-cli-tests imports it
/// rather than keeping its own copy, so a new panic marker or abort
/// signature added here covers every harness at once.
pub fn ice_message(status: &std::process::ExitStatus, stderr: &str) -> Option<TestFailure> {
    if stderr.contains("panicked at") || stderr.contains("internal compiler error") {
        return Some(TestFailure::fatal(format!(
            "INTERNAL COMPILER ERROR: compiler panicked\n--- compiler stderr ---\n{}",
            stderr
        )));
    }
    if status.code().is_none() {
        return Some(TestFailure::fatal(format!(
            "INTERNAL COMPILER ERROR: compiler killed by signal ({:?})\n--- compiler stderr ---\n{}",
            status, stderr
        )));
    }
    None
}

fn test_case_compiler_command(case: &Case, binary: &Path) -> Command {
    let mut command = compiler_command(binary);
    if case.real_std {
        let std_path = find_dir("RUE_REAL_STD_PATH", &["std", "../std", "../../std"], "std");
        let std_path = std_path.canonicalize().unwrap_or(std_path);
        command.env("RUE_STD_PATH", std_path);
    }
    if let Some(ref target) = case.target {
        command.arg("--target").arg(target);
    }
    if let Some(ref feature) = case.preview {
        command.arg("--preview").arg(feature);
    }
    if let Some(level) = case.opt_level {
        command.arg(format!("-O{}", level));
    }
    command
}

/// Parse the CLI's documented JSON-lines diagnostic framing and return every
/// emitted error code in publication order. The typed spec assertion treats a
/// malformed diagnostic as a harness failure rather than falling back to
/// rendered prose.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StructuredDiagnostic {
    code: String,
    severity: String,
    message: String,
    #[allow(dead_code)]
    spans: Vec<serde_json::Value>,
    #[allow(dead_code)]
    suggestions: Vec<serde_json::Value>,
    #[allow(dead_code)]
    notes: Vec<String>,
    #[allow(dead_code)]
    helps: Vec<String>,
}

fn parse_json_error_codes(stderr: &str) -> Result<Vec<String>, String> {
    let mut codes = Vec::new();
    for (line_index, line) in stderr.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let diagnostics: Vec<StructuredDiagnostic> = serde_json::from_str(line).map_err(|error| {
            format!(
                "diagnostic line {} does not match the canonical JSON diagnostic schema: {error}",
                line_index + 1
            )
        })?;
        if diagnostics.is_empty() {
            return Err(format!(
                "diagnostic line {} is an empty JSON batch",
                line_index + 1
            ));
        }
        for (diagnostic_index, diagnostic) in diagnostics.into_iter().enumerate() {
            if diagnostic.message.is_empty() {
                return Err(format!(
                    "diagnostic {} on line {} has an empty `message`",
                    diagnostic_index + 1,
                    line_index + 1
                ));
            }
            match diagnostic.severity.as_str() {
                "error" => codes.push(diagnostic.code),
                "warning" => {}
                other => {
                    return Err(format!(
                        "diagnostic {} on line {} has unknown severity {other:?}",
                        diagnostic_index + 1,
                        line_index + 1
                    ));
                }
            }
        }
    }
    Ok(codes)
}

/// Run a single test case.
pub fn run_test_case(case: &Case, rue_binary: &Path) -> TestResult {
    // Create a temporary directory for this test
    let temp_dir = tempfile::tempdir()
        .map_err(|e| TestFailure::fatal(format!("Failed to create temp dir: {}", e)))?;
    let source_path = temp_dir.path().join("test.rue");
    let output_path = temp_dir.path().join("test");

    // Write source to file
    let mut source_file = fs::File::create(&source_path)
        .map_err(|e| TestFailure::fatal(format!("Failed to create source file: {}", e)))?;
    source_file
        .write_all(case.source.as_bytes())
        .map_err(|e| TestFailure::fatal(format!("Failed to write source: {}", e)))?;

    // Write auxiliary files for multi-file tests (module imports)
    for (filename, content) in &case.aux_files {
        // Create subdirectories if needed (e.g., "foo/bar.rue")
        let aux_path = temp_dir.path().join(filename);
        if let Some(parent) = aux_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                TestFailure::fatal(format!("Failed to create dir for {}: {}", filename, e))
            })?;
        }
        fs::write(&aux_path, content).map_err(|e| {
            TestFailure::fatal(format!("Failed to write aux file {}: {}", filename, e))
        })?;
    }

    // Build base command with target, preview, and optimization flags if needed
    let build_command = |binary: &Path| test_case_compiler_command(case, binary);

    // Timeout applied to every compiler invocation in this case (golden-IR
    // emits and the compile step), so a compiler hang fails the case instead of
    // wedging the whole suite.
    let compile_timeout = Duration::from_millis(case.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));

    // Check for golden IR tests.
    if case.has_golden_ir_assertions() {
        // A program that fails to compile has no IR to dump, so this
        // combination can never be satisfied — reject it loudly rather than
        // letting one half of the case go unchecked.
        if case.compile_fail {
            return Err(TestFailure::assertion(
                "golden IR assertions cannot be combined with compile_fail \
                 (use expected_error_code / expected_error / error_contains for diagnostics instead)",
            ));
        }

        // Run dump commands and check golden output
        if let Some(ref expected) = case.expected_tokens {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "tokens",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_ast {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "ast",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_rir {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "rir",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_air {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "air",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_cfg {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "cfg",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if case.has_target_specific_golden_ir_assertions() && case.target.is_none() {
            return Err(TestFailure::assertion(
                "target-specific golden IR tests require a 'target' field \
                 (e.g., target = \"x86-64-linux\")",
            ));
        }

        if let Some(ref expected) = case.expected_mir {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "mir",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_lowering {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "lowering",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_liveness {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "liveness",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_regalloc {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "regalloc",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_asm {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "asm",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_stackframe {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "stackframe",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        if let Some(ref expected) = case.expected_abi {
            run_golden_ir_test(
                rue_binary,
                &source_path,
                "abi",
                expected,
                &build_command,
                compile_timeout,
            )?;
        }

        // A case may combine golden-IR assertions with execution assertions
        // (exit_code, expected_stdout, ...). When it does, fall through and
        // verify those too — returning here would silently skip them
        // (RUE-132). A pure golden case is done at this point.
        if !case.has_execution_assertions() {
            return Ok(());
        }
    }

    // Compile with rue. Auxiliary files are written to disk so the root
    // module's import graph can discover them; they are not appended as
    // positional source files.
    let mut compile_cmd = build_command(rue_binary);
    compile_cmd.arg(&source_path);
    compile_cmd.arg("-o").arg(&output_path);
    // Run the compiler under the same timeout as the golden emits and the
    // compiled program, so a compiler hang fails the case instead of wedging
    // the whole suite.
    let compile_output = run_with_timeout(compile_cmd, compile_timeout, None)
        .map_err(|error| error.with_context("Failed to run rue compiler"))?;

    let compile_succeeded = compile_output.status.success();
    let stderr = String::from_utf8_lossy(&compile_output.stderr);

    // A compiler crash is NEVER a pass — not even for compile_fail cases, where a
    // panic would otherwise be indistinguishable from the expected diagnostic
    // failure. Report it as a distinct ICE failure class instead.
    if let Some(ice) = ice_message(&compile_output.status, &stderr) {
        return Err(ice.with_context(format!("source: {}", case.source)));
    }

    if case.compile_fail {
        // Expected to fail compilation
        if compile_succeeded {
            return Err(TestFailure::assertion(format!(
                "Expected compilation to fail, but it succeeded\n  source: {}",
                case.source
            )));
        }

        if let Some(expected_code) = &case.expected_error_code {
            let mut json_cmd = build_command(rue_binary);
            json_cmd.arg("--error-format").arg("json");
            json_cmd.arg(&source_path);
            json_cmd.arg("-o").arg(temp_dir.path().join("test-json"));
            let json_output =
                run_with_timeout(json_cmd, compile_timeout, None).map_err(|error| {
                    error.with_context("Failed to run rue compiler for typed diagnostic assertion")
                })?;
            let json_stderr = String::from_utf8_lossy(&json_output.stderr);
            if let Some(ice) = ice_message(&json_output.status, &json_stderr) {
                return Err(ice.with_context(format!("source: {}", case.source)));
            }
            if json_output.status.success() {
                return Err(TestFailure::assertion(
                    "Typed diagnostic assertion invocation unexpectedly compiled successfully",
                ));
            }
            let actual_codes = parse_json_error_codes(&json_stderr).map_err(|error| {
                TestFailure::fatal(format!(
                    "Malformed structured diagnostic metadata: {error}\n--- compiler stderr ---\n{json_stderr}"
                ))
            })?;
            match actual_codes.as_slice() {
                [actual_code] if actual_code == expected_code => {}
                [actual_code] => {
                    return Err(TestFailure::assertion(format!(
                        "Diagnostic code mismatch: expected {expected_code}, emitted {actual_code}"
                    )));
                }
                [] => {
                    return Err(TestFailure::assertion(format!(
                        "Diagnostic code mismatch: expected exactly one {expected_code} diagnostic, emitted none"
                    )));
                }
                _ => {
                    return Err(TestFailure::assertion(format!(
                        "Ambiguous diagnostic emission: expected exactly one {expected_code} diagnostic, emitted {}",
                        actual_codes.join(", ")
                    )));
                }
            }
        }

        // Check exact error message (golden test)
        if let Some(ref expected) = case.expected_error {
            let actual_normalized = normalize_error_output(&stderr, &source_path);
            let expected_normalized = normalize_golden(expected);
            if actual_normalized != expected_normalized {
                return Err(TestFailure::assertion(format!(
                    "Error mismatch:\n--- expected ---\n{}\n--- actual ---\n{}\n",
                    expected_normalized, actual_normalized
                )));
            }
        }

        // Check error message contains all expected substrings
        for expected_error in case.error_contains.iter() {
            if !stderr.contains(expected_error) {
                return Err(TestFailure::assertion(format!(
                    "Error message mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                    expected_error, stderr, case.source
                )));
            }
        }

        return Ok(());
    }

    // Expected to succeed
    if !compile_succeeded {
        return Err(TestFailure::assertion(format!(
            "Compilation failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&compile_output.stdout),
            stderr
        )));
    }

    // Check warning-related assertions
    let compile_stderr = stderr.to_string();

    // Check if no warnings expected
    if case.no_warnings {
        if compile_stderr.contains("warning:") {
            return Err(TestFailure::assertion(format!(
                "Expected no warnings but got:\n{}\n  source: {}",
                compile_stderr, case.source
            )));
        }
    }

    // Check expected warning count
    if let Some(expected_count) = case.expected_warning_count {
        let actual_count = compile_stderr.matches("warning:").count();
        if actual_count != expected_count {
            return Err(TestFailure::assertion(format!(
                "Warning count mismatch:\n  expected: {}\n  actual: {}\n  stderr: {}\n  source: {}",
                expected_count, actual_count, compile_stderr, case.source
            )));
        }
    }

    // Check that warnings contain expected substrings
    if let Some(ref expected_warnings) = case.warning_contains {
        for expected in expected_warnings {
            if !compile_stderr.contains(expected) {
                return Err(TestFailure::assertion(format!(
                    "Warning message mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                    expected, compile_stderr, case.source
                )));
            }
        }
    }

    // If compile_only, we're done after successful compilation
    if case.compile_only {
        return Ok(());
    }

    // Run the compiled binary with timeout and optional stdin
    let timeout = Duration::from_millis(case.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let run_output = run_with_timeout(Command::new(&output_path), timeout, case.stdin.as_deref())?;

    if run_output.status.code().is_none() {
        return Err(TestFailure::fatal(format!(
            "TEST PROGRAM CRASH: process killed by signal ({:?})\n--- program stderr ---\n{}",
            run_output.status,
            String::from_utf8_lossy(&run_output.stderr)
        )));
    }

    let actual_exit_code = run_output.status.code().expect("signal handled above");
    let stderr = String::from_utf8_lossy(&run_output.stderr);

    // Handle runtime error tests
    if let Some(ref expected_error) = case.runtime_error {
        let expected_exit = case.runtime_exit_code.unwrap_or(RUNTIME_ERROR_EXIT_CODE);

        // Check exit code
        if actual_exit_code != expected_exit {
            return Err(TestFailure::assertion(format!(
                "Runtime error exit code mismatch:\n  expected: {}\n  actual: {}\n  source: {}",
                expected_exit, actual_exit_code, case.source
            )));
        }

        // Check that stderr contains the expected error message
        if !stderr.contains(expected_error.as_str()) {
            return Err(TestFailure::assertion(format!(
                "Runtime error message mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                expected_error, stderr, case.source
            )));
        }

        return Ok(());
    }

    // Check expected stdout output (e.g., from @dbg calls).
    //
    // The compare is byte-exact — matching the CLI runner in rue-cli-tests —
    // except for the single boundary newline the TOML `"""` block adds (see
    // `strip_block_boundary_newlines`). It deliberately does NOT run
    // `normalize_golden`, which would trim per-line trailing whitespace and
    // internal blank lines and thereby let a stdout-formatting regression pass
    // the spec suite while failing the byte-exact CLI runner (RUE-132). Values
    // are shown `{:?}`-quoted so a whitespace-only difference is visible.
    if let Some(ref expected) = case.expected_stdout {
        let stdout = String::from_utf8_lossy(&run_output.stdout);
        let expected_cmp = strip_block_boundary_newlines(expected);
        let actual_cmp = strip_block_boundary_newlines(&stdout);
        if actual_cmp != expected_cmp {
            return Err(TestFailure::assertion(format!(
                "Stdout mismatch:\n--- expected ---\n{:?}\n--- actual ---\n{:?}\n  source: {}",
                expected_cmp, actual_cmp, case.source
            )));
        }
    }

    // Check stderr contains expected substring (for non-error cases)
    if let Some(ref expected) = case.stderr_contains {
        if !stderr.contains(expected.as_str()) {
            return Err(TestFailure::assertion(format!(
                "Stderr mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                expected, stderr, case.source
            )));
        }
    }

    // Normal exit code test
    let expected_exit_code = case.exit_code.ok_or_else(|| {
        TestFailure::assertion(
            "Test case should have exit_code when compile_fail is false and runtime_error is not set",
        )
    })?;

    if actual_exit_code != expected_exit_code {
        return Err(TestFailure::assertion(format!(
            "Exit code mismatch:\n  expected: {}\n  actual: {}\n  source: {}",
            expected_exit_code, actual_exit_code, case.source
        )));
    }

    Ok(())
}

/// Find a directory by checking an environment variable, then a list of possible paths.
///
/// This function provides a consistent way to locate directories across different
/// working directory contexts (project root, crate directory, etc.).
///
/// # Arguments
/// * `env_var` - Environment variable to check first (e.g., "RUE_SPEC_DIR")
/// * `possible_paths` - List of relative paths to try if env var is not set
/// * `fallback` - Default path to return if no existing path is found
///
/// # Returns
/// The first existing path found, or the fallback if none exist.
pub fn find_dir(env_var: &str, possible_paths: &[&str], fallback: &str) -> PathBuf {
    std::env::var(env_var)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            Path::new(fallback).to_path_buf()
        })
}

/// Return the compiler path supplied through `RUE_BINARY`.
///
/// Test entry points set this explicitly; absence is an error because choosing
/// among Buck output configurations is not reliable.
pub fn find_rue_binary() -> PathBuf {
    // Explicit override — what test.sh, Buck targets, and scripts/rue set.
    if let Ok(p) = std::env::var("RUE_BINARY") {
        return PathBuf::from(p);
    }
    // Buck output directories may contain multiple configurations, so an
    // implicit mtime-based choice can run a compiler unrelated to this test.
    panic!(
        "cannot locate the rue compiler: set RUE_BINARY to an explicit path, \
         for example `RUE_BINARY=$(scripts/rue-bin)`"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TEST_FILE: &str = r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "valid"
source = "fn main() -> i32 { 0 }"
exit_code = 0
"#;

    #[test]
    fn discovery_is_recursive_and_deterministic() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("b")).unwrap();
        fs::create_dir(directory.path().join("a")).unwrap();
        fs::write(directory.path().join("b/z.toml"), "").unwrap();
        fs::write(directory.path().join("a/y.toml"), "").unwrap();
        fs::write(directory.path().join("a/ignored.md"), "").unwrap();

        let discovered = discover_files(directory.path(), "toml").unwrap();
        assert_eq!(
            discovered,
            vec![
                directory.path().join("a/y.toml"),
                directory.path().join("b/z.toml")
            ]
        );
    }

    #[test]
    fn discovery_rejects_a_missing_root() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        assert!(discover_files(&missing, "toml").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_an_unreadable_root() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&root).is_ok() {
            // Mode bits are not enforced for this user (root / CAP_DAC_OVERRIDE),
            // so the unreadable-directory premise is vacuous here. Skip, don't fail.
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            eprintln!("skipped: permission bits not enforced for this user");
            return;
        }
        let result = discover_files(&root, "toml");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn malformed_file_cannot_hide_behind_a_valid_sibling() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("a-valid.toml"), VALID_TEST_FILE).unwrap();
        fs::write(directory.path().join("b-malformed.toml"), "not = [valid").unwrap();

        let error = load_test_files(directory.path()).unwrap_err();
        assert!(error.contains("b-malformed.toml"), "{error}");
        assert!(error.contains("failed to load"), "{error}");
    }

    #[test]
    fn duplicate_test_names_are_rejected_with_both_same_file_cases() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("duplicates.toml"),
            r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "same"
source = "fn main() -> i32 { 0 }"
exit_code = 0

[[case]]
name = "same"
source = "fn main() -> i32 { 0 }"
exit_code = 0
"#,
        )
        .unwrap();

        let error = load_test_files(directory.path()).expect_err("duplicate must not load");
        assert!(
            error.contains("duplicate test name 'test.section::same'"),
            "{error}"
        );
        assert!(error.contains("duplicates.toml"), "{error}");
        assert!(error.matches("case 'same'").count() >= 2, "{error}");
    }

    #[test]
    fn duplicate_test_names_are_rejected_across_files() {
        let directory = tempfile::tempdir().unwrap();
        let file = || {
            format!(
                r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "same"
source = "fn main() -> i32 {{ 0 }}"
exit_code = 0
"#
            )
        };
        fs::write(directory.path().join("a.toml"), file()).unwrap();
        fs::write(directory.path().join("b.toml"), file()).unwrap();

        let error = load_test_files(directory.path()).expect_err("duplicate must not load");
        assert!(error.contains("a.toml"), "{error}");
        assert!(error.contains("b.toml"), "{error}");
        assert!(error.contains("case 'same'"), "{error}");
    }

    #[test]
    fn duplicate_test_names_are_rejected_after_parameter_expansion() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("parameterized.toml"),
            r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "case_{variant}"
source = "fn main() -> i32 { 0 }"
exit_code = 0
params = [
  { variant = "same" },
  { variant = "same" },
]
"#,
        )
        .unwrap();

        let error = load_test_files(directory.path()).expect_err("duplicate must not load");
        assert!(error.contains("test.section::case_same"), "{error}");
        assert!(error.contains("parameterized.toml"), "{error}");
        assert!(error.matches("case 'case_same'").count() >= 2, "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_subdirectory_fails_discovery() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let unreadable = directory.path().join("unreadable");
        fs::create_dir(&unreadable).unwrap();
        fs::write(unreadable.join("hidden.toml"), VALID_TEST_FILE).unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&unreadable).is_ok() {
            // Mode bits are not enforced for this user (root / CAP_DAC_OVERRIDE),
            // so the unreadable-subdirectory premise is vacuous here. Skip.
            fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
            eprintln!("skipped: permission bits not enforced for this user");
            return;
        }
        let result = discover_files(directory.path(), "toml");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_test_file_fails_loading() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let unreadable = directory.path().join("unreadable.toml");
        fs::write(&unreadable, VALID_TEST_FILE).unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&unreadable).is_ok() {
            // Mode bits are not enforced for this user (root / CAP_DAC_OVERRIDE),
            // so the unreadable-file premise is vacuous here. Skip, don't fail.
            fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o600)).unwrap();
            eprintln!("skipped: permission bits not enforced for this user");
            return;
        }
        let result = load_test_files(directory.path());
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.is_err());
    }

    #[cfg(unix)]
    fn fake_compiler(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary fake compiler directory");
        let binary = directory.path().join("rue");
        fs::write(&binary, script).expect("write fake compiler");
        let mut permissions = fs::metadata(&binary)
            .expect("fake compiler metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).expect("make fake compiler executable");
        (directory, binary)
    }

    fn case_with_param_override(override_entry: &str) -> Case {
        let toml = format!(
            r#"
[section]
id = "t.section"
name = "T"

[[case]]
name = "case_{{variant}}"
source = "fn main() -> i32 {{ 0 }}"
params = [
  {{ variant = "probe", {override_entry} }},
]
"#
        );
        let mut test_file: TestFile = toml::from_str(&toml).expect("valid TOML");
        test_file.case.pop().expect("one case")
    }

    #[test]
    fn test_substitute_placeholders_basic() {
        let mut params = HashMap::new();
        params.insert("type".to_string(), toml::Value::String("i32".to_string()));
        params.insert("value".to_string(), toml::Value::Integer(42));

        let result = substitute_placeholders("fn main() -> {type} { {value} }", &params);
        assert_eq!(result, "fn main() -> i32 { 42 }");
    }

    #[test]
    fn test_substitute_placeholders_multiple_occurrences() {
        let mut params = HashMap::new();
        params.insert("type".to_string(), toml::Value::String("i64".to_string()));

        let result = substitute_placeholders("{type} and {type} again", &params);
        assert_eq!(result, "i64 and i64 again");
    }

    #[test]
    fn test_substitute_placeholders_no_match() {
        let params = HashMap::new();
        let result = substitute_placeholders("no placeholders here", &params);
        assert_eq!(result, "no placeholders here");
    }

    #[test]
    fn test_parameter_placeholders_resolve_chains_independent_of_insertion_order() {
        let mut first = HashMap::new();
        first.insert(
            "name".to_string(),
            toml::Value::String("{type}".to_string()),
        );
        first.insert("type".to_string(), toml::Value::String("i32".to_string()));
        let mut second = HashMap::new();
        second.insert("type".to_string(), toml::Value::String("i32".to_string()));
        second.insert(
            "name".to_string(),
            toml::Value::String("{type}".to_string()),
        );

        let first_resolved = resolve_param_values(&first).unwrap();
        let second_resolved = resolve_param_values(&second).unwrap();
        assert_eq!(first_resolved, second_resolved);
        assert_eq!(
            substitute_placeholders("case_{name}", &first_resolved),
            "case_i32"
        );

        let expanded = expand_case(Case {
            name: "case_{name}".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            params: vec![ParamSet { values: first }, ParamSet { values: second }],
            ..Default::default()
        });
        assert_eq!(
            expanded
                .iter()
                .map(|case| (case.name.as_str(), case.source.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("case_i32", "fn main() -> i32 { 0 }"),
                ("case_i32", "fn main() -> i32 { 0 }"),
            ]
        );
    }

    #[test]
    fn test_parameter_placeholders_reject_unknown_references_and_cycles() {
        let unknown = HashMap::from([(
            "name".to_string(),
            toml::Value::String("{missing}".to_string()),
        )]);
        assert_eq!(
            resolve_param_values(&unknown),
            Err(ParamPlaceholderError::Unknown {
                key: "missing".to_string(),
                referenced_by: "name".to_string(),
            })
        );

        let cycle = HashMap::from([
            ("a".to_string(), toml::Value::String("{b}".to_string())),
            ("b".to_string(), toml::Value::String("{a}".to_string())),
        ]);
        assert_eq!(
            resolve_param_values(&cycle),
            Err(ParamPlaceholderError::Cycle {
                path: vec!["a".to_string(), "b".to_string(), "a".to_string()]
            })
        );
    }

    #[test]
    fn test_expand_case_no_params() {
        let case = Case {
            name: "test".to_string(),
            description: None,
            source: "fn main() {}".to_string(),
            exit_code: Some(0),
            compile_fail: false,
            compile_only: false,
            error_contains: ErrorContains::default(),
            expected_error: None,
            expected_error_code: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_lowering: None,
            expected_liveness: None,
            expected_regalloc: None,
            expected_asm: None,
            expected_stackframe: None,
            expected_abi: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec!["1.0:1".to_string()],
            expected_stdout: None,
            preview: None,
            preview_should_pass: false,
            real_std: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![],
            aux_files: HashMap::new(),
            only_on: vec![],
        };

        let expanded = expand_case(case);
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].name, "test");
    }

    #[test]
    fn test_expand_case_with_params() {
        let mut param1 = HashMap::new();
        param1.insert("type".to_string(), toml::Value::String("i8".to_string()));
        param1.insert("exit_code".to_string(), toml::Value::Integer(42));

        let mut param2 = HashMap::new();
        param2.insert("type".to_string(), toml::Value::String("i16".to_string()));
        param2.insert("exit_code".to_string(), toml::Value::Integer(100));

        let case = Case {
            name: "{type}_return".to_string(),
            description: None,
            source: "fn main() -> {type} { 0 }".to_string(),
            exit_code: None, // Will be overridden
            compile_fail: false,
            compile_only: false,
            error_contains: ErrorContains::default(),
            expected_error: None,
            expected_error_code: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_lowering: None,
            expected_liveness: None,
            expected_regalloc: None,
            expected_asm: None,
            expected_stackframe: None,
            expected_abi: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec!["3.1:1".to_string()],
            expected_stdout: None,
            preview: None,
            preview_should_pass: false,
            real_std: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![ParamSet { values: param1 }, ParamSet { values: param2 }],
            aux_files: HashMap::new(),
            only_on: vec![],
        };

        let expanded = expand_case(case);
        assert_eq!(expanded.len(), 2);

        assert_eq!(expanded[0].name, "i8_return");
        assert_eq!(expanded[0].source, "fn main() -> i8 { 0 }");
        assert_eq!(expanded[0].exit_code, Some(42));
        assert!(expanded[0].params.is_empty());

        assert_eq!(expanded[1].name, "i16_return");
        assert_eq!(expanded[1].source, "fn main() -> i16 { 0 }");
        assert_eq!(expanded[1].exit_code, Some(100));
        assert!(expanded[1].params.is_empty());
    }

    #[test]
    fn test_expand_case_spec_extra() {
        let mut params = HashMap::new();
        params.insert("type".to_string(), toml::Value::String("i8".to_string()));
        params.insert(
            "spec_extra".to_string(),
            toml::Value::Array(vec![toml::Value::String("3.1:2".to_string())]),
        );

        let case = Case {
            name: "{type}_test".to_string(),
            description: None,
            source: "fn main() {}".to_string(),
            exit_code: Some(0),
            compile_fail: false,
            compile_only: false,
            error_contains: ErrorContains::default(),
            expected_error: None,
            expected_error_code: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_lowering: None,
            expected_liveness: None,
            expected_regalloc: None,
            expected_asm: None,
            expected_stackframe: None,
            expected_abi: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec!["3.1:1".to_string()],
            expected_stdout: None,
            preview: None,
            preview_should_pass: false,
            real_std: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![ParamSet { values: params }],
            aux_files: HashMap::new(),
            only_on: vec![],
        };

        let expanded = expand_case(case);
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].spec, vec!["3.1:1", "3.1:2"]);
    }

    #[test]
    fn test_expand_case_compile_fail_override() {
        let mut params = HashMap::new();
        params.insert("type".to_string(), toml::Value::String("i8".to_string()));
        params.insert("compile_fail".to_string(), toml::Value::Boolean(true));
        params.insert(
            "error_msg".to_string(),
            toml::Value::String("type mismatch".to_string()),
        );

        let case = Case {
            name: "{type}_error".to_string(),
            description: None,
            source: "fn main() -> {type} { true }".to_string(),
            exit_code: None,
            compile_fail: false, // Will be overridden
            compile_only: false,
            error_contains: ErrorContains(vec!["{error_msg}".to_string()]),
            expected_error: None,
            expected_error_code: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_lowering: None,
            expected_liveness: None,
            expected_regalloc: None,
            expected_asm: None,
            expected_stackframe: None,
            expected_abi: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec![],
            expected_stdout: None,
            preview: None,
            preview_should_pass: false,
            real_std: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![ParamSet { values: params }],
            aux_files: HashMap::new(),
            only_on: vec![],
        };

        let expanded = expand_case(case);
        assert_eq!(expanded.len(), 1);
        assert!(expanded[0].compile_fail);
        assert_eq!(
            expanded[0].error_contains,
            ErrorContains(vec!["type mismatch".to_string()])
        );
    }

    #[test]
    fn test_expand_case_per_param_error_contains_override() {
        // A mixed parameterized case: one failing variant with its own
        // `error_contains`, one succeeding variant with none. The per-param
        // override must land on the failing variant (and be substituted), while
        // the succeeding variant carries no assertion — so the bare-compile_fail
        // guard is satisfied for the former and doesn't fire on the latter.
        let toml = r#"
[section]
id = "t.section"
name = "T"

[[case]]
name = "mixed_{variant}"
source = "fn f() -> {ty} { {body} }"
params = [
  { variant = "bad", ty = "i32", body = "true", compile_fail = true, error_contains = "expected {ty}" },
  { variant = "ok", ty = "i32", body = "0", compile_fail = false, exit_code = 0 },
]
"#;
        let tf: TestFile = toml::from_str(toml).expect("valid TOML");
        let expanded = expand_test_file(tf);
        let cases = &expanded.case;
        assert_eq!(cases.len(), 2);

        let bad = cases.iter().find(|c| c.name == "mixed_bad").unwrap();
        assert!(bad.compile_fail);
        // Placeholder in the override is substituted with the param value.
        assert_eq!(
            bad.error_contains,
            ErrorContains(vec!["expected i32".to_string()])
        );

        let ok = cases.iter().find(|c| c.name == "mixed_ok").unwrap();
        assert!(!ok.compile_fail);
        assert!(ok.error_contains.is_empty());

        // The guard accepts the failing variant and ignores the succeeding one.
        assert!(validate_compile_fail_assertions(&expanded).is_empty());
        assert!(validate_error_assertions(&expanded).is_empty());
    }

    #[test]
    fn test_expand_case_rejects_unknown_param_key() {
        let toml = r#"
[section]
id = "t.section"
name = "T"

[[case]]
name = "bad_{variant}"
source = "fn main() -> i32 { {body} }"
params = [
  { variant = "typo", body = "0", exit_cod = 42 },
]
"#;
        let tf: TestFile = toml::from_str(toml).expect("valid TOML");
        let unknown = unknown_param_keys(&tf.case[0]);

        assert_eq!(unknown, vec!["exit_cod"]);
    }

    #[test]
    fn test_param_overrides_reject_wrong_types_and_ranges() {
        let invalid = [
            ("no_warnings = \"true\"", "no_warnings", "a boolean"),
            (
                "exit_code = 2147483648",
                "exit_code",
                "an integer in the i32 range",
            ),
            (
                "runtime_exit_code = -2147483649",
                "runtime_exit_code",
                "an integer in the i32 range",
            ),
            ("opt_level = 4", "opt_level", "an integer from 0 through 3"),
            ("timeout_ms = -1", "timeout_ms", "a non-negative integer"),
            (
                "warning_contains = [\"warning\", 1]",
                "warning_contains",
                "a string or an array of strings",
            ),
            (
                "expected_warning_count = -1",
                "expected_warning_count",
                "a non-negative integer",
            ),
            (
                "spec_extra = \"1.2:3\"",
                "spec_extra",
                "an array of strings",
            ),
            ("expected_mir = 1", "expected_mir", "a string"),
        ];

        for (override_entry, key, expected) in invalid {
            let case = case_with_param_override(override_entry);
            let errors = invalid_param_overrides(&case);
            assert_eq!(errors.len(), 1, "{override_entry}");
            assert_eq!(errors[0].param_index, 1);
            assert_eq!(errors[0].key, key);
            assert_eq!(errors[0].expected, expected);
        }
    }

    #[test]
    fn test_param_overrides_accept_valid_boundaries() {
        let case = case_with_param_override(
            "exit_code = -2147483648, runtime_exit_code = 2147483647, \
             compile_fail = true, compile_only = false, skip = false, no_warnings = true, \
             preview_should_pass = false, opt_level = 3, target = \"x86-64-linux\", \
             preview = \"modules\", timeout_ms = 0, error_contains = [\"one\", \"two\"], \
             expected_error = \"error\", warning_contains = \"warning\", \
             expected_warning_count = 0, spec_extra = [\"1.2:3\"]",
        );

        assert!(invalid_param_overrides(&case).is_empty());
    }

    #[test]
    #[should_panic(expected = "invalid parameter override value")]
    fn test_expand_case_rejects_invalid_param_override() {
        expand_case(case_with_param_override("no_warnings = \"true\""));
    }

    #[test]
    fn test_expand_case_allows_param_key_used_only_in_error_contains() {
        let toml = r#"
[section]
id = "t.section"
name = "T"

[[case]]
name = "uses_error_placeholder"
source = "fn main() -> i32 { true }"
compile_fail = true
error_contains = "expected {ty}"
params = [
  { ty = "i32" },
]
"#;
        let tf: TestFile = toml::from_str(toml).expect("valid TOML");
        let expanded = expand_test_file(tf);

        assert_eq!(expanded.case.len(), 1);
        assert_eq!(
            expanded.case[0].error_contains,
            ErrorContains(vec!["expected i32".to_string()])
        );
    }

    #[test]
    fn test_expand_case_substitutes_placeholders_in_warning_golden_and_aux_fields() {
        let toml = r#"
[section]
id = "t.section"
name = "T"

[[case]]
name = "case_{variant}"
description = "description for {variant}"
source = "fn main() -> i32 { {value} }"
expected_air = "air {variant}"
warning_contains = ["warning {variant}"]
spec = ["1.0:{spec_id}"]
expected_stdout = "stdout {variant}"
preview = "{preview_name}"
target = "{target_name}"
stdin = "stdin {variant}"
stderr_contains = "stderr {variant}"
aux_files = { "helper_{variant}.rue" = "fn helper() -> i32 { {value} }" }
only_on = ["{target_name}"]
params = [
  { variant = "alpha", value = 1, spec_id = 2, preview_name = "modules", target_name = "x86-64-linux" },
]
"#;
        let tf: TestFile = toml::from_str(toml).expect("valid TOML");
        let expanded = expand_test_file(tf);
        let case = &expanded.case[0];

        assert_eq!(case.name, "case_alpha");
        assert_eq!(case.description.as_deref(), Some("description for alpha"));
        assert_eq!(case.source, "fn main() -> i32 { 1 }");
        assert_eq!(case.expected_air.as_deref(), Some("air alpha"));
        assert_eq!(
            case.warning_contains,
            Some(vec!["warning alpha".to_string()])
        );
        assert_eq!(case.spec, vec!["1.0:2"]);
        assert_eq!(case.expected_stdout.as_deref(), Some("stdout alpha"));
        assert_eq!(case.preview.as_deref(), Some("modules"));
        assert_eq!(case.target.as_deref(), Some("x86-64-linux"));
        assert_eq!(case.stdin.as_deref(), Some("stdin alpha"));
        assert_eq!(case.stderr_contains.as_deref(), Some("stderr alpha"));
        assert_eq!(
            case.aux_files.get("helper_alpha.rue").map(String::as_str),
            Some("fn helper() -> i32 { 1 }")
        );
        assert_eq!(case.only_on, vec!["x86-64-linux"]);
    }

    #[test]
    fn test_expand_case_supports_warning_and_golden_param_overrides() {
        let toml = r#"
[section]
id = "t.section"
name = "T"

[[case]]
name = "case_{variant}"
source = "fn main() -> i32 { 0 }"
warning_contains = ["base warning"]
expected_warning_count = 99
expected_cfg = "base cfg"
expected_mir = "base mir"
params = [
  { variant = "warn", warning_name = "unused variable", warning_contains = ["{warning_name}", "second warning"], expected_warning_count = 2, expected_cfg = "cfg {variant}", expected_mir = "mir {variant}" },
  { variant = "quiet", warning_contains = [], expected_warning_count = 0, expected_cfg = "cfg {variant}", expected_mir = "mir {variant}" },
]
"#;
        let tf: TestFile = toml::from_str(toml).expect("valid TOML");
        let expanded = expand_test_file(tf);

        let warn = expanded
            .case
            .iter()
            .find(|case| case.name == "case_warn")
            .unwrap();
        assert_eq!(
            warn.warning_contains,
            Some(vec![
                "unused variable".to_string(),
                "second warning".to_string()
            ])
        );
        assert_eq!(warn.expected_warning_count, Some(2));
        assert_eq!(warn.expected_cfg.as_deref(), Some("cfg warn"));
        assert_eq!(warn.expected_mir.as_deref(), Some("mir warn"));

        let quiet = expanded
            .case
            .iter()
            .find(|case| case.name == "case_quiet")
            .unwrap();
        assert_eq!(quiet.warning_contains, Some(vec![]));
        assert_eq!(quiet.expected_warning_count, Some(0));
        assert_eq!(quiet.expected_cfg.as_deref(), Some("cfg quiet"));
        assert_eq!(quiet.expected_mir.as_deref(), Some("mir quiet"));
    }

    #[test]
    fn test_toml_value_to_string() {
        assert_eq!(
            toml_value_to_string(&toml::Value::String("hello".to_string())),
            "hello"
        );
        assert_eq!(toml_value_to_string(&toml::Value::Integer(42)), "42");
        assert_eq!(toml_value_to_string(&toml::Value::Float(3.14)), "3.14");
        assert_eq!(toml_value_to_string(&toml::Value::Boolean(true)), "true");
    }

    // Tests for normalize_golden
    #[test]
    fn test_normalize_golden_trims_trailing_whitespace() {
        let input = "line1   \nline2  \nline3\t\t";
        let expected = "line1\nline2\nline3";
        assert_eq!(normalize_golden(input), expected);
    }

    #[test]
    fn test_normalize_golden_trims_leading_and_trailing_empty_lines() {
        let input = "\n\nline1\nline2\n\n";
        let expected = "line1\nline2";
        assert_eq!(normalize_golden(input), expected);
    }

    #[test]
    fn test_normalize_golden_preserves_internal_indentation() {
        // Leading whitespace on the first line is trimmed by the final .trim() call,
        // but internal indentation (relative to the first line) is preserved.
        let input = "line1\n    indented line\n  less indented";
        let expected = "line1\n    indented line\n  less indented";
        assert_eq!(normalize_golden(input), expected);
    }

    #[test]
    fn test_normalize_golden_empty_string() {
        assert_eq!(normalize_golden(""), "");
    }

    #[test]
    fn test_normalize_golden_only_whitespace() {
        assert_eq!(normalize_golden("   \n  \t  \n  "), "");
    }

    #[test]
    fn test_normalize_golden_single_line() {
        assert_eq!(normalize_golden("hello world  "), "hello world");
    }

    #[test]
    fn test_normalize_golden_mixed_line_endings() {
        // normalize_golden uses .lines() which handles \r\n, \n, and \r
        let input = "line1\r\nline2\nline3";
        let result = normalize_golden(input);
        // Result should have normalized line endings
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
    }

    // Tests for normalize_error_output
    #[test]
    fn test_normalize_error_output_replaces_path() {
        let source_path = Path::new("/tmp/test123/source.rue");
        let input = "error[E001]: type mismatch at /tmp/test123/source.rue:5:10";
        let result = normalize_error_output(input, source_path);
        assert_eq!(result, "error[E001]: type mismatch at <source>:5:10");
    }

    #[test]
    fn test_normalize_error_output_multiple_occurrences() {
        let source_path = Path::new("/path/to/file.rue");
        let input = "error at /path/to/file.rue:1\nnote: see /path/to/file.rue:2";
        let result = normalize_error_output(input, source_path);
        assert_eq!(result, "error at <source>:1\nnote: see <source>:2");
    }

    #[test]
    fn test_normalize_error_output_no_path_present() {
        let source_path = Path::new("/nonexistent/path.rue");
        let input = "error: something went wrong";
        let result = normalize_error_output(input, source_path);
        assert_eq!(result, "error: something went wrong");
    }

    #[test]
    fn test_normalize_error_output_also_normalizes_whitespace() {
        let source_path = Path::new("/tmp/test.rue");
        let input = "/tmp/test.rue:1  \n  /tmp/test.rue:2  ";
        let result = normalize_error_output(input, source_path);
        assert_eq!(result, "<source>:1\n  <source>:2");
    }

    // Tests for strip_emit_header
    #[test]
    fn test_strip_emit_header_simple() {
        let input = "=== RIR ===\nfn main() {\n  ret 0\n}";
        let result = strip_emit_header(input, "RIR");
        assert_eq!(result, "fn main() {\n  ret 0\n}");
    }

    #[test]
    fn test_strip_emit_header_with_target() {
        let input = "=== MIR (x86-64-linux) ===\nmov rax, 0\nret";
        let result = strip_emit_header(input, "MIR");
        assert_eq!(result, "mov rax, 0\nret");
    }

    #[test]
    fn test_strip_emit_header_with_macos_target() {
        let input = "=== MIR (aarch64-macos) ===\nmov x0, #0\nret";
        let result = strip_emit_header(input, "MIR");
        assert_eq!(result, "mov x0, #0\nret");
    }

    #[test]
    fn test_strip_emit_header_no_header_present() {
        let input = "fn main() {\n  ret 0\n}";
        let result = strip_emit_header(input, "RIR");
        assert_eq!(result, "fn main() {\n  ret 0\n}");
    }

    #[test]
    fn test_strip_emit_header_wrong_stage() {
        let input = "=== AST ===\nsome ast content";
        let result = strip_emit_header(input, "RIR");
        // Should not strip AST header when looking for RIR
        assert_eq!(result, "=== AST ===\nsome ast content");
    }

    #[test]
    fn test_strip_emit_header_multiple_headers() {
        let input = "=== Tokens ===\ntoken1\n=== AST ===\nast content";
        let result = strip_emit_header(input, "Tokens");
        assert_eq!(result, "token1\n=== AST ===\nast content");
    }

    #[test]
    fn test_strip_emit_header_preserves_similar_text() {
        // Ensure we don't strip lines that merely contain the stage name
        let input = "=== RIR ===\nThis is RIR output\nRIR is great";
        let result = strip_emit_header(input, "RIR");
        assert_eq!(result, "This is RIR output\nRIR is great");
    }

    // Tests for check_golden
    #[test]
    fn test_check_golden_matching() {
        let actual = "line1\nline2";
        let expected = "line1\nline2";
        assert!(check_golden(actual, expected, "Test").is_ok());
    }

    #[test]
    fn test_check_golden_matching_with_whitespace_differences() {
        let actual = "line1  \nline2\t";
        let expected = "line1\nline2";
        assert!(check_golden(actual, expected, "Test").is_ok());
    }

    #[test]
    fn test_check_golden_mismatch() {
        let actual = "line1\nline2";
        let expected = "line1\nline3";
        let result = check_golden(actual, expected, "Test");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Test mismatch"));
        assert!(err.contains("expected"));
        assert!(err.contains("actual"));
    }

    #[test]
    fn test_check_golden_empty_strings() {
        assert!(check_golden("", "", "Test").is_ok());
    }

    #[test]
    fn test_check_golden_whitespace_only() {
        assert!(check_golden("  \n  ", "\t\n\t", "Test").is_ok());
    }

    #[test]
    fn test_check_golden_leading_trailing_differences() {
        let actual = "\n\nline1\n\n";
        let expected = "line1";
        assert!(check_golden(actual, expected, "Test").is_ok());
    }

    #[test]
    fn test_expected_failure_classification_preserves_fatal_errors_and_xpass() {
        assert_eq!(
            classify_expected_failure(Ok(())),
            ExpectedFailureOutcome::UnexpectedPass
        );

        let assertion = TestFailure::assertion("wrong output");
        assert_eq!(
            classify_expected_failure(Err(assertion.clone())),
            ExpectedFailureOutcome::ExpectedFailure(assertion)
        );

        let fatal = TestFailure::fatal("compiler timed out").with_context("at -O2");
        assert!(fatal.is_fatal());
        assert_eq!(
            classify_expected_failure(Err(fatal.clone())),
            ExpectedFailureOutcome::FatalFailure(fatal)
        );
    }

    #[test]
    fn test_case_compiler_command_removes_ambient_configuration() {
        let case = Case {
            target: Some("x86-64-linux".to_string()),
            preview: Some("test_infra".to_string()),
            opt_level: Some(2),
            ..Default::default()
        };
        let command = test_case_compiler_command(&case, Path::new("rue"));
        let environments: HashMap<_, _> = command.get_envs().collect();

        assert_eq!(
            environments.get(std::ffi::OsStr::new("RUE_STD_PATH")),
            Some(&None)
        );
        assert_eq!(
            environments.get(std::ffi::OsStr::new("RUST_LOG")),
            Some(&None)
        );
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--target", "x86-64-linux", "--preview", "test_infra", "-O2"]
        );
    }

    #[test]
    fn test_case_compiler_command_opts_into_real_standard_library() {
        let case = Case {
            real_std: true,
            ..Default::default()
        };
        let command = test_case_compiler_command(&case, Path::new("rue"));
        let environments: HashMap<_, _> = command.get_envs().collect();

        assert!(matches!(
            environments.get(std::ffi::OsStr::new("RUE_STD_PATH")),
            Some(Some(path)) if path.to_string_lossy().ends_with("std")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn test_fake_compiler_panic_is_fatal() {
        let (_directory, binary) =
            fake_compiler("#!/bin/sh\nprintf 'panicked at fake compiler' >&2\nexit 101\n");
        let case = Case {
            name: "compiler_panic".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            ..Default::default()
        };

        let error = run_test_case(&case, &binary).expect_err("compiler panic must fail");
        assert!(error.is_fatal());
        assert!(error.contains("INTERNAL COMPILER ERROR"));
    }

    #[cfg(unix)]
    #[test]
    fn test_fake_compiler_signal_is_fatal() {
        let (_directory, binary) = fake_compiler("#!/bin/sh\nkill -ABRT $$\n");
        let case = Case {
            name: "compiler_signal".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            ..Default::default()
        };

        let error = run_test_case(&case, &binary).expect_err("compiler signal must fail");
        assert!(error.is_fatal());
        assert!(error.contains("compiler killed by signal"));
    }

    #[cfg(unix)]
    fn typed_diagnostic_case() -> Case {
        Case {
            name: "typed_diagnostic".to_string(),
            source: "fn main() { missing }".to_string(),
            compile_fail: true,
            expected_error_code: Some("E0201".to_string()),
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_typed_diagnostic_assertion_accepts_one_exact_json_code() {
        let (_directory, binary) = fake_compiler(
            r#"#!/bin/sh
case " $* " in
  *" --error-format json "*) printf '%s\n' '[{"code":"E0201","helps":[],"message":"missing name","notes":[],"severity":"error","spans":[],"suggestions":[]}]' >&2 ;;
  *) printf '%s\n' 'error: compile failed' >&2 ;;
esac
exit 1
"#,
        );
        run_test_case(&typed_diagnostic_case(), &binary).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_typed_diagnostic_assertion_rejects_mismatch_and_duplicate_emission() {
        for (json, expected_message) in [
            (
                r#"[{"code":"E0206","helps":[],"message":"mismatch","notes":[],"severity":"error","spans":[],"suggestions":[]}]"#,
                "Diagnostic code mismatch",
            ),
            (
                r#"[{"code":"E0201","helps":[],"message":"first","notes":[],"severity":"error","spans":[],"suggestions":[]},{"code":"E0201","helps":[],"message":"second","notes":[],"severity":"error","spans":[],"suggestions":[]}]"#,
                "Ambiguous diagnostic emission",
            ),
            (
                r#"[{"code":"W0001","helps":[],"message":"warning only","notes":[],"severity":"warning","spans":[],"suggestions":[]}]"#,
                "emitted none",
            ),
        ] {
            let script = format!(
                "#!/bin/sh\ncase \" $* \" in\n  *\" --error-format json \"*) printf '%s\\n' '{json}' >&2 ;;\n  *) printf '%s\\n' 'error: compile failed' >&2 ;;\nesac\nexit 1\n"
            );
            let (_directory, binary) = fake_compiler(&script);
            let error = run_test_case(&typed_diagnostic_case(), &binary).unwrap_err();
            assert!(!error.is_fatal());
            assert!(error.contains(expected_message));
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_typed_diagnostic_assertion_preserves_compiler_options() {
        let (_directory, binary) = fake_compiler(
            r#"#!/bin/sh
case " $* " in
  *" --target x86-64-linux --preview modules -O2 --error-format json "*) printf '%s\n' '[{"code":"E0201","helps":[],"message":"missing name","notes":[],"severity":"error","spans":[],"suggestions":[]}]' >&2 ;;
  *" --target x86-64-linux --preview modules -O2 "*) printf '%s\n' 'error: compile failed' >&2 ;;
  *) printf '%s\n' 'missing compiler option' >&2; exit 2 ;;
esac
exit 1
"#,
        );
        let mut case = typed_diagnostic_case();
        case.target = Some("x86-64-linux".to_string());
        case.preview = Some("modules".to_string());
        case.opt_level = Some(2);
        run_test_case(&case, &binary).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_typed_diagnostic_assertion_rejects_malformed_json_as_fatal() {
        let (_directory, binary) = fake_compiler(
            r#"#!/bin/sh
case " $* " in
  *" --error-format json "*) printf '%s\n' 'not-json' >&2 ;;
  *) printf '%s\n' 'error: compile failed' >&2 ;;
esac
exit 1
"#,
        );
        let error = run_test_case(&typed_diagnostic_case(), &binary).unwrap_err();
        assert!(error.is_fatal());
        assert!(error.contains("Malformed structured diagnostic metadata"));
    }

    #[cfg(unix)]
    #[test]
    fn test_fake_golden_compiler_panic_is_fatal() {
        let (_directory, binary) =
            fake_compiler("#!/bin/sh\nprintf 'panicked at fake emit' >&2\nexit 101\n");
        let case = Case {
            name: "golden_compiler_panic".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            expected_tokens: Some("token".to_string()),
            ..Default::default()
        };

        let error = run_test_case(&case, &binary).expect_err("emit panic must fail");
        assert!(error.is_fatal());
        assert!(error.contains("rue --emit tokens"));
        assert!(error.contains("INTERNAL COMPILER ERROR"));
    }

    #[cfg(unix)]
    #[test]
    fn test_fake_compiler_timeout_is_fatal() {
        let (_directory, binary) = fake_compiler("#!/bin/sh\nsleep 5\n");
        let case = Case {
            name: "compiler_timeout".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            timeout_ms: Some(20),
            ..Default::default()
        };

        let error = run_test_case(&case, &binary).expect_err("compiler timeout must fail");
        assert!(error.is_fatal());
        assert!(error.contains(TIMEOUT_PREFIX));
    }

    #[cfg(unix)]
    #[test]
    fn test_generated_program_signal_is_fatal() {
        let (_directory, binary) = fake_compiler(
            r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
    if [ "$1" = "-o" ]; then
        output="$2"
        break
    fi
    shift
done
cat > "$output" <<'EOF'
#!/bin/sh
kill -ABRT $$
EOF
chmod +x "$output"
"#,
        );
        let case = Case {
            name: "program_signal".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            exit_code: Some(0),
            ..Default::default()
        };

        let error = run_test_case(&case, &binary).expect_err("program signal must fail");
        assert!(error.is_fatal());
        assert!(error.contains("TEST PROGRAM CRASH"));
    }

    // Tests for run_with_timeout
    #[test]
    fn test_run_with_timeout_completes_normally() {
        // A simple command that completes quickly
        let cmd = Command::new("echo");
        let result = run_with_timeout(cmd, Duration::from_secs(5), None);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn test_run_with_timeout_captures_stdout() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let result = run_with_timeout(cmd, Duration::from_secs(5), None);
        assert!(result.is_ok());
        let output = result.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("hello"));
    }

    #[test]
    fn test_run_with_timeout_kills_slow_process() {
        // Sleep for 10 seconds but timeout after 100ms
        let mut cmd = Command::new("sleep");
        cmd.arg("10");
        let result = run_with_timeout(cmd, Duration::from_millis(100), None);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_fatal());
        assert!(
            err.contains("timed out"),
            "Error should mention timeout: {}",
            err
        );
        assert!(
            err.starts_with(TIMEOUT_PREFIX),
            "Timeout error should be a distinct TIMEOUT failure class: {}",
            err
        );
    }

    #[test]
    fn test_run_with_timeout_captures_exit_code() {
        // Use a command that exits with a non-zero status
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("exit 42");
        let result = run_with_timeout(cmd, Duration::from_secs(5), None);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.status.code(), Some(42));
    }

    #[test]
    fn test_run_with_timeout_pipes_stdin() {
        // Use cat to echo back stdin
        let cmd = Command::new("cat");
        let result = run_with_timeout(cmd, Duration::from_secs(5), Some("hello from stdin"));
        assert!(result.is_ok());
        let output = result.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout, "hello from stdin");
    }

    #[test]
    fn test_run_with_timeout_stdin_with_newlines() {
        // Use cat to echo back stdin with newlines
        let cmd = Command::new("cat");
        let result = run_with_timeout(cmd, Duration::from_secs(5), Some("line1\nline2\n"));
        assert!(result.is_ok());
        let output = result.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout, "line1\nline2\n");
    }

    // Tests for validate_preview_features

    fn make_test_case(name: &str, preview: Option<&str>) -> Case {
        Case {
            name: name.to_string(),
            description: None,
            source: "fn main() {}".to_string(),
            exit_code: Some(0),
            compile_fail: false,
            compile_only: false,
            error_contains: ErrorContains::default(),
            expected_error: None,
            expected_error_code: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_lowering: None,
            expected_liveness: None,
            expected_regalloc: None,
            expected_asm: None,
            expected_stackframe: None,
            expected_abi: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec![],
            expected_stdout: None,
            preview: preview.map(|s| s.to_string()),
            preview_should_pass: false,
            real_std: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![],
            aux_files: HashMap::new(),
            only_on: vec![],
        }
    }

    fn make_test_file(section_id: &str, cases: Vec<Case>) -> TestFile {
        TestFile {
            section: Section {
                id: section_id.to_string(),
                name: "Test Section".to_string(),
                description: String::new(),
                spec_chapter: None,
            },
            case: cases,
        }
    }

    #[test]
    fn test_validate_preview_features_no_preview() {
        // Test with no preview features - should return no errors
        let test_file = make_test_file(
            "test",
            vec![
                make_test_case("basic_test", None),
                make_test_case("another_test", None),
            ],
        );

        let errors = validate_preview_features(&test_file);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_preview_features_valid_feature() {
        // Test with a valid preview feature
        let test_file = make_test_file(
            "test",
            vec![make_test_case("preview_test", Some("test_infra"))],
        );

        let errors = validate_preview_features(&test_file);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_preview_features_unknown_feature() {
        // Test with an unknown preview feature
        let test_file = make_test_file(
            "expressions",
            vec![make_test_case("bad_test", Some("nonexistent_feature"))],
        );

        let errors = validate_preview_features(&test_file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].feature_name, "nonexistent_feature");
        assert_eq!(errors[0].test_name, "bad_test");
        assert_eq!(errors[0].section_id, "expressions");
    }

    #[test]
    fn test_validate_preview_features_typo() {
        // Test with a typo in the preview feature name (common case)
        let test_file = make_test_file(
            "items",
            vec![
                make_test_case("typo_test", Some("test_infr")), // Missing 'a'
            ],
        );

        let errors = validate_preview_features(&test_file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].feature_name, "test_infr");
    }

    #[test]
    fn test_validate_preview_features_multiple_errors() {
        // Test with multiple unknown preview features
        let test_file = make_test_file(
            "test",
            vec![
                make_test_case("good_test", Some("test_infra")), // Valid
                make_test_case("bad_test_1", Some("unknown1")),  // Invalid
                make_test_case("normal_test", None),             // No preview
                make_test_case("bad_test_2", Some("unknown2")),  // Invalid
            ],
        );

        let errors = validate_preview_features(&test_file);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].feature_name, "unknown1");
        assert_eq!(errors[1].feature_name, "unknown2");
    }

    // Tests for golden-IR / execution assertion classification (RUE-132)

    #[test]
    fn test_has_golden_ir_assertions() {
        let mut case = make_test_case("golden", None);
        case.exit_code = None;
        assert!(!case.has_golden_ir_assertions());
        case.expected_cfg = Some("cfg main {}".to_string());
        assert!(case.has_golden_ir_assertions());
    }

    #[test]
    fn test_has_execution_assertions_exit_code() {
        // make_test_case sets exit_code = Some(0)
        let case = make_test_case("exec", None);
        assert!(case.has_execution_assertions());
    }

    #[test]
    fn test_has_execution_assertions_stdout_only() {
        let mut case = make_test_case("exec", None);
        case.exit_code = None;
        assert!(!case.has_execution_assertions());
        case.expected_stdout = Some("1\n2\n".to_string());
        assert!(
            case.has_execution_assertions(),
            "expected_stdout must force the program to actually run"
        );
    }

    #[test]
    fn test_pure_golden_case_has_no_execution_assertions() {
        let mut case = make_test_case("golden", None);
        case.exit_code = None;
        case.expected_air = Some("air".to_string());
        assert!(case.has_golden_ir_assertions());
        assert!(!case.has_execution_assertions());
    }

    #[test]
    fn test_unknown_preview_feature_error_display() {
        let error = UnknownPreviewFeatureError {
            feature_name: "bad_feature".to_string(),
            test_name: "my_test".to_string(),
            section_id: "section.id".to_string(),
        };

        let msg = error.to_string();
        assert!(msg.contains("bad_feature"), "Should contain feature name");
        assert!(msg.contains("my_test"), "Should contain test name");
        assert!(msg.contains("section.id"), "Should contain section ID");
        assert!(msg.contains("test_infra"), "Should list valid features");
    }

    // Tests for strip_block_boundary_newlines (RUE-132: exact stdout compare).

    #[test]
    fn test_strip_block_boundary_newlines_strips_one_each_end() {
        // A leading and a trailing newline (the TOML `"""` authoring boundary)
        // are stripped; content in between is untouched.
        assert_eq!(strip_block_boundary_newlines("\n2\n1\n"), "2\n1");
        assert_eq!(strip_block_boundary_newlines("2\n1\n"), "2\n1");
        assert_eq!(strip_block_boundary_newlines("2\n1"), "2\n1");
    }

    #[test]
    fn test_strip_block_boundary_newlines_preserves_internal_and_trailing_ws() {
        // Internal blank lines and per-line trailing whitespace survive, so a
        // stdout-formatting regression is still caught by the byte-exact compare
        // (unlike normalize_golden, which would erase both).
        assert_eq!(strip_block_boundary_newlines("a\n\nb\n"), "a\n\nb");
        assert_eq!(strip_block_boundary_newlines("a \nb\n"), "a \nb");
    }

    #[test]
    fn test_strip_block_boundary_newlines_only_one_trailing() {
        // Only ONE trailing newline is stripped: an extra trailing blank line
        // survives and would (correctly) fail the compare.
        assert_eq!(strip_block_boundary_newlines("a\n\n"), "a\n");
    }

    #[test]
    fn test_strip_block_boundary_newlines_handles_crlf() {
        assert_eq!(strip_block_boundary_newlines("\r\na\r\n"), "a");
    }

    // Tests for validate_error_assertions (RUE-132: reject stray compile-error
    // assertions on cases that aren't compile_fail).

    #[test]
    fn test_validate_error_assertions_rejects_error_contains_without_compile_fail() {
        // make_test_case sets compile_fail = false, exit_code = Some(0).
        let mut case = make_test_case("stray", None);
        case.error_contains = ErrorContains(vec!["type mismatch".to_string()]);
        let tf = make_test_file("sec", vec![case]);
        let errors = validate_error_assertions(&tf);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].test_name, "stray");
        assert!(errors[0].fields.contains("error_contains"));
    }

    #[test]
    fn test_validate_error_assertions_rejects_expected_error_without_compile_fail() {
        let mut case = make_test_case("stray2", None);
        case.expected_error = Some("error[E0001]".to_string());
        let tf = make_test_file("sec", vec![case]);
        let errors = validate_error_assertions(&tf);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].fields.contains("expected_error"));
    }

    #[test]
    fn test_validate_error_assertions_rejects_expected_error_code_without_compile_fail() {
        let mut case = make_test_case("typed_stray", None);
        case.expected_error_code = Some("E0206".to_string());
        let errors = validate_error_assertions(&make_test_file("sec", vec![case]));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].fields.contains("expected_error_code"));
    }

    #[test]
    fn test_validate_expected_error_codes_uses_compiler_inventory() {
        let mut known = make_test_case("known", None);
        known.expected_error_code = Some("E0206".to_string());
        let mut unknown = make_test_case("unknown", None);
        unknown.expected_error_code = Some("E9999".to_string());
        let errors = validate_expected_error_codes(&make_test_file("sec", vec![known, unknown]));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "E9999");
        assert_eq!(errors[0].test_name, "unknown");
    }

    #[test]
    fn test_expected_error_code_override_is_expanded_then_validated() {
        let test_file: TestFile = toml::from_str(
            r#"
[section]
id = "sec"
name = "Section"

[[case]]
name = "typed_{variant}"
source = "fn main() { missing }"
compile_fail = true
params = [
  { variant = "known", expected_error_code = "E0201" },
  { variant = "unknown", expected_error_code = "E9999" },
]
"#,
        )
        .unwrap();
        let expanded = expand_test_file(test_file);
        assert_eq!(
            expanded.case[0].expected_error_code.as_deref(),
            Some("E0201")
        );
        let errors = validate_expected_error_codes(&expanded);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].test_name, "typed_unknown");
    }

    #[test]
    fn test_expected_error_code_override_rejects_non_string_metadata() {
        let case = case_with_param_override("expected_error_code = 206");
        let errors = invalid_param_overrides(&case);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].key, "expected_error_code");
        assert_eq!(errors[0].expected, "a string");
    }

    #[test]
    fn test_json_error_code_parser_is_exact_and_fails_closed() {
        let single = r#"[{"code":"E0206","helps":[],"message":"mismatch","notes":[],"severity":"error","spans":[],"suggestions":[]}]"#;
        assert_eq!(parse_json_error_codes(single).unwrap(), vec!["E0206"]);

        let duplicate = r#"[{"code":"E0206","helps":[],"message":"first","notes":[],"severity":"error","spans":[],"suggestions":[]},{"code":"E0206","helps":[],"message":"second","notes":[],"severity":"error","spans":[],"suggestions":[]}]"#;
        assert_eq!(
            parse_json_error_codes(duplicate).unwrap(),
            vec!["E0206", "E0206"]
        );
        let warning = r#"[{"code":"W0001","helps":[],"message":"warning","notes":[],"severity":"warning","spans":[],"suggestions":[]}]"#;
        assert!(parse_json_error_codes(warning).unwrap().is_empty());
        assert!(parse_json_error_codes("[]").is_err());
        assert!(parse_json_error_codes(r#"{"code":"E0206","severity":"error"}"#).is_err());
        assert!(parse_json_error_codes(r#"[{"code":206,"severity":"error"}]"#).is_err());
        assert!(parse_json_error_codes(r#"[{"code":"E0206","severity":"error"}]"#).is_err());
        assert!(parse_json_error_codes("not json").is_err());
    }

    #[test]
    fn test_validate_error_assertions_allows_error_contains_with_compile_fail() {
        let mut case = make_test_case("ok", None);
        case.compile_fail = true;
        case.exit_code = None;
        case.error_contains = ErrorContains(vec!["type mismatch".to_string()]);
        let tf = make_test_file("sec", vec![case]);
        assert!(validate_error_assertions(&tf).is_empty());
    }

    #[test]
    fn test_validate_error_assertions_clean_case_ok() {
        let case = make_test_case("plain", None);
        let tf = make_test_file("sec", vec![case]);
        assert!(validate_error_assertions(&tf).is_empty());
    }

    #[test]
    fn test_compile_only_rejects_each_runtime_field() {
        for field in [
            "exit_code",
            "expected_stdout",
            "runtime_error",
            "runtime_exit_code",
            "stdin",
            "stderr_contains",
        ] {
            let mut case = make_test_case(field, None);
            case.compile_only = true;
            case.exit_code = None;
            match field {
                "exit_code" => case.exit_code = Some(0),
                "expected_stdout" => case.expected_stdout = Some("output".to_string()),
                "runtime_error" => case.runtime_error = Some("panic".to_string()),
                "runtime_exit_code" => case.runtime_exit_code = Some(RUNTIME_ERROR_EXIT_CODE),
                "stdin" => case.stdin = Some("input".to_string()),
                "stderr_contains" => case.stderr_contains = Some("stderr".to_string()),
                _ => unreachable!(),
            }
            let errors =
                validate_compile_only_runtime_assertions(&make_test_file("runtime", vec![case]));
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].fields, format!("`{field}`"));
        }
        let mut valid = make_test_case("valid", None);
        valid.compile_only = true;
        valid.exit_code = None;
        assert!(
            validate_compile_only_runtime_assertions(&make_test_file("runtime", vec![valid]))
                .is_empty()
        );
    }

    #[test]
    fn test_empty_contains_assertions_reject_empty_entries_and_allow_empty_arrays() {
        let mut error = make_test_case("empty", None);
        error.error_contains = ErrorContains(vec!["".to_string(), "error".to_string()]);
        error.warning_contains = Some(vec!["".to_string()]);
        error.stderr_contains = Some(String::new());
        let errors = validate_empty_contains_assertions(&make_test_file("runtime", vec![error]));
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].fields,
            "`error_contains[0]`, `warning_contains[0]`, `stderr_contains`"
        );

        let valid = Case {
            warning_contains: Some(vec![]),
            ..make_test_case("valid", None)
        };
        assert!(
            validate_empty_contains_assertions(&make_test_file("runtime", vec![valid])).is_empty()
        );
    }

    #[test]
    fn test_empty_error_contains_from_param_override_is_rejected_after_expansion() {
        let toml = r#"
[section]
id = "runtime"
name = "Runtime"

[[case]]
name = "param_override"
source = "fn main() -> i32 { 0 }"
params = [{ compile_fail = true, error_contains = "" }]
"#;
        let expanded = expand_test_file(toml::from_str::<TestFile>(toml).unwrap());
        let errors = validate_empty_contains_assertions(&expanded);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].fields, "`error_contains[0]`");
    }

    #[test]
    fn test_validate_compile_fail_exit_codes_rejects_ignored_assertion() {
        let mut case = make_test_case("compile_error", None);
        case.compile_fail = true;
        case.exit_code = Some(1);
        case.error_contains = ErrorContains(vec!["[E0206]".to_string()]);
        let test_file = make_test_file("diagnostics", vec![case]);

        let errors = validate_compile_fail_exit_codes(&test_file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].section_id, "diagnostics");
        assert_eq!(errors[0].test_name, "compile_error");
        assert!(errors[0].to_string().contains("Remove `exit_code`"));
    }

    #[test]
    fn test_validate_compile_fail_exit_codes_allows_runtime_case() {
        let case = make_test_case("runtime", None);
        let test_file = make_test_file("runtime", vec![case]);

        assert!(validate_compile_fail_exit_codes(&test_file).is_empty());
    }

    #[test]
    fn test_validate_nonempty_case_corpus_checks_loaded_case_count() {
        let cases_dir = Path::new("/tmp/cases");
        assert!(validate_nonempty_case_corpus(cases_dir, 1, "spec").is_ok());

        let error = validate_nonempty_case_corpus(cases_dir, 0, "spec")
            .expect_err("an empty configured corpus must fail");
        assert_eq!(error, "no spec test cases found in /tmp/cases");
    }

    // Tests for run_with_timeout draining large output without deadlock
    // (RUE-132 / same class as RUE-338). With the pre-fix code — write all
    // stdin, then read the pipes only after exit — each of these would fill the
    // ~64KB OS pipe buffer, block the child in write(), and time out.

    #[test]
    fn test_run_with_timeout_drains_large_stdout() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("head -c 200000 /dev/zero");
        let output = run_with_timeout(cmd, Duration::from_secs(10), None)
            .expect("large-stdout program should complete, not time out");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200_000);
    }

    #[test]
    fn test_run_with_timeout_drains_large_stdout_and_stderr() {
        // Both pipes must be drained concurrently; filling either alone wedges
        // the child.
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("head -c 200000 /dev/zero; head -c 200000 /dev/zero >&2");
        let output = run_with_timeout(cmd, Duration::from_secs(10), None)
            .expect("large stdout+stderr program should complete");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200_000);
        assert_eq!(output.stderr.len(), 200_000);
    }

    #[test]
    fn test_run_with_timeout_output_limit_rejects_overflow_while_draining() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("head -c 200000 /dev/zero");
        let error =
            run_with_timeout_and_output_limit(cmd, Duration::from_secs(10), None, 16 * 1024)
                .expect_err("stdout beyond the retention limit must fail");

        assert!(error.is_fatal());
        assert!(error.contains("OUTPUT LIMIT: stdout exceeded"));
        assert!(error.contains("16384-byte capture limit"));
    }

    #[test]
    fn test_run_with_timeout_large_stdin_and_stdout() {
        // `cat` echoes stdin to stdout: feeding a large stdin and draining a
        // large stdout must proceed concurrently, or both sides deadlock.
        let big = "a".repeat(200_000);
        let cmd = Command::new("cat");
        let output = run_with_timeout(cmd, Duration::from_secs(10), Some(&big))
            .expect("large stdin echoed to stdout should complete");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200_000);
    }

    #[test]
    fn test_run_with_timeout_does_not_join_forever_on_inherited_stdout() {
        // The direct child exits immediately, but the background process
        // inherits stdout and keeps the pipe's write end open. A direct
        // reader-thread join waits for that descendant to exit; bounded drain
        // collection returns promptly with the bytes already captured.
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 5 & printf done");

        let start = Instant::now();
        let output = run_with_timeout(cmd, Duration::from_secs(10), None)
            .expect("direct child exits successfully");
        let elapsed = start.elapsed();

        assert!(output.status.success());
        assert_eq!(output.stdout, b"done");
        assert!(
            elapsed < Duration::from_secs(2),
            "run_with_timeout waited for inherited stdout to close: {elapsed:?}"
        );
    }

    // Tests for the load-time guard requiring every `compile_fail` case to
    // declare an error assertion (RUE-132). A bare `compile_fail` case passes on
    // ANY rejection, verifying nothing about *why* the program is rejected.

    #[test]
    fn test_bare_compile_fail_case_is_rejected() {
        let toml = r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "bare_reject"
compile_fail = true
source = "fn main() -> i32 { true }"
"#;
        let tf: TestFile = toml::from_str(toml).expect("valid TOML");
        let errors = validate_compile_fail_assertions(&tf);
        assert_eq!(errors.len(), 1, "bare compile_fail case must be flagged");
        assert_eq!(errors[0].test_name, "bare_reject");
        assert_eq!(errors[0].section_id, "test.section");
    }

    #[test]
    fn test_compile_fail_with_error_contains_is_accepted() {
        let toml = r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "pinned_reject"
compile_fail = true
error_contains = "[E0206]"
source = "fn main() -> i32 { true }"
"#;
        let tf: TestFile = toml::from_str(toml).expect("valid TOML");
        assert!(validate_compile_fail_assertions(&tf).is_empty());
    }

    #[test]
    fn test_compile_fail_with_expected_error_is_accepted() {
        let toml = r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "golden_reject"
compile_fail = true
expected_error = "error: [E0206]: type mismatch"
source = "fn main() -> i32 { true }"
"#;
        let tf: TestFile = toml::from_str(toml).expect("valid TOML");
        assert!(validate_compile_fail_assertions(&tf).is_empty());
    }

    #[test]
    fn test_non_compile_fail_case_needs_no_error_assertion() {
        let toml = r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "runs_fine"
source = "fn main() -> i32 { 42 }"
exit_code = 42
"#;
        let tf: TestFile = toml::from_str(toml).expect("valid TOML");
        assert!(validate_compile_fail_assertions(&tf).is_empty());
    }

    #[test]
    fn test_known_targets_cover_compiler_targets() {
        for target in Target::all() {
            assert!(
                KNOWN_TARGETS.contains(&target.name()),
                "test harness target whitelist is missing compiler target {target}"
            );
        }
    }

    #[test]
    fn ci_executed_targets_are_known_platforms() {
        for target in CI_EXECUTED_TARGETS {
            assert!(
                KNOWN_TARGETS.contains(target),
                "CI-executed platform {target} is not a known target name"
            );
        }
        // Every compiler target must be executed somewhere in required CI, or a
        // backend's runtime behavior would only ever be cross-compiled.
        for target in Target::all() {
            assert!(
                CI_EXECUTED_TARGETS.contains(&target.name()),
                "compiler target {target} has no required CI lane executing its cases"
            );
        }
    }

    #[test]
    fn required_ci_reachability_follows_the_platform_matrix() {
        assert!(runs_on_required_ci(&[]), "unscoped cases run everywhere");
        assert!(runs_on_required_ci(&["aarch64-macos".to_string()]));
        assert!(runs_on_required_ci(&[
            "x86-64-macos".to_string(),
            "x86-64-linux".to_string(),
        ]));
        // Intel macOS is a legal host for a developer, but no required lane
        // runs it, so a case scoped only to it executes nowhere in CI.
        assert!(!runs_on_required_ci(&["x86-64-macos".to_string()]));
    }

    #[test]
    fn target_architecture_splits_known_platform_names() {
        assert_eq!(target_architecture("x86-64-linux"), Some("x86-64"));
        assert_eq!(target_architecture("x86-64-macos"), Some("x86-64"));
        assert_eq!(target_architecture("aarch64-macos"), Some("aarch64"));
        assert_eq!(target_architecture("riscv64-linux"), None);
    }

    fn case_with(mutate: impl FnOnce(&mut Case)) -> Case {
        let mut case = Case {
            name: "case".to_string(),
            source: "fn main() -> i32 { 0 }".to_string(),
            ..Default::default()
        };
        mutate(&mut case);
        case
    }

    #[test]
    fn responsibility_classifies_target_independent_cases_as_semantic() {
        let diagnostic = case_with(|case| {
            case.compile_fail = true;
            case.error_contains = ErrorContains(vec!["type mismatch".to_string()]);
        });
        assert_eq!(
            classify_platform_responsibility(&diagnostic),
            Ok(PlatformResponsibility::Semantic)
        );

        let golden_air = case_with(|case| case.expected_air = Some("air {}".to_string()));
        assert_eq!(
            classify_platform_responsibility(&golden_air),
            Ok(PlatformResponsibility::Semantic)
        );
    }

    #[test]
    fn responsibility_classifies_executing_cases_as_native() {
        let unscoped = case_with(|case| case.exit_code = Some(0));
        assert_eq!(
            classify_platform_responsibility(&unscoped),
            Ok(PlatformResponsibility::Native)
        );

        let scoped = case_with(|case| {
            case.exit_code = Some(0);
            case.only_on = vec!["aarch64-macos".to_string()];
        });
        assert_eq!(
            classify_platform_responsibility(&scoped),
            Ok(PlatformResponsibility::Native)
        );
    }

    #[test]
    fn responsibility_classifies_declared_cross_compilation_as_backend() {
        let emit_only = case_with(|case| {
            case.target = Some("aarch64-linux".to_string());
            case.expected_asm = Some("ret".to_string());
        });
        assert_eq!(
            classify_platform_responsibility(&emit_only),
            Ok(PlatformResponsibility::Backend)
        );

        // Cross-compiling without running the result is host-independent, so
        // it needs no `only_on` scope.
        let cross_compile_only = case_with(|case| {
            case.target = Some("aarch64-linux".to_string());
            case.compile_only = true;
        });
        assert_eq!(
            classify_platform_responsibility(&cross_compile_only),
            Ok(PlatformResponsibility::Backend)
        );

        let executed_on_its_own_arch = case_with(|case| {
            case.target = Some("aarch64-linux".to_string());
            case.exit_code = Some(0);
            case.only_on = vec!["aarch64-linux".to_string(), "aarch64-macos".to_string()];
        });
        assert_eq!(
            classify_platform_responsibility(&executed_on_its_own_arch),
            Ok(PlatformResponsibility::Backend)
        );
    }

    #[test]
    fn responsibility_rejects_backend_golden_output_without_a_target() {
        let ambiguous = case_with(|case| {
            case.expected_asm = Some("ret".to_string());
            case.expected_regalloc = Some("v0 -> x0".to_string());
        });
        assert_eq!(
            classify_platform_responsibility(&ambiguous),
            Err(PlatformResponsibilityAmbiguity::UndeclaredArchitecture {
                fields: "expected_regalloc, expected_asm".to_string(),
            })
        );
    }

    #[test]
    fn responsibility_rejects_foreign_architecture_execution() {
        let unscoped = case_with(|case| {
            case.target = Some("aarch64-linux".to_string());
            case.exit_code = Some(0);
        });
        assert_eq!(
            classify_platform_responsibility(&unscoped),
            Err(PlatformResponsibilityAmbiguity::UnscopedForeignExecution {
                target: "aarch64-linux".to_string(),
            })
        );

        let mismatched = case_with(|case| {
            case.target = Some("aarch64-linux".to_string());
            case.exit_code = Some(0);
            case.only_on = vec!["x86-64-linux".to_string()];
        });
        assert_eq!(
            classify_platform_responsibility(&mismatched),
            Err(PlatformResponsibilityAmbiguity::ScopeArchitectureMismatch {
                target: "aarch64-linux".to_string(),
                platform: "x86-64-linux".to_string(),
            })
        );
    }

    #[test]
    fn load_rejects_ambiguous_platform_responsibility() {
        let directory = tempfile::tempdir().expect("temporary cases directory");
        std::fs::write(
            directory.path().join("cases.toml"),
            r#"
[section]
id = "test.section"
name = "Test"

[[case]]
name = "undeclared_arch"
source = "fn main() -> i32 { 0 }"
expected_asm = "ret"
"#,
        )
        .expect("write case file");

        let error = load_test_files(directory.path()).expect_err("ambiguous case must not load");
        assert!(
            error.contains("ambiguous platform responsibility")
                && error.contains("undeclared_arch")
                && error.contains("expected_asm"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn native_platform_selection_is_declarative_and_host_scoped() {
        let host = get_host_target().to_string();
        let other = KNOWN_TARGETS
            .iter()
            .find(|target| **target != host)
            .expect("at least one non-host target")
            .to_string();

        assert!(PlatformCaseSelection::All.includes(&[]));
        assert!(PlatformCaseSelection::All.includes(std::slice::from_ref(&other)));
        assert!(!PlatformCaseSelection::Native.includes(&[]));
        assert!(PlatformCaseSelection::Native.includes(&[host, other.clone()]));
        assert!(!PlatformCaseSelection::Native.includes(&[other]));
    }

    #[test]
    fn platform_selection_rejects_unknown_modes() {
        assert_eq!(
            PlatformCaseSelection::parse(None),
            Ok(PlatformCaseSelection::All)
        );
        assert_eq!(
            PlatformCaseSelection::parse(Some("native")),
            Ok(PlatformCaseSelection::Native)
        );
        assert!(
            PlatformCaseSelection::parse(Some("platform-ish"))
                .unwrap_err()
                .contains("expected all or native")
        );
    }
}
