//! Direct tests for the production provider body path.
//!
//! These tests drive [`analyze_provider_ordinary_body`] — the exact entry point
//! the compiler's body transaction uses — through the in-memory durable fact
//! source in [`super::provider_fixture`], so body analysis is exercised against
//! the production `ProviderBodyHost`/`OrdinaryBodyEngine` seam. A structural
//! guard at the bottom of this file keeps the fixture module and this file off
//! the retired whole-program `Sema` drivers by name.

use rue_error::ErrorKind;
use std::sync::Arc;

use super::comptime::MAX_COMPTIME_CALL_DEPTH;
use super::comptime_eval::{
    checked_const_index_test_stats, reset_checked_const_index_test_stats,
    with_cancellation_on_checked_const_index_hit,
};
use super::ordinary_engine::{
    comptime_reduction_test_keys, comptime_reduction_test_stats,
    reset_comptime_reduction_test_stats,
};
use super::provider_fixture::{
    FixtureKey, MethodShape, ProviderFixture, StructShape, comptime_type_param,
    comptime_value_param, error_source_slice, mode_param, value_param,
    with_fixture_cancellation_after, with_fixture_durable_integer,
};
use super::{
    ComptimeCallKey, ComptimeCallMemoLookup, ComptimeCompletedCallMemo, ComptimeMemoizedOutcome,
};
use crate::{
    ConstValue, SemanticDefinitionToken, SemanticImportConstValue, SemanticImportNominalKind,
    SemanticImportType, SemanticModuleToken, StableDefinitionKind, StableProducerId, Type,
};
use rue_rir::SymbolHandle;
use rue_target::Target;

// Migrated from `tests::test_analyze_addition`: ordinary expression typing on
// the production provider path.
#[test]
fn provider_body_types_integer_addition() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { 1 + 2 }", "main")
        .expect("addition body analyzes");

    let air = &body.function.air;
    assert_eq!(air.return_type(), crate::types::Type::I32);
    // Const(1) + Const(2) + Add + Ret = 4 instructions
    assert_eq!(air.len(), 4);
    let add = air.get(crate::AirRef::from_raw(2));
    assert!(matches!(add.data, crate::AirInstData::Add(_, _)));
    assert_eq!(add.ty, crate::types::Type::I32);
}

#[test]
fn checked_const_index_is_shared_between_inference_and_ownership() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let source = "fn main() { let mut values: [i32; 2] = [10, 20]; values[1 + 0] += 2; }";

    let first = fixture
        .analyze(source, "main")
        .expect("constant array index analyzes");
    let retained = fixture
        .analyze(source, "main")
        .expect("retained constant array index analyzes");

    assert_eq!(first.work.checked_const_index_evaluations, 1);
    assert_eq!(first.work.checked_const_index_cache_hits, 1);
    assert_eq!(first.work.checked_const_index_candidate_comparisons, 1);
    assert_eq!(first.work.checked_const_index_comparison_nodes, 3);
    assert_eq!(retained.work.checked_const_index_evaluations, 1);
    assert_eq!(retained.work.checked_const_index_cache_hits, 1);
    assert_eq!(first.export.body, retained.export.body);
}

#[test]
fn runtime_index_failure_is_not_cached_between_probes() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "lookup",
        vec![value_param("index", SemanticImportType::I32)],
        SemanticImportType::Unit,
    );
    let body = fixture
        .analyze(
            "fn lookup(index: i32) { let mut values: [i32; 2] = [10, 20]; values[index] += 2; }",
            "lookup",
        )
        .expect("runtime array index analyzes");

    assert_eq!(body.work.checked_const_index_evaluations, 2);
    assert_eq!(body.work.checked_const_index_cache_hits, 0);
    assert_eq!(body.work.checked_const_index_candidate_comparisons, 0);
    assert_eq!(body.work.checked_const_index_comparison_nodes, 0);
}

#[test]
fn checked_const_index_cache_preserves_runtime_shadow_scope() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_const(
        "index",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(1),
    );
    fixture.declare_function("update", Vec::new(), SemanticImportType::Unit);
    let source = "fn update() { let mut values: [i32; 2] = [10, 20]; values[index] += 2; { let index: i32 = 0; values[index] += 2; } }";
    let mut occurrences = source.match_indices("index").map(|(start, _)| start);
    let outer = occurrences.next().expect("outer index spelling");
    let binding = occurrences.next().expect("shadow binding spelling");
    let inner = occurrences.next().expect("inner index spelling");
    assert!(binding < inner);
    let outer_span = rue_span::Span::new(outer as u32, (outer + 5) as u32);
    let inner_span = rue_span::Span::new(inner as u32, (inner + 5) as u32);
    let body = fixture
        .analyze_span_edited(source, "update", |_, span| {
            if span == inner_span { outer_span } else { span }
        })
        .expect("runtime shadow preserves index classification");

    // The outer comptime parameter evaluates once and is shared. The inner
    // runtime local shadows that same name and both probes must retry as
    // non-constant rather than hitting the outer result.
    assert_eq!(body.work.checked_const_index_evaluations, 3);
    assert_eq!(body.work.checked_const_index_cache_hits, 1);
    assert_eq!(body.work.checked_const_index_candidate_comparisons, 3);
}

#[test]
fn checked_const_index_cache_preserves_type_alias_bind_shadow_and_restore() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let source = "fn main() { let mut values: [i32; 16] = [0; 16]; let Alias = i32; values[@size_of(Alias)] += 1; { let Alias = i64; values[@size_of(Alias)] += 1; } values[@size_of(Alias)] += 1; }";
    let occurrences = source
        .match_indices("@size_of(Alias)")
        .map(|(start, spelling)| rue_span::Span::new(start as u32, (start + spelling.len()) as u32))
        .collect::<Vec<_>>();
    let [first, shadowed, restored] = occurrences.as_slice() else {
        panic!("fixture has three type-alias index occurrences");
    };
    let body = fixture
        .analyze_span_edited(source, "main", |_, span| {
            if span == *shadowed || span == *restored {
                *first
            } else {
                span
            }
        })
        .expect("type alias scope is restored after the nested block");

    // All three roots counterfeit the same occurrence coordinate. These
    // type-intrinsic probes do not produce cache-eligible integers at this
    // checked-index phase, so bind, shadow, and restored probes all retry and
    // preserve their source-order semantics rather than publishing a result.
    assert_eq!(body.work.checked_const_index_evaluations, 6);
    assert_eq!(body.work.checked_const_index_cache_hits, 0);
    assert_eq!(body.work.checked_const_index_candidate_comparisons, 0);
}

