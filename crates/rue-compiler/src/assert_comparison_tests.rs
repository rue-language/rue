//! `@assert_eq` and `@assert_ne` (ADR-0083 Phase 2.5, spec 4.13:5f).
//!
//! These are the session-level half of the family's coverage: the lowered shape
//! the runtime contract depends on, and the identity rule that makes the
//! structural printer shared rather than per-site. What a failing comparison
//! actually reports — the frame's `left` and `right`, the runner's diff,
//! and the human rendering — is covered end to end by the `cli.rue_test_assert`
//! cases, and the ordinary-build trap by `cli.rue_test_assert`'s executable
//! case and the `4.13:5f` spec cases.
//!
//! The requests here are ordinary executable ones, deliberately. The family is
//! usable anywhere `@assert` is, and lowering it identically in and out of a
//! test image is the property worth pinning: nothing here mentions a test item.

use crate::*;

use ahash::AHashMap;

/// Lower `source` as an ordinary executable request and return its
/// pre-optimization CFGs.
fn rooted_cfg(source: &str) -> Result<crate::session::RootedCfgOutput, CompileErrors> {
    let snapshot = SourceSnapshot::single("main.rue", source).map_err(CompileErrors::from)?;
    let (_, semantic, _) = crate::test_frontend_snapshot(&snapshot, &CompileOptions::default())?;
    Ok(semantic)
}

/// Lower `source` as an ordinary executable request against the trusted
/// standard-library fixtures, for the operand shapes that need a non-`Copy`
/// type: `StrBuf` is where "read, don't consume" has observable consequences.
fn rooted_cfg_with_std(source: &str) -> Result<crate::session::RootedCfgOutput, CompileErrors> {
    let snapshot = crate::test_body_try_tests::trusted_snapshot(source);
    let (_, semantic, _) = crate::test_frontend_snapshot(&snapshot, &CompileOptions::default())?;
    Ok(semantic)
}

fn unit<'a>(
    output: &'a crate::session::RootedCfgOutput,
    name: &str,
) -> &'a crate::session::RootedCfgUnit {
    output
        .functions()
        .iter()
        .find(|unit| unit.definition_source_name() == Some(name))
        .unwrap_or_else(|| panic!("the request lowers `{name}`"))
}

fn printers(output: &crate::session::RootedCfgOutput) -> Vec<&crate::FunctionInstanceKey> {
    output
        .functions()
        .iter()
        .map(|function| &function.function)
        .filter(|function| matches!(function, crate::FunctionInstanceKey::ErrorPrinter(_)))
        .collect()
}

/// Every call one function makes, in block order then instruction order, named
/// either by its runtime helper or by the callee symbol.
fn call_sequence(unit: &crate::session::RootedCfgUnit) -> Vec<String> {
    let mut names = AHashMap::new();
    let mut sequence = Vec::new();
    for block in unit.cfg().blocks() {
        for value in &block.insts {
            if let rue_cfg::CfgInstData::Call { runtime, name, .. } =
                &unit.cfg().get_inst(*value).data
            {
                let label = match runtime {
                    Some(runtime) => format!("{runtime:?}"),
                    None => {
                        let symbol = unit.interner().resolve(name).to_owned();
                        let next = names.len();
                        // Printer symbols carry a content digest, which is not
                        // a stable thing to assert on; number them by first
                        // appearance so two calls to one printer are visibly
                        // the same callee.
                        if symbol.contains("error_printer") {
                            format!("printer#{}", *names.entry(symbol).or_insert(next))
                        } else {
                            symbol
                        }
                    }
                };
                sequence.push(label);
            }
        }
    }
    sequence
}

/// The failing arm's order is a contract with the runtime: both renderings
/// first, then the staging call, then the terminal report — because the site
/// the second call adopts is whatever the first staged, and nothing may run
/// between them.
#[test]
fn the_failing_arm_renders_then_stages_then_reports() {
    let output = rooted_cfg(
        r#"
fn value() -> i32 { 41 }
fn check() {
    @assert_eq(value(), 42);
}
fn main() -> i32 {
    check();
    0
}
"#,
    )
    .expect("`@assert_eq` is legal in an ordinary function");
    assert_eq!(
        call_sequence(unit(&output, "check")),
        [
            "__rue_fn_main_2erue__value",
            "printer#0",
            "printer#0",
            "TestFailureSite",
            "TestFailComparison",
        ]
    );
}

/// `@assert_ne` is the same lowering with the other comparison, and reports
/// under its own kind.
#[test]
fn inequality_lowers_the_same_way() {
    let output = rooted_cfg(
        r#"
fn value() -> i32 { 41 }
fn check() {
    @assert_ne(value(), 41);
}
fn main() -> i32 {
    check();
    0
}
"#,
    )
    .expect("`@assert_ne` is legal in an ordinary function");
    let calls = call_sequence(unit(&output, "check"));
    assert_eq!(
        calls.last().map(String::as_str),
        Some("TestFailComparison"),
        "{calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("printer"))
            .count(),
        2,
        "both operands are rendered: {calls:?}"
    );
}

