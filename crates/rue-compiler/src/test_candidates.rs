//! Declared test-candidate inventory and the parse-only candidate scan
//! (ADR-0083 §1, "discovery is the import closure").
//!
//! Discovery is the import closure, so a test-only file nothing imports does
//! not exist to any request. The build system knows which files it declared —
//! its `srcs` — and hands that set to the compiler as a *candidate inventory*
//! (`--test-candidates`). A candidate is not a module: acquiring one never
//! mints a [`ModuleId`](crate::ModuleId), never joins the module closure, and
//! never creates a semantic root. It is a bounded host observation whose only
//! consumer is the parse-only scan below.
//!
//! The acquisition contract mirrors import discovery exactly (ADR-0063 §2/§7):
//! the host reads under the same read policy that governs imports and publishes
//! each candidate's bytes — or a typed [`TestCandidateOutcome::Absent`] /
//! [`TestCandidateOutcome::Unreadable`] observation — as a revisioned input
//! leaf. Candidate reads are therefore recorded like any other host read and
//! cannot become a second undeclared-read route.
//!
//! The scan itself is deliberately the smallest possible query: lex, parse,
//! count `test` items. No RIR, no semantic analysis, no reachability. Its
//! answer is `{ tests, parse_failed }` and nothing else.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::{CompileError, CompileResult, ErrorKind, ImportDiscoveryContext};

/// What the host observed when it tried to acquire one declared candidate.
///
/// Missing, unreadable, and accepted stay distinguishable typed observations
/// for the same reason import observations do (ADR-0063 §2.1): any transition
/// between them must invalidate the dependent scan rather than be silently
/// indistinguishable from "no tests here".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TestCandidateOutcome {
    /// The candidate was read and decoded as UTF-8 source text.
    Present(Arc<str>),
    /// The candidate does not exist. Absence is silent at the warning surface:
    /// a build system may legitimately declare a file a sibling target owns.
    Absent,
    /// The candidate exists but could not be read or decoded, with the host's
    /// reason retained for the diagnostic.
    Unreadable(Arc<str>),
}

/// One declared candidate: its project-root-relative logical path plus the
/// host's typed acquisition outcome.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TestCandidate {
    identity: TestCandidateIdentity,
    outcome: TestCandidateOutcome,
}

impl TestCandidate {
    pub fn path(&self) -> &str {
        self.identity.path()
    }

    pub fn outcome(&self) -> &TestCandidateOutcome {
        &self.outcome
    }

    pub(crate) fn identity(&self) -> &TestCandidateIdentity {
        &self.identity
    }
}

/// Stable identity of one candidate observation.
///
/// The read regime travels with the path for the same reason it travels with
/// an [`ImportDiscoveryRequest`](crate::ImportDiscoveryRequest): a candidate
/// read observed under one project root, standard-library root, and read policy
/// is not the same observation as a read of the same spelling under another.
/// A regime change therefore mints new leaf identities instead of re-stamping
/// the old ones.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TestCandidateIdentity {
    project_root: Arc<str>,
    std_root: Arc<str>,
    read_policy_revision: Arc<str>,
    path: Arc<str>,
}

impl TestCandidateIdentity {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// The revisioned input key for this candidate, in the same length-prefixed
    /// encoding `ImportDiscoveryRequest::runtime_input_key` uses so distinct
    /// field boundaries cannot be forged by concatenation.
    pub(crate) fn runtime_input_key(&self) -> Arc<str> {
        use std::fmt::Write as _;

        fn field(output: &mut String, value: &str) {
            write!(output, "{}:{value}", value.len()).expect("string writes cannot fail");
        }

        let mut key = String::from("v1:");
        field(&mut key, &self.project_root);
        field(&mut key, &self.std_root);
        field(&mut key, &self.read_policy_revision);
        field(&mut key, &self.path);
        key.into()
    }
}