#[test]
fn counterfeit_same_span_index_expression_cannot_hit() {
    let source =
        "fn main() { let mut values: [i32; 2] = [10, 20]; values[1] += 2; values[1 / 0] += 2; }";
    let first_index = source.find("values[1]").expect("first index") + "values[".len();
    let failed_index = source.find("1 / 0").expect("failing index");
    let first_index_span = rue_span::Span::new(first_index as u32, (first_index + 1) as u32);
    let failed_index_span = rue_span::Span::new(failed_index as u32, (failed_index + 5) as u32);
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let mut first_failure = None;
    for _attempt in 0..2 {
        reset_checked_const_index_test_stats();
        let error = fixture
            .analyze_span_edited(source, "main", |_, span| {
                if span == failed_index_span {
                    first_index_span
                } else {
                    span
                }
            })
            .map(|_| ())
            .expect_err("the same-span trapping expression must be evaluated and rejected");
        let stats = checked_const_index_test_stats();
        assert_eq!(stats.evaluations, 2);
        // The first candidate hit belongs to the first successful compound
        // index's duplicate. The distinct trapping root reaches a second
        // comparison, is rejected, evaluated, and never publishes a value.
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.candidate_hits, 1);
        assert_eq!(stats.candidate_rejections, 1);
        assert_eq!(stats.candidate_comparisons, 2);
        assert_eq!(stats.comparison_nodes, 2);
        assert!(
            matches!(
                &error.kind,
                ErrorKind::ComptimeEvaluationFailed { reason }
                    if reason == "division by zero (this operation would panic at runtime)"
            ),
            "unexpected counterfeit diagnostic: {error:?}"
        );
        assert_eq!(error_source_slice(source, &error), "1");
        let span = error.span();
        if let Some((first_kind, first_span)) = &first_failure {
            assert_eq!(
                first_kind, &error.kind,
                "malformed class must retry exactly"
            );
            assert_eq!(*first_span, span, "malformed span must retry exactly");
        } else {
            first_failure = Some((error.kind, span));
        }
    }
}

#[test]
fn distinct_successful_same_span_expression_cannot_reuse_candidate() {
    let source = "fn main() { let mut values: [i32; 2] = [0, 0]; values[1] += 1; values[2] += 1; }";
    let one = source.find("values[1]").expect("first index") + "values[".len();
    let two = source.find("values[2]").expect("second index") + "values[".len();
    let one_span = rue_span::Span::new(one as u32, (one + 1) as u32);
    let two_span = rue_span::Span::new(two as u32, (two + 1) as u32);
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    reset_checked_const_index_test_stats();
    let error = fixture
        .analyze_span_edited(source, "main", |_, span| {
            if span == two_span { one_span } else { span }
        })
        .map(|_| ())
        .expect_err("the counterfeit integer two remains out of bounds");
    assert!(
        matches!(
            error.kind,
            ErrorKind::IndexOutOfBounds {
                index: 2,
                length: 2
            }
        ),
        "unexpected child-file diagnostic: {error:?}"
    );
    let stats = checked_const_index_test_stats();
    assert_eq!(stats.evaluations, 2);
    assert_eq!(stats.hits, 1);
}

#[test]
fn same_root_span_with_different_child_files_preserves_uncached_behavior() {
    let source = "fn main() { let mut values: [i32; 2] = [0, 0]; values[first.LIMIT] += 1; values[second.LIMIT] += 1; }";
    let first_root_start = source.find("first.LIMIT").expect("first qualified index");
    let second_root_start = source.find("second.LIMIT").expect("second qualified index");
    let first_root = rue_span::Span::new(first_root_start as u32, (first_root_start + 11) as u32);
    let second_root =
        rue_span::Span::new(second_root_start as u32, (second_root_start + 12) as u32);
    let second_base = rue_span::Span::new(second_root_start as u32, (second_root_start + 6) as u32);
    let first_file = rue_span::FileId::new(1);

    let mut fixture = ProviderFixture::new();
    fixture.declare_imported_const(
        "first",
        "fixture/first.rue",
        "LIMIT",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(1),
    );
    fixture.declare_imported_const(
        "second",
        "fixture/second.rue",
        "LIMIT",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(2),
    );
    fixture.declare_imported_module_binding("fixture/first.rue", "second", "fixture/second.rue");
    fixture.set_imported_module_file("fixture/first.rue", first_file);
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    reset_checked_const_index_test_stats();
    let result = fixture.analyze_span_edited_with_files(
        source,
        "main",
        &[(first_file, source.len() as u32)],
        |_, span| {
            if span == second_root {
                first_root
            } else if span == second_base {
                rue_span::Span::with_file(first_file, span.start, span.end)
            } else {
                span
            }
        },
    );
    let error = result
        .map(|_| ())
        .expect_err("the foreign child span remains invalid at publication");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::OutputPublication(reason)
                if reason == "provider body export failed: ForeignSpan"
        ),
        "unexpected child-file diagnostic: {error:?}"
    );
    assert_eq!(error.span(), None);
    let stats = checked_const_index_test_stats();
    assert_eq!(stats.evaluations, 4);
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.candidate_comparisons, 0);
}

#[test]
fn non_integer_index_diagnostic_retries_before_checked_cache_admission() {
    let source = "fn main() { let mut values: [i32; 2] = [0, 0]; values[true] += 1; }";
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let mut first_failure = None;
    for _attempt in 0..2 {
        reset_checked_const_index_test_stats();
        let error = fixture
            .analyze(source, "main")
            .map(|_| ())
            .expect_err("a boolean index is rejected");
        assert!(matches!(
            &error.kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "integer type" && found == "bool"
        ));
        assert_eq!(error_source_slice(source, &error), "true");
        assert_eq!(checked_const_index_test_stats().evaluations, 0);
        let span = error.span();
        if let Some((first_kind, first_span)) = &first_failure {
            assert_eq!(first_kind, &error.kind);
            assert_eq!(*first_span, span);
        } else {
            first_failure = Some((error.kind, span));
        }
    }
}

#[test]
fn cancellation_wins_at_checked_const_index_cache_hit() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let source = "fn main() { let mut values: [i32; 2] = [10, 20]; values[1 + 0] += 2; }";
    reset_checked_const_index_test_stats();
    let result = with_cancellation_on_checked_const_index_hit(|| fixture.analyze(source, "main"));
    let stats = checked_const_index_test_stats();
    let error = result
        .map(|_| ())
        .expect_err("cancellation at the primed cache hit must win");
    assert!(
        matches!(&error.kind, ErrorKind::InternalError(message) if message == "body analysis query canceled"),
        "unexpected cancellation diagnostic: {error:?}"
    );
    assert_eq!(stats.evaluations, 1);
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.canceled_hits, 1);
}

#[test]
fn checked_const_index_cache_is_body_and_specialization_local() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "update",
        vec![comptime_type_param("T")],
        SemanticImportType::Unit,
    );
    let four = fixture.declare_struct("Four", vec![("x", SemanticImportType::I32)], true);
    let eight = fixture.declare_struct("Eight", vec![("x", SemanticImportType::I64)], true);
    let source = "fn update(comptime T: type) { let mut values: [i32; 16] = [0; 16]; values[@size_of(T)] += 2; }";

    let i32_body = fixture
        .analyze_specialized_with_types(source, "update", &[SemanticImportType::Nominal(four)], &[])
        .expect("four-byte specialization analyzes");
    let i64_body = fixture
        .analyze_specialized_with_types(
            source,
            "update",
            &[SemanticImportType::Nominal(eight)],
            &[],
        )
        .expect("eight-byte specialization analyzes");

    for body in [&i32_body, &i64_body] {
        assert_eq!(body.work.checked_const_index_evaluations, 2);
        assert_eq!(body.work.checked_const_index_cache_hits, 0);
        assert_eq!(body.work.checked_const_index_candidate_comparisons, 0);
    }
    assert_ne!(i32_body.export.body, i64_body.export.body);
}

