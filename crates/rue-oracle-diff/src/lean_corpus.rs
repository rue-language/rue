//! `lean-corpus` — the Rust half of ADR-0097's differential bridge (RUE-2228).
//!
//! The Lean mechanization (`docs/formal/lean`) exports a corpus of small Rue
//! programs, each carrying what the *verified* checker and interpreter say
//! about it: an accept/reject verdict and, for an accepted program, the stdout
//! and exit status (or the trap) its evaluation produces. `RueCore/Corpus.lean`
//! owns that JSON contract; `docs/formal/lean/README.md` explains it in prose.
//!
//! This mode runs the three *implementation* views on the same source — the
//! compiler's accept/reject decision, the `rue_oracle` reference interpreter,
//! and the native binary at O1/O2/O3 — and names every pairwise disagreement
//! with the Lean expectation and with each other. Four views, four pairs:
//!
//! * `checker <-> compiler` — the Lean verdict against the compiler's
//!   accept/reject decision, with the diagnostic codes of a rejection.
//! * `lean <-> oracle` — the Lean expectation against the oracle's run.
//! * `lean <-> native` — the Lean expectation against the native binary, per
//!   optimization lane.
//! * `oracle <-> native` — the existing [`crate::fuzz::classify`] comparison,
//!   per optimization lane.
//!
//! A disagreement names the *pair*, never a side: which view is wrong is a
//! question for a human reading the evidence (RUE-305). The process exits
//! non-zero when any disagreement exists.
//!
//! A malformed corpus is a hard error naming the case, not a disagreement:
//! the case types deny unknown fields, so a contract change on the Lean side
//! stops the bridge instead of being silently ignored (RUE-1987's rule for the
//! other corpora).
//!
//! # Blind spots, inherited from the corpus contract
//!
//! * A Lean `panic` outcome carries no trace, so drops before a trap are not
//!   compared — only the trap category is (RUE-2282 gives `.panic` its trace).
//! * A Lean `reject` case's `stuck` violation is unobservable here: the
//!   compiler rejects the program, so nothing runs. Only the rejection itself
//!   is compared.
//!
//! # `--report-json` schema
//!
//! One object, with these stable field names:
//!
//! ```text
//! corpus              string   the corpus file this run read
//! cases_total         integer  cases evaluated
//! cases_agreeing      integer  cases with no disagreement
//! cases_disagreeing   integer  cases with at least one
//! tally               object   pair key -> number of disagreements
//! cases               array    one object per case, in corpus order:
//!   name              string
//!   description       string
//!   rules             array of string
//!   source            string   the printed Rue program
//!   verdict           "accept" | "reject"
//!   accepted_type     string | null   the Lean type of an accepted program
//!   expected          object   {"kind": "ok"|"panic"|"stuck", ...} as exported
//!   compiler          object   {"outcome": "accepted"|"rejected"|"internal_error"
//!                               |"crashed"|"timed_out", "codes": [...],
//!                               "detail": string}
//!   unexpected_codes  array of string  rejection codes outside the seed set
//!   oracle            object | null    {"observed": string}
//!   native            array    [{"optimization": "O1", "compiler": {...},
//!                                "observed": string | null}]
//!   agrees            boolean
//!   disagreements     array    [{"pair": <key>, "optimization": string | null,
//!                                "detail": string}]
//! ```
//!
//! The pair keys are `checker-compiler`, `lean-oracle`, `lean-native`, and
//! `oracle-native`.

use crate::fuzz::{self, CompileOptions, Compiled, OptimizationLevel};
use crate::trap::native_runtime_trap_kind;
use rue_compiler::CompilerSessionConfig;
use rue_error::PreviewFeatures;
use rue_oracle::{Outcome, TrapKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// Where `//:lean-bridge` hands the exported corpus to the harness.
const CORPUS_ENV: &str = "RUE_LEAN_CORPUS";

/// Per-phase budget. These programs are a few dozen lines with no loops; a
/// lane that needs longer than this is itself the finding.
const COMPILE_TIMEOUT: Duration = Duration::from_millis(rue_test_runner::DEFAULT_TIMEOUT_MS * 6);
const RUNTIME_TIMEOUT: Duration = Duration::from_millis(rue_test_runner::DEFAULT_TIMEOUT_MS);

/// The ownership diagnostics the seed corpus's rejected cases are expected to
/// produce. A rejection carrying any other code still *agrees* with the Lean
/// checker about accept/reject; the report prints it as unexpected so a
/// reviewer sees that the compiler refused for a different stated reason.
const EXPECTED_REJECTION_CODES: [&str; 5] = ["E0205", "E0406", "E0443", "E0478", "E0493"];

// ---------------------------------------------------------------------------
// The corpus contract
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCase {
    name: String,
    description: String,
    rules: Vec<String>,
    source: String,
    verdict: RawVerdict,
    expected: RawExpected,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawVerdict {
    Accept(RawAccept),
    Reject(RawReject),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccept {
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReject {}

/// The exported `expected` object, read flat so this harness can say exactly
/// which field is missing or does not belong to the declared `kind` instead of
/// letting serde's enum machinery collapse that into "data did not match any
/// variant".
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExpected {
    kind: String,
    stdout: Option<Vec<String>>,
    exit: Option<i32>,
    panic: Option<String>,
    violation: Option<String>,
}

/// What the verified checker says about a case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Accept { ty: String },
    Reject,
}

impl Verdict {
    fn key(&self) -> &'static str {
        match self {
            Self::Accept { .. } => "accept",
            Self::Reject => "reject",
        }
    }
}

/// What the verified interpreter says a case does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Expectation {
    /// Normal completion: one stdout line per drop event in trace order, then
    /// the program's value.
    Ok { stdout: Vec<String>, exit: i32 },
    /// A §6.12 trap. `name` is the Lean spelling, `trap` the modeled category
    /// both implementation views report.
    Panic { name: String, trap: TrapKind },
    /// The machine's named refusal for a rejected program. Unobservable here.
    Stuck { violation: String },
}

impl Expectation {
    /// The exact bytes an `ok` outcome must appear on stdout as: `@dbg` prints
    /// one value per line, each with its own trailing newline.
    fn expected_stdout_bytes(stdout: &[String]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for line in stdout {
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
        }
        bytes
    }

    fn describe(&self) -> String {
        match self {
            Self::Ok { stdout, exit } => {
                let printed = if stdout.is_empty() {
                    "nothing".to_string()
                } else {
                    stdout.join(", ")
                };
                format!("runs, prints {printed}, exit {exit}")
            }
            Self::Panic { name, trap } => format!("traps with {name} ({trap:?})"),
            Self::Stuck { violation } => {
                format!("refused by the machine with {violation} (not observable here)")
            }
        }
    }
}

/// One corpus case, after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Case {
    name: String,
    description: String,
    rules: Vec<String>,
    source: String,
    verdict: Verdict,
    expected: Expectation,
}

fn panic_trap_kind(name: &str) -> Option<TrapKind> {
    match name {
        "overflow" => Some(TrapKind::ArithmeticOverflow),
        "divZero" => Some(TrapKind::DivisionByZero),
        _ => None,
    }
}

/// Parse and validate a corpus document.
///
/// Every failure names the offending case: the exporter and this consumer are
/// two halves of one contract, so a mismatch is a build error on both sides
/// rather than a finding about the language.
pub(crate) fn parse_corpus(json: &str) -> Result<Vec<Case>, String> {
    // Two steps, so a schema failure can name the case it is in: serde reports
    // an unknown field by line and column, which is not what a reader of this
    // report has in front of them.
    let document: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|error| format!("corpus is not a JSON array of cases: {error}"))?;
    let mut raw = Vec::with_capacity(document.len());
    for (index, value) in document.into_iter().enumerate() {
        let identity = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| format!("at index {index}"), |name| format!("{name:?}"));
        raw.push(
            serde_json::from_value::<RawCase>(value)
                .map_err(|error| format!("case {identity}: {error}"))?,
        );
    }
    let mut seen = BTreeSet::new();
    let mut cases = Vec::with_capacity(raw.len());
    for case in raw {
        if !seen.insert(case.name.clone()) {
            return Err(format!("case {:?}: duplicate case name", case.name));
        }
        let verdict = match case.verdict {
            RawVerdict::Accept(accept) => Verdict::Accept { ty: accept.ty },
            RawVerdict::Reject(RawReject {}) => Verdict::Reject,
        };
        let expected = validate_expected(&case.name, &verdict, case.expected)?;
        cases.push(Case {
            name: case.name,
            description: case.description,
            rules: case.rules,
            source: case.source,
            verdict,
            expected,
        });
    }
    Ok(cases)
}

