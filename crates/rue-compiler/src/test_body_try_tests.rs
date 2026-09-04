//! `?` in a test body: unwrap-and-report (ADR-0083 §1, spec 6.7:13 - 6.7:17).
//!
//! These are the session-level half of the rule's coverage. They fix the shape
//! the runtime contract depends on — the failure-channel pair, adjacent and in
//! order — and the identity rules that make the printer shared rather than
//! per-site. The observable end of the same rule (exit status, stderr, and the
//! frame the channel carries) is covered end to end in `test_image_tests.rs`.
//!
//! Every fixture here supplies its own trusted `std.option` and `std.result`
//! modules at the trusted logical paths, because `?` legality is producer
//! identity rather than shape (RUE-1112): a same-shape local lookalike is an
//! ordinary enum, so a fixture that declared its own `Option` would be testing
//! nothing.

use crate::*;
use std::sync::Arc;

use ahash::{AHashMap, AHashSet};

/// The trusted standard-library `Option`, verbatim from `std/option.rue`.
const TRUSTED_OPTION_SOURCE: &str = r#"
pub fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,
    }
}
"#;

/// The trusted standard-library `Result`, verbatim from `std/result.rue`.
const TRUSTED_RESULT_SOURCE: &str = r#"
pub fn Result(comptime T: type, comptime E: type) -> type {
    enum {
        Ok(T),
        Err(E),
    }
}
"#;

/// A trusted `StrBuf` with the shape the real one has: the byte pointer lives
/// one aggregate down, and the length is the buffer's own field. The printer
/// finds a byte string's view structurally rather than by field index, so this
/// fixture exercises the same discovery the real `std/strbuf.rue` does without
/// pulling its algorithms into a request that only needs the shape.
const TRUSTED_STRBUF_SOURCE: &str = r#"
pub struct RawBuf {
    buf: ptr mut u8,
    cap: u64,
}

pub struct StrBuf {
    core: RawBuf,
    len: u64,
}

pub fn repeated(byte: u8, count: u64) -> StrBuf {
    let p: ptr mut u8 = checked { @alloc(count, 1) };
    let mut i: u64 = 0;
    while i < count {
        checked { @ptr_write(@ptr_offset(p, i), byte); };
        i += 1;
    }
    StrBuf { core: RawBuf { buf: p, cap: count }, len: count }
}

pub fn owned(bytes: str) -> StrBuf {
    let n = bytes.len();
    let p: ptr mut u8 = checked { @alloc(n, 1) };
    let mut i: u64 = 0;
    while i < n {
        checked { @ptr_write(@ptr_offset(p, i), bytes[i]); };
        i += 1;
    }
    StrBuf { core: RawBuf { buf: p, cap: n }, len: n }
}
"#;

const ROOT_FILE: FileId = FileId::new(1);
const TRUSTED_OPTION_FILE: FileId = FileId::new(2);
const TRUSTED_RESULT_FILE: FileId = FileId::new(3);
const TRUSTED_STRBUF_FILE: FileId = FileId::new(4);

/// Publish `root_source` with the trusted `Option` and `Result` modules present.
pub(crate) fn trusted_snapshot(root_source: &str) -> SourceSnapshot {
    let metadata = SourceMetadata::new_with_trusted_standard_library(
        ROOT_FILE,
        AHashMap::from([
            (ROOT_FILE, "/project/main.rue".to_owned()),
            (TRUSTED_OPTION_FILE, "/project/std/option.rue".to_owned()),
            (TRUSTED_RESULT_FILE, "/project/std/result.rue".to_owned()),
            (TRUSTED_STRBUF_FILE, "/project/std/strbuf.rue".to_owned()),
        ]),
        AHashMap::from([
            (ROOT_FILE, "main.rue".to_owned()),
            (TRUSTED_OPTION_FILE, "\0rue-std/option.rue".to_owned()),
            (TRUSTED_RESULT_FILE, "\0rue-std/result.rue".to_owned()),
            (TRUSTED_STRBUF_FILE, "\0rue-std/strbuf.rue".to_owned()),
        ]),
        AHashSet::from([
            TRUSTED_OPTION_FILE,
            TRUSTED_RESULT_FILE,
            TRUSTED_STRBUF_FILE,
        ]),
    )
    .expect("trusted-std metadata is valid");
    SourceSnapshot::new(
        metadata,
        vec![
            (ROOT_FILE, Arc::new(root_source.to_owned())),
            (
                TRUSTED_OPTION_FILE,
                Arc::new(TRUSTED_OPTION_SOURCE.to_owned()),
            ),
            (
                TRUSTED_RESULT_FILE,
                Arc::new(TRUSTED_RESULT_SOURCE.to_owned()),
            ),
            (
                TRUSTED_STRBUF_FILE,
                Arc::new(TRUSTED_STRBUF_SOURCE.to_owned()),
            ),
        ],
    )
    .expect("trusted-std snapshot is valid")
}

