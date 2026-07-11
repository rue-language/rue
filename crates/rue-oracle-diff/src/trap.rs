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

pub(crate) fn trap_expectation<'a>(
    fragments: impl IntoIterator<Item = &'a str>,
) -> TrapExpectation {
    let mut expected = None;

    for fragment in fragments {
        let Some(kind) = expected_trap_kind(fragment) else {
            return TrapExpectation::Unmodeled;
        };
        if expected.is_some_and(|prior| prior != kind) {
            return TrapExpectation::Unmodeled;
        }
        expected = Some(kind);
    }

    expected.map_or(TrapExpectation::Undeclared, TrapExpectation::Modeled)
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
    }

    #[test]
    fn expectation_fragments_user_panics_and_extra_output_are_not_proof() {
        assert_eq!(native_runtime_trap_kind("integer overflow"), None);
        assert_eq!(native_runtime_trap_kind("panic: integer overflow\n"), None);
        assert_eq!(
            native_runtime_trap_kind("panic: index out of bounds\n"),
            None
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