fn validate_expected(
    name: &str,
    verdict: &Verdict,
    raw: RawExpected,
) -> Result<Expectation, String> {
    let reject = |field: &str| {
        Err(format!(
            "case {name:?}: expected kind {:?} does not take a {field:?} field",
            raw.kind
        ))
    };
    match raw.kind.as_str() {
        "ok" => {
            if raw.panic.is_some() {
                return reject("panic");
            }
            if raw.violation.is_some() {
                return reject("violation");
            }
            if !matches!(verdict, Verdict::Accept { .. }) {
                return Err(format!(
                    "case {name:?}: an \"ok\" expectation needs an accept verdict"
                ));
            }
            let stdout = raw
                .stdout
                .ok_or_else(|| format!("case {name:?}: an \"ok\" expectation needs \"stdout\""))?;
            let exit = raw
                .exit
                .ok_or_else(|| format!("case {name:?}: an \"ok\" expectation needs \"exit\""))?;
            Ok(Expectation::Ok { stdout, exit })
        }
        "panic" => {
            if raw.stdout.is_some() {
                return reject("stdout");
            }
            if raw.exit.is_some() {
                return reject("exit");
            }
            if raw.violation.is_some() {
                return reject("violation");
            }
            if !matches!(verdict, Verdict::Accept { .. }) {
                return Err(format!(
                    "case {name:?}: a \"panic\" expectation needs an accept verdict"
                ));
            }
            let panic = raw
                .panic
                .ok_or_else(|| format!("case {name:?}: a \"panic\" expectation needs \"panic\""))?;
            let trap = panic_trap_kind(&panic).ok_or_else(|| {
                format!("case {name:?}: unknown panic kind {panic:?} in the corpus")
            })?;
            Ok(Expectation::Panic { name: panic, trap })
        }
        "stuck" => {
            if raw.stdout.is_some() {
                return reject("stdout");
            }
            if raw.exit.is_some() {
                return reject("exit");
            }
            if raw.panic.is_some() {
                return reject("panic");
            }
            if !matches!(verdict, Verdict::Reject) {
                return Err(format!(
                    "case {name:?}: a \"stuck\" expectation needs a reject verdict; an accepted \
                     program cannot reach a violation (RueCore.soundness)"
                ));
            }
            let violation = raw.violation.ok_or_else(|| {
                format!("case {name:?}: a \"stuck\" expectation needs \"violation\"")
            })?;
            Ok(Expectation::Stuck { violation })
        }
        other => Err(format!(
            "case {name:?}: unknown expected kind {other:?}; this consumer knows \"ok\", \
             \"panic\", and \"stuck\""
        )),
    }
}

// ---------------------------------------------------------------------------
// Observations
// ---------------------------------------------------------------------------

/// The compiler's answer to the accept/reject question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompilerVerdict {
    Accepted,
    Rejected {
        exit: i32,
        codes: Vec<String>,
        detail: String,
    },
    InternalError(String),
    Crashed {
        signal: i32,
        detail: String,
    },
    TimedOut,
}

impl CompilerVerdict {
    fn key(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected { .. } => "rejected",
            Self::InternalError(_) => "internal_error",
            Self::Crashed { .. } => "crashed",
            Self::TimedOut => "timed_out",
        }
    }

    fn codes(&self) -> &[String] {
        match self {
            Self::Rejected { codes, .. } => codes,
            _ => &[],
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Accepted => "accepted the program".to_string(),
            Self::Rejected {
                exit,
                codes,
                detail,
            } => {
                let codes = if codes.is_empty() {
                    "no coded diagnostic".to_string()
                } else {
                    codes.join(", ")
                };
                format!("rejected the program (exit {exit}, {codes}): {detail}")
            }
            Self::InternalError(detail) => format!("reported an internal compiler error: {detail}"),
            Self::Crashed { signal, detail } => {
                format!("was killed by signal {signal}: {detail}")
            }
            Self::TimedOut => "did not terminate within the compile budget".to_string(),
        }
    }
}

/// A diagnostic batch line of `--error-format json`, read for the two fields
/// this bridge reports. The surface is versioned (docs/process/diagnostics.md);
/// unknown keys are deliberately tolerated here, because a *new* diagnostic
/// field is not a corpus-contract change.
#[derive(Debug, Deserialize)]
struct RawDiagnostic {
    severity: String,
    code: String,
    message: String,
}

/// What a compiler stderr stream said, in emission order.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Diagnostics {
    /// Error codes, deduplicated. Warnings carry an empty `code` and are not
    /// part of an accept/reject answer, so only `error` diagnostics count.
    pub(crate) codes: Vec<String>,
    /// The `message` of each error diagnostic.
    pub(crate) messages: Vec<String>,
    /// Lines that were not a diagnostic batch — a linker error, an ICE banner.
    /// Kept rather than dropped: unreadable stderr is evidence too.
    pub(crate) other: Vec<String>,
}

impl Diagnostics {
    /// The readable half: the diagnostics when there are any, else whatever
    /// else the compiler wrote.
    fn detail(&self) -> String {
        let text = if self.messages.is_empty() {
            self.other.join("; ")
        } else {
            self.messages.join("; ")
        };
        if text.is_empty() {
            "(no diagnostic text)".to_string()
        } else {
            text
        }
    }
}

pub(crate) fn parse_diagnostics(stderr: &str) -> Diagnostics {
    let mut parsed = Diagnostics::default();
    for line in stderr.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Vec<RawDiagnostic>>(line) {
            Ok(batch) => {
                for diagnostic in batch {
                    if diagnostic.severity != "error" {
                        continue;
                    }
                    if !diagnostic.code.is_empty() && !parsed.codes.contains(&diagnostic.code) {
                        parsed.codes.push(diagnostic.code);
                    }
                    parsed.messages.push(diagnostic.message);
                }
            }
            Err(_) => parsed.other.push(line.to_string()),
        }
    }
    parsed
}

/// Read the compiler's accept/reject answer out of a native compile-and-run.
///
/// `Ran`, `Crash`, and `Timeout` all mean the compiler produced a binary: what
/// that binary then did is a *native* observation, not an accept/reject one.
pub(crate) fn compiler_verdict(compiled: &Compiled) -> CompilerVerdict {
    match compiled {
        Compiled::Ran { .. } | Compiled::Crash(_) | Compiled::Timeout => CompilerVerdict::Accepted,
        Compiled::CompileRejected { exit, stderr } => {
            let parsed = parse_diagnostics(stderr);
            CompilerVerdict::Rejected {
                exit: *exit,
                detail: parsed.detail(),
                codes: parsed.codes,
            }
        }
        // `rue_test_runner::ice_message` wraps the compiler's stderr in its own
        // banner, so the structured E9000 diagnostic is in there too: report it
        // rather than the banner.
        Compiled::CompileIce(detail) => {
            let parsed = parse_diagnostics(detail);
            CompilerVerdict::InternalError(if parsed.codes.is_empty() {
                parsed.detail()
            } else {
                format!("{} [{}]", parsed.detail(), parsed.codes.join(", "))
            })
        }
        Compiled::CompileCrash { signal, stderr } => CompilerVerdict::Crashed {
            signal: *signal,
            detail: parse_diagnostics(stderr).detail(),
        },
        Compiled::CompileTimeout => CompilerVerdict::TimedOut,
    }
}