#[test]
fn checked_const_index_cache_is_imported_generic_identity_local() {
    let mut fixture = ProviderFixture::new();
    let limit = fixture.declare_const(
        "LIMIT",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(1),
    );
    fixture.declare_function(
        "update",
        vec![comptime_type_param("T")],
        SemanticImportType::Unit,
    );
    let four = fixture.declare_struct("Four", vec![("x", SemanticImportType::I32)], true);
    let eight = fixture.declare_struct("Eight", vec![("x", SemanticImportType::I64)], true);
    let source = "fn update(comptime T: type) { let mut values: [i32; 16] = [0; 16]; values[LIMIT + @size_of(T)] += 2; }";

    let mut exports = Vec::new();
    for ty in [four, eight] {
        let body = fixture
            .analyze_specialized_with_types(
                source,
                "update",
                &[SemanticImportType::Nominal(ty)],
                &[],
            )
            .expect("imported generic index analyzes");
        assert_eq!(body.work.checked_const_index_evaluations, 2);
        assert_eq!(body.work.checked_const_index_cache_hits, 0);
        assert_eq!(body.work.checked_const_index_candidate_comparisons, 0);
        assert!(body.referenced_values.contains(&limit));
        exports.push(body.export.body);
    }
    assert_ne!(exports[0], exports[1]);
}

#[test]
fn qualified_imported_const_index_preserves_uncached_retry_behavior() {
    let mut fixture = ProviderFixture::new();
    let limit = fixture.declare_imported_const(
        "module",
        "fixture/module.rue",
        "LIMIT",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(1),
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let body = fixture
        .analyze(
            "fn main() { let mut values: [i32; 2] = [0, 0]; values[module.LIMIT] += 1; }",
            "main",
        )
        .expect("qualified imported constant index analyzes");

    assert_eq!(
        (
            body.work.checked_const_index_evaluations,
            body.work.checked_const_index_cache_hits,
            body.work.checked_const_index_candidate_comparisons,
            body.work.checked_const_index_comparison_nodes,
        ),
        (2, 0, 0, 0)
    );
    assert!(body.referenced_values.contains(&limit));
}

#[test]
fn checked_const_index_cache_isolated_across_imported_const_bodies() {
    let source = "fn main() { let mut values: [i32; 2] = [0, 0]; values[LIMIT] += 1; }";

    let mut in_bounds = ProviderFixture::new();
    in_bounds.declare_const(
        "LIMIT",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(1),
    );
    in_bounds.declare_function("main", Vec::new(), SemanticImportType::Unit);
    in_bounds
        .analyze(source, "main")
        .expect("the first imported value is in bounds");

    let mut out_of_bounds = ProviderFixture::new();
    out_of_bounds.declare_const(
        "LIMIT",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(2),
    );
    out_of_bounds.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let error = out_of_bounds
        .analyze(source, "main")
        .map(|_| ())
        .expect_err("the second imported value must not reuse the first body");
    assert!(matches!(
        error.kind,
        ErrorKind::IndexOutOfBounds {
            index: 2,
            length: 2
        }
    ));
    assert_eq!(error_source_slice(source, &error), "LIMIT");
}

#[test]
fn checked_const_index_cache_isolated_across_anonymous_producers() {
    let safe_source = "fn main() { let T = struct { x: i32 }; let mut values: [i32; 8] = [0; 8]; values[@size_of(T)] += 1; }";
    let wide_source = "fn main() { let T = struct { x: i64 }; let mut values: [i32; 8] = [0; 8]; values[@size_of(T)] += 1; }";

    let mut safe = ProviderFixture::new();
    safe.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let safe_body = safe
        .analyze(safe_source, "main")
        .expect("the four-byte anonymous producer analyzes");

    let mut wide = ProviderFixture::new();
    wide.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let wide_body = wide
        .analyze(wide_source, "main")
        .expect("the wide anonymous producer analyzes independently");
    for body in [&safe_body, &wide_body] {
        assert_eq!(body.work.checked_const_index_evaluations, 2);
        assert_eq!(body.work.checked_const_index_cache_hits, 0);
        assert_eq!(body.work.checked_const_index_candidate_comparisons, 0);
    }
    assert_ne!(safe_body.export.body, wide_body.export.body);
}

#[test]
fn provider_specialized_body_uses_local_comptime_memo_for_branching_recursion() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "recur",
        vec![comptime_value_param("n", SemanticImportType::I32)],
        SemanticImportType::I32,
    );
    reset_comptime_reduction_test_stats();
    let first = fixture.analyze_specialized(
        "fn recur(comptime n: i32) -> i32 { comptime { if n <= 0 { 1 } else { recur(n - 1) + recur(n - 1) } } }",
        "recur",
        &[8],
    )
    .expect("branching comptime recursion analyzes");
    let first_stats = comptime_reduction_test_stats();
    assert_eq!(first.function.air.return_type(), crate::types::Type::I32);
    assert_eq!(
        first_stats.last_known_integer,
        Some(128),
        "the largest memoized child reduction is the exact 2^7 result"
    );
    assert!(
        first
            .function
            .air
            .iter()
            .any(|(_, instruction)| matches!(instruction.data, crate::AirInstData::Const(256))),
        "the specialized root must retain the exact 2^8 branch result"
    );
    assert_eq!(first_stats.misses, 8, "one miss per unique recursive state");
    assert_eq!(
        first_stats.hits, 8,
        "the repeated branch hits the body memo"
    );
    assert_eq!(first_stats.publications, 8, "only completed states publish");
    assert_eq!(
        first_stats.canonical_issuances,
        first_stats.misses + first_stats.hits,
        "each local call issues its producer identity once without run-frame duplication"
    );
    assert_eq!(first_stats.non_successful_completions, 0);
    assert!(first_stats.body_instances_dropped > 0);
    assert_eq!(first_stats.max_entries, 8);
    assert_eq!(first_stats.last_dropped_entries, 8);

    // A second request gets a fresh body-local memo, yet the durable export
    // remains deterministic byte-for-byte.
    reset_comptime_reduction_test_stats();
    let second = fixture
        .analyze_specialized(
            "fn recur(comptime n: i32) -> i32 { comptime { if n <= 0 { 1 } else { recur(n - 1) + recur(n - 1) } } }",
            "recur",
            &[8],
        )
        .expect("retained recursion analyzes");
    let second_stats = comptime_reduction_test_stats();
    assert_eq!(first.export.body, second.export.body);
    assert_eq!(first_stats, second_stats);
}