/// The staged site must be consumed by the very next instruction: a call
/// between them would report against a location it did not stage.
#[test]
fn the_channel_pair_is_adjacent_in_the_lowered_cfg() {
    let output = rooted_cfg(
        r#"
fn value() -> i32 { 41 }
fn check() {
    @assert_eq(value(), 42);
}
fn main() -> i32 {
    check();
    0
}
"#,
    )
    .expect("`@assert_eq` is legal in an ordinary function");
    let unit = unit(&output, "check");
    let runtime_of = |value: &rue_cfg::CfgValue| match &unit.cfg().get_inst(*value).data {
        rue_cfg::CfgInstData::Call { runtime, .. } => *runtime,
        _ => None,
    };
    let block =
        unit.cfg()
            .blocks()
            .iter()
            .find(|block| {
                block.insts.iter().any(|value| {
                    runtime_of(value) == Some(rue_air::RuntimeCallKind::TestFailureSite)
                })
            })
            .expect("the failing arm stages a site");
    let staged = block
        .insts
        .iter()
        .position(|value| runtime_of(value) == Some(rue_air::RuntimeCallKind::TestFailureSite))
        .expect("the staging call is in this block");
    assert_eq!(
        block.insts.get(staged + 1).and_then(runtime_of),
        Some(rue_air::RuntimeCallKind::TestFailComparison),
        "a staged site must be consumed by the very next instruction"
    );
}

/// One printer per operand type, however many sites render it — the same
/// identity rule a test body's `?` follows (ADR-0083 §1).
#[test]
fn two_sites_on_one_type_share_a_single_printer() {
    let output = rooted_cfg(
        r#"
struct Point { x: i32, y: bool }
fn origin() -> Point { Point { x: 0, y: false } }
fn check() {
    @assert_eq(origin(), Point { x: 1, y: true });
    @assert_ne(origin(), Point { x: 0, y: false });
}
fn main() -> i32 {
    check();
    0
}
"#,
    )
    .expect("a struct operand supports `==` and is renderable");
    let printers = printers(&output);
    assert_eq!(
        printers.len(),
        1,
        "an `@assert_eq` and an `@assert_ne` on one type share one printer: {printers:?}"
    );
    let calls = call_sequence(unit(&output, "check"));
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.as_str() == "printer#0")
            .count(),
        4,
        "four operands, one printer: {calls:?}"
    );
}

/// Two operand types are two printers, exactly as two error types are.
#[test]
fn two_operand_types_get_one_printer_each() {
    let output = rooted_cfg(
        r#"
struct Point { x: i32 }
struct Size { w: i32 }
fn check(p: Point, s: Size) {
    @assert_eq(p, Point { x: 1 });
    @assert_eq(s, Size { w: 1 });
}
fn main() -> i32 {
    check(Point { x: 0 }, Size { w: 0 });
    0
}
"#,
    )
    .expect("two renderable operand types are legal");
    assert_eq!(
        printers(&output).len(),
        2,
        "two operand types are two printers: {:?}",
        printers(&output)
    );
}

/// A comparison both of whose operands are compile-time constants has its
/// answer now, so it lowers to exactly the `@assert` it could have been written
/// as: no branch to report from, no rendering, and no printer synthesized for a
/// type nothing will render at run time.
#[test]
fn a_comptime_known_comparison_folds_to_a_plain_assert() {
    let output = rooted_cfg(
        r#"
fn check() {
    @assert_eq(1, 2);
    @assert_ne(true, true);
}
fn main() -> i32 {
    check();
    0
}
"#,
    )
    .expect("a constant comparison is legal");
    assert_eq!(
        call_sequence(unit(&output, "check")),
        Vec::<String>::new(),
        "a folded comparison makes no channel or printer call"
    );
    assert!(
        printers(&output).is_empty(),
        "a folded comparison synthesizes no printer: {:?}",
        printers(&output)
    );
    let intrinsics: Vec<rue_air::IntrinsicOperation> = unit(&output, "check")
        .cfg()
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter())
        .filter_map(
            |value| match &unit(&output, "check").cfg().get_inst(*value).data {
                rue_cfg::CfgInstData::Intrinsic { operation, .. } => Some(*operation),
                _ => None,
            },
        )
        .collect();
    assert_eq!(
        intrinsics,
        [
            rue_air::IntrinsicOperation::AssertFailed,
            rue_air::IntrinsicOperation::AssertFailed
        ],
        "the fold is the ordinary `@assert` lowering"
    );
}

/// A byte string is deliberately not folded: `==` on text is a content
/// comparison the run-time lowering performs, and answering it from the string
/// interning table would be a second, subtly different equality.
#[test]
fn two_string_literals_are_not_folded() {
    let output = rooted_cfg(
        r#"
fn check() {
    @assert_eq("left", "right");
}
fn main() -> i32 {
    check();
    0
}
"#,
    )
    .expect("two string literals support `==`");
    assert_eq!(
        printers(&output).len(),
        1,
        "an unfolded comparison renders its operands"
    );
}