/// An executed program, as one view observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunObservation {
    Ran {
        exit: i32,
        stdout: Vec<u8>,
        trap: Option<TrapKind>,
    },
    /// The view produced no comparable observation, and why.
    NotObserved(String),
}

impl RunObservation {
    fn describe(&self) -> String {
        match self {
            Self::Ran { exit, stdout, trap } => {
                let trap = match trap {
                    Some(kind) => format!("{kind:?}"),
                    None => "no trap".to_string(),
                };
                format!(
                    "exit {exit}, stdout {:?}, {trap}",
                    String::from_utf8_lossy(stdout)
                )
            }
            Self::NotObserved(reason) => format!("not observed: {reason}"),
        }
    }
}

fn oracle_observation(outcome: &Outcome) -> RunObservation {
    RunObservation::Ran {
        exit: outcome.exit_code,
        stdout: outcome.stdout_bytes.clone(),
        trap: outcome.panic,
    }
}

/// The native view's observation, or `None` when the compiler produced no
/// binary at this lane (an accept/reject finding, reported as that pair).
fn native_observation(compiled: &Compiled) -> Option<RunObservation> {
    match compiled {
        Compiled::Ran {
            exit,
            stdout,
            stdout_truncated,
            stderr,
            stderr_truncated,
        } => Some(if *stdout_truncated || *stderr_truncated {
            RunObservation::NotObserved(
                "the captured output hit the harness limit, so the retained prefix cannot prove \
                 anything"
                    .to_string(),
            )
        } else {
            RunObservation::Ran {
                exit: *exit,
                stdout: stdout.clone(),
                trap: native_runtime_trap_kind(stderr),
            }
        }),
        Compiled::Crash(signal) => Some(RunObservation::NotObserved(format!(
            "the binary was killed by signal {signal}"
        ))),
        Compiled::Timeout => Some(RunObservation::NotObserved(
            "the binary did not terminate within the run budget".to_string(),
        )),
        Compiled::CompileRejected { .. }
        | Compiled::CompileCrash { .. }
        | Compiled::CompileIce(_)
        | Compiled::CompileTimeout => None,
    }
}

// ---------------------------------------------------------------------------
// Pairwise comparison
// ---------------------------------------------------------------------------

/// The two views a disagreement is between. Never which one is wrong: that is
/// the reviewer's call on the evidence (RUE-305).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Pair {
    CheckerCompiler,
    LeanOracle,
    LeanNative,
    OracleNative,
}

impl Pair {
    /// Declaration order is the stable report order.
    const ALL: [Self; 4] = [
        Self::CheckerCompiler,
        Self::LeanOracle,
        Self::LeanNative,
        Self::OracleNative,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::CheckerCompiler => "checker-compiler",
            Self::LeanOracle => "lean-oracle",
            Self::LeanNative => "lean-native",
            Self::OracleNative => "oracle-native",
        }
    }
}

impl fmt::Display for Pair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CheckerCompiler => "checker <-> compiler",
            Self::LeanOracle => "lean <-> oracle",
            Self::LeanNative => "lean <-> native",
            Self::OracleNative => "oracle <-> native",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pair: Pair,
    optimization: Option<OptimizationLevel>,
    detail: String,
}

/// Compare the Lean verdict with the compiler's answer.
pub(crate) fn checker_compiler_finding(
    verdict: &Verdict,
    compiler: &CompilerVerdict,
) -> Option<String> {
    match (verdict, compiler) {
        (Verdict::Accept { ty }, CompilerVerdict::Rejected { codes, detail, .. }) => {
            let codes = if codes.is_empty() {
                "no coded diagnostic".to_string()
            } else {
                codes.join(", ")
            };
            Some(format!(
                "the checker accepts this program at type {ty}, and the compiler rejected it \
                 ({codes}): {detail}"
            ))
        }
        (Verdict::Reject, CompilerVerdict::Accepted) => {
            Some("the checker rejects this program, and the compiler accepted it".to_string())
        }
        (_, CompilerVerdict::InternalError(detail)) => Some(format!(
            "the checker reaches a verdict on this program, and the compiler reported an \
             internal compiler error instead: {detail}"
        )),
        (_, CompilerVerdict::Crashed { signal, detail }) => Some(format!(
            "the checker reaches a verdict on this program, and the compiler was killed by \
             signal {signal}: {detail}"
        )),
        (_, CompilerVerdict::TimedOut) => Some(
            "the checker reaches a verdict on this program, and the compiler did not terminate \
             within the compile budget"
                .to_string(),
        ),
        (Verdict::Accept { .. }, CompilerVerdict::Accepted)
        | (Verdict::Reject, CompilerVerdict::Rejected { .. }) => None,
    }
}

/// Rejection codes outside the seed corpus's expected ownership set. These are
/// not disagreements — the two views agree the program is rejected — but a
/// reviewer should see that the stated reason moved.
pub(crate) fn unexpected_rejection_codes(compiler: &CompilerVerdict) -> Vec<String> {
    compiler
        .codes()
        .iter()
        .filter(|code| !EXPECTED_REJECTION_CODES.contains(&code.as_str()))
        .cloned()
        .collect()
}

/// Compare the Lean expectation with one executed view.
pub(crate) fn expectation_finding(
    expected: &Expectation,
    observed: &RunObservation,
) -> Option<String> {
    let (exit, stdout, trap) = match observed {
        RunObservation::Ran { exit, stdout, trap } => (*exit, stdout.as_slice(), *trap),
        RunObservation::NotObserved(reason) => {
            return Some(format!(
                "the interpreter says the program {}, and the run produced no comparable \
                 observation: {reason}",
                expected.describe()
            ));
        }
    };
    match expected {
        Expectation::Ok {
            stdout: lines,
            exit: want_exit,
        } => {
            let want = Expectation::expected_stdout_bytes(lines);
            let mut reasons = Vec::new();
            if exit != *want_exit {
                reasons.push(format!("exit: interpreter {want_exit}, run {exit}"));
            }
            if stdout != want.as_slice() {
                reasons.push(format!(
                    "stdout: interpreter {:?}, run {:?}",
                    String::from_utf8_lossy(&want),
                    String::from_utf8_lossy(stdout)
                ));
            }
            if let Some(kind) = trap {
                reasons.push(format!(
                    "the interpreter completes normally, the run trapped with {kind:?}"
                ));
            }
            (!reasons.is_empty()).then(|| reasons.join("; "))
        }
        Expectation::Panic { name, trap: want } => {
            let mut reasons = Vec::new();
            if exit != rue_test_runner::RUNTIME_ERROR_EXIT_CODE {
                reasons.push(format!(
                    "exit: a {name} trap exits {}, run {exit}",
                    rue_test_runner::RUNTIME_ERROR_EXIT_CODE
                ));
            }
            match trap {
                Some(got) if got == *want => {}
                Some(got) => reasons.push(format!(
                    "trap category: interpreter {want:?} ({name}), run {got:?}"
                )),
                None => reasons.push(format!(
                    "trap category: interpreter {want:?} ({name}), run reported no recognized trap"
                )),
            }
            (!reasons.is_empty()).then(|| reasons.join("; "))
        }
        Expectation::Stuck { violation } => Some(format!(
            "the checker rejects this program, so its {violation} refusal is not observable; \
             a run of it is itself the disagreement"
        )),
    }
}

