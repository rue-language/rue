//! The analyzed-operand backstop of RUE-2438, reached directly (RUE-2474).
//!
//! Sema compares the type AIR emission computed for a value against the slot
//! it flows into, or the type its operator demands, at every position where
//! inference would otherwise be trusted: a call argument, a local, field or
//! element store, an `inout` parameter store, a function result, an array element, a comparison's right
//! operand, an `if` or `while` condition, the operand of `!`, `&&`, `||` and
//! unary `-` (`require_slot_type`, `require_operand_type` and
//! `require_comparison_operand`). The check matters only when inference has
//! no fact for the operand: an operand it types `<error>` unifies with
//! anything, while AIR emission still reduces it to a concrete nominal.
//!
//! The one known such operand was an inline-`@import` constructor head
//! (`@import("lib.rue").Box(i64) { v: 7 }`), and RUE-2439 gave inference its
//! fact, so the `cli.inline_import_head_slot_type` cases are now rejected by
//! inference first and no longer reach the backstop. These tests withhold that
//! fact again through the compiler's test-only switch
//! ([`with_inline_import_head_fact_withheld`]) and drive each position
//! through the real session, so a regression in the backstop fails here
//! rather than reopening the miscompiles the next time inference loses a
//! fact. Each rejection must be the backstop's ordinary diagnostic (E0206,
//! or E0801 for a unary `-`), never an internal error. Each family also has a
//! control that compiles with the fact withheld, so the tests cannot pass by
//! rejecting everything.

use crate::*;
use std::cell::Cell;
use std::sync::Arc;

use ahash::{AHashMap, AHashSet};

thread_local! {
    static INLINE_IMPORT_HEAD_FACT_WITHHELD: Cell<bool> = const { Cell::new(false) };
}

/// Whether body analysis on this thread withholds the inline-`@import`
/// constructor-head fact. The compiler's durable body source answers
/// `DurableBodyLookupSource::withholds_inline_import_head_fact` from this,
/// and only under `#[cfg(test)]`; every other build keeps the `false`
/// default, so nothing outside this test binary can set it.
pub(crate) fn inline_import_head_fact_withheld() -> bool {
    INLINE_IMPORT_HEAD_FACT_WITHHELD.with(Cell::get)
}

/// Run `action` with the fact withheld on this thread, restoring the previous
/// setting however `action` ends. Body analysis runs on the requesting
/// thread, so the setting reaches it and no other test.
fn with_inline_import_head_fact_withheld<R>(action: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            INLINE_IMPORT_HEAD_FACT_WITHHELD.with(|withheld| withheld.set(self.0));
        }
    }
    let _restore =
        Restore(INLINE_IMPORT_HEAD_FACT_WITHHELD.with(|withheld| withheld.replace(true)));
    action()
}

const ROOT_FILE: FileId = FileId::new(1);
const LIB_FILE: FileId = FileId::new(2);
const WIDE_FILE: FileId = FileId::new(3);
const TRUSTED_STRBUF_FILE: FileId = FileId::new(4);

/// The generic constructor every probe reaches through an inline `@import`.
const LIB_SOURCE: &str = "pub fn Box(comptime T: type) -> type { struct { v: T } }\n";

/// Another module's same-named constructor with a wider layout: the instance a
/// mismatched one-field `Box(i64)` would be read as.
const WIDE_SOURCE: &str =
    "pub fn Box(comptime T: type) -> type { struct { v: T, w: T, x: T, y: T } }\n";

/// A trusted `StrBuf` with the real one's shape and the `equals_borrowed`
/// method its equality lowers to, at the trusted logical path, so `StrBuf` is
/// the string-family left operand `require_comparison_operand` special-cases.
const TRUSTED_STRBUF_SOURCE: &str = r#"
pub struct RawBuf {
    buf: ptr mut u8,
    cap: u64,
}

pub struct StrBuf {
    core: RawBuf,
    len: u64,