#[test]
fn provider_local_memo_hit_still_obeys_depth_before_lookup() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "recur",
        vec![comptime_value_param("n", SemanticImportType::I32)],
        SemanticImportType::I32,
    );
    let source = "fn recur(comptime n: i32) -> i32 { comptime { if n == 66 { recur(0) + recur(65) } else if n <= 0 { 1 } else { recur(n - 1) } } }";
    reset_comptime_reduction_test_stats();
    let error = match fixture.analyze_specialized(
        source,
        "recur",
        &[MAX_COMPTIME_CALL_DEPTH as i128 + 2],
    ) {
        Ok(_) => panic!("the cached zero state cannot bypass the depth gate"),
        Err(error) => error,
    };
    let stats = comptime_reduction_test_stats();
    assert_eq!(
        stats.publications, 1,
        "the shallow zero state was cached first"
    );
    assert_eq!(
        stats.hits, 0,
        "the only over-limit frame is rejected before lookup"
    );
    assert!(matches!(
        &error.kind,
        ErrorKind::ComptimeEvaluationFailed { reason }
            if reason == "specialization of 'recur' exceeded the maximum nesting depth (64); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?"
    ));
    assert_eq!(error_source_slice(source, &error), source);
}

#[test]
fn provider_specialized_body_failed_reduction_is_not_cached_and_retries() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "looping",
        vec![comptime_value_param("n", SemanticImportType::I32)],
        SemanticImportType::I32,
    );
    let source = "fn looping(comptime n: i32) -> i32 { comptime { looping(n) } }";
    reset_comptime_reduction_test_stats();
    let first_error = match fixture.analyze_specialized(source, "looping", &[0]) {
        Ok(_) => panic!("non-terminating comptime recursion is rejected"),
        Err(error) => error,
    };
    let first_stats = comptime_reduction_test_stats();
    assert!(matches!(
        &first_error.kind,
        ErrorKind::ComptimeEvaluationFailed { reason }
            if reason == "specialization of 'looping' exceeded the maximum nesting depth (64); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?"
    ));
    assert_eq!(error_source_slice(source, &first_error), source);
    assert_eq!(first_stats.publications, 0);
    assert_eq!(first_stats.hits, 0);
    assert!(first_stats.non_successful_completions > 0);
    assert!(first_stats.misses > 0);

    reset_comptime_reduction_test_stats();
    let second_error = match fixture.analyze_specialized(source, "looping", &[0]) {
        Ok(_) => panic!("a failed reduction must be retried"),
        Err(error) => error,
    };
    let second_stats = comptime_reduction_test_stats();
    assert_eq!(first_error.kind, second_error.kind);
    assert_eq!(first_error.span(), second_error.span());
    assert_eq!(first_stats, second_stats);
}

#[test]
fn provider_specialized_body_canceled_reduction_is_not_cached() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "recur",
        vec![comptime_value_param("n", SemanticImportType::I32)],
        SemanticImportType::I32,
    );
    reset_comptime_reduction_test_stats();
    let canceled_error = match with_fixture_cancellation_after(100, || {
        fixture.analyze_specialized(
            "fn recur(comptime n: i32) -> i32 { comptime { if n <= 0 { 1 } else { recur(n - 1) + recur(n - 1) } } }",
            "recur",
            &[8],
        )
    }) {
        Ok(_) => panic!("canceled reduction must not complete"),
        Err(error) => error,
    };
    let canceled_stats = comptime_reduction_test_stats();
    assert!(
        matches!(canceled_error.kind, ErrorKind::InternalError(ref reason) if reason == "body analysis query canceled")
    );
    assert_eq!(canceled_stats.publications, 0);
    assert_eq!(canceled_stats.hits, 0);
    assert!(canceled_stats.non_successful_completions > 0);

    reset_comptime_reduction_test_stats();
    let retry = fixture
        .analyze_specialized(
            "fn recur(comptime n: i32) -> i32 { comptime { if n <= 0 { 1 } else { recur(n - 1) + recur(n - 1) } } }",
            "recur",
            &[8],
        )
        .expect("canceled reduction is retried in a fresh body");
    let retry_stats = comptime_reduction_test_stats();
    assert_eq!(retry.function.air.return_type(), crate::types::Type::I32);
    assert_eq!(retry_stats.publications, 8);
    assert_eq!(retry_stats.hits, 8);
}

#[test]
fn provider_durable_first_comptime_query_bypasses_local_body_memo() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "external",
        vec![comptime_value_param("n", SemanticImportType::I32)],
        SemanticImportType::I32,
    );
    fixture.declare_function("caller", Vec::new(), SemanticImportType::I32);
    reset_comptime_reduction_test_stats();
    let (body, durable_calls) = with_fixture_durable_integer("external", 77, || {
        fixture
            .analyze("fn caller() -> i32 { comptime { external(1) } }", "caller")
            .expect("the durable comptime query produces its configured result")
    });
    let stats = comptime_reduction_test_stats();
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
    assert_eq!(
        durable_calls, 1,
        "the durable result was actually consulted"
    );
    assert!(
        body.function
            .air
            .iter()
            .any(|(_, instruction)| matches!(instruction.data, crate::AirInstData::Const(77))),
        "the AIR contains the exact durable reduction result"
    );
    assert_eq!(stats.misses, 0);
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.publications, 0);
}