/// Every disagreement among the three executed views and the Lean expectation.
///
/// `lanes` carries the native result per optimization level; a lane whose
/// compile failed is reported on the `checker <-> compiler` pair, because the
/// disagreement is about acceptance rather than behavior.
pub(crate) fn observation_findings(
    expected: &Expectation,
    oracle: Option<&Outcome>,
    lanes: &[(OptimizationLevel, &Compiled)],
) -> Vec<Finding> {
    let mut findings = Vec::new();
    if let Some(outcome) = oracle
        && let Some(detail) = expectation_finding(expected, &oracle_observation(outcome))
    {
        findings.push(Finding {
            pair: Pair::LeanOracle,
            optimization: None,
            detail,
        });
    }
    for (level, compiled) in lanes {
        let Some(observed) = native_observation(compiled) else {
            findings.push(Finding {
                pair: Pair::CheckerCompiler,
                optimization: Some(*level),
                detail: format!(
                    "the compiler produced a binary at other lanes but not here: {}",
                    compiler_verdict(compiled).describe()
                ),
            });
            continue;
        };
        if let Some(detail) = expectation_finding(expected, &observed) {
            findings.push(Finding {
                pair: Pair::LeanNative,
                optimization: Some(*level),
                detail,
            });
        }
        if let Some(outcome) = oracle
            && let fuzz::Verdict::Disagree(detail) = fuzz::classify(outcome, compiled)
        {
            findings.push(Finding {
                pair: Pair::OracleNative,
                optimization: Some(*level),
                detail,
            });
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaseReport {
    case: Case,
    compiler: CompilerVerdict,
    unexpected_codes: Vec<String>,
    oracle: Option<RunObservation>,
    native: Vec<(OptimizationLevel, CompilerVerdict, Option<RunObservation>)>,
    findings: Vec<Finding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    corpus: String,
    cases: Vec<CaseReport>,
}

impl Report {
    fn tally(&self) -> BTreeMap<Pair, usize> {
        let mut tally: BTreeMap<Pair, usize> = Pair::ALL.iter().map(|pair| (*pair, 0)).collect();
        for case in &self.cases {
            for finding in &case.findings {
                *tally.entry(finding.pair).or_default() += 1;
            }
        }
        tally
    }

    fn disagreeing(&self) -> usize {
        self.cases
            .iter()
            .filter(|case| !case.findings.is_empty())
            .count()
    }
}

/// The plain-text report: a line per case, then the full evidence for each
/// disagreeing case, then the tally by pair.
pub(crate) fn render_report(report: &Report) -> String {
    let mut out = String::new();
    out += &format!(
        "rue-oracle-diff lean-corpus: {} cases from {}\n\n",
        report.cases.len(),
        report.corpus
    );
    let rows: Vec<(String, String, String)> = report
        .cases
        .iter()
        .map(|case| {
            let verdict = match &case.case.verdict {
                Verdict::Accept { ty } => format!("accept({ty})"),
                Verdict::Reject => "reject".to_string(),
            };
            let mut status = if case.findings.is_empty() {
                "agree".to_string()
            } else {
                format!("DISAGREE ({})", case.findings.len())
            };
            // The codes of an agreed rejection belong in the report: the two
            // views agree the program is refused, and a reader still wants to
            // see which ownership rule the compiler cited.
            if !case.compiler.codes().is_empty() {
                status += &format!(" ({})", case.compiler.codes().join(", "));
            }
            if !case.unexpected_codes.is_empty() {
                status += &format!(
                    " — rejected with an unexpected code: {}",
                    case.unexpected_codes.join(", ")
                );
            }
            (case.case.name.clone(), verdict, status)
        })
        .collect();
    let name_width = rows.iter().map(|(name, ..)| name.len()).max().unwrap_or(0);
    let verdict_width = rows
        .iter()
        .map(|(_, verdict, _)| verdict.len())
        .max()
        .unwrap_or(0);
    for (name, verdict, status) in &rows {
        out += &format!("  {name:name_width$}  {verdict:verdict_width$}  {status}\n");
    }

    for case in report.cases.iter().filter(|case| !case.findings.is_empty()) {
        out += &format!("\n===== {} =====\n", case.case.name);
        out += &format!("{}\n", case.case.description);
        out += &format!("rules: {}\n", case.case.rules.join(", "));
        out += "\n--- the program ---\n";
        out += &case.case.source;
        if !case.case.source.ends_with('\n') {
            out.push('\n');
        }
        out += "\n--- the four views ---\n";
        out += &format!(
            "  Lean     : {}; the interpreter says it {}\n",
            match &case.case.verdict {
                Verdict::Accept { ty } => format!("the checker accepts the program at type {ty}"),
                Verdict::Reject => "the checker rejects the program".to_string(),
            },
            case.case.expected.describe()
        );
        out += &format!("  compiler : {}\n", case.compiler.describe());
        out += &format!(
            "  oracle   : {}\n",
            case.oracle
                .as_ref()
                .map_or_else(|| "not run".to_string(), RunObservation::describe)
        );
        if case.native.is_empty() {
            out += "  native   : not run\n";
        } else {
            for (level, compiler, observed) in &case.native {
                out += &format!(
                    "  native {level}: {}\n",
                    observed
                        .as_ref()
                        .map_or_else(|| compiler.describe(), RunObservation::describe)
                );
            }
        }
        out += "\n--- disagreeing pairs ---\n";
        for finding in &case.findings {
            let lane = finding
                .optimization
                .map_or_else(String::new, |level| format!(" [{level}]"));
            out += &format!("  {}{lane}: {}\n", finding.pair, finding.detail);
        }
    }

    let tally = report.tally();
    out += "\n===== tally =====\n";
    out += &format!(
        "  cases: {} ({} agree, {} disagree)\n",
        report.cases.len(),
        report.cases.len() - report.disagreeing(),
        report.disagreeing()
    );
    for pair in Pair::ALL {
        out += &format!("  {pair}: {}\n", tally.get(&pair).copied().unwrap_or(0));
    }
    out
}

// ---------------------------------------------------------------------------
// The JSON report
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct JsonReport<'a> {
    corpus: &'a str,
    cases_total: usize,
    cases_agreeing: usize,
    cases_disagreeing: usize,
    tally: BTreeMap<&'static str, usize>,
    cases: Vec<JsonCase<'a>>,
}

#[derive(Debug, Serialize)]
struct JsonCase<'a> {
    name: &'a str,
    description: &'a str,
    rules: &'a [String],
    source: &'a str,
    verdict: &'static str,
    accepted_type: Option<&'a str>,
    expected: JsonExpected<'a>,
    compiler: JsonCompiler<'a>,
    unexpected_codes: &'a [String],
    oracle: Option<JsonObserved>,
    native: Vec<JsonLane<'a>>,
    agrees: bool,
    disagreements: Vec<JsonFinding<'a>>,
}

#[derive(Debug, Serialize)]
struct JsonExpected<'a> {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    panic: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    violation: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct JsonCompiler<'a> {
    outcome: &'static str,
    codes: &'a [String],
    detail: String,
}

#[derive(Debug, Serialize)]
struct JsonObserved {
    observed: String,
}

#[derive(Debug, Serialize)]
struct JsonLane<'a> {
    optimization: String,
    compiler: JsonCompiler<'a>,
    observed: Option<String>,
}

#[derive(Debug, Serialize)]
struct JsonFinding<'a> {
    pair: &'static str,
    optimization: Option<String>,
    detail: &'a str,
}

fn json_expected(expected: &Expectation) -> JsonExpected<'_> {
    match expected {
        Expectation::Ok { stdout, exit } => JsonExpected {
            kind: "ok",
            stdout: Some(stdout),
            exit: Some(*exit),
            panic: None,
            violation: None,
        },
        Expectation::Panic { name, .. } => JsonExpected {
            kind: "panic",
            stdout: None,
            exit: None,
            panic: Some(name),
            violation: None,
        },
        Expectation::Stuck { violation } => JsonExpected {
            kind: "stuck",
            stdout: None,
            exit: None,
            panic: None,
            violation: Some(violation),
        },
    }
}

fn json_compiler(compiler: &CompilerVerdict) -> JsonCompiler<'_> {
    JsonCompiler {
        outcome: compiler.key(),
        codes: compiler.codes(),
        detail: compiler.describe(),
    }
}