    fn equals_borrowed(borrow first: Self, borrow second: Self) -> bool {
        first.len == second.len
    }
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

/// `main.rue` beside `lib.rue`, `wide.rue` and the trusted `std/strbuf.rue`.
fn snapshot(main: &str) -> SourceSnapshot {
    let metadata = SourceMetadata::new_with_trusted_standard_library(
        ROOT_FILE,
        AHashMap::from([
            (ROOT_FILE, "/project/main.rue".to_owned()),
            (LIB_FILE, "/project/lib.rue".to_owned()),
            (WIDE_FILE, "/project/wide.rue".to_owned()),
            (TRUSTED_STRBUF_FILE, "/project/std/strbuf.rue".to_owned()),
        ]),
        AHashMap::from([
            (ROOT_FILE, "main.rue".to_owned()),
            (LIB_FILE, "lib.rue".to_owned()),
            (WIDE_FILE, "wide.rue".to_owned()),
            (TRUSTED_STRBUF_FILE, "\0rue-std/strbuf.rue".to_owned()),
        ]),
        AHashSet::from([TRUSTED_STRBUF_FILE]),
    )
    .expect("probe metadata is valid");
    SourceSnapshot::new(
        metadata,
        vec![
            (ROOT_FILE, Arc::new(main.to_owned())),
            (LIB_FILE, Arc::new(LIB_SOURCE.to_owned())),
            (WIDE_FILE, Arc::new(WIDE_SOURCE.to_owned())),
            (
                TRUSTED_STRBUF_FILE,
                Arc::new(TRUSTED_STRBUF_SOURCE.to_owned()),
            ),
        ],
    )
    .expect("probe snapshot is valid")
}

/// Analyze `main` and build its CFGs, with or without the inline-head fact.
/// The CFG builder and verifier run too, so a mismatch the backstop missed
/// surfaces here as the internal error it used to be, not only as a silent
/// pass.
fn frontend(main: &str, withheld: bool) -> Result<(), CompileErrors> {
    let snapshot = snapshot(main);
    let run = || crate::test_frontend_snapshot(&snapshot, &CompileOptions::default()).map(|_| ());
    if withheld {
        with_inline_import_head_fact_withheld(run)
    } else {
        run()
    }
}

/// `main` must be rejected with the fact withheld, with `code` and a message
/// containing `message`, and with nothing else: no internal error beside it.
fn assert_backstop_rejects(position: &str, main: &str, code: &str, message: &str) {
    let errors = frontend(main, true).expect_err(&format!(
        "{position}: a mismatched operand compiled with the inline-head fact withheld"
    ));
    let rendered = errors
        .iter()
        .map(|error| format!("[{}] {}", error.kind.code(), error.kind))
        .collect::<Vec<_>>();
    let [only] = rendered.as_slice() else {
        panic!("{position}: expected exactly one diagnostic, got {rendered:?}");
    };
    assert!(
        only.starts_with(&format!("[{code}] ")) && only.contains(message),
        "{position}: expected [{code}] containing {message:?}, got {only:?}"
    );
}

/// `main` must compile with the fact withheld: the backstop admits a
/// correctly-typed operand whose inferred type is `<error>`.
fn assert_backstop_admits(position: &str, main: &str) {
    if let Err(errors) = frontend(main, true) {
        let rendered = errors
            .iter()
            .map(|error| format!("[{}] {}", error.kind.code(), error.kind))
            .collect::<Vec<_>>();
        panic!(
            "{position}: a well-typed operand was rejected with the fact withheld: {rendered:?}"
        );
    }
}

const BOX_MISMATCH: &str = "type mismatch: expected Box(i32), found Box(i64)";

// --- The switch itself -----------------------------------------------------

/// The switch reaches inference. With the head's fact, inference types
/// `.v` as `i64` and the literal compared with it takes that type; with the
/// fact withheld, the literal falls back to `i32` and is out of range. Were
/// the switch inert, every rejection below could be inference's and prove
/// nothing about the backstop. Inference and the backstop otherwise agree on
/// a comparison mismatch, in orientation (`expected Box(i32), found
/// Box(i64)`, RUE-2582) and place (the right operand, RUE-2583).
#[test]
fn withholding_the_fact_hands_the_comparison_to_the_backstop() {
    let typed_by_the_head = r#"
const lib = @import("lib.rue");
fn main() -> i32 {
    if (@import("lib.rue").Box(i64) { v: 7 }.v == 3000000000) { 1 } else { 0 }
}
"#;
    frontend(typed_by_the_head, false).expect("the head's fact types the literal as i64");
    let withheld = frontend(typed_by_the_head, true)
        .expect_err("without the fact the literal defaults to i32");
    let withheld = withheld.first().expect("one diagnostic");
    let rendered = format!("[{}] {}", withheld.kind.code(), withheld.kind);
    assert!(
        rendered.starts_with("[E0800] ")
            && rendered.contains("3000000000")
            && rendered.contains("'i32'"),
        "the literal falls back to i32: {rendered}"
    );

    let main = r#"
const lib = @import("lib.rue");
fn main() -> i32 {
    let q = lib.Box(i32) { v: 7 };
    if (q == @import("lib.rue").Box(i64) { v: 7 }) { 1 } else { 0 }
}
"#;
    let right_operand_start = main.find("@import(\"lib.rue\").Box(i64)").expect("operand") as u32;
    for (withheld, reporter) in [(false, "inference"), (true, "the backstop")] {
        let errors = frontend(main, withheld).expect_err("the mismatch is rejected");
        let error = errors.first().expect("one diagnostic");
        assert_eq!(error.kind.code().to_string(), "E0206");
        assert!(
            error.kind.to_string().contains(BOX_MISMATCH),
            "{reporter}'s orientation: {}",
            error.kind
        );
        assert_eq!(
            error.span().map(|span| span.start),
            Some(right_operand_start),
            "{reporter} reports at the right operand"
        );
    }
    assert_backstop_rejects("struct equality", main, "E0206", BOX_MISMATCH);
}

// --- Slots: require_slot_type ----------------------------------------------

#[test]
fn backstop_rejects_a_mismatched_call_argument() {
    assert_backstop_rejects(
        "call argument",
        r#"
const lib = @import("lib.rue");
fn take(b: lib.Box(i32)) -> i32 { b.v }
fn main() -> i32 { take(@import("lib.rue").Box(i64) { v: 7 }) }
"#,
        "E0206",
        BOX_MISMATCH,
    );
    assert_backstop_rejects(
        "method argument",
        r#"
const lib = @import("lib.rue");
struct H { k: i32, fn go(self, b: lib.Box(i32)) -> i32 { b.v + self.k } }
fn main() -> i32 { let h = H { k: 0 }; h.go(@import("lib.rue").Box(i64) { v: 7 }) }
"#,
        "E0206",
        BOX_MISMATCH,
    );
    assert_backstop_rejects(
        "callback argument",
        r#"
const lib = @import("lib.rue");
fn take(b: lib.Box(i32)) -> i32 { b.v }
fn ap(f: fn(lib.Box(i32)) -> i32) -> i32 { f(@import("lib.rue").Box(i64) { v: 7 }) }
fn main() -> i32 { ap(take) }
"#,
        "E0206",
        BOX_MISMATCH,
    );
    assert_backstop_rejects(
        "argument of another module's wider instance",
        r#"
const wide = @import("wide.rue");
fn sum(p: wide.Box(u64)) -> i32 { @intCast((p.v + p.w + p.x + p.y) % 256) }
fn main() -> i32 { sum(@import("lib.rue").Box(u64) { v: 3 }) }
"#,
        "E0206",
        "type mismatch: expected Box(u64) (in wide.rue), found Box(u64) (in lib.rue)",
    );
}

#[test]
fn backstop_rejects_a_mismatched_store() {
    assert_backstop_rejects(
        "local assignment",
        r#"
const lib = @import("lib.rue");
fn main() -> i32 { let mut p = lib.Box(i32) { v: 1 }; p = @import("lib.rue").Box(i64) { v: 7 }; p.v }
"#,
        "E0206",
        BOX_MISMATCH,
    );
    assert_backstop_rejects(
        "field assignment",
        r#"
const lib = @import("lib.rue");
struct H { b: lib.Box(i32) }
fn main() -> i32 { let mut h = H { b: lib.Box(i32) { v: 1 } }; h.b = @import("lib.rue").Box(i64) { v: 7 }; h.b.v }
"#,
        "E0206",
        BOX_MISMATCH,
    );
    assert_backstop_rejects(
        "index assignment",
        r#"
const lib = @import("lib.rue");
fn main() -> i32 { let mut xs = [lib.Box(i32) { v: 1 }]; xs[0] = @import("lib.rue").Box(i64) { v: 7 }; xs[0].v }
"#,
        "E0206",
        BOX_MISMATCH,
    );
    assert_backstop_rejects(
        "inout parameter assignment",
        r#"
const lib = @import("lib.rue");
fn set(inout b: lib.Box(i32)) { b = @import("lib.rue").Box(i64) { v: 7 }; }
fn main() -> i32 { let mut p = lib.Box(i32) { v: 1 }; set(inout p); p.v }
"#,
        "E0206",
        BOX_MISMATCH,
    );
}

#[test]
fn backstop_rejects_a_mismatched_function_result() {
    assert_backstop_rejects(
        "tail result",
        r#"
const lib = @import("lib.rue");
fn mk() -> lib.Box(i32) { @import("lib.rue").Box(i64) { v: 7 } }
fn main() -> i32 { mk().v }
"#,
        "E0206",
        BOX_MISMATCH,
    );
    assert_backstop_rejects(
        "return statement",
        r#"
const lib = @import("lib.rue");
fn mk(c: bool) -> lib.Box(i32) { if c { return @import("lib.rue").Box(i64) { v: 7 }; } lib.Box(i32) { v: 1 } }
fn main() -> i32 { mk(true).v }
"#,
        "E0206",
        BOX_MISMATCH,
    );
}

#[test]
fn backstop_rejects_a_mismatched_array_element() {
    assert_backstop_rejects(
        "array element",
        r#"
const lib = @import("lib.rue");
fn main() -> i32 { let xs = [lib.Box(i32) { v: 1 }, @import("lib.rue").Box(i64) { v: 7 }]; xs[1].v }
"#,
        "E0206",
        BOX_MISMATCH,
    );
}

/// Control: the same instance through the inline head flows into every slot
/// kind above.
#[test]
fn backstop_admits_a_same_instance_in_every_slot() {
    assert_backstop_admits(
        "same-instance slots",
        r#"
const lib = @import("lib.rue");
struct H { b: lib.Box(i64), fn go(borrow self, b: lib.Box(i64)) -> i64 { b.v } }
fn take(b: lib.Box(i64)) -> i64 { b.v }
fn ap(f: fn(lib.Box(i64)) -> i64) -> i64 { f(@import("lib.rue").Box(i64) { v: 1 }) }
fn mk() -> lib.Box(i64) { @import("lib.rue").Box(i64) { v: 10 } }
fn early(c: bool) -> lib.Box(i64) { if c { return @import("lib.rue").Box(i64) { v: 2 }; } lib.Box(i64) { v: 0 } }
fn set(inout b: lib.Box(i64)) { b = @import("lib.rue").Box(i64) { v: 20 }; }
fn main() -> i32 {
    let mut p = lib.Box(i64) { v: 0 };
    set(inout p);
    let mut h = H { b: lib.Box(i64) { v: 0 } };
    h.b = @import("lib.rue").Box(i64) { v: 5 };
    let mut xs = [lib.Box(i64) { v: 0 }, @import("lib.rue").Box(i64) { v: 4 }];
    xs[0] = @import("lib.rue").Box(i64) { v: 6 };
    let t = take(@import("lib.rue").Box(i64) { v: 3 });
    let m = h.go(@import("lib.rue").Box(i64) { v: 7 });
    @intCast(p.v + h.b.v + xs[0].v + xs[1].v + t + m + ap(take) + mk().v + early(true).v)
}
"#,
    );
}

// --- Comparisons: require_comparison_operand -------------------------------

#[test]
fn backstop_rejects_a_mismatched_comparison_operand() {
    assert_backstop_rejects(
        "scalar equality",
        r#"
fn main() -> i32 { let n: i64 = 3; if (n == @import("lib.rue").Box(i64) { v: 3 }) { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected i64, found Box(i64)",
    );
    assert_backstop_rejects(
        "scalar ordering",
        r#"
fn main() -> i32 { let n: i64 = 3; if (n < @import("lib.rue").Box(i64) { v: 3 }) { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected i64, found Box(i64)",
    );
}

/// A string-family left operand admits any string-family right operand, and
/// only those: a struct beside a `StrBuf` reached `StrBuf::equals_borrowed`,
/// which read it as a string (RUE-2438).
#[test]
fn backstop_rejects_a_struct_beside_a_string_operand() {
    assert_backstop_rejects(
        "StrBuf equality",
        r#"
const sb = @import("std/strbuf.rue");
fn main() -> i32 { let s = sb.owned("hi"); if (s == @import("lib.rue").Box(i64) { v: 1 }) { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected StrBuf, found Box(i64)",
    );
    assert_backstop_rejects(
        "str inequality",
        r#"
fn main() -> i32 { let s: str = "hi"; if (s != @import("lib.rue").Box(i64) { v: 1 }) { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected str, found Box(i64)",
    );
}

/// Control: a right operand of the left's type, reached through the inline
/// head so its inferred type is `<error>`, compares.
#[test]
fn backstop_admits_a_same_typed_comparison_operand() {
    assert_backstop_admits(
        "scalar comparisons",
        r#"
fn main() -> i32 {
    let n: i64 = 3;
    let e = n == @import("lib.rue").Box(i64) { v: 3 }.v;
    let o = n < @import("lib.rue").Box(i64) { v: 4 }.v;
    if e && o { 1 } else { 2 }
}
"#,
    );
    assert_backstop_admits(
        "string comparisons",
        r#"
const sb = @import("std/strbuf.rue");
fn main() -> i32 {
    let s = sb.owned("hi");
    let t: str = "hi";
    let a = s == @import("lib.rue").Box(sb.StrBuf) { v: sb.owned("hi") }.v;
    let b = t != @import("lib.rue").Box(str) { v: "ho" }.v;
    if a && b { 1 } else { 2 }
}
"#,
    );
}

// --- Operators: require_operand_type ---------------------------------------

#[test]
fn backstop_rejects_a_non_bool_condition() {
    assert_backstop_rejects(
        "if condition",
        r#"
fn main() -> i32 { if (@import("lib.rue").Box(i64) { v: 3 }) { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected bool, found Box(i64)",
    );
    assert_backstop_rejects(
        "while condition",
        r#"
fn main() -> i32 { while (@import("lib.rue").Box(i64) { v: 3 }) { } 0 }
"#,
        "E0206",
        "type mismatch: expected bool, found Box(i64)",
    );
}

#[test]
fn backstop_rejects_a_non_bool_logical_operand() {
    assert_backstop_rejects(
        "! operand",
        r#"
fn main() -> i32 { let x = !@import("lib.rue").Box(i64) { v: 3 }; if x { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected bool, found Box(i64)",
    );
    assert_backstop_rejects(
        "&& right operand",
        r#"
fn main() -> i32 { let c = true; if (c && (@import("lib.rue").Box(i64) { v: 3 })) { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected bool, found Box(i64)",
    );
    assert_backstop_rejects(
        "&& left operand",
        r#"
fn main() -> i32 { let c = true; if ((@import("lib.rue").Box(i64) { v: 3 }) && c) { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected bool, found Box(i64)",
    );
    assert_backstop_rejects(
        "|| left operand",
        r#"
fn main() -> i32 { let c = false; if ((@import("lib.rue").Box(i64) { v: 3 }) || c) { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected bool, found Box(i64)",
    );
    assert_backstop_rejects(
        "|| right operand",
        r#"
fn main() -> i32 { let c = false; if (c || (@import("lib.rue").Box(i64) { v: 3 })) { 1 } else { 2 } }
"#,
        "E0206",
        "type mismatch: expected bool, found Box(i64)",
    );
}

/// A unary `-` whose inferred type is `<error>` takes the analyzed operand's
/// type and holds it to the negation rule, so a struct is E0801 rather than
/// the E1403 it used to reach at output publication.
#[test]
fn backstop_rejects_a_negated_struct() {
    assert_backstop_rejects(
        "unary -",
        r#"
fn main() -> i32 { let x = -@import("lib.rue").Box(i64) { v: 3 }; 0 }
"#,
        "E0801",
        "cannot apply unary operator `-` to type 'Box(i64)'",
    );
}

/// Control: correctly-typed operands reached through the inline head, so
/// their inferred types are `<error>`, pass every operator check.
#[test]
fn backstop_admits_well_typed_operator_operands() {
    assert_backstop_admits(
        "operator operands",
        r#"
fn main() -> i32 {
    let c = true;
    let a = !(@import("lib.rue").Box(bool) { v: false }.v);
    let b = c && (@import("lib.rue").Box(bool) { v: true }.v);
    let d = (@import("lib.rue").Box(bool) { v: false }.v) || c;
    let n = -(@import("lib.rue").Box(i64) { v: 3 }.v);
    while (@import("lib.rue").Box(bool) { v: false }.v) { }
    if (@import("lib.rue").Box(bool) { v: a && b && d }.v) { @intCast(-n) } else { 2 }
}
"#,
    );
}