/// The declared candidate set for one request, together with the read regime it
/// was acquired under.
///
/// Construction takes the request's [`ImportDiscoveryContext`] so the inventory
/// cannot be assembled against a different read policy than the compile it is
/// reported against.
#[derive(Debug, Clone)]
pub struct TestCandidateInventory {
    project_root: Arc<str>,
    std_root: Arc<str>,
    read_policy_revision: Arc<str>,
    candidates: Vec<TestCandidate>,
}

impl TestCandidateInventory {
    /// Begin an inventory for the read regime `context` describes.
    pub fn new(context: &ImportDiscoveryContext) -> Self {
        Self {
            project_root: Arc::from(context.project_root()),
            std_root: Arc::from(context.std_root().unwrap_or("")),
            read_policy_revision: Arc::from(context.read_policy_revision()),
            candidates: Vec::new(),
        }
    }

    /// Record one declared candidate's acquisition outcome.
    ///
    /// `path` is the project-root-relative spelling the build system declared.
    /// It is normalized lexically to the logical path a module of that file
    /// would carry, so closure membership can be decided by comparing strings
    /// against published module identities.
    ///
    /// Declaring the same candidate twice keeps the first outcome: the
    /// inventory is a set, and a repeated `srcs` entry is not two observations.
    pub fn declare(
        &mut self,
        path: &str,
        outcome: TestCandidateOutcome,
    ) -> CompileResult<&TestCandidate> {
        let normalized = self.normalize_candidate_path(path)?;
        let identity = TestCandidateIdentity {
            project_root: self.project_root.clone(),
            std_root: self.std_root.clone(),
            read_policy_revision: self.read_policy_revision.clone(),
            path: normalized,
        };
        let index = match self
            .candidates
            .binary_search_by(|candidate| candidate.identity.cmp(&identity))
        {
            Ok(index) => index,
            Err(index) => {
                self.candidates
                    .insert(index, TestCandidate { identity, outcome });
                index
            }
        };
        Ok(&self.candidates[index])
    }

    /// The declared candidates, ordered by logical path.
    pub fn candidates(&self) -> &[TestCandidate] {
        &self.candidates
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub(crate) fn project_root(&self) -> &str {
        &self.project_root
    }

    pub(crate) fn std_root(&self) -> Option<&str> {
        (!self.std_root.is_empty()).then_some(&self.std_root)
    }

    pub(crate) fn read_policy_revision(&self) -> &str {
        &self.read_policy_revision
    }

    /// Compatibility token for the candidate input namespace.
    ///
    /// Candidate leaves are published in their own revision namespace, keyed by
    /// the same regime fields import discovery uses under a distinct domain tag.
    /// Keeping the namespaces separate is what lets a candidate scan be
    /// requested without perturbing the import namespace's certificate lineage:
    /// candidate acquisition is an addition to the request, not a new
    /// observation of the program's sources.
    pub(crate) fn regime_token(&self) -> u64 {
        use sha2::{Digest, Sha256};

        let mut digest = Sha256::new();
        let mut field = |bytes: &[u8]| {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        field(b"rue-test-candidate-regime-v1");
        field(self.project_root.as_bytes());
        field(self.std_root.as_bytes());
        field(self.read_policy_revision.as_bytes());
        let bytes = digest.finalize();
        u64::from_le_bytes(bytes[..8].try_into().expect("sha256 yields 32 bytes"))
    }

    /// Reduce a declared spelling to the logical path a module of that file
    /// would carry, rejecting anything that cannot be a user module.
    ///
    /// Standard-library files are never candidates: the inventory exists to
    /// find *unwired project files*, and std is reached through the toolchain
    /// facade rather than through a build target's `srcs`.
    fn normalize_candidate_path(&self, path: &str) -> CompileResult<Arc<str>> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err(invalid_candidate("a test candidate path cannot be empty"));
        }
        let project_root = Path::new(self.project_root.as_ref());
        let requested = Path::new(trimmed);
        let relative = if requested.is_absolute() {
            requested.strip_prefix(project_root).map_err(|_| {
                invalid_candidate(format!(
                    "test candidate '{trimmed}' is outside the project root '{}'",
                    self.project_root
                ))
            })?
        } else {
            requested
        };
        let normalized = normalize_relative(relative).ok_or_else(|| {
            invalid_candidate(format!(
                "test candidate '{trimmed}' escapes the project root '{}'",
                self.project_root
            ))
        })?;
        if let Some(std_root) = self.std_root() {
            let absolute = project_root.join(&normalized);
            if absolute.starts_with(Path::new(std_root)) {
                return Err(invalid_candidate(format!(
                    "test candidate '{trimmed}' resolves inside the standard-library root '{std_root}'; \
                     standard-library files are never test candidates"
                )));
            }
        }
        let Some(normalized) = normalized.to_str() else {
            return Err(invalid_candidate(format!(
                "test candidate '{trimmed}' is not valid UTF-8"
            )));
        };
        Ok(Arc::from(normalized))
    }
}