pub(crate) fn render_report_json(report: &Report) -> String {
    let document = JsonReport {
        corpus: &report.corpus,
        cases_total: report.cases.len(),
        cases_agreeing: report.cases.len() - report.disagreeing(),
        cases_disagreeing: report.disagreeing(),
        tally: report
            .tally()
            .into_iter()
            .map(|(pair, count)| (pair.key(), count))
            .collect(),
        cases: report
            .cases
            .iter()
            .map(|case| JsonCase {
                name: &case.case.name,
                description: &case.case.description,
                rules: &case.case.rules,
                source: &case.case.source,
                verdict: case.case.verdict.key(),
                accepted_type: match &case.case.verdict {
                    Verdict::Accept { ty } => Some(ty.as_str()),
                    Verdict::Reject => None,
                },
                expected: json_expected(&case.case.expected),
                compiler: json_compiler(&case.compiler),
                unexpected_codes: &case.unexpected_codes,
                oracle: case.oracle.as_ref().map(|observed| JsonObserved {
                    observed: observed.describe(),
                }),
                native: case
                    .native
                    .iter()
                    .map(|(level, compiler, observed)| JsonLane {
                        optimization: level.to_string(),
                        compiler: json_compiler(compiler),
                        observed: observed.as_ref().map(RunObservation::describe),
                    })
                    .collect(),
                agrees: case.findings.is_empty(),
                disagreements: case
                    .findings
                    .iter()
                    .map(|finding| JsonFinding {
                        pair: finding.pair.key(),
                        optimization: finding.optimization.map(|level| level.to_string()),
                        detail: &finding.detail,
                    })
                    .collect(),
            })
            .collect(),
    };
    serde_json::to_string_pretty(&document).unwrap_or_else(|error| {
        // Every field is a string, number, bool, or sequence of those, so this
        // is unreachable; fail loudly rather than emit a half-written report.
        panic!("lean-corpus report is not serializable: {error}")
    })
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Config {
    corpus: PathBuf,
    cases: Vec<String>,
    report_json: Option<PathBuf>,
}

const USAGE: &str = "usage: rue-oracle-diff lean-corpus [--corpus <corpus.json>] \
                     [--case <name>]... [--report-json <path>]";

pub(crate) fn parse_args(args: &[String], corpus_env: Option<&str>) -> Result<Config, String> {
    let mut corpus: Option<PathBuf> = None;
    let mut cases = Vec::new();
    let mut report_json = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut value_for = |name: &str, inline: Option<&str>| -> Result<String, String> {
            match inline {
                Some(value) => Ok(value.to_string()),
                None => {
                    index += 1;
                    args.get(index)
                        .cloned()
                        .ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
                }
            }
        };
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg, None),
        };
        match name {
            "--corpus" => {
                let value = value_for("--corpus", inline)?;
                corpus = Some(PathBuf::from(value));
            }
            "--case" => {
                let value = value_for("--case", inline)?;
                cases.push(value);
            }
            "--report-json" => {
                let value = value_for("--report-json", inline)?;
                report_json = Some(PathBuf::from(value));
            }
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
        index += 1;
    }
    let corpus = corpus
        .or_else(|| corpus_env.map(PathBuf::from))
        .ok_or_else(|| {
            format!("the corpus path is required: pass --corpus or set {CORPUS_ENV}\n{USAGE}")
        })?;
    Ok(Config {
        corpus,
        cases,
        report_json,
    })
}

