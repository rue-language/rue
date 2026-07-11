//! Trap classification at Rue's two text boundaries.
//!
//! The oracle emits [`TrapKind`] directly, while native executions expose the
//! runtime's stderr. Only a complete, stable runtime diagnostic proves a native
//! trap kind: loose substring matching could mistake `@panic("integer
//! overflow")` or unrelated prose for the overflow handler. Corpus expectations
//! use a separate exact fragment vocabulary so their looser source contract can
//! never weaken native classification.

use rue_oracle::TrapKind;

/// How precisely a corpus case declares its expected runtime trap.
///
/// Every declared fragment must name the same modeled category. Unknown or
/// contradictory fragments are an explicit coverage gap, never evidence that
/// an exit-code-only comparison is sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrapExpectation {
    Undeclared,
    Modeled(TrapKind),
    Unmodeled,
}

pub(crate) fn native_runtime_trap_kind(stderr: &str) -> Option<TrapKind> {
    match stderr {
        "error: integer cast overflow\n" => Some(TrapKind::IntegerCastOverflow),
        "error: integer overflow\n" => Some(TrapKind::ArithmeticOverflow),
        "error: division by zero\n" => Some(TrapKind::DivisionByZero),
        "error: index out of bounds\n" => Some(TrapKind::IndexOutOfBounds),
        "error: invalid UTF-8\n" => Some(TrapKind::InvalidUtf8),
        "panic\n" => Some(TrapKind::UserPanic),
        "assertion failed\n" => Some(TrapKind::AssertionFailure),
        stderr if stderr.starts_with("panic: ") && stderr.ends_with('\n') => {
            Some(TrapKind::UserPanic)
        }
        _ => None,
    }
}

fn expected_trap_kind(fragment: &str) -> Option<TrapKind> {
    match fragment {
        "integer overflow" => Some(TrapKind::ArithmeticOverflow),
        "division by zero" => Some(TrapKind::DivisionByZero),
        "integer cast overflow" => Some(TrapKind::IntegerCastOverflow),
        "index out of bounds" => Some(TrapKind::IndexOutOfBounds),
        "invalid UTF-8" => Some(TrapKind::InvalidUtf8),
        _ => None,
    }
}

/// CLI cases also pin the two abort intrinsics' exact runtime prefixes. Keep
/// this vocabulary separate from rue-spec so adding the CLI registry does not
/// silently reclassify that corpus's existing observation coverage.
fn cli_expected_trap_kind(fragment: &str) -> Option<TrapKind> {
    expected_trap_kind(fragment).or_else(|| match fragment {
        "panic" => Some(TrapKind::UserPanic),
        "assertion failed" => Some(TrapKind::AssertionFailure),
        fragment if fragment.starts_with("panic: ") => Some(TrapKind::UserPanic),
        _ => None,
    })
}

fn classify_expectation<'a>(
    fragments: impl IntoIterator<Item = &'a str>,
    classify: fn(&str) -> Option<TrapKind>,
) -> TrapExpectation {
    let mut expected = None;

    for fragment in fragments {
        let Some(kind) = classify(fragment) else {
            return TrapExpectation::Unmodeled;
        };
        if expected.is_some_and(|prior| prior != kind) {
            return TrapExpectation::Unmodeled;
        }
        expected = Some(kind);
    }

    expected.map_or(TrapExpectation::Undeclared, TrapExpectation::Modeled)
}

pub(crate) fn trap_expectation<'a>(
    fragments: impl IntoIterator<Item = &'a str>,
) -> TrapExpectation {
    classify_expectation(fragments, expected_trap_kind)
}

pub(crate) fn cli_trap_expectation<'a>(
    fragments: impl IntoIterator<Item = &'a str>,
) -> TrapExpectation {
    classify_expectation(fragments, cli_expected_trap_kind)
}