#[test]
fn production_comptime_key_separates_real_identity_target_type_and_value() {
    let mut fixture = ProviderFixture::new();
    let marker = fixture.declare_struct("Marker", vec![("value", SemanticImportType::I32)], true);
    fixture.declare_function(
        "recur",
        vec![
            comptime_type_param("T"),
            comptime_value_param("n", SemanticImportType::I32),
            comptime_value_param("step", SemanticImportType::I32),
        ],
        SemanticImportType::I32,
    );
    reset_comptime_reduction_test_stats();
    fixture
        .analyze_specialized_with_types(
            "fn recur(comptime T: type, comptime n: i32, comptime step: i32) -> i32 { comptime { if n <= 0 { step } else { recur(T, n - 1, step) } } }",
            "recur",
            &[SemanticImportType::Nominal(marker.clone())],
            &[2, 7],
        )
        .expect("the production builder emits keys for real recursive calls");
    let observed = comptime_reduction_test_keys();
    assert!(
        !observed.is_empty(),
        "the test uses keys issued by the production path"
    );
    assert!(
        observed.iter().any(|key| {
            key.type_arguments.len() == 1
                && key.type_arguments[0] == observed[0].type_arguments[0]
                && key.value_arguments.as_ref() == [ConstValue::Integer(1), ConstValue::Integer(7)]
        }),
        "production keys retain the declared T, n, step order"
    );
    assert!(
        observed.iter().any(|key| {
            key.type_arguments.len() == 1
                && key.type_arguments[0] == observed[0].type_arguments[0]
                && key.value_arguments.as_ref() == [ConstValue::Integer(0), ConstValue::Integer(7)]
        }),
        "recursive production keys carry a distinct real value substitution"
    );
    let mut identity_fixture = ProviderFixture::new();
    identity_fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let identity_body = identity_fixture
        .analyze(
            "fn main() -> i32 { let A = struct { value: i32, fn helper() -> i32 { 0 } }; let B = struct { value: i32 }; 0 }",
            "main",
        )
        .expect("the identity source produces real anonymous and symbol identities");
    let mut anonymous_types = identity_body
        .type_pool
        .all_struct_ids()
        .into_iter()
        .filter(|id| identity_body.type_pool.is_anonymous_struct(*id))
        .map(Type::new_struct);
    let anonymous_a = anonymous_types.next().expect("first anonymous type");
    let anonymous_b = anonymous_types.next().expect("second anonymous type");
    let function_a = SymbolHandle::new(identity_body.interner.get("main").expect("main symbol"));
    let function_b = SymbolHandle::new(
        identity_body
            .interner
            .get("helper")
            .expect("anonymous method symbol"),
    );

    type Producer = StableProducerId<SemanticDefinitionToken, SemanticModuleToken>;
    type Memo = ComptimeCompletedCallMemo<Producer, Target, Type, ConstValue, ConstValue>;
    let key = |producer, target, ty, value| ComptimeCallKey {
        declaration: producer,
        configuration: target,
        type_arguments: Arc::from([ty]),
        value_arguments: Arc::from([value, ConstValue::Integer(7)]),
    };
    let base = observed
        .iter()
        .find(|key| {
            key.value_arguments.as_ref() == [ConstValue::Integer(1), ConstValue::Integer(7)]
        })
        .expect("production key is available")
        .clone();
    let mut memo = Memo::new();
    memo.insert(
        base.clone(),
        ComptimeMemoizedOutcome::Known(ConstValue::Integer(17)),
    )
    .unwrap();
    assert!(matches!(
        memo.lookup(&base),
        ComptimeCallMemoLookup::Memoized(ComptimeMemoizedOutcome::Known(ConstValue::Integer(17)))
    ));
    let function_base = key(
        base.declaration.clone(),
        base.configuration,
        anonymous_a,
        ConstValue::Function(function_a),
    );
    memo.insert(
        function_base.clone(),
        ComptimeMemoizedOutcome::Known(ConstValue::Integer(18)),
    )
    .unwrap();
    assert!(matches!(
        memo.lookup(&function_base),
        ComptimeCallMemoLookup::Memoized(ComptimeMemoizedOutcome::Known(ConstValue::Integer(18)))
    ));
    let generic_specialization = observed
        .iter()
        .find(|candidate| candidate.declaration != base.declaration)
        .expect("recursive states issue distinct generic specializations")
        .declaration
        .clone();
    let mut other_fixture = ProviderFixture::new();
    other_fixture.declare_function(
        "other",
        vec![
            comptime_type_param("T"),
            comptime_value_param("n", SemanticImportType::I32),
            comptime_value_param("step", SemanticImportType::I32),
        ],
        SemanticImportType::I32,
    );
    reset_comptime_reduction_test_stats();
    let other_marker =
        other_fixture.declare_struct("Marker", vec![("value", SemanticImportType::I32)], true);
    other_fixture
        .analyze_specialized_with_types(
            "fn other(comptime T: type, comptime n: i32, comptime step: i32) -> i32 { comptime { if n <= 0 { step } else { other(T, n - 1, step) } } }",
            "other",
            &[SemanticImportType::Nominal(other_marker)],
            &[2, 7],
        )
        .expect("a second production callable issues a real identity");
    // Each provider run owns request-local Type handles, so use the second
    // production-issued declaration only after the memo's base substitutions
    // have been fixed. The explicit key below isolates callable identity while
    // preserving those substitutions.
    let callable_issuer = comptime_reduction_test_keys()
        .into_iter()
        .find(|candidate| candidate.declaration != base.declaration)
        .expect("the second callable has a distinct producer identity")
        .declaration;
    let callable_counterfeit = key(
        callable_issuer,
        base.configuration,
        base.type_arguments[0],
        base.value_arguments[0],
    );
    let generic_counterfeit = key(
        generic_specialization,
        base.configuration,
        base.type_arguments[0],
        base.value_arguments[0],
    );
    let target_counterfeit = key(
        observed[0].declaration.clone(),
        if base.configuration == Target::X86_64Linux {
            Target::Aarch64Linux
        } else {
            Target::X86_64Linux
        },
        base.type_arguments[0],
        base.value_arguments[0],
    );
    let type_counterfeit = key(
        function_base.declaration.clone(),
        function_base.configuration,
        anonymous_b,
        function_base.value_arguments[0],
    );
    let value_counterfeit = key(
        function_base.declaration.clone(),
        function_base.configuration,
        function_base.type_arguments[0],
        ConstValue::Function(function_b),
    );
    for counterfeit in [
        callable_counterfeit,
        generic_counterfeit,
        target_counterfeit,
        type_counterfeit,
        value_counterfeit,
    ] {
        assert!(matches!(
            memo.lookup(&counterfeit),
            ComptimeCallMemoLookup::Miss
        ));
    }
}

// Migrated from `tests::test_undefined_variable`: the diagnostic keeps its
// exact source span across the provider boundary.
#[test]
fn provider_body_reports_undefined_variable_with_exact_span() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let source = "fn main() -> i32 { missing + 1 }";
    let error = fixture
        .analyze(source, "main")
        .map(|_| ())
        .expect_err("undefined operand is rejected");

    assert!(
        matches!(&error.kind, ErrorKind::UndefinedVariable(name) if name == "missing"),
        "unexpected diagnostic: {error:?}"
    );
    assert_eq!(error_source_slice(source, &error), "missing");
}

// Migrated from `tests::test_use_after_move_error`: ownership diagnostics on
// the production provider path, with the callee crossing the boundary as an
// explicit durable signature fact.
#[test]
fn memoized_comptime_result_keeps_linear_ownership_diagnostic_on_production_path() {
    let mut fixture = ProviderFixture::new();
    let token = fixture.declare_struct_with(
        "Token",
        vec![("id", SemanticImportType::I32)],
        false,
        StructShape {
            is_linear: true,
            ..StructShape::default()
        },
    );
    fixture.declare_function(
        "recur",
        vec![
            comptime_type_param("T"),
            comptime_value_param("n", SemanticImportType::I32),
        ],
        SemanticImportType::I32,
    );
    fixture.declare_function(
        "consume",
        vec![value_param("n", SemanticImportType::Nominal(token.clone()))],
        SemanticImportType::I32,
    );
    reset_comptime_reduction_test_stats();
    let source = "fn recur(comptime T: type, comptime n: i32) -> i32 {
    let result = comptime { if n <= 0 { 1 } else { recur(T, n - 1) + recur(T, n - 1) } };
    if n > 1 {
        let TokenType = Token;
        let token: TokenType = Token { id: 42 };
        let consumed = consume(token);
        consumed + token.id
    } else {
        result
    }
}";
    let error = fixture
        .analyze_specialized_with_types(
            source,
            "recur",
            &[SemanticImportType::Nominal(token.clone())],
            &[2],
        )
        .map(|_| ())
        .expect_err("the move/use check still runs after a memoized child reduction");
    let stats = comptime_reduction_test_stats();
    assert!(
        stats.publications > 0,
        "the local memo published the successful child result"
    );
    assert!(
        stats.hits > 0,
        "the duplicate recursive child reaches the local memo"
    );
    assert!(
        comptime_reduction_test_keys()
            .iter()
            .any(|key| key.type_arguments.len() == 1),
        "the memoized child key retains the linear nominal type substitution"
    );
    assert!(
        matches!(&error.kind, ErrorKind::UseAfterMove(..)),
        "unexpected diagnostic after a memoized result: {error:?}"
    );
}

