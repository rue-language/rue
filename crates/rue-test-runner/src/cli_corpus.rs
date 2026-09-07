//! The CLI-corpus TOML schema and its platform validation.
//!
//! `crates/rue-cli-tests/cases/*.toml` is the authoritative declarative corpus
//! for the CLI integration suite, and more than one harness reads it: the CLI
//! suite runs the cases, and `rue-oracle-diff` re-reads the same files to check
//! the reference interpreter against them. When each harness declared its own
//! `Deserialize` view of the format, the differential's view was a permissive
//! subset that ignored unknown fields — so a case could grow a field that
//! changes what the CLI suite does (staged symlinks, a `--watch` scenario, a
//! driver-only exit contract) while the differential kept "agreeing" about
//! semantics it had never applied.
//!
//! This module is the single schema. It carries `deny_unknown_fields`
//! throughout, so an authored field this type does not know is a load error
//! rather than a silent omission, and every consumer projects the subset it can
//! honour *explicitly* — naming the fields it refuses instead of dropping them
//! on the floor.
//!
//! The schema is deliberately data only. Which field combinations are legal,
//! and what running a case means, stay with the CLI harness that owns those
//! semantics; what lives here is the shape of the file and the platform
//! vocabulary (`only_on` / `known_bug_on`) that every reader must agree on.

use serde::Deserialize;
use std::collections::HashMap;