/// The operands are *read*, not consumed — the borrowing read `==` performs
/// (4.3:3f, spec 4.13:5f). The report arm passes what that read produced to the
/// printer, so a place a body may only borrow is a legal operand: a field of a
/// `borrow` parameter, a non-`Copy` field of a borrowed local, and a `Copy`
/// field of one all analyze.
///
/// This is the shape most at risk from the report arm, because the printer's
/// parameter is by value: an arm that read the *place* instead of the read's
/// result would make every one of these a move out of a borrow.
#[test]
fn an_operand_may_be_a_place_the_body_may_only_borrow() {
    let output = rooted_cfg_with_std(
        r#"
const sb = @import("std/strbuf.rue");

struct Thing { name: sb.StrBuf, size: i32 }

fn check_name(borrow t: Thing, borrow expected: sb.StrBuf) {
    @assert_eq(t.name, expected);
}

fn check_size(borrow t: Thing) {
    @assert_eq(t.size, 3);
}

fn main() -> i32 {
    let thing = Thing { name: sb.owned("rue"), size: 3 };
    let expected = sb.owned("rue");
    check_name(borrow thing, borrow expected);
    check_size(borrow thing);
    // A non-`Copy` field of a borrowed local, compared in place, with both
    // operands still live afterwards.
    @assert_eq(thing.name, expected);
    0
}
"#,
    )
    .expect("a borrowed place is a legal comparison operand");
    // One printer per operand type, however the operands were reached.
    assert_eq!(
        printers(&output).len(),
        2,
        "the `StrBuf` operands and the `i32` one are two printers: {:?}",
        printers(&output)
    );
}

/// A local compared by `@assert_eq` is still owned afterwards, including by a
/// later by-value use: the comparison reads it and the rendering reads it, and
/// neither takes it.
#[test]
fn a_compared_local_survives_a_later_by_value_use() {
    rooted_cfg_with_std(
        r#"
const sb = @import("std/strbuf.rue");

fn consume(s: sb.StrBuf) -> i32 { 0 }

fn main() -> i32 {
    let v = sb.owned("same");
    let w = sb.owned("same");
    @assert_eq(v, w);
    consume(v) + consume(w)
}
"#,
    )
    .expect("a compared local is not consumed by the comparison");
}

/// The operands must agree on a type. Inference constrains them to one type
/// variable, so the mismatch is reported there rather than as a second
/// intrinsic-shaped diagnostic.
#[test]
fn operands_of_different_types_are_rejected() {
    let errors = rooted_cfg(
        r#"
fn main() -> i32 {
    @assert_eq(1, true);
    0
}
"#,
    )
    .expect_err("`@assert_eq` requires both operands to have one type");
    let rendered = errors.to_string();
    assert!(
        rendered.contains("type mismatch") && rendered.contains("found bool"),
        "unexpected diagnostics: {rendered}"
    );
}

/// "Supports `==`" means exactly what it means for `==`, down to the wording: a
/// raw pointer is not an equality operand, and the family reuses that one check
/// rather than answering the question a second time.
#[test]
fn an_operand_type_without_equality_is_rejected_like_it_is_for_eq() {
    let comparison = rooted_cfg(
        r#"
fn main() -> i32 {
    let n: i32 = 1;
    checked {
        let p = @raw(n);
        @assert_eq(p, p);
    };
    0
}
"#,
    )
    .expect_err("a raw pointer is not an equality operand")
    .to_string();
    let operator = rooted_cfg(
        r#"
fn main() -> i32 {
    let n: i32 = 1;
    checked {
        let p = @raw(n);
        @assert(p == p);
    };
    0
}
"#,
    )
    .expect_err("a raw pointer is not an equality operand")
    .to_string();
    assert!(
        comparison.contains("integer, float, bool, string, unit, struct, array, or enum"),
        "unexpected diagnostics: {comparison}"
    );
    assert!(
        operator.contains("integer, float, bool, string, unit, struct, array, or enum"),
        "unexpected diagnostics: {operator}"
    );
}

/// The arity is exact, and the wrong one is the intrinsic's own diagnostic
/// rather than a type error about a missing operand.
#[test]
fn a_wrong_operand_count_is_the_intrinsic_arity_error() {
    let errors = rooted_cfg(
        r#"
fn main() -> i32 {
    @assert_eq(1);
    0
}
"#,
    )
    .expect_err("`@assert_eq` takes exactly two operands");
    assert!(
        errors
            .iter()
            .any(|error| matches!(&error.kind, ErrorKind::IntrinsicWrongArgCount { name, expected, found }
                if name == "assert_eq" && *expected == 2 && *found == 1)),
        "unexpected diagnostics: {errors:?}"
    );
}