/// One declared candidate the request's module closure does not contain, and
/// which is worth telling the user about (ADR-0083 §1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnimportedTestFile {
    /// The candidate's project-root-relative logical path.
    pub path: String,
    /// Test declarations counted in the candidate.
    pub tests: u32,
    /// The candidate could not be read or parsed, so `tests` is not a count of
    /// anything and the report says only that the answer is unknown.
    pub parse_failed: bool,
}

/// The `compiler.test-candidate-scan` answer for one candidate revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TestCandidateScan {
    pub(crate) tests: u32,
    pub(crate) parse_failed: bool,
}

/// Count the test items one candidate's bytes declare.
///
/// Lex and parse only. A candidate that does not lex or parse reports
/// `parse_failed` rather than a count: the file may or may not declare tests,
/// and guessing either way would be worse than saying so.
pub(crate) fn scan_candidate_outcome(
    path: &str,
    outcome: &TestCandidateOutcome,
) -> TestCandidateScan {
    let source = match outcome {
        TestCandidateOutcome::Present(source) => source,
        // Absence is not a failure; there is nothing to report about a file the
        // host proved is not there.
        TestCandidateOutcome::Absent => return TestCandidateScan::default(),
        // An unreadable candidate is exactly as opaque as an unparsable one.
        TestCandidateOutcome::Unreadable(_) => {
            return TestCandidateScan {
                tests: 0,
                parse_failed: true,
            };
        }
    };
    let view = crate::queries::SourceView::new(path, source, rue_span::FileId::DEFAULT);
    let outcome = crate::syntax::parse_file(view, lasso::ThreadedRodeo::new());
    let Ok(ast) = outcome.result else {
        return TestCandidateScan {
            tests: 0,
            parse_failed: true,
        };
    };
    let tests = ast
        .items
        .iter()
        .filter(|item| matches!(item, rue_parser::Item::Test(_)))
        .count();
    TestCandidateScan {
        tests: u32::try_from(tests).unwrap_or(u32::MAX),
        parse_failed: false,
    }
}

/// Lexically reduce a relative path, refusing one that escapes its anchor.
///
/// This is a pure string reduction, deliberately: the candidate may not exist,
/// and probing the filesystem to normalize it would be a read outside the
/// declared policy.
fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!normalized.as_os_str().is_empty()).then_some(normalized)
}

