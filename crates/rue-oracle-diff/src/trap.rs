//! Native-runtime trap classification at the text boundary.
//!
//! The oracle emits [`TrapKind`] directly, while native executions expose the
//! runtime's stderr. Only a complete, stable runtime diagnostic proves a native
//! trap kind: loose substring matching could mistake `@panic("integer
//! overflow")` or unrelated prose for the overflow handler.

use rue_oracle::TrapKind;

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
}