pub(crate) fn test_options() -> CompileOptions {
    CompileOptions {
        root_selection: RootSelection::Tests,
        ..CompileOptions::default()
    }
}

/// Lower `root_source` as a test request and return its pre-optimization CFGs.
fn rooted_test_cfg(root_source: &str) -> Result<crate::session::RootedCfgOutput, CompileErrors> {
    let snapshot = trusted_snapshot(root_source);
    let (_, semantic, _) = crate::test_frontend_snapshot(&snapshot, &test_options())?;
    Ok(semantic)
}

/// The ordered runtime-call kinds of one function's CFG, per block.
fn runtime_call_sequence(
    unit: &crate::session::RootedCfgUnit,
) -> Vec<Vec<rue_air::RuntimeCallKind>> {
    unit.cfg()
        .blocks()
        .iter()
        .map(|block| {
            block
                .insts
                .iter()
                .filter_map(|value| match &unit.cfg().get_inst(*value).data {
                    rue_cfg::CfgInstData::Call {
                        runtime: Some(runtime),
                        ..
                    } => Some(*runtime),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

fn error_printers(output: &crate::session::RootedCfgOutput) -> Vec<&crate::FunctionInstanceKey> {
    output
        .functions()
        .iter()
        .map(|unit| &unit.function)
        .filter(|function| matches!(function, crate::FunctionInstanceKey::ErrorPrinter(_)))
        .collect()
}

fn test_unit<'a>(
    output: &'a crate::session::RootedCfgOutput,
    name: &str,
) -> &'a crate::session::RootedCfgUnit {
    output
        .functions()
        .iter()
        .find(|unit| unit.definition_source_name() == Some(name))
        .unwrap_or_else(|| panic!("the request lowers the test `{name}`"))
}

/// 6.7:13 - `?` on a trusted `Option` is legal in a test body, and 6.7:14's
/// failure arm is what it lowers to.
#[test]
fn question_on_option_in_a_test_body_analyzes_and_lowers() {
    let output = rooted_test_cfg(
        r#"
const opt = @import("std/option.rue");
fn maybe() -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(7)
}
test "unwraps an option" {
    let v = maybe()?;
    @assert(v == 7);
}
"#,
    )
    .expect("`?` on a trusted Option is legal in a test body");
    let unit = test_unit(&output, "unwraps an option");
    let calls = runtime_call_sequence(unit);
    assert!(
        calls.iter().any(|block| {
            block.windows(2).any(|pair| {
                pair == [
                    rue_air::RuntimeCallKind::TestFailureSite,
                    rue_air::RuntimeCallKind::TestFail,
                ]
            })
        }),
        "the failure arm must stage the site and then report: {calls:?}"
    );
    assert!(
        error_printers(&output).is_empty(),
        "`Option`'s failure carries nothing, so it needs no printer"
    );
}

/// 6.7:14 - the two channel calls are adjacent, with nothing between them.
#[test]
fn the_failure_channel_pair_is_adjacent_in_the_lowered_cfg() {
    let output = rooted_test_cfg(
        r#"
const res = @import("std/result.rue");
fn fallible() -> res.Result(i64, i32) {
    let R = res.Result(i64, i32);
    R.Ok(1)
}
test "reports the error" {
    let v = fallible()?;
    @assert(v == 1);
}
"#,
    )
    .expect("`?` on a trusted Result is legal in a test body");
    let unit = test_unit(&output, "reports the error");
    let block = unit
        .cfg()
        .blocks()
        .iter()
        .find(|block| {
            block.insts.iter().any(|value| {
                matches!(
                    unit.cfg().get_inst(*value).data,
                    rue_cfg::CfgInstData::Call {
                        runtime: Some(rue_air::RuntimeCallKind::TestFailureSite),
                        ..
                    }
                )
            })
        })
        .expect("the failure arm stages a site");
    let staged = block
        .insts
        .iter()
        .position(|value| {
            matches!(
                unit.cfg().get_inst(*value).data,
                rue_cfg::CfgInstData::Call {
                    runtime: Some(rue_air::RuntimeCallKind::TestFailureSite),
                    ..
                }
            )
        })
        .expect("the staging call is in this block");
    let next = block
        .insts
        .get(staged + 1)
        .map(|value| &unit.cfg().get_inst(*value).data);
    assert!(
        matches!(
            next,
            Some(rue_cfg::CfgInstData::Call {
                runtime: Some(rue_air::RuntimeCallKind::TestFail),
                ..
            })
        ),
        "a staged site must be consumed by the very next instruction, not by one \
         a later instruction could re-stage: {next:?}"
    );
}

/// 6.7:15 - one printer serves every site on the same error type.
#[test]
fn two_sites_on_one_error_type_share_a_single_printer() {
    let output = rooted_test_cfg(
        r#"
const res = @import("std/result.rue");
struct Failure { code: i32, retryable: bool }
fn first() -> res.Result((), Failure) {
    let R = res.Result((), Failure);
    R.Ok(())
}
fn second() -> res.Result((), Failure) {
    let R = res.Result((), Failure);
    R.Ok(())
}
test "two sites, one printer" {
    first()?;
    second()?;
}
"#,
    )
    .expect("a struct error type is renderable");
    let printers = error_printers(&output);
    assert_eq!(
        printers.len(),
        1,
        "two `?` sites on one error type share one printer: {printers:?}"
    );
}

/// 6.7:13 - each site is independent, so one test body may `?` two `Result`s
/// with different error types. That is exactly what 4.15:4 forbids elsewhere,
/// and it is legal here because no enclosing `Err` is constructed; each error
/// type gets its own printer.
#[test]
fn two_error_types_in_one_test_body_get_one_printer_each() {
    let output = rooted_test_cfg(
        r#"
const res = @import("std/result.rue");
struct Missing { code: i32 }
struct Invalid { code: i32 }
fn first() -> res.Result((), Missing) {
    let R = res.Result((), Missing);
    R.Ok(())
}
fn second() -> res.Result((), Invalid) {
    let R = res.Result((), Invalid);
    R.Ok(())
}
test "two error types" {
    first()?;
    second()?;
}
"#,
    )
    .expect("`?` sites in a test body do not have to agree on an error type");
    assert_eq!(
        error_printers(&output).len(),
        2,
        "two error types are two printers: {:?}",
        error_printers(&output)
    );
}

/// The rule is scoped to the test item's own block: a helper it calls keeps the
/// ordinary `?` rules, so a `()`-returning helper is still E0503/E0505.
#[test]
fn a_helper_called_from_a_test_keeps_the_ordinary_question_rules() {
    let errors = rooted_test_cfg(
        r#"
const opt = @import("std/option.rue");
fn maybe() -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(7)
}
fn helper() {
    let v = maybe()?;
}
test "calls the helper" {
    helper();
}
"#,
    )
    .expect_err("a `()`-returning helper still rejects `?`");
    assert!(
        errors
            .iter()
            .any(|error| matches!(error.kind, ErrorKind::QuestionOutsideOptionFn { .. })),
        "unexpected diagnostics: {errors:?}"
    );
    // The wording is unchanged by this rule, and pinned here rather than as a UI
    // case because the UI harness compiles executable requests, which by
    // 6.7:9 never analyze a test body at all.
    let rendered = errors.to_string();
    assert!(
        rendered
            .contains("the `?` operator can only be used in a function that returns an `Option`"),
        "unexpected wording: {rendered}"
    );
}

/// Producer identity is unchanged inside a test body: a same-shape lookalike is
/// an ordinary enum and gets no `?` behavior (spec 4.15:3, E0504).
#[test]
fn a_non_standard_producer_is_still_rejected_in_a_test_body() {
    let errors = rooted_test_cfg(
        r#"
fn Lookalike(comptime T: type) -> type {
    enum {
        Some(T),
        None,
    }
}
fn maybe() -> Lookalike(i64) {
    let L = Lookalike(i64);
    L.Some(7)
}
test "uses a lookalike" {
    let v = maybe()?;
}
"#,
    )
    .expect_err("a lookalike producer gets no `?` behavior in a test body either");
    assert!(
        errors
            .iter()
            .any(|error| matches!(error.kind, ErrorKind::QuestionOnNonOption { .. })),
        "unexpected diagnostics: {errors:?}"
    );
    // Wording pinned here for the same reason as above: a test body is not
    // reachable from the executable requests the UI harness builds.
    let rendered = errors.to_string();
    assert!(
        rendered.contains("the `?` operator can only be applied to an `Option` or `Result`"),
        "unexpected wording: {rendered}"
    );
}