/// A deliberately narrow trap declaration vocabulary for rue-spec's
/// successful-run `stderr_contains` field. The spec runner does not treat this
/// field as `runtime_error`, so keep it separate from both that vocabulary and
/// the CLI registry. Only the two deterministic abort intrinsic spellings are
/// declarations; arbitrary stderr text must not hide an unrelated typed trap.
pub(crate) fn spec_stderr_trap_kind(fragment: &str) -> Option<TrapKind> {
    match fragment {
        "panic" => Some(TrapKind::UserPanic),
        "assertion failed" => Some(TrapKind::AssertionFailure),
        fragment if fragment.starts_with("panic: ") => Some(TrapKind::UserPanic),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_stable_runtime_trap_spelling() {
        assert_eq!(
            native_runtime_trap_kind("error: integer cast overflow\n"),
            Some(TrapKind::IntegerCastOverflow)
        );
        assert_eq!(
            native_runtime_trap_kind("error: integer overflow\n"),
            Some(TrapKind::ArithmeticOverflow)
        );
        assert_eq!(
            native_runtime_trap_kind("error: division by zero\n"),
            Some(TrapKind::DivisionByZero)
        );
        assert_eq!(
            native_runtime_trap_kind("error: index out of bounds\n"),
            Some(TrapKind::IndexOutOfBounds)
        );
        assert_eq!(
            native_runtime_trap_kind("error: invalid UTF-8\n"),
            Some(TrapKind::InvalidUtf8)
        );
        assert_eq!(
            native_runtime_trap_kind("panic\n"),
            Some(TrapKind::UserPanic)
        );
        assert_eq!(
            native_runtime_trap_kind("panic: boom\n"),
            Some(TrapKind::UserPanic)
        );
        assert_eq!(
            native_runtime_trap_kind("assertion failed\n"),
            Some(TrapKind::AssertionFailure)
        );
    }

    #[test]
    fn user_panics_are_distinct_from_builtin_traps_and_extra_output_is_not_proof() {
        assert_eq!(native_runtime_trap_kind("integer overflow"), None);
        assert_eq!(
            native_runtime_trap_kind("panic: index out of bounds\n"),
            Some(TrapKind::UserPanic)
        );
        assert_eq!(
            native_runtime_trap_kind("error: integer cast overflow\nerror: integer overflow\n"),
            None
        );
        assert_eq!(
            native_runtime_trap_kind("note\nerror: index out of bounds\n"),
            None
        );
        assert_eq!(
            native_runtime_trap_kind("error: invalid UTF-8\nnote\n"),
            None
        );
    }

    #[test]
    fn corpus_expectations_use_a_separate_exact_vocabulary() {
        let cases = [
            ("integer overflow", TrapKind::ArithmeticOverflow),
            ("division by zero", TrapKind::DivisionByZero),
            ("integer cast overflow", TrapKind::IntegerCastOverflow),
            ("index out of bounds", TrapKind::IndexOutOfBounds),
            ("invalid UTF-8", TrapKind::InvalidUtf8),
        ];
        for (fragment, kind) in cases {
            assert_eq!(trap_expectation([fragment]), TrapExpectation::Modeled(kind));
        }

        assert_eq!(
            trap_expectation(std::iter::empty()),
            TrapExpectation::Undeclared
        );
        assert_eq!(
            trap_expectation(["division by zero", "division by zero"]),
            TrapExpectation::Modeled(TrapKind::DivisionByZero)
        );

        for fragment in ["panic", "panic: boom", "panic: integer overflow"] {
            assert_eq!(
                cli_trap_expectation([fragment]),
                TrapExpectation::Modeled(TrapKind::UserPanic)
            );
            assert_eq!(trap_expectation([fragment]), TrapExpectation::Unmodeled);
        }
        assert_eq!(
            cli_trap_expectation(["assertion failed"]),
            TrapExpectation::Modeled(TrapKind::AssertionFailure)
        );
        assert_eq!(
            trap_expectation(["assertion failed"]),
            TrapExpectation::Unmodeled
        );

        for fragment in ["panic", "panic: boom"] {
            assert_eq!(spec_stderr_trap_kind(fragment), Some(TrapKind::UserPanic));
        }
        assert_eq!(
            spec_stderr_trap_kind("assertion failed"),
            Some(TrapKind::AssertionFailure)
        );
        for fragment in ["integer overflow", "arbitrary stderr", "boom"] {
            assert_eq!(
                spec_stderr_trap_kind(fragment),
                None,
                "generic spec stderr must not declare a trap category"
            );
        }
    }

    #[test]
    fn unknown_or_contradictory_expectations_are_unmodeled() {
        for fragments in [
            vec!["panic: integer overflow"],
            vec!["integer overflow and division by zero"],
            vec!["integer overflow", "panic detail"],
            vec!["integer overflow", "division by zero"],
        ] {
            assert_eq!(trap_expectation(fragments), TrapExpectation::Unmodeled);
        }
    }
}