/// Restrict a corpus to the `--case` selection, failing on a name that is not
/// in the corpus so a typo cannot report a green run over zero cases.
pub(crate) fn select_cases(cases: Vec<Case>, selection: &[String]) -> Result<Vec<Case>, String> {
    if selection.is_empty() {
        return Ok(cases);
    }
    let available: BTreeSet<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    for name in selection {
        if !available.contains(name.as_str()) {
            return Err(format!(
                "no case named {name:?} in the corpus; it has: {}",
                available.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
    }
    Ok(cases
        .into_iter()
        .filter(|case| selection.contains(&case.name))
        .collect())
}

pub(crate) fn run(args: &[String], configuration: CompilerSessionConfig) -> ExitCode {
    let config = match parse_args(args, std::env::var(CORPUS_ENV).ok().as_deref()) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("rue-oracle-diff lean-corpus: {message}");
            return ExitCode::FAILURE;
        }
    };
    let rue = match std::env::var_os("RUE_BINARY") {
        Some(path) => PathBuf::from(path),
        None => {
            eprintln!("rue-oracle-diff lean-corpus: RUE_BINARY must point to the Rue compiler");
            return ExitCode::FAILURE;
        }
    };
    let std_path = std::env::var_os("RUE_ORACLE_DIFF_STD")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("std"));
    let document = match std::fs::read_to_string(&config.corpus) {
        Ok(document) => document,
        Err(error) => {
            eprintln!(
                "rue-oracle-diff lean-corpus: cannot read {}: {error}",
                config.corpus.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let cases = match parse_corpus(&document).and_then(|cases| select_cases(cases, &config.cases)) {
        Ok(cases) => cases,
        Err(message) => {
            eprintln!(
                "rue-oracle-diff lean-corpus: {}: {message}",
                config.corpus.display()
            );
            return ExitCode::FAILURE;
        }
    };
    if cases.is_empty() {
        eprintln!("rue-oracle-diff lean-corpus: the corpus is empty");
        return ExitCode::FAILURE;
    }
    let workdir = match tempfile::Builder::new()
        .prefix("rue-lean-bridge-")
        .tempdir()
    {
        Ok(workdir) => workdir,
        Err(error) => {
            eprintln!("rue-oracle-diff lean-corpus: cannot create a work directory: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut report = Report {
        corpus: config.corpus.display().to_string(),
        cases: Vec::with_capacity(cases.len()),
    };
    for case in cases {
        match evaluate_case(case, &rue, workdir.path(), &std_path, configuration) {
            Ok(entry) => report.cases.push(entry),
            Err(message) => {
                eprintln!("rue-oracle-diff lean-corpus: {message}");
                return ExitCode::FAILURE;
            }
        }
    }

    print!("{}", render_report(&report));
    if let Some(path) = &config.report_json
        && let Err(error) = std::fs::write(path, render_report_json(&report))
    {
        eprintln!(
            "rue-oracle-diff lean-corpus: cannot write {}: {error}",
            path.display()
        );
        return ExitCode::FAILURE;
    }
    if report.disagreeing() == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn compile_options<'a>(
    optimization: OptimizationLevel,
    std_path: &'a Path,
    error_format_json: bool,
) -> CompileOptions<'a> {
    CompileOptions {
        optimization,
        previews: &[],
        std_path: Some(std_path),
        compile_timeout: COMPILE_TIMEOUT,
        runtime_timeout: RUNTIME_TIMEOUT,
        error_format_json,
    }
}

/// Run the three implementation views on one case.
///
/// The accept/reject probe is a single O1 compile with `--error-format json`;
/// when it produces a binary, that same result is the O1 native lane, so the
/// bridge never compiles a lane twice to answer two questions about it.
fn evaluate_case(
    case: Case,
    rue: &Path,
    workdir: &Path,
    std_path: &Path,
    configuration: CompilerSessionConfig,
) -> Result<CaseReport, String> {
    let probe = fuzz::compile_and_run(
        rue,
        workdir,
        &case.source,
        compile_options(OptimizationLevel::O1, std_path, true),
    )
    .map_err(|error| format!("case {:?}: native harness error: {error}", case.name))?;
    let compiler = compiler_verdict(&probe);
    let unexpected_codes = unexpected_rejection_codes(&compiler);

    let mut findings = Vec::new();
    if let Some(detail) = checker_compiler_finding(&case.verdict, &compiler) {
        findings.push(Finding {
            pair: Pair::CheckerCompiler,
            optimization: None,
            detail,
        });
    }

    // A rejected program has no behavior to compare: the Lean machine's
    // `stuck` refusal is not observable through a compiler that refuses first.
    if matches!(case.verdict, Verdict::Reject) {
        return Ok(CaseReport {
            case,
            compiler,
            unexpected_codes,
            oracle: None,
            native: Vec::new(),
            findings,
        });
    }

    // The oracle is an independent view: run it even when the compiler refused
    // the program, so a compiler defect cannot hide an oracle one.
    let (oracle, observed_oracle) = match crate::run_source_with_real_std_with_configuration(
        &case.source,
        &PreviewFeatures::new(),
        configuration,
    ) {
        Ok(Ok(outcome)) => {
            let observed = oracle_observation(&outcome);
            (Some(outcome), observed)
        }
        Ok(Err(error)) => {
            findings.push(Finding {
                pair: Pair::LeanOracle,
                optimization: None,
                detail: format!("the oracle could not run the program: {error}"),
            });
            (None, RunObservation::NotObserved(error.to_string()))
        }
        Err(error) => {
            return Err(format!(
                "case {:?}: oracle harness error: {error}",
                case.name
            ));
        }
    };

    // Native lanes, only once the compiler produced a binary at the probe.
    let mut compiled = Vec::new();
    if compiler == CompilerVerdict::Accepted {
        compiled.push((OptimizationLevel::O1, probe));
        for optimization in [OptimizationLevel::O2, OptimizationLevel::O3] {
            let result = fuzz::compile_and_run(
                rue,
                workdir,
                &case.source,
                compile_options(optimization, std_path, false),
            )
            .map_err(|error| {
                format!(
                    "case {:?} [{optimization}]: native harness error: {error}",
                    case.name
                )
            })?;
            compiled.push((optimization, result));
        }
    }
    let lanes: Vec<(OptimizationLevel, &Compiled)> = compiled
        .iter()
        .map(|(level, result)| (*level, result))
        .collect();
    findings.extend(observation_findings(
        &case.expected,
        oracle.as_ref(),
        &lanes,
    ));

    Ok(CaseReport {
        native: compiled
            .iter()
            .map(|(level, result)| (*level, compiler_verdict(result), native_observation(result)))
            .collect(),
        oracle: Some(observed_oracle),
        case,
        compiler,
        unexpected_codes,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_CASE: &str = r#"[
      {
        "name": "scalars",
        "description": "Well-typed scalar flow.",
        "rules": ["(Use-Copy) §5.1"],
        "source": "fn main() -> i32 { 0 }\n",
        "verdict": {"accept": {"type": "i64"}},
        "expected": {"kind": "ok", "stdout": ["10"], "exit": 0}
      }
    ]"#;

    fn ran(exit: i32, stdout: &str, stderr: &str) -> Compiled {
        Compiled::Ran {
            exit,
            stdout: stdout.as_bytes().to_vec(),
            stdout_truncated: false,
            stderr: stderr.to_string(),
            stderr_truncated: false,
        }
    }

    fn outcome(exit: i32, stdout: &str, stderr: &str, panic: Option<TrapKind>) -> Outcome {
        Outcome {
            exit_code: exit,
            stdout: stdout.to_string(),
            stdout_bytes: stdout.as_bytes().to_vec(),
            stderr: stderr.to_string(),
            panic,
        }
    }

    fn ok_expectation() -> Expectation {
        Expectation::Ok {
            stdout: vec!["7".to_string(), "1".to_string()],
            exit: 0,
        }
    }

    #[test]
    fn parses_the_exported_contract() {
        let cases = parse_corpus(ONE_CASE).expect("valid corpus");
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "scalars");
        assert_eq!(
            cases[0].verdict,
            Verdict::Accept {
                ty: "i64".to_string()
            }
        );
        assert_eq!(
            cases[0].expected,
            Expectation::Ok {
                stdout: vec!["10".to_string()],
                exit: 0,
            }
        );
    }

    #[test]
    fn parses_reject_and_panic_shapes() {
        let document = r#"[
          {"name": "leak", "description": "", "rules": [], "source": "",
           "verdict": {"reject": {}},
           "expected": {"kind": "stuck", "violation": "linearLeak"}},
          {"name": "overflow", "description": "", "rules": [], "source": "",
           "verdict": {"accept": {"type": "i64"}},
           "expected": {"kind": "panic", "panic": "divZero"}}
        ]"#;
        let cases = parse_corpus(document).expect("valid corpus");
        assert_eq!(cases[0].verdict, Verdict::Reject);
        assert_eq!(
            cases[0].expected,
            Expectation::Stuck {
                violation: "linearLeak".to_string()
            }
        );
        assert_eq!(
            cases[1].expected,
            Expectation::Panic {
                name: "divZero".to_string(),
                trap: TrapKind::DivisionByZero,
            }
        );
    }

    #[test]
    fn an_unknown_corpus_field_is_a_hard_error() {
        let document = ONE_CASE.replace("\"rules\":", "\"laws\":");
        let error = parse_corpus(&document).expect_err("unknown field must be refused");
        assert!(error.contains("laws"), "{error}");
        // The message names the case, not a line and column.
        assert!(error.contains("case \"scalars\""), "{error}");

        let nested = ONE_CASE.replace(
            "{\"kind\": \"ok\", \"stdout\": [\"10\"], \"exit\": 0}",
            "{\"kind\": \"ok\", \"stdout\": [\"10\"], \"exit\": 0, \"trace\": []}",
        );
        let error = parse_corpus(&nested).expect_err("unknown expected field must be refused");
        assert!(error.contains("trace"), "{error}");
    }

    #[test]
    fn kind_and_verdict_must_agree() {
        let stuck_on_accept = ONE_CASE.replace(
            "{\"kind\": \"ok\", \"stdout\": [\"10\"], \"exit\": 0}",
            "{\"kind\": \"stuck\", \"violation\": \"linearLeak\"}",
        );
        let error = parse_corpus(&stuck_on_accept).expect_err("stuck needs a reject verdict");
        assert!(error.contains("reject verdict"), "{error}");

        let ok_on_reject =
            ONE_CASE.replace("{\"accept\": {\"type\": \"i64\"}}", "{\"reject\": {}}");
        let error = parse_corpus(&ok_on_reject).expect_err("ok needs an accept verdict");
        assert!(error.contains("accept verdict"), "{error}");

        let unknown_kind = ONE_CASE.replace("\"kind\": \"ok\"", "\"kind\": \"diverges\"");
        let error = parse_corpus(&unknown_kind).expect_err("unknown kind");
        assert!(error.contains("unknown expected kind"), "{error}");

        let missing_exit =
            ONE_CASE.replace("\"stdout\": [\"10\"], \"exit\": 0", "\"stdout\": [\"10\"]");
        let error = parse_corpus(&missing_exit).expect_err("ok needs exit");
        assert!(error.contains("\"exit\""), "{error}");

        let bad_panic = ONE_CASE.replace(
            "{\"kind\": \"ok\", \"stdout\": [\"10\"], \"exit\": 0}",
            "{\"kind\": \"panic\", \"panic\": \"stackOverflow\"}",
        );
        let error = parse_corpus(&bad_panic).expect_err("unknown panic kind");
        assert!(error.contains("unknown panic kind"), "{error}");
    }

    #[test]
    fn duplicate_case_names_are_refused() {
        let document = format!("[{},{}]", &ONE_CASE[1..ONE_CASE.len() - 1].trim(), {
            let inner = &ONE_CASE[1..ONE_CASE.len() - 1];
            inner.trim()
        });
        let error = parse_corpus(&document).expect_err("duplicate names");
        assert!(error.contains("duplicate case name"), "{error}");
    }

    #[test]
    fn ok_expectations_compare_lines_exit_and_absence_of_a_trap() {
        let expected = ok_expectation();
        assert_eq!(
            expectation_finding(
                &expected,
                &RunObservation::Ran {
                    exit: 0,
                    stdout: b"7\n1\n".to_vec(),
                    trap: None,
                }
            ),
            None
        );
        let wrong_order = expectation_finding(
            &expected,
            &RunObservation::Ran {
                exit: 0,
                stdout: b"1\n7\n".to_vec(),
                trap: None,
            },
        )
        .expect("drop order is observable");
        assert!(wrong_order.contains("stdout"), "{wrong_order}");

        let wrong_exit = expectation_finding(
            &expected,
            &RunObservation::Ran {
                exit: 1,
                stdout: b"7\n1\n".to_vec(),
                trap: None,
            },
        )
        .expect("exit is compared");
        assert!(wrong_exit.contains("exit"), "{wrong_exit}");

        let trapped = expectation_finding(
            &expected,
            &RunObservation::Ran {
                exit: 101,
                stdout: b"7\n1\n".to_vec(),
                trap: Some(TrapKind::ArithmeticOverflow),
            },
        )
        .expect("a trap is not a normal completion");
        assert!(trapped.contains("ArithmeticOverflow"), "{trapped}");

        // No drop events and no value line is an empty stdout, not a newline.
        assert_eq!(
            expectation_finding(
                &Expectation::Ok {
                    stdout: Vec::new(),
                    exit: 0
                },
                &RunObservation::Ran {
                    exit: 0,
                    stdout: Vec::new(),
                    trap: None,
                }
            ),
            None
        );
    }

    #[test]
    fn panic_expectations_compare_the_trap_category_and_exit() {
        let expected = Expectation::Panic {
            name: "overflow".to_string(),
            trap: TrapKind::ArithmeticOverflow,
        };
        assert_eq!(
            expectation_finding(
                &expected,
                &RunObservation::Ran {
                    exit: 101,
                    stdout: b"anything\n".to_vec(),
                    trap: Some(TrapKind::ArithmeticOverflow),
                }
            ),
            None,
            "a trap discards the Lean trace, so stdout is not compared"
        );
        let wrong_kind = expectation_finding(
            &expected,
            &RunObservation::Ran {
                exit: 101,
                stdout: Vec::new(),
                trap: Some(TrapKind::DivisionByZero),
            },
        )
        .expect("the category is compared");
        assert!(wrong_kind.contains("DivisionByZero"), "{wrong_kind}");

        let no_trap = expectation_finding(
            &expected,
            &RunObservation::Ran {
                exit: 101,
                stdout: Vec::new(),
                trap: None,
            },
        )
        .expect("exit 101 alone is not a trap");
        assert!(no_trap.contains("no recognized trap"), "{no_trap}");

        let not_observed = expectation_finding(
            &expected,
            &RunObservation::NotObserved("the binary was killed by signal 11".to_string()),
        )
        .expect("an unobserved run is a disagreement");
        assert!(not_observed.contains("signal 11"), "{not_observed}");
    }

    #[test]
    fn diagnostic_codes_come_from_the_json_error_format() {
        let stderr = "[{\"code\":\"\",\"helps\":[],\"message\":\"unused function 'f'\",\
                      \"notes\":[],\"severity\":\"warning\",\"spans\":[],\"suggestions\":[]}]\n\
                      [{\"code\":\"E0406\",\"helps\":[],\"message\":\"linear value 'v0' must be \
                      consumed but was dropped\",\"notes\":[],\"severity\":\"error\",\
                      \"spans\":[],\"suggestions\":[]}]\n";
        let parsed = parse_diagnostics(stderr);
        assert_eq!(parsed.codes, vec!["E0406".to_string()]);
        assert_eq!(
            parsed.messages,
            vec!["linear value 'v0' must be consumed but was dropped".to_string()]
        );
        assert!(parsed.other.is_empty());

        // Stderr that is not a diagnostic batch is kept, not dropped, and the
        // ICE banner around a structured E9000 does not become the detail.
        let parsed = parse_diagnostics("ld: symbol not found\n");
        assert!(parsed.codes.is_empty());
        assert_eq!(parsed.other, vec!["ld: symbol not found".to_string()]);
        assert_eq!(parsed.detail(), "ld: symbol not found");

        let ice = compiler_verdict(&Compiled::CompileIce(
            "INTERNAL COMPILER ERROR: compiler panicked\n--- compiler stderr ---\n\
             [{\"code\":\"E9000\",\"helps\":[],\"message\":\"internal compiler error: CFG \
             verification failed\",\"notes\":[],\"severity\":\"error\",\"spans\":[],\
             \"suggestions\":[]}]\n"
                .to_string(),
        ));
        assert_eq!(
            ice,
            CompilerVerdict::InternalError(
                "internal compiler error: CFG verification failed [E9000]".to_string()
            )
        );
    }

    #[test]
    fn a_produced_binary_means_the_compiler_accepted() {
        assert_eq!(
            compiler_verdict(&ran(0, "7\n", "")),
            CompilerVerdict::Accepted
        );
        assert_eq!(
            compiler_verdict(&Compiled::Crash(11)),
            CompilerVerdict::Accepted
        );
        assert_eq!(
            compiler_verdict(&Compiled::Timeout),
            CompilerVerdict::Accepted
        );
        assert_eq!(
            compiler_verdict(&Compiled::CompileTimeout),
            CompilerVerdict::TimedOut
        );
        let rejected = compiler_verdict(&Compiled::CompileRejected {
            exit: 1,
            stderr: "[{\"code\":\"E0205\",\"helps\":[],\"message\":\"use of moved value\",\
                     \"notes\":[],\"severity\":\"error\",\"spans\":[],\"suggestions\":[]}]"
                .to_string(),
        });
        assert_eq!(rejected.codes(), ["E0205".to_string()]);
        assert!(unexpected_rejection_codes(&rejected).is_empty());
    }

    #[test]
    fn checker_and_compiler_agree_only_on_the_same_answer() {
        let accept = Verdict::Accept {
            ty: "i64".to_string(),
        };
        assert_eq!(
            checker_compiler_finding(&accept, &CompilerVerdict::Accepted),
            None
        );
        assert_eq!(
            checker_compiler_finding(
                &Verdict::Reject,
                &CompilerVerdict::Rejected {
                    exit: 1,
                    codes: vec!["E0406".to_string()],
                    detail: "linear value leaked".to_string(),
                }
            ),
            None
        );
        let accepted_a_rejection =
            checker_compiler_finding(&Verdict::Reject, &CompilerVerdict::Accepted)
                .expect("the checker rejects, the compiler accepted");
        assert!(
            accepted_a_rejection.contains("accepted"),
            "{accepted_a_rejection}"
        );

        let ice = checker_compiler_finding(
            &accept,
            &CompilerVerdict::InternalError("CFG verification failed".to_string()),
        )
        .expect("an ICE is a disagreement on either verdict");
        assert!(ice.contains("internal compiler error"), "{ice}");
        assert!(
            checker_compiler_finding(
                &Verdict::Reject,
                &CompilerVerdict::InternalError("boom".to_string())
            )
            .is_some(),
            "an ICE on a reject case is still a disagreement"
        );
    }

    #[test]
    fn an_unexpected_rejection_code_is_not_a_disagreement() {
        let rejected = CompilerVerdict::Rejected {
            exit: 1,
            codes: vec!["E0406".to_string(), "E0999".to_string()],
            detail: "two diagnostics".to_string(),
        };
        assert_eq!(checker_compiler_finding(&Verdict::Reject, &rejected), None);
        assert_eq!(unexpected_rejection_codes(&rejected), ["E0999".to_string()]);
    }

    #[test]
    fn every_pair_is_named_separately() {
        let expected = ok_expectation();
        let agreeing = ran(0, "7\n1\n", "");
        let lanes = [
            (OptimizationLevel::O1, &agreeing),
            (OptimizationLevel::O2, &agreeing),
        ];
        let good = outcome(0, "7\n1\n", "", None);
        assert!(observation_findings(&expected, Some(&good), &lanes).is_empty());

        // The oracle alone diverges from Lean: one lean<->oracle finding, plus
        // an oracle<->native finding per lane, and no lean<->native finding.
        let oracle_only = outcome(0, "1\n7\n", "", None);
        let findings = observation_findings(&expected, Some(&oracle_only), &lanes);
        let pairs: Vec<Pair> = findings.iter().map(|finding| finding.pair).collect();
        assert_eq!(
            pairs,
            vec![Pair::LeanOracle, Pair::OracleNative, Pair::OracleNative]
        );
        assert_eq!(findings[1].optimization, Some(OptimizationLevel::O1));
        assert_eq!(findings[2].optimization, Some(OptimizationLevel::O2));

        // One lane alone diverges: both the Lean and the oracle pair fire, and
        // only for that lane.
        let bad_lane = ran(0, "7\n", "");
        let mixed = [
            (OptimizationLevel::O1, &agreeing),
            (OptimizationLevel::O2, &bad_lane),
        ];
        let findings = observation_findings(&expected, Some(&good), &mixed);
        assert_eq!(
            findings
                .iter()
                .map(|finding| (finding.pair, finding.optimization))
                .collect::<Vec<_>>(),
            vec![
                (Pair::LeanNative, Some(OptimizationLevel::O2)),
                (Pair::OracleNative, Some(OptimizationLevel::O2)),
            ]
        );

        // A lane that did not compile is an accept/reject finding for that lane.
        let refused = Compiled::CompileRejected {
            exit: 1,
            stderr: "[]".to_string(),
        };
        let findings =
            observation_findings(&expected, Some(&good), &[(OptimizationLevel::O3, &refused)]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pair, Pair::CheckerCompiler);
        assert_eq!(findings[0].optimization, Some(OptimizationLevel::O3));

        // With no oracle observation the oracle pair simply does not appear.
        let findings = observation_findings(&expected, None, &mixed);
        assert_eq!(
            findings.iter().map(|f| f.pair).collect::<Vec<_>>(),
            vec![Pair::LeanNative]
        );
    }

    fn agreeing_report() -> Report {
        let case = parse_corpus(ONE_CASE).expect("valid corpus").remove(0);
        Report {
            corpus: "corpus.json".to_string(),
            cases: vec![CaseReport {
                case,
                compiler: CompilerVerdict::Accepted,
                unexpected_codes: Vec::new(),
                oracle: Some(RunObservation::Ran {
                    exit: 0,
                    stdout: b"10\n".to_vec(),
                    trap: None,
                }),
                native: vec![(
                    OptimizationLevel::O1,
                    CompilerVerdict::Accepted,
                    Some(RunObservation::Ran {
                        exit: 0,
                        stdout: b"10\n".to_vec(),
                        trap: None,
                    }),
                )],
                findings: Vec::new(),
            }],
        }
    }

    #[test]
    fn a_green_report_is_one_line_per_case_plus_the_tally() {
        let report = agreeing_report();
        let text = render_report(&report);
        assert!(text.contains("scalars"), "{text}");
        assert!(text.contains("accept(i64)"), "{text}");
        assert!(text.contains("agree"), "{text}");
        assert!(!text.contains("DISAGREE"), "{text}");
        assert!(text.contains("checker <-> compiler: 0"), "{text}");
        // A green run prints no program text.
        assert!(!text.contains("fn main"), "{text}");
    }

    #[test]
    fn a_disagreeing_report_shows_the_program_the_views_and_the_pairs() {
        let mut report = agreeing_report();
        report.cases[0].compiler = CompilerVerdict::InternalError("CFG verification".to_string());
        report.cases[0].native.clear();
        report.cases[0].findings = vec![Finding {
            pair: Pair::CheckerCompiler,
            optimization: None,
            detail: "the checker accepts, the compiler ICEd".to_string(),
        }];
        let text = render_report(&report);
        assert!(text.contains("DISAGREE (1)"), "{text}");
        assert!(text.contains("===== scalars ====="), "{text}");
        assert!(text.contains("fn main() -> i32 { 0 }"), "{text}");
        assert!(text.contains("--- the four views ---"), "{text}");
        assert!(text.contains("Lean     : the checker accepts"), "{text}");
        assert!(text.contains("compiler : reported an internal"), "{text}");
        assert!(text.contains("oracle   : exit 0"), "{text}");
        assert!(text.contains("native   : not run"), "{text}");
        assert!(
            text.contains("checker <-> compiler: the checker accepts, the compiler ICEd"),
            "{text}"
        );
        assert!(text.contains("checker <-> compiler: 1"), "{text}");
        assert!(text.contains("(0 agree, 1 disagree)"), "{text}");
    }

    #[test]
    fn an_unexpected_code_is_noted_in_the_summary_line() {
        let mut report = agreeing_report();
        report.cases[0].compiler = CompilerVerdict::Rejected {
            exit: 1,
            codes: vec!["E0406".to_string(), "E0999".to_string()],
            detail: "two diagnostics".to_string(),
        };
        report.cases[0].unexpected_codes = unexpected_rejection_codes(&report.cases[0].compiler);
        let text = render_report(&report);
        // An agreed rejection still shows its codes, and an unexpected one is
        // called out without being counted as a disagreement.
        assert!(text.contains("agree (E0406, E0999)"), "{text}");
        assert!(
            text.contains("rejected with an unexpected code: E0999"),
            "{text}"
        );
        assert!(!text.contains("DISAGREE"), "{text}");
    }

    #[test]
    fn the_json_report_carries_the_documented_fields() {
        let report = agreeing_report();
        let document: serde_json::Value =
            serde_json::from_str(&render_report_json(&report)).expect("valid JSON");
        assert_eq!(document["corpus"], "corpus.json");
        assert_eq!(document["cases_total"], 1);
        assert_eq!(document["cases_agreeing"], 1);
        assert_eq!(document["cases_disagreeing"], 0);
        assert_eq!(document["tally"]["checker-compiler"], 0);
        assert_eq!(document["tally"]["oracle-native"], 0);
        let case = &document["cases"][0];
        assert_eq!(case["name"], "scalars");
        assert_eq!(case["verdict"], "accept");
        assert_eq!(case["accepted_type"], "i64");
        assert_eq!(case["expected"]["kind"], "ok");
        assert_eq!(case["expected"]["stdout"][0], "10");
        assert_eq!(case["compiler"]["outcome"], "accepted");
        assert_eq!(case["native"][0]["optimization"], "O1");
        assert_eq!(case["agrees"], true);
        assert_eq!(case["disagreements"].as_array().expect("array").len(), 0);
    }

    #[test]
    fn arguments_and_case_selection() {
        let args = |values: &[&str]| -> Vec<String> {
            values.iter().map(|value| value.to_string()).collect()
        };
        let config = parse_args(
            &args(&[
                "--corpus",
                "c.json",
                "--case",
                "a",
                "--case=b",
                "--report-json=r.json",
            ]),
            None,
        )
        .expect("valid arguments");
        assert_eq!(config.corpus, PathBuf::from("c.json"));
        assert_eq!(config.cases, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(config.report_json, Some(PathBuf::from("r.json")));

        let from_env = parse_args(&[], Some("env.json")).expect("env fallback");
        assert_eq!(from_env.corpus, PathBuf::from("env.json"));
        assert!(parse_args(&[], None).is_err(), "no corpus is an error");
        assert!(
            parse_args(&args(&["--corpus"]), None).is_err(),
            "a missing value is an error"
        );
        assert!(
            parse_args(&args(&["--seeds", "4"]), None).is_err(),
            "an unknown argument is an error"
        );

        let cases = parse_corpus(ONE_CASE).expect("valid corpus");
        assert_eq!(select_cases(cases.clone(), &[]).expect("all").len(), 1);
        assert_eq!(
            select_cases(cases.clone(), &["scalars".to_string()])
                .expect("selected")
                .len(),
            1
        );
        let error = select_cases(cases, &["typo".to_string()]).expect_err("unknown case");
        assert!(error.contains("no case named"), "{error}");
    }
}