#[test]
fn provider_body_reports_use_after_move() {
    let mut fixture = ProviderFixture::new();
    let non_copy = fixture.declare_struct("NonCopy", vec![("x", SemanticImportType::I32)], false);
    fixture.declare_function(
        "consume",
        vec![value_param("n", SemanticImportType::Nominal(non_copy))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let source = "fn main() -> i32 {
    let n = NonCopy { x: 42 };
    let consumed = consume(n);
    consumed + n.x
}";
    let error = fixture
        .analyze(source, "main")
        .map(|_| ())
        .expect_err("the moved value cannot be read again");

    assert!(
        matches!(&error.kind, ErrorKind::UseAfterMove { .. }),
        "unexpected diagnostic: {error:?}"
    );
    assert!(error.span().is_some(), "move diagnostic keeps its span");
}

// Migrated from `tests::test_copy_type_not_moved`: `@copy` metadata crosses
// the boundary inside the durable nominal fact.
#[test]
fn provider_body_copy_type_is_not_moved() {
    let mut fixture = ProviderFixture::new();
    let point = fixture.declare_struct("Point", vec![("x", SemanticImportType::I32)], true);
    fixture.declare_function(
        "give",
        vec![value_param("p", SemanticImportType::Nominal(point))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let p = Point { x: 7 };
    let a = give(p);
    let b = give(p);
    a + b + p.x
}",
            "main",
        )
        .expect("a copy value survives repeated calls");
    assert!(body.warnings.is_empty(), "no incidental warnings expected");
}

// Provider-path counterpart of `tests::test_struct_field_type_resolution`
// (which keeps its whole-program type-pool assertions): aggregate
// construction with nested nominal fields resolved from durable facts.
#[test]
fn provider_body_resolves_nested_struct_field_types() {
    let mut fixture = ProviderFixture::new();
    let inner = fixture.declare_struct("Inner", vec![("x", SemanticImportType::I32)], true);
    fixture.declare_struct(
        "Outer",
        vec![("inner", SemanticImportType::Nominal(inner))],
        true,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let o = Outer { inner: Inner { x: 42 } };
    o.inner.x
}",
            "main",
        )
        .expect("nested aggregate construction analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// Direct provider-path coverage of a method call: the member is resolved
// through the durable member lookup plus its durable method signature.
#[test]
fn provider_body_types_method_call() {
    let mut fixture = ProviderFixture::new();
    let point = fixture.declare_struct(
        "Point",
        vec![
            ("x", SemanticImportType::I32),
            ("y", SemanticImportType::I32),
        ],
        true,
    );
    fixture.declare_method(&point, "x_value", Vec::new(), SemanticImportType::I32);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let p = Point { x: 3, y: 4 };
    p.x_value()
}",
            "main",
        )
        .expect("method call analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// Provider-path counterpart of the body-local alias half of
// `tests::comptime_type_alias_filter_preserves_analysis_and_diagnostics`:
// a comptime type alias types a later binding on the production path.
#[test]
fn provider_body_reduces_local_comptime_type_alias() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let Direct = i32;
    let value: Direct = 40 + 2;
    value
}",
            "main",
        )
        .expect("comptime alias body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// A comptime value crossing the boundary as a durable const fact.
#[test]
fn provider_body_reads_durable_const_value() {
    let mut fixture = ProviderFixture::new();
    let limit = fixture.declare_const(
        "LIMIT",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(40),
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { LIMIT + 2 }", "main")
        .expect("durable const body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
    assert!(
        body.referenced_values.contains(&limit),
        "the consulted const is recorded as a referenced value"
    );
}

/// The production provider body path must fall through from a missing value
/// constant to a same-named nominal. The resulting type identity is the exact
/// durable nominal key, rather than an invented local type.
#[test]
fn provider_body_named_type_fallback_keeps_exact_nominal_identity() {
    let mut fixture = ProviderFixture::new();
    let nominal = fixture.declare_struct("Thing", vec![("value", SemanticImportType::I32)], true);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let T = Thing;
    let value: T = Thing { value: 42 };
    value.value
}",
            "main",
        )
        .expect("a missing const falls through to the named nominal");

    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
    let nominal_tokens = body
        .definition_tokens
        .iter()
        .filter(|(_, key)| *key == nominal)
        .count();
    assert_eq!(
        nominal_tokens, 1,
        "the exact nominal identity is registered once"
    );
    assert!(
        body.referenced_values.is_empty(),
        "a nominal fallback is not misreported as a value-constant dependency"
    );
}

/// A string constant wins over a same-named nominal. The provider path keeps
/// the value-constant observation and does not fall through to the nominal
/// type branch when the constant is present but not comptime-representable.
#[test]
fn provider_body_string_const_shadows_same_named_nominal() {
    let mut fixture = ProviderFixture::new();
    let nominal = fixture.declare_struct("Thing", vec![("value", SemanticImportType::I32)], true);
    let string_const = fixture.declare_const(
        "Thing",
        SemanticImportType::BuiltinNominal {
            name: Arc::from("str"),
            kind: SemanticImportNominalKind::Struct,
        },
        SemanticImportConstValue::String(Arc::from("runtime")),
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let value = Thing;
    42
}",
            "main",
        )
        .expect("a string constant remains a runtime value in body analysis");

    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
    assert_eq!(body.referenced_values, vec![string_const]);
    assert!(
        body.definition_tokens
            .iter()
            .all(|(_, key)| key != &nominal),
        "the present string constant must not fall through to the nominal"
    );
}

#[test]
fn provider_body_string_const_blocks_same_named_type_alias() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_struct("Thing", vec![("value", SemanticImportType::I32)], true);
    fixture.declare_const(
        "Thing",
        SemanticImportType::BuiltinNominal {
            name: Arc::from("str"),
            kind: SemanticImportNominalKind::Struct,
        },
        SemanticImportConstValue::String(Arc::from("runtime")),
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = match fixture.analyze(
        "fn main() -> i32 {
    let T = Thing;
    let value: T = Thing { value: 42 };
    value.value
}",
        "main",
    ) {
        Ok(_) => panic!("a runtime-dependent string constant cannot become a type alias"),
        Err(error) => error,
    };
    assert!(
        matches!(&error.kind, ErrorKind::UnknownType(name) if name == "T"),
        "unexpected diagnostic: {error:?}"
    );
}

// Provider-path counterpart of the local-alias half of
// `tests::direct_anonymous_type_alias_and_const_receive_authoritative_
// producers`: a body-local anonymous nominal is produced with a durable
// identity, and its member initialization type-checks.
#[test]
fn provider_body_produces_anonymous_nominal_identity() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let T = struct { value: i32 };
    let holder: T = T { value: 42 };
    holder.value
}",
            "main",
        )
        .expect("anonymous nominal body analyzes");
    assert_eq!(body.produced_anonymous_nominals.len(), 1);
    let produced = &body.produced_anonymous_nominals[0];
    assert!(matches!(
        &produced.shape,
        super::provider_body_host::SemanticProducedAnonymousNominalShape::Struct { fields, .. }
            if fields.len() == 1 && &*fields[0].0 == "value"
    ));
}