fn invalid_candidate(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(std_root: Option<&str>) -> ImportDiscoveryContext {
        ImportDiscoveryContext::new(1, "/rue-fixture", std_root, "test-candidate-policy").unwrap()
    }

    fn inventory(std_root: Option<&str>) -> TestCandidateInventory {
        TestCandidateInventory::new(&context(std_root))
    }

    fn present(source: &str) -> TestCandidateOutcome {
        TestCandidateOutcome::Present(Arc::from(source))
    }

    #[test]
    fn declared_paths_reduce_to_the_logical_path_a_module_would_carry() {
        let mut candidates = inventory(None);
        candidates
            .declare("./app/./parser_tests.rue", present(""))
            .unwrap();
        candidates
            .declare("/rue-fixture/app/other/../main.rue", present(""))
            .unwrap();
        assert_eq!(
            candidates
                .candidates()
                .iter()
                .map(TestCandidate::path)
                .collect::<Vec<_>>(),
            ["app/main.rue", "app/parser_tests.rue"]
        );
    }

    /// A repeated `srcs` entry is one declared candidate, not two observations.
    #[test]
    fn declaring_the_same_candidate_twice_keeps_one_entry() {
        let mut candidates = inventory(None);
        candidates.declare("app/a.rue", present("")).unwrap();
        candidates
            .declare("./app/a.rue", present("fn f() {}"))
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates.candidates()[0].outcome(), &present(""));
    }

    /// Standard-library files are reached through the toolchain facade, never
    /// through a build target's `srcs`. Accepting one as a candidate would make
    /// the inventory report on files no project owns.
    #[test]
    fn standard_library_paths_are_refused() {
        let mut candidates = inventory(Some("/rue-fixture/std"));
        let error = candidates
            .declare("std/option.rue", present(""))
            .unwrap_err();
        assert!(
            error.to_string().contains("standard-library root"),
            "{error}"
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn paths_outside_the_project_root_are_refused() {
        let mut candidates = inventory(None);
        assert!(
            candidates
                .declare("../elsewhere/a.rue", present(""))
                .is_err()
        );
        assert!(
            candidates
                .declare("/somewhere/else/a.rue", present(""))
                .is_err()
        );
        assert!(candidates.declare("   ", present("")).is_err());
        assert!(candidates.is_empty());
    }

    /// A candidate observed under one read regime is not the same observation
    /// as the same spelling under another, so its input key differs.
    #[test]
    fn the_read_regime_travels_with_the_candidate_key() {
        let mut open = inventory(None);
        let mut sandboxed = TestCandidateInventory::new(
            &ImportDiscoveryContext::new(1, "/rue-fixture", None, "manifest-abc").unwrap(),
        );
        open.declare("app/a.rue", present("")).unwrap();
        sandboxed.declare("app/a.rue", present("")).unwrap();
        assert_ne!(
            open.candidates()[0].identity().runtime_input_key(),
            sandboxed.candidates()[0].identity().runtime_input_key()
        );
        assert_ne!(open.regime_token(), sandboxed.regime_token());
    }

    #[test]
    fn the_scan_counts_test_items_and_nothing_else() {
        let scan = scan_candidate_outcome(
            "app/parser_tests.rue",
            &present(
                "fn helper() -> i32 { 1 }\n\
                 test \"first\" { }\n\
                 test \"second\" { }\n",
            ),
        );
        assert_eq!(
            scan,
            TestCandidateScan {
                tests: 2,
                parse_failed: false
            }
        );
    }

    #[test]
    fn a_candidate_without_tests_scans_to_zero() {
        let scan = scan_candidate_outcome("app/plain.rue", &present("fn main() -> i32 { 0 }\n"));
        assert_eq!(
            scan,
            TestCandidateScan {
                tests: 0,
                parse_failed: false
            }
        );
    }

    /// An unparsable candidate reports that the answer is unknown rather than
    /// guessing a count in either direction.
    #[test]
    fn an_unparsable_candidate_reports_parse_failure() {
        let scan = scan_candidate_outcome("app/broken.rue", &present("fn main( -> { \n"));
        assert!(scan.parse_failed);
        assert_eq!(scan.tests, 0);
    }

    /// An unreadable candidate is exactly as opaque as an unparsable one; an
    /// absent one is not a failure at all.
    #[test]
    fn absent_and_unreadable_outcomes_are_distinguishable() {
        assert_eq!(
            scan_candidate_outcome("app/gone.rue", &TestCandidateOutcome::Absent),
            TestCandidateScan {
                tests: 0,
                parse_failed: false
            }
        );
        assert_eq!(
            scan_candidate_outcome(
                "app/denied.rue",
                &TestCandidateOutcome::Unreadable(Arc::from("permission denied"))
            ),
            TestCandidateScan {
                tests: 0,
                parse_failed: true
            }
        );
    }
}
