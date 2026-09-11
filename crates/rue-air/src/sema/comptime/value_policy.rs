//! The one value-domain policy every `ComptimeHost` obeys (RUE-1968).
//!
//! Two hosts implement [`ComptimeHost`](super::ComptimeHost): ordinary body
//! evaluation in sema (`const` initializers reached from a body, `comptime {}`
//! blocks, comptime parameters) and durable declaration-time evaluation in
//! `rue-compiler`. They answer different questions about *their own* value
//! representations -- that is what the host boundary is for -- but the
//! questions below are not about a representation. They are language
//! decisions, and while each host answered them in its own copy the copies
//! drifted: the same fragment reported one diagnostic in `const` position and
//! a different one in `comptime {}` position.
//!
//! Every decision that depends only on already-reduced values belongs here, so
//! that a host cannot answer it a second way. Hosts still own how a decision
//! is *reported*, because a declaration-time failure and a body diagnostic are
//! carried by different error types.

use super::{ComptimeMatchPattern, ComptimeValue};
use crate::integer_semantics::CheckedIntegerResult;

/// The reason text for a comptime-known `match` whose reached arms all
/// declined. By 4.14:19 the arm is selected from an exhaustive pattern set
/// (4.7:9), so reaching this point means the pattern set was not exhaustive --
/// an error in `const` position and in `comptime {}` position alike.
pub const COMPTIME_MATCH_NO_SELECTED_ARM: &str = "comptime match has no selected arm";

/// What one decoded arm pattern decides about an already-reduced scrutinee.
///
/// The scalar half of the decision is the same in every value domain and is
/// made by [`comptime_scalar_pattern_decision`]. Only [`Self::HostPath`] is
/// deferred, because an enum-variant path is the one pattern whose meaning
/// depends on which values a host's domain can represent at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimePatternDecision {
    /// The pattern definitely matches, or definitely does not.
    Decided(bool),
    /// The scrutinee does not inhabit the pattern's scalar domain, so this
    /// `match` is not decidable at compile time here.
    Undecidable,
    /// An enum-variant path pattern: the host's value domain decides.
    HostPath,
}

/// Decide a decoded arm pattern against a reduced scrutinee (spec 4.14:19).
///
/// A wildcard always matches. A boolean or integer pattern matches when the
/// scrutinee carries that scalar and the values are equal; a scrutinee of any
/// other kind makes the `match` undecidable rather than a definite non-match,
/// because a value the pattern cannot be compared against carries no evidence
/// either way.
pub fn comptime_scalar_pattern_decision<N, V: ComptimeValue>(
    pattern: &ComptimeMatchPattern<N>,
    value: &V,
) -> ComptimePatternDecision {
    match pattern {
        ComptimeMatchPattern::Wildcard => ComptimePatternDecision::Decided(true),
        ComptimeMatchPattern::Bool(expected) => value
            .as_boolean()
            .map_or(ComptimePatternDecision::Undecidable, |actual| {
                ComptimePatternDecision::Decided(actual == *expected)
            }),
        ComptimeMatchPattern::Integer(expected) => value
            .as_integer()
            .map_or(ComptimePatternDecision::Undecidable, |actual| {
                ComptimePatternDecision::Decided(actual == *expected)
            }),
        ComptimeMatchPattern::Path { .. } => ComptimePatternDecision::HostPath,
        // A struct value is never a comptime scalar (RUE-2175).
        ComptimeMatchPattern::Struct => ComptimePatternDecision::Undecidable,
    }
}

/// The one spelling of an arithmetic operation in comptime diagnostics.
///
/// The engine names operations by their source operator, plus `negation` for
/// unary minus, which has no distinct operator spelling of its own.
pub fn comptime_arithmetic_operation_name(operation: &str) -> &str {
    match operation {
        "+" => "addition",
        "-" => "subtraction",
        "*" => "multiplication",
        "/" => "division",
        "%" => "remainder",
        "<<" => "left shift",
        ">>" => "right shift",
        other => other,
    }
}

/// The one reason text for an integer operation whose result does not fit its
/// type (spec 4.14:4: the operation would trap at run time, so evaluating it
/// at compile time is an error).
pub fn comptime_arithmetic_overflow_reason(
    operation: &str,
    type_name: &str,
    result: CheckedIntegerResult,
) -> String {
    let operation = comptime_arithmetic_operation_name(operation);
    let detail = result.raw().map_or_else(
        || format!("the result does not fit in {type_name}"),
        |value| format!("the result {value} does not fit in {type_name}"),
    );
    format!(
        "integer overflow evaluating {operation} at type {type_name}: {detail} \
         (this operation would panic at runtime)"
    )
}

/// The value of an integer operation the host could not give a semantic type.
///
/// An untyped result is not a language value: nothing has told the evaluator
/// which type's arithmetic to check, so there is no width at which to report
/// an overflow. Results inside the `i64` window are still exact and usable
/// (array lengths and comptime arguments are folded from them); anything wider
/// is simply not evaluable, and the expression stays runtime-dependent instead
/// of acquiring an invented width.
pub fn comptime_untyped_integer_result(result: CheckedIntegerResult) -> Option<i128> {
    result
        .raw()
        .filter(|value| *value >= i128::from(i64::MIN) && *value <= i128::from(i64::MAX))
}