#[test]
fn provider_body_consulted_identity_conflict_cannot_fall_through_to_mint() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let identity = crate::AnonymousNominalKey {
        kind: crate::AnonymousNominalKind::Struct,
        producer: StableProducerId::Function(crate::Node::new(
            crate::FunctionInstanceKey::Definition(FixtureKey::function("main")),
        )),
        anchor: rue_rir::RirStructuralAnchor::new(vec![
            rue_rir::RirStructuralPathSegment::Body,
            rue_rir::RirStructuralPathSegment::AnonymousType(0),
        ]),
    };
    let probe = fixture
        .probe_consulted_anonymous_struct_conflict("fn main() -> i32 { 0 }", "main", &identity)
        .expect("counterfeit consulted registry reaches the production mint boundary");
    let error = probe
        .result
        .expect_err("ambiguous consulted identities must not mint a replacement");
    assert!(matches!(error.kind, ErrorKind::OutputPublication(_)));
    assert_eq!(
        probe.export_result,
        Err(crate::SemanticBodyExportFailure::AmbiguousStableIdentity)
    );
    assert!(!probe.generated_symbol_was_added);
    assert!(!probe.additional_canonical_type_was_published);
    assert!(!probe.type_pool_len_changed);
}

// A fact that was never seeded fails closed: the miss surfaces as an ordinary
// visible diagnostic, never as an invented fact.
#[test]
fn provider_body_missing_function_fact_fails_closed() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { helper() }", "main")
        .map(|_| ())
        .expect_err("an unseeded callee cannot resolve");
    assert!(
        matches!(&error.kind, ErrorKind::UndefinedFunction(name) if name == "helper"),
        "unexpected diagnostic: {error:?}"
    );
}

// Warnings survive the provider boundary alongside the analyzed body.
#[test]
fn provider_body_preserves_unused_variable_warning() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { let unused = 1; 2 }", "main")
        .expect("body with unused binding analyzes");
    assert!(
        body.warnings.iter().any(|warning| matches!(
            &warning.kind,
            rue_error::WarningKind::UnusedVariable(name) if name == "unused"
        )),
        "unexpected warnings: {:?}",
        body.warnings
    );
}

// Referenced definitions are reported exactly, so the compiler can register
// its dependency edges from the analysis result.
#[test]
fn provider_body_records_referenced_callee_definitions() {
    let mut fixture = ProviderFixture::new();
    let helper = fixture.declare_function("helper", Vec::new(), SemanticImportType::I32);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { helper() }", "main")
        .expect("callee body analyzes");
    assert!(
        body.referenced_definitions.contains(&helper),
        "the resolved callee is a referenced definition"
    );
    assert!(
        body.referenced_specializations.is_empty(),
        "an ordinary call requests no specialization"
    );
}

// Structural guard: the fixture helper drives the production provider entry
// point and never re-enters a retired whole-program `Sema` driver, and that
// entry point runs the one canonical ordinary-body engine.
#[test]
fn fixture_helper_drives_only_the_production_provider_entry_point() {
    let fixture_source = include_str!("provider_fixture.rs");
    let entry = concat!("analyze_provider_", "ordinary_body(");
    assert!(
        fixture_source.contains(entry),
        "the fixture helper must call the production provider entry point"
    );
    for source in [fixture_source, include_str!("provider_fixture_tests.rs")] {
        for retired in [
            concat!("Sema::", "new_synthetic"),
            concat!("new_", "synthetic("),
            concat!("bind_declarations", "_for_test"),
            concat!("analyze_all", "_for_test"),
            concat!("analyze_", "all("),
            concat!("bind_", "declarations("),
        ] {
            assert!(
                !source.contains(retired),
                "the fixture must not re-enter a retired Sema driver: {retired}"
            );
        }
    }
    // The entry point the helper calls runs the one canonical ordinary-body
    // engine. Matching the constructor and the resolved-signature entry
    // separately keeps the guard insensitive to formatting of the call chain.
    let provider_host = include_str!("provider_body_host.rs");
    assert!(
        provider_host.contains("OrdinaryBodyEngine::new"),
        "the provider host must construct the canonical ordinary-body engine"
    );
    assert!(
        provider_host.contains(".analyze_single_function_resolved("),
        "the provider entry point must run the engine's resolved ordinary-body analysis"
    );
}

// A named method body analyzed directly: the single declaration is the owning
// nominal, and the member is selected exactly as the compiler's member body
// transaction selects it.
#[test]
fn provider_member_body_types_receiver_field_read() {
    let mut fixture = ProviderFixture::new();
    let point = fixture.declare_struct("Point", vec![("x", SemanticImportType::I32)], true);
    fixture.declare_method(&point, "double", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze_member(
            "struct Point {
    x: i32,

    fn double(self) -> i32 {
        self.x + self.x
    }
}",
            "Point",
            "double",
            StableDefinitionKind::Method,
        )
        .expect("method body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// A destructor body analyzed directly: the single declaration is the
// `drop fn` item, and the owning nominal crosses as a durable fact.
#[test]
fn provider_destructor_body_analyzes_against_durable_owner() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_struct_with(
        "Resource",
        vec![("handle", SemanticImportType::I32)],
        false,
        StructShape {
            has_destructor: true,
            ..StructShape::default()
        },
    );
    let body = fixture
        .analyze_destructor(
            "drop fn Resource(self) { let _open = self.handle; }",
            "Resource",
        )
        .expect("destructor body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::UNIT);
}

// A RIR edit between lowering and validation probes analysis behavior on
// instruction shapes the frontend cannot produce: a malformed internal
// intrinsic arity is diagnosed instead of panicking.
#[test]
fn provider_body_reports_malformed_internal_intrinsic_arity() {
    use rue_rir::{InstData, InternalIntrinsic};

    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let error = fixture
        .analyze_edited("fn main() { @dbg(1); }", "main", |rir| {
            let intrinsic_ref = rir
                .iter()
                .find_map(|(inst_ref, inst)| match inst.data {
                    InstData::Intrinsic { .. } => Some(inst_ref),
                    _ => None,
                })
                .expect("lowered body contains the probed intrinsic");
            rir.replace_internal_intrinsic(intrinsic_ref, InternalIntrinsic::IterLen, &[])
                .expect("intrinsic replacement applies");
        })
        .map(|_| ())
        .expect_err("malformed compiler RIR must be diagnosed");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::InternalError(message)
                if message.contains("`__rue_iter_len` expects 1 argument, found 0")
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// An `inout` argument that names no place is rejected during body analysis;
// the callee's parameter mode crosses the boundary as a durable signature
// fact.
#[test]
fn provider_body_rejects_inout_argument_without_a_place() {
    use crate::SemanticParameterMode;

    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "take",
        vec![mode_param(
            "x",
            SemanticImportType::I32,
            SemanticParameterMode::Inout,
        )],
        SemanticImportType::Unit,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { take(inout 1); 0 }", "main")
        .map(|_| ())
        .expect_err("non-place must fail in sema");
    assert!(
        matches!(&error.kind, ErrorKind::InoutNonLvalue),
        "unexpected diagnostic: {error:?}"
    );
}

// A declared enum's variant is constructed from its durable nominal fact.
#[test]
fn provider_body_constructs_declared_enum_variant() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_enum("Color", vec![("Red", Vec::new()), ("Green", Vec::new())]);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let c = Color.Red;
    0
}",
            "main",
        )
        .expect("enum construction analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// A legal accessor body is analyzed directly from durable method facts.
#[test]
fn provider_accessor_body_analyzes_stably() {
    use crate::SemanticParameterMode;
    let mut fixture = ProviderFixture::new();
    let holder = fixture.declare_struct("Holder", vec![("x", SemanticImportType::I64)], true);
    fixture.declare_method_with(
        &holder,
        "xr",
        Vec::new(),
        SemanticImportType::I64,
        MethodShape {
            has_self: true,
            self_mode: SemanticParameterMode::Borrow,
            is_accessor: true,
            returns_borrow: true,
            returns_inout: false,
        },
    );
    let body = fixture
        .analyze_member(
            "struct Holder {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        yield self.x;
    }
}",
            "Holder",
            "xr",
            StableDefinitionKind::Method,
        )
        .expect("legal accessor body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I64);
}

// Durable body exports anchor spans relative to the body, so surrounding
// source relocation leaves the exported body identical.
#[test]
fn provider_body_export_ignores_surrounding_source_relocation() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let original = fixture
        .analyze("fn main() -> i32 { 42 }", "main")
        .expect("original body analyzes");
    let relocated = fixture
        .analyze("\n\nfn main() -> i32 { 42 }\n", "main")
        .expect("relocated body analyzes");
    assert_eq!(original.export.body, relocated.export.body);
}