use crate::KNOWN_TARGETS;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestFile {
    pub section: Section,
    /// Named execution contracts contributed by this file. Keeping these in
    /// the same declarative corpus as cases lets automatic examples and TOML
    /// cases share one scheduling and timeout policy.
    #[serde(default, rename = "contract")]
    pub contracts: HashMap<String, ExecutionContractDeclaration>,
    /// The one declarative authority for correctness hang guards. Contracts
    /// select a named profile instead of inventing raw deadlines.
    #[serde(default, rename = "timeout_profile")]
    pub timeout_profiles: HashMap<TimeoutProfile, HangTimeoutProfile>,
    /// Parsed here so unknown policy fields fail closed. The CI wrapper owns
    /// the whole-suite derivation from these values.
    #[serde(default)]
    pub timeout_policy: Option<TimeoutPolicy>,
    /// Contracts and tiers for recursively discovered examples, keyed by the
    /// path that also determines their `cli.examples::...` test name.
    #[serde(default)]
    pub automatic_example: Vec<AutomaticExampleContract>,
    #[serde(default, rename = "case")]
    pub cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Default execution contract for every case in this section. Individual
    /// cases may override it when only one scenario is heavyweight.
    #[serde(default)]
    pub contract: Option<String>,
    /// Logical execution tier for this section's explicit cases. Automatic
    /// examples declare their tier independently.
    #[serde(default)]
    pub tier: CliCaseTier,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliCaseTier {
    #[default]
    Premerge,
    Slow,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClass {
    Ordinary,
    Heavyweight,
}

#[derive(Debug, Clone, Copy, Deserialize, Hash, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutProfile {
    Ordinary,
    Slow,
    Stress,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HangTimeoutProfile {
    pub compile_hang_timeout_ms: u64,
    pub runtime_hang_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TimeoutPolicy {
    pub expected_cost_multiplier_percent: u64,
    pub fixed_headroom_ms: u64,
    pub minimum_shard_timeout_ms: u64,
    pub minimum_monolith_timeout_ms: u64,
    pub minimum_slow_suite_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContractDeclaration {
    pub class: ExecutionClass,
    pub timeout_profile: TimeoutProfile,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutomaticExampleContract {
    pub path: String,
    pub contract: String,
    #[serde(default)]
    pub tier: CliCaseTier,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFile {
    pub path: String,
    pub source: String,
}

/// A symbolic link staged in the temp directory before the case runs.
///
/// `target` is written verbatim and is deliberately not validated or resolved:
/// a dangling link, a self-referential link, and a link whose target the
/// compiled program creates at run time are all legitimate fixtures for
/// filesystem-facing cases.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymlinkFixture {
    pub link: String,
    pub target: String,
}

/// A regular hard link staged after the source files. Hard-link support is a
/// filesystem capability of the test host, so failure is reported rather than
/// silently turning a regression case into an ordinary-file control.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HardLinkFixture {
    pub link: String,
    pub target: String,
}

/// An exact-count expectation over a watch process's stderr.
///
/// `stderr_contains` answers "was this said at all", which cannot distinguish
/// one report from thirty. A watcher that repeats a diagnostic while nothing
/// changed is the bug RUE-2091 fixed, so the assertion that pins it has to
/// count.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StderrOccurrence {
    pub text: String,
    pub count: usize,
}

/// Imperative end-to-end watch scenario. These cases use the watch protocol
/// seam in `rue` to synchronize edits and then terminate the watch process;
/// ordinary CLI cases remain declarative and use the normal compile/run path.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchScenario {
    pub kind: WatchScenarioKind,
    /// Optional diagnostic format passed to the real watch process. JSON
    /// scenarios additionally assert that every non-empty stderr line is a
    /// non-empty diagnostic array.
    #[serde(default)]
    pub error_format: Option<String>,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub compile_delay_ms: Option<u64>,
    /// Widen the gap between the watcher's change monitor stopping and its
    /// trailing input check, so an edit can be made to land inside it on
    /// purpose. That window is where RUE-1783 lived: a change discovered there
    /// was acted on but never announced on the protocol.
    #[serde(default)]
    pub boundary_delay_ms: Option<u64>,
    /// Hold retained re-observation inside its first physical-read boundary,
    /// so an edit deterministically supersedes in-progress discovery.
    #[serde(default)]
    pub reobserve_delay_ms: Option<u64>,
    /// Hold the watcher between re-observation and reached-toolchain
    /// acquisition, so an edit can deterministically land while acquisition is
    /// reading demanded modules or re-closing (RUE-1863).
    #[serde(default)]
    pub acquire_delay_ms: Option<u64>,
    pub edits: Vec<WatchEdit>,
    /// Substrings the watch process's stderr must contain. The milestone
    /// protocol says which boundary a cycle reached; this pins what the cycle
    /// told the user when it got there.
    #[serde(default)]
    pub stderr_contains: Vec<String>,
    /// Substrings whose number of occurrences in stderr is itself the
    /// assertion.
    #[serde(default)]
    pub stderr_occurrences: Vec<StderrOccurrence>,
    /// The exit status the published program has at each publication the
    /// scenario waits for. An `initial_failure` scenario publishes only once,
    /// so it declares one; every other kind declares two.
    pub expected_exit_codes: Vec<i32>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchScenarioKind {
    Edit,
    Cancel,
    Delete,
    SymlinkRetarget,
    SupersedeReobserve,
    SupersedeAcquire,
    /// The FIRST cycle fails, so the watcher publishes nothing before the
    /// edit repairs the program. Every other kind opens with a publication,
    /// which is exactly what a first-cycle failure cannot produce.
    InitialFailure,
    /// A parse error in a closure module, which fails re-observation rather
    /// than compilation and so puts the loop on its retry timer. The case
    /// walks every trigger the report obeys: it sits inside one failure for
    /// several retries, replaces it with a different one, edits an unrelated
    /// closure file while the broken one holds still, repairs it, and finally
    /// restores the broken bytes verbatim (RUE-2091).
    RepeatedFailure,
    /// The same retry timer, but the broken module is one no successful close
    /// ever contained: a pre-existing file wired into the closure for the first
    /// time. Edits to it must still be acknowledged even though the diagnostic
    /// they produce is byte-identical, because that file is the one the user is
    /// editing (RUE-2103).
    FailureOutsideClosure,
}

/// A synchronized end-to-end `rue test --watch` scenario (RUE-2023).
///
/// The sibling of [`WatchScenario`] for the test-mode watcher. It drives the
/// same milestone protocol, so a case waits for the cycle it is about to assert
/// on instead of sleeping, and it asserts on the event stream the watcher
/// publishes rather than on a produced executable — a test watcher publishes
/// none.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchTestScenario {
    pub kind: WatchTestScenarioKind,
    /// `--format` for the run. Defaults to `json`, because the event stream is
    /// the surface these cases exist to pin.
    #[serde(default)]
    pub format: Option<String>,
    /// `--timeout-ms` for the run, when a case needs one that is not the
    /// default (a deliberately spinning test wants a short leash).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Extra compiler arguments, appended after the standard ones.
    #[serde(default)]
    pub args: Vec<String>,
    pub edits: Vec<WatchEdit>,
    /// Substrings the whole of stdout must contain, and must not.
    #[serde(default)]
    pub stdout_contains: Vec<String>,
    #[serde(default)]
    pub stdout_not_contains: Vec<String>,
    #[serde(default)]
    pub stderr_contains: Vec<String>,
    /// Substrings whose number of occurrences in stderr is itself the
    /// assertion.
    #[serde(default)]
    pub stderr_occurrences: Vec<StderrOccurrence>,
    /// The status the watcher exits with when the case interrupts it: the last
    /// COMPLETED cycle's, which is the only exit status a watcher produces.
    pub expected_exit: i32,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchTestScenarioKind {
    /// One edit, one further cycle. The case's `stdout_contains` says what the
    /// second cycle's verdicts must be.
    Edit,
    /// Break the closure, observe the failed cycle, repair it, observe the
    /// next completed one.
    CompileError,
    /// Edit while a test is running: the cycle is abandoned with
    /// `run_canceled` and the next one completes.
    Cancel,
    /// The `--watch` half of [`WatchScenarioKind::RepeatedFailure`]: the two
    /// watchers share one loop, so the suppression they share is pinned on
    /// both surfaces (RUE-2091).
    RepeatedFailure,
    /// [`WatchScenarioKind::FailureOutsideClosure`] on the test watcher. Both
    /// watchers share the re-observation arm, so they shared the gap and are
    /// pinned together (RUE-2103).
    FailureOutsideClosure,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchEdit {
    pub path: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub delete: bool,
    /// Restamp the file's modification time and leave its bytes alone — the
    /// save an editor performs on a document nobody changed. The watcher keys
    /// on content, so this must produce no new revision and no new report; a
    /// key built from file metadata instead would report on it (RUE-2103).
    #[serde(default)]
    pub touch: bool,
    #[serde(default)]
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub name: String,
    /// Human-readable explanation of what this case pins and why. Not used by
    /// the harness; it exists so case files can document intent inline.
    #[serde(default)]
    pub description: Option<String>,
    /// Named execution contract. Overrides the section default.
    #[serde(default)]
    pub contract: Option<String>,
    /// Files written to the temp directory before invoking the compiler.
    #[serde(default)]
    pub files: Vec<SourceFile>,
    /// Symbolic links staged in the temp directory alongside `files`. A `files`
    /// entry can only produce a regular file, so this is what lets a case pin
    /// symlink behavior.
    #[serde(default)]
    pub symlinks: Vec<SymlinkFixture>,
    /// Hard links staged in the temp directory alongside `files`.
    #[serde(default)]
    pub hard_links: Vec<HardLinkFixture>,
    /// Run only when distinct path spellings differing by case resolve to one
    /// file. Hosts without that filesystem capability report an explicit
    /// ignored result rather than dropping the regression.
    #[serde(default)]
    pub requires_case_insensitive_fs: bool,
    /// Repo-root-relative source file to compile directly instead of copying
    /// inline source into the temp directory. Use this when a CLI case should
    /// pin a checked-in example/program rather than duplicating its source.
    #[serde(default)]
    pub source_path: Option<String>,
    /// Compiler arguments, relative to the temp dir (default: first file + `-o prog`).
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Synthesize a tiny C-free static archive exporting `answer() -> 42` as
    /// pure machine code for the case's target, and substitute its path for the
    /// `${FFI_ARCHIVE}` token in `args` (ADR-0064 C FFI P1 proof program). The
    /// archive is produced with the compiler's own object machinery
    /// (`rue_linker::ObjectBuilder`), mirroring how the runtime archive is
    /// linked in — no C toolchain is required.
    #[serde(default)]
    pub ffi_answer_archive: bool,
    /// Name of the executable the compiler is expected to produce.
    #[serde(default)]
    pub output: Option<String>,
    /// A synchronized, imperative `--watch` integration scenario.
    #[serde(default)]
    pub watch: Option<WatchScenario>,
    /// A synchronized, imperative `rue test --watch` integration scenario.
    #[serde(default)]
    pub watch_test: Option<WatchTestScenario>,
    /// Extra environment variables for the compiler invocation.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Command-line arguments passed to the COMPILED PROGRAM when it runs
    /// (RUE-935). These become `argv[1..]`; `argv[0]` is the program path the
    /// harness invokes. Distinct from `args`, which are the compiler's flags.
    #[serde(default)]
    pub program_args: Vec<String>,
    /// Extra environment variables for the COMPILED PROGRAM's run (RUE-935),
    /// layered on top of the inherited environment. Distinct from `env`, which
    /// applies to the compiler invocation.
    #[serde(default)]
    pub program_env: HashMap<String, String>,
    /// Piped to the compiled program's stdin.
    #[serde(default)]
    pub stdin: Option<String>,
    /// Expect compilation to fail.
    #[serde(default)]
    pub compile_fail: bool,
    /// Substrings expected in the compiler's stderr when compilation fails.
    #[serde(default)]
    pub error_contains: Vec<String>,
    /// Compile but don't run the produced binary.
    #[serde(default)]
    pub compile_only: bool,
    /// Target whose executable structure must be validated after compilation.
    #[serde(default)]
    pub executable_target: Option<String>,
    /// Run a structurally validated executable only when it is native to this
    /// host. This lets one case cover every host-target compile pair without
    /// attempting to execute foreign machine code.
    #[serde(default)]
    pub execute_if_native: bool,
    /// Substrings expected in the compiler's stdout (e.g. `--emit` output).
    #[serde(default)]
    pub compile_stdout_contains: Vec<String>,
    /// Substrings that must NOT appear in the compiler's stdout.
    #[serde(default)]
    pub compile_stdout_not_contains: Vec<String>,
    /// Substrings that MUST appear in the compiler's stderr, regardless of
    /// whether compilation succeeds or fails. Use for warnings that must
    /// survive a successful compile (e.g. under `--emit`).
    #[serde(default)]
    pub compile_stderr_contains: Vec<String>,
    /// Substrings that must NOT appear in the compiler's stderr, regardless of
    /// whether compilation succeeds or fails. Use to guard against debug spew
    /// or leaked internal diagnostics (e.g. raw `DEBUG:` eprintln lines).
    #[serde(default)]
    pub compile_stderr_not_contains: Vec<String>,
    /// Validate the compiler's stderr as the `--error-format json` surface
    /// (RUE-436): EVERY non-empty stderr line must parse as a JSON array of
    /// diagnostic objects, and every object must carry the full documented
    /// schema (see `docs/process/diagnostics.md`). Substring assertions cannot
    /// catch malformed JSON, a dropped field, or a renamed key — this can, and
    /// it fails the case when they happen. Under `--error-format json` stderr
    /// carries diagnostics only (the `Compiled ... -> ...` banner is stdout),
    /// so the "every line" rule is exact rather than a filter.
    #[serde(default)]
    pub json_diagnostics: bool,
    /// Exact, ordered digest of the diagnostics `json_diagnostics` parsed, as
    /// `"<severity> <code> <file>:<line>:<column>"` per diagnostic, flattened
    /// across every stderr line in emission order. An absent field renders as
    /// `-`: warnings are uncoded (`"warning - main.rue:2:5"`) and a diagnostic
    /// with no span has no locator (`"error E1403 -"`). This pins diagnostic
    /// ORDER, not merely presence: a case that lists the same diagnostics in a
    /// different order fails. Requires `json_diagnostics`.
    #[serde(default)]
    pub json_diagnostic_order: Vec<String>,
    /// Exact expected program stdout.
    #[serde(default)]
    pub stdout: Option<String>,
    /// Substrings expected in the program's stdout.
    #[serde(default)]
    pub stdout_contains: Vec<String>,
    /// Substrings expected in the program's stderr (runtime panics).
    #[serde(default)]
    pub runtime_error_contains: Vec<String>,
    /// Expected program exit code (default 0).
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Expected failure: reference to the Linear issue tracking the bug.
    #[serde(default)]
    pub known_bug: Option<String>,
    /// Platforms the known_bug applies to (e.g. ["x86-64-linux"]). Empty
    /// means all platforms. On other platforms the case runs as a normal
    /// test. Useful for ABI bugs that manifest differently per target.
    #[serde(default)]
    pub known_bug_on: Vec<String>,
    /// Platforms this case runs on (e.g. ["x86-64-linux"]); elsewhere it is
    /// reported as ignored. Empty means all platforms. Use when the expected
    /// behavior itself depends on the host (e.g. `--target X` is a
    /// cross-compile on some hosts and a native compile on others).
    #[serde(default)]
    pub only_on: Vec<String>,
    /// Skip this case entirely.
    #[serde(default)]
    pub skip: bool,
    /// Opt-level differential test (RUE-236): compile+run this case once per
    /// optimization level (`-O0`, `-O1`, `-O2`, `-O3`) and assert IDENTICAL
    /// exit code AND stdout across all levels. A divergence fails the case,
    /// naming the level that differs. This catches optimizer passes that break
    /// semantics — the analogue, at the *program's* opt level, of the
    /// release-mode CI job (RUE-45) that catches `cfg(debug_assertions)`
    /// divergence in the *compiler*. `-O2`/`-O3` alias `-O1` today, so results
    /// match now; the net is set so a future divergence is caught.
    ///
    /// Marked cases must be plain compile-and-run cases: `compile_fail`,
    /// `compile_only`, and an explicit `-O` in `args` are rejected (the runner
    /// drives the opt level itself). Give the case exact `stdout` and
    /// `exit_code` so each level is also checked against the known-good result,
    /// not merely against the other levels.
    #[serde(default)]
    pub differential_opt: bool,
    /// Report the case as ignored when no system `cc` driver is on `PATH`.
    /// For cases exercising the `--linker cc` symbolized profiling build
    /// (RUE-1173): every supported CI host provides `cc`, but a minimal local
    /// environment without a C toolchain should skip rather than fail.
    #[serde(default)]
    pub requires_system_linker: bool,
    /// Substrings that must each match at least one defined symbol name in
    /// the produced executable's symbol table (ELF `.symtab` / Mach-O
    /// `LC_SYMTAB`). Verifies the symbolized profiling build keeps function
    /// symbols (RUE-1173). Incompatible with `compile_fail`.
    #[serde(default)]
    pub symbols_contain: Vec<String>,
    /// Assert the produced executable carries no symbol table entries. Pins
    /// that default internal-linker output is unsymbolized — the documented
    /// motivation for the `--linker cc` profiling workflow (RUE-1173).
    /// Incompatible with `compile_fail` and `symbols_contain`.
    #[serde(default)]
    pub no_symbol_table: bool,
    /// Upper bound, in bytes, on the produced executable's size.
    ///
    /// A case whose point is that emitted machine code does not grow with a
    /// count written in the source needs the artifact measured, not merely
    /// produced: `[0; 100000000]` once emitted seven bytes of code per element
    /// (RUE-2069), which is a correct program and a 700 MB executable.
    /// Inspects a produced executable, so it is incompatible with
    /// `compile_fail`.
    #[serde(default)]
    pub max_executable_bytes: Option<u64>,
    /// Exact expected exit status of the DRIVER invocation itself, for a
    /// subcommand that neither compiles-and-runs nor fails to compile.
    ///
    /// `rue test` (ADR-0083) is the case this exists for: its exit status is a
    /// documented four-way contract (0 passed / 1 failures / 2 runner error /
    /// 3 empty selection) that agents branch on, and `compile_fail`'s
    /// "nonzero" cannot tell 1 from 3. Setting it also declares that the
    /// invocation produces no executable to run, so the case ends after the
    /// compiler's own stdout, stderr, and status are checked — assert the
    /// driver's output with `compile_stdout_contains` and
    /// `compile_stderr_contains`. Incompatible with `compile_fail`, whose
    /// contract is exactly the weaker one this replaces.
    #[serde(default)]
    pub driver_exit_code: Option<i32>,
}
/// The `only_on` platform names in `case` that are not [`KNOWN_TARGETS`].
///
/// An unknown name can never equal the host, so the case would be skipped on
/// EVERY platform — silently, while still counting as corpus coverage. Both
/// readers of this corpus reject at load time on the same answer.
pub fn unknown_only_on_platforms(case: &Case) -> Vec<&str> {
    unknown_platforms(&case.only_on)
}

/// The `known_bug_on` platform names in `case` that are not [`KNOWN_TARGETS`].
///
/// A typo here is the mirror-image failure of an unknown `only_on`: the xfail
/// scope matches no host, so the case runs as an ordinary test everywhere and
/// the marker silently stops meaning anything.
pub fn unknown_known_bug_on_platforms(case: &Case) -> Vec<&str> {
    unknown_platforms(&case.known_bug_on)
}

fn unknown_platforms(platforms: &[String]) -> Vec<&str> {
    platforms
        .iter()
        .map(String::as_str)
        .filter(|platform| !KNOWN_TARGETS.contains(platform))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case_with(only_on: &[&str], known_bug_on: &[&str]) -> Case {
        Case {
            name: "probe".to_string(),
            only_on: only_on.iter().map(|s| (*s).to_string()).collect(),
            known_bug_on: known_bug_on.iter().map(|s| (*s).to_string()).collect(),
            ..Case::default()
        }
    }

    #[test]
    fn known_platform_names_are_accepted_on_both_axes() {
        for platform in KNOWN_TARGETS {
            let case = case_with(&[platform], &[platform]);
            assert!(unknown_only_on_platforms(&case).is_empty());
            assert!(unknown_known_bug_on_platforms(&case).is_empty());
        }
    }

    #[test]
    fn misspelled_platform_names_are_reported_per_axis() {
        let case = case_with(&["x86_64-linux"], &["darwin"]);
        assert_eq!(unknown_only_on_platforms(&case), vec!["x86_64-linux"]);
        assert_eq!(unknown_known_bug_on_platforms(&case), vec!["darwin"]);
    }

    #[test]
    fn an_unknown_field_is_a_load_error_rather_than_a_silent_omission() {
        let error = toml::from_str::<TestFile>(
            r#"
[section]
id = "cli.probe"
name = "Probe"

[[case]]
name = "probe"
invented_field = true
"#,
        )
        .expect_err("an unknown case field must not parse");
        assert!(error.to_string().contains("invented_field"), "{error}");
    }

    #[test]
    fn the_schema_parses_a_representative_case_file() {
        let file: TestFile = toml::from_str(
            r#"
[timeout_profile.ordinary]
compile_hang_timeout_ms = 1000
runtime_hang_timeout_ms = 1000

[contract.heavyweight]
class = "heavyweight"
timeout_profile = "ordinary"

[section]
id = "cli.probe"
name = "Probe"
tier = "slow"

[[case]]
name = "probe"
files = [{ path = "main.rue", source = "fn main() -> i32 { 0 }" }]
exit_code = 0
"#,
        )
        .expect("parse representative corpus file");
        assert_eq!(file.section.id, "cli.probe");
        assert_eq!(file.section.tier, CliCaseTier::Slow);
        assert_eq!(
            file.timeout_profiles[&TimeoutProfile::Ordinary],
            HangTimeoutProfile {
                compile_hang_timeout_ms: 1000,
                runtime_hang_timeout_ms: 1000,
            }
        );
        assert_eq!(
            file.contracts["heavyweight"].class,
            ExecutionClass::Heavyweight
        );
        assert_eq!(file.cases.len(), 1);
    }
}