// A body that only warns still exports, with the warning preserved both in
// the analysis result and inside the durable export.
#[test]
fn provider_body_with_warning_exports_and_keeps_the_warning() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let body = fixture
        .analyze("fn main() { let unused = 1; }", "main")
        .expect("warning-only body analyzes");
    assert_eq!(body.export.body.warnings.len(), 1);
    assert!(body.warnings.iter().any(|warning| {
        matches!(warning.kind, rue_error::WarningKind::UnusedVariable(ref name) if name == "unused")
    }));
    assert_eq!(body.export.body.instructions.len(), body.function.air.len());
    assert_eq!(
        body.export.body.places.len(),
        body.function.air.places().len()
    );
    assert!(body.export.body.strings.is_empty());
}

// The durable export of a supported body imports into a fresh AIR epoch
// byte-for-byte: instruction stream, types, spans, places, projections,
// param drops, slot counts, borrow slots, and strings all round-trip.
#[test]
fn provider_body_export_round_trips_through_a_fresh_air_epoch_exactly() {
    use std::cell::Cell;

    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let source = "fn main() -> i32 { if 1 < 2 { (3 + 4) * 5 } else { 6 - 7 } }";
    let body_span = Cell::new(None);
    let body = fixture
        .analyze_edited(source, "main", |rir| {
            body_span.set(rir.iter().find_map(|(_, inst)| match inst.data {
                rue_rir::InstData::FnDecl { body, .. } => Some(rir.get(body).span),
                _ => None,
            }));
        })
        .expect("supported body analyzes");
    let body_span = body_span.get().expect("main body span");

    let epoch = crate::SemanticImportEpoch::<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >::new(vec![], vec![], vec![])
    .expect("empty import epoch constructs");
    let imported = epoch
        .import_body(&body.export.body, body_span)
        .expect("exported body imports");
    let source_function = &body.function;

    assert_eq!(
        source_function.air.return_type(),
        imported.air.return_type()
    );
    assert_eq!(source_function.air.len(), imported.air.len());
    for ((source_ref, source_inst), (imported_ref, imported_inst)) in
        source_function.air.iter().zip(imported.air.iter())
    {
        assert_eq!(source_ref.as_u32(), imported_ref.as_u32());
        assert_eq!(
            format!("{:?}", source_inst.data),
            format!("{:?}", imported_inst.data)
        );
        assert_eq!(source_inst.ty, imported_inst.ty);
        assert_eq!(source_inst.span, imported_inst.span);
    }
    assert_eq!(
        format!("{:?}", source_function.air.places()),
        format!("{:?}", imported.air.places())
    );
    assert_eq!(
        format!("{:?}", source_function.air.projections()),
        format!("{:?}", imported.air.projections())
    );
    assert_eq!(
        source_function.air.param_drops(),
        imported.air.param_drops()
    );
    assert_eq!(source_function.num_locals, imported.num_locals);
    assert_eq!(source_function.num_param_slots, imported.num_param_slots);
    assert_eq!(source_function.param_modes, imported.param_modes);
    assert_eq!(
        source_function.allow_unreachable_code,
        imported.allow_unreachable_code
    );
    assert_eq!(body.strings, imported.strings);
    assert!(imported.warnings.is_empty());
    for slot in 0..source_function.num_locals {
        assert_eq!(
            source_function.air.is_borrow_slot(slot),
            imported.air.is_borrow_slot(slot)
        );
    }
}

// A function-valued durable const resolves to the free function even when a
// type-owned member shares its name: the durable lookup vocabulary keys
// members and free functions separately, and the alias call binds the free
// declaration's signature.
#[test]
fn provider_body_const_alias_selects_the_free_function_over_a_member() {
    let mut fixture = ProviderFixture::new();
    let named = fixture.declare_struct("Named", Vec::new(), true);
    fixture.declare_method_with(
        &named,
        "collide",
        Vec::new(),
        SemanticImportType::I64,
        MethodShape {
            has_self: false,
            ..MethodShape::default()
        },
    );
    let free = fixture.declare_function("collide", Vec::new(), SemanticImportType::I32);
    fixture.declare_const(
        "alias",
        SemanticImportType::I32,
        SemanticImportConstValue::Function(free.clone()),
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { alias() }", "main")
        .expect("const-alias call analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
    assert!(
        body.referenced_definitions.contains(&free),
        "the alias call resolves to the free declaration"
    );
}

// Moving a field out of a value whose type declares a destructor is rejected
// (E0456, RUE-158): the partially-moved owner could not run its destructor.
// Pinning the rejection keeps CFG drop elaboration's assumption valid that a
// destructor-owning aggregate is never partially moved.
#[test]
fn provider_body_rejects_field_move_out_of_destructor_owner() {
    let mut fixture = ProviderFixture::new();
    let a = fixture.declare_struct_with(
        "A",
        vec![("x", SemanticImportType::I32)],
        false,
        StructShape {
            has_destructor: true,
            ..StructShape::default()
        },
    );
    fixture.declare_struct_with(
        "O",
        vec![
            ("a", SemanticImportType::Nominal(a.clone())),
            ("b", SemanticImportType::I32),
        ],
        false,
        StructShape {
            has_destructor: true,
            ..StructShape::default()
        },
    );
    fixture.declare_function(
        "eat",
        vec![value_param("a", SemanticImportType::Nominal(a))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
    let o = O { a: A { x: 1 }, b: 2 };
    eat(o.a);
    0
}",
            "main",
        )
        .map(|_| ())
        .expect_err("field move out of a destructor-having struct must be rejected");
    assert!(
        format!("{error}").contains("cannot move field"),
        "unexpected error: {error}"
    );
}
