//! Correctness corpus for the reference interpreter. Each case asserts the
//! oracle's observable behavior (exit code, `@dbg` output, panic-or-not) matches
//! the value the language semantics require — which is also what the compiled
//! binary must produce. Agreement here is the differential check in miniature;
//! `tests/differential.rs`-style wiring against the real binary across the whole
//! CLI/spec corpus is the next step (RUE-50).

use super::*;

fn run(src: &str) -> Outcome {
    run_source(src).unwrap_or_else(|error| panic!("oracle failed: {error}"))
}

fn run_with_budget(src: &str, budget: u64) -> Result<Outcome, Unsupported> {
    let state = compile_to_cfg(src).unwrap_or_else(|e| panic!("compile error: {e:#?}"));
    run_state_with_budget(state, budget)
}

fn run_with_stdout_cap(src: &str, stdout_cap: usize) -> Result<Outcome, Unsupported> {
    let state = compile_to_cfg(src).unwrap_or_else(|e| panic!("compile error: {e:#?}"));
    run_state_with_limits(state, STEP_BUDGET, stdout_cap)
}

fn run_string_trio(src: &str) -> Outcome {
    let mut preview_features = PreviewFeatures::new();
    preview_features.insert(rue_compiler::PreviewFeature::StringTrio);
    run_source_with_preview_features(src, &preview_features)
        .unwrap_or_else(|error| panic!("oracle failed: {error}"))
}

fn expect_unsupported(src: &str) -> Unsupported {
    match run_source(src) {
        Err(RunSourceError::Unsupported(unsupported)) => unsupported,
        Err(RunSourceError::Compile(errors)) => {
            panic!("expected oracle-unsupported source, but it failed to compile: {errors:#?}")
        }
        Ok(outcome) => panic!("expected oracle-unsupported source, got {outcome:?}"),
    }
}

fn exit(src: &str) -> i32 {
    run(src).exit_code
}

#[test]
fn returns_literal() {
    assert_eq!(exit("fn main() -> i32 { 42 }"), 42);
}

#[test]
fn arithmetic_precedence() {
    assert_eq!(exit("fn main() -> i32 { 2 + 3 * 4 }"), 14);
    assert_eq!(exit("fn main() -> i32 { (2 + 3) * 4 }"), 20);
    assert_eq!(exit("fn main() -> i32 { 100 / 7 }"), 14);
    assert_eq!(exit("fn main() -> i32 { 100 % 7 }"), 2);
}

#[test]
fn locals_and_shadowing() {
    let src = "fn main() -> i32 {
        let x = 1;
        let y = { let x = 10; x + 5 };
        x + y
    }";
    assert_eq!(exit(src), 16);
}

#[test]
fn if_expression_value() {
    assert_eq!(exit("fn main() -> i32 { if true { 42 } else { 0 } }"), 42);
    assert_eq!(
        exit("fn main() -> i32 { let n = 5; if n > 3 { 100 } else { 0 } }"),
        100
    );
}

#[test]
fn dominated_ssa_value_survives_nested_control_flow() {
    // The first short-circuit expression produces a block parameter and `!`
    // derives a value from it. That derived value dominates the second
    // short-circuit expression and must remain available across its blocks.
    let src = "fn main() -> i32 {
        let f = false;
        let t = true;
        if (!(f && f)) != (!(t || t)) { 0 } else { 1 }
    }";

    assert_eq!(exit(src), 0);
}

#[test]
fn pre_branch_load_remains_an_evaluation_snapshot() {
    // Rue evaluates the left operand before the right-hand `if`. Its load of
    // `x` is an SSA value: entering the branch must not discard and recompute
    // it after the branch mutates the local.
    let src = "fn main() -> i32 {
        let mut x = 1;
        let take = true;
        let result: i32 = x + (if take {
            x = 2;
            10
        } else {
            20
        });
        result
    }";

    assert_eq!(exit(src), 11);
}

#[test]
fn loop_reentry_recomputes_the_current_blocks() {
    // A persistent cache must still invalidate a block's own values when that
    // block is re-entered. Keep the test budget small so a stale loop condition
    // fails promptly instead of consuming the production 50M-step budget.
    let src = "fn main() -> i32 {
        let mut i = 0;
        let mut sum = 0;
        while i < 3 {
            sum = sum + i;
            i = i + 1;
        }
        sum
    }";

    let out =
        run_with_budget(src, 1_000).unwrap_or_else(|u| panic!("bounded loop unsupported: {}", u.0));
    assert_eq!(out.exit_code, 3);
}

#[test]
fn calls_and_recursion() {
    let src = "fn factorial(n: i32) -> i32 {
        if n <= 1 { 1 } else { n * factorial(n - 1) }
    }
    fn main() -> i32 { factorial(5) }";
    assert_eq!(exit(src), 120);
}

#[test]
fn forward_reference() {
    let src = "fn main() -> i32 { helper() }
    fn helper() -> i32 { 42 }";
    assert_eq!(exit(src), 42);
}

#[test]
fn early_return() {
    let src = "fn abs(x: i32) -> i32 {
        if x < 0 { return 0 - x; }
        x
    }
    fn main() -> i32 { abs(0 - 5) }";
    assert_eq!(exit(src), 5);
}

#[test]
fn dbg_output() {
    let out = run("fn main() -> i32 { @dbg(7); @dbg(true); 0 }");
    assert_eq!(out.stdout, "7\ntrue\n");
    assert_eq!(out.exit_code, 0);
}

#[test]
fn stdout_at_cap_remains_exact() {
    let out = run_with_stdout_cap("fn main() -> i32 { @dbg(7); @dbg(true); 0 }", 7)
        .unwrap_or_else(|unsupported| panic!("exactly capped output failed: {unsupported}"));
    assert_eq!(out.stdout, "7\ntrue\n");
}

#[test]
fn repeated_dbg_over_stdout_cap_is_unsupported() {
    let unsupported = run_with_stdout_cap("fn main() -> i32 { @dbg(7); @dbg(8); 0 }", 3)
        .expect_err("the second complete output line must exceed the cap");
    assert_eq!(unsupported.0, "stdout byte limit exceeded (3-byte limit)");
}

#[test]
fn oversized_string_dbg_is_unsupported() {
    let unsupported = run_with_stdout_cap("fn main() -> i32 { let s = \"hello\"; @dbg(s); 0 }", 5)
        .expect_err("the String plus its newline must exceed the cap");
    assert_eq!(unsupported.0, "stdout byte limit exceeded (5-byte limit)");
}

#[test]
fn stdout_cap_counts_raw_bytes_before_lossy_rendering() {
    let src = "fn main() -> i32 {
        let mut s = String.new();
        s.push(195);
        @dbg(s);
        0
    }";
    let out = run_with_stdout_cap(src, 2)
        .unwrap_or_else(|unsupported| panic!("two raw output bytes failed: {unsupported}"));
    assert_eq!(out.stdout, "\u{fffd}\n");

    let unsupported = run_with_stdout_cap(src, 1)
        .expect_err("one content byte plus newline must exceed a one-byte cap");
    assert_eq!(unsupported.0, "stdout byte limit exceeded (1-byte limit)");
}

#[test]
fn preview_features_reach_oracle_frontend() {
    let src = r#"fn main() -> i32 {
        let s: str = "hi";
        0
    }"#;

    assert!(
        matches!(run_source(src), Err(RunSourceError::Compile(_))),
        "str should stay gated by default with a compile error"
    );
    assert_eq!(run_string_trio(src).exit_code, 0);
}

#[test]
fn compile_errors_are_distinct_from_unsupported_programs() {
    let error = run_source("fn main() -> i32 { missing }")
        .expect_err("an undefined name must fail compilation");

    match error {
        RunSourceError::Compile(errors) => {
            assert!(!errors.is_empty(), "compile failure must carry diagnostics");
        }
        RunSourceError::Unsupported(unsupported) => {
            panic!("compile failure was misclassified as unsupported: {unsupported}");
        }
    }
}

#[test]
fn valid_unmodeled_program_is_unsupported_not_a_compile_error() {
    let src = "fn main() -> i32 {
        let value: u32 = @random_u32();
        if value == 0 { 0 } else { 1 }
    }";

    let unsupported = expect_unsupported(src);
    assert_eq!(unsupported.0, "intrinsic @random_u32");
}

#[test]
fn str_arguments_use_two_abi_slots() {
    let src = r#"fn take(s: str, n: i32) -> i32 {
        n
    }
    fn main() -> i32 {
        take("hi", 7)
    }"#;

    assert_eq!(run_string_trio(src).exit_code, 7);
}

#[test]
fn wider_int_dbg_and_unsigned() {
    let out = run("fn main() -> i32 { let x: u64 = 18446744073709551615; @dbg(x); 0 }");
    assert_eq!(out.stdout, "18446744073709551615\n");
}

#[test]
fn overflow_traps() {
    let out = run("fn main() -> i32 {
        let a: i32 = 2147483647;
        let b: i32 = a + 1;
        b
    }");
    assert_eq!(out.exit_code, 101);
    assert!(out.panic.as_deref() == Some("arithmetic overflow"));
}

#[test]
fn divide_by_zero_traps() {
    let out = run("fn main() -> i32 { let z = 0; 10 / z }");
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.panic.as_deref(), Some("divide by zero"));
}

#[test]
fn bitwise_and_shift() {
    assert_eq!(exit("fn main() -> i32 { 0xF0 & 0x0F }"), 0);
    assert_eq!(exit("fn main() -> i32 { 0xF0 | 0x0F }"), 255);
    assert_eq!(exit("fn main() -> i32 { 1 << 4 }"), 16);
    assert_eq!(exit("fn main() -> i32 { 256 >> 2 }"), 64);
    assert_eq!(exit("fn main() -> i32 { 6 ^ 3 }"), 5);
}

#[test]
fn intcast_in_range_and_overflow() {
    assert_eq!(
        exit("fn main() -> i32 { let x: i64 = 100; @intCast(x) }"),
        100
    );
    let out = run("fn main() -> i32 {
        let x: i64 = 4294967296;
        let y: i32 = @intCast(x);
        y
    }");
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.panic.as_deref(), Some("integer cast overflow"));
}

#[test]
fn unbounded_recursion_is_unsupported() {
    // A recursive function with no base case would recurse forever and overflow
    // the Rust stack (an uncatchable abort). The shared recursion-depth bound
    // (MAX_DEPTH) must turn it into a clean `Unsupported` skip instead (RUE-340).
    let src = "fn r(n: i32) -> i32 { r(n) }
    fn main() -> i32 { r(0) }";
    let err = expect_unsupported(src);
    assert!(
        err.0.contains("depth budget"),
        "expected a depth-budget skip, got: {}",
        err.0
    );
}

#[test]
fn deep_bounded_recursion_is_unsupported() {
    // Recursion that is bounded but far deeper than MAX_DEPTH must also skip
    // cleanly (hitting the depth bound) rather than overflow the stack.
    let src = "fn r(n: i32) -> i32 { if n <= 0 { 0 } else { r(n - 1) } }
    fn main() -> i32 { r(100000) }";
    let err = expect_unsupported(src);
    assert!(
        err.0.contains("depth budget"),
        "expected a depth-budget skip, got: {}",
        err.0
    );
}

#[test]
fn shallow_recursion_still_runs() {
    // Recursion well within MAX_DEPTH must still execute and agree, so the depth
    // bound doesn't reject legitimate programs. Fibonacci(15) = 610.
    let src = "fn fib(n: i32) -> i32 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
    fn main() -> i32 { fib(15) & 0xFF }";
    // 610 & 0xFF = 98.
    assert_eq!(exit(src), 98);
    // And a linear recursion right up to (just under) the depth bound resolves.
    let src = "fn r(n: i32) -> i32 { if n <= 0 { 7 } else { r(n - 1) } }
    fn main() -> i32 { r(1500) }";
    assert_eq!(exit(src), 7);
}

// Ignored by default: exercises the full 50M-step budget, which takes ~12s in
// an unoptimized build. Run explicitly (`--ignored`) as a loop-bounding
// regression guard.
#[test]
#[ignore = "burns the full step budget (~12s in debug); run with --ignored"]
fn runaway_loop_is_unsupported() {
    // A loop that never terminates must be bounded by the shared step budget and
    // reported `Unsupported`, never hang. Each iteration flips `b` (a `Not`
    // instruction), so the budget is decremented every turn of the loop.
    let src = "fn main() -> i32 { let mut b = true; loop { b = !b; } }";
    let err = expect_unsupported(src);
    assert!(
        err.0.contains("step budget"),
        "expected a step-budget skip, got: {}",
        err.0
    );
}

#[test]
fn loop_via_recursion_sum() {
    // Gauss sum 1..=10 = 55, exercised through recursion + branch joins.
    let src = "fn sum(n: i32, acc: i32) -> i32 {
        if n == 0 { acc } else { sum(n - 1, acc + n) }
    }
    fn main() -> i32 { sum(10, 0) }";
    assert_eq!(exit(src), 55);
}

#[test]
fn struct_field_access() {
    let src = "struct P { x: i32, y: i32 }
    fn main() -> i32 { let p = P { x: 1, y: 2 }; p.x + p.y }";
    assert_eq!(exit(src), 3);
}

#[test]
fn nested_struct() {
    let src = "struct Inner { v: i32 }
    struct Outer { a: Inner, b: Inner }
    fn main() -> i32 {
        let o = Outer { a: Inner { v: 10 }, b: Inner { v: 32 } };
        o.a.v + o.b.v
    }";
    assert_eq!(exit(src), 42);
}

#[test]
fn struct_passed_and_returned() {
    let src = "struct P { x: i32, y: i32 }
    fn sum(p: P) -> i32 { p.x + p.y }
    fn make() -> P { P { x: 20, y: 22 } }
    fn main() -> i32 { sum(make()) }";
    assert_eq!(exit(src), 42);
}

#[test]
fn array_indexing() {
    let src = "fn main() -> i32 {
        let a: [i32; 3] = [4, 5, 6];
        a[0] + a[2]
    }";
    assert_eq!(exit(src), 10);
}

#[test]
fn array_out_of_bounds_traps() {
    // A constant OOB index still lowers to a checked read at O0.
    let src = "fn get(a: [i32; 3], i: usize) -> i32 { a[i] }
    fn main() -> i32 {
        let a: [i32; 3] = [1, 2, 3];
        let i: usize = 5;
        get(a, i)
    }";
    let out = run(src);
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.panic.as_deref(), Some("index out of bounds"));
}

#[test]
fn struct_field_mutation() {
    let src = "struct P { x: i32, y: i32 }
    fn main() -> i32 {
        let mut p = P { x: 1, y: 2 };
        p.x = 40;
        p.x + p.y
    }";
    assert_eq!(exit(src), 42);
}

#[test]
fn inout_scalar() {
    let src = "fn increment(inout x: i32) { x = x + 1; }
    fn main() -> i32 {
        let mut n = 10;
        increment(inout n);
        n
    }";
    assert_eq!(exit(src), 11);
}

#[test]
fn inout_swap() {
    let src = "fn swap(inout a: i32, inout b: i32) {
        let tmp = a;
        a = b;
        b = tmp;
    }
    fn main() -> i32 {
        let mut x = 3;
        let mut y = 7;
        swap(inout x, inout y);
        x * 10 + y
    }";
    // after swap: x=7, y=3 -> 73
    assert_eq!(exit(src), 73);
}

#[test]
fn borrow_read_only() {
    let src = "struct Point { x: i32, y: i32 }
    fn sum_coords(borrow p: Point) -> i32 { p.x + p.y }
    fn main() -> i32 {
        let p = Point { x: 10, y: 32 };
        let r = sum_coords(borrow p);
        r + p.x - p.x
    }";
    assert_eq!(exit(src), 42);
}

#[test]
fn inout_struct_field() {
    let src = "struct P { x: i32, y: i32 }
    fn bump(inout p: P) { p.x = p.x + 100; }
    fn main() -> i32 {
        let mut p = P { x: 1, y: 2 };
        bump(inout p);
        p.x + p.y
    }";
    // p.x becomes 101 -> 101 + 2 = 103
    assert_eq!(exit(src), 103);
}

#[test]
fn destructor_runs_at_scope_exit() {
    let src = "struct D { v: i32 }
    drop fn D(self) { @dbg(self.v); }
    fn main() -> i32 {
        let d = D { v: 7 };
        @dbg(100);
        0
    }";
    assert_eq!(run(src).stdout, "100\n7\n");
}

#[test]
fn locals_drop_in_reverse_order() {
    let src = "struct D { v: i32 }
    drop fn D(self) { @dbg(self.v); }
    fn main() -> i32 {
        let a = D { v: 1 };
        let b = D { v: 2 };
        0
    }";
    // LIFO: b dropped before a.
    assert_eq!(run(src).stdout, "2\n1\n");
}

#[test]
fn destructor_then_field_drop_order() {
    let src = "struct A { x: i32 }
    struct O { a: A, b: i32 }
    drop fn A(self) { @dbg(self.x); }
    drop fn O(self) { @dbg(800); }
    fn main() -> i32 {
        let o = O { a: A { x: 5 }, b: 9 };
        0
    }";
    // O's destructor first, then field `a`'s destructor (b is trivially droppable).
    assert_eq!(run(src).stdout, "800\n5\n");
}

#[test]
fn array_elements_drop_ascending() {
    let src = "struct D { v: i32 }
    drop fn D(self) { @dbg(self.v); }
    fn main() -> i32 {
        let xs = [D { v: 1 }, D { v: 2 }, D { v: 3 }];
        0
    }";
    assert_eq!(run(src).stdout, "1\n2\n3\n");
}

#[test]
fn moved_value_drops_once_at_destination() {
    let src = "struct D { v: i32 }
    drop fn D(self) { @dbg(self.v); }
    fn sink(d: D) -> i32 { d.v }
    fn main() -> i32 {
        let d = D { v: 42 };
        sink(d);
        0
    }";
    // d is moved into sink; it drops exactly once, inside sink at its scope exit.
    assert_eq!(run(src).stdout, "42\n");
}

// --- regressions found by the differential harness (rue-oracle-diff) ---

#[test]
fn i32_min_negated_literal() {
    // -2147483648 is stored as the 64-bit sign-extended bit pattern; the Const
    // must reinterpret it as signed, not as a huge positive.
    let src = "fn main() -> i32 {
        let a: i32 = -2147483648;
        let b: i32 = 1;
        a / b - a
    }";
    // a/b = i32::MIN; i32::MIN - i32::MIN = 0.
    assert_eq!(exit(src), 0);
}

#[test]
fn min_mod_neg_one_traps() {
    // i32::MIN % -1 faults at the operand width even though the math is 0.
    let src = "fn main() -> i32 {
        let a: i32 = -2147483648;
        let b: i32 = -1;
        a % b
    }";
    assert_eq!(run(src).exit_code, 101);
}

#[test]
fn string_equality_by_content() {
    let src = "fn main() -> i32 {
        let mut a = String.new();
        a.push_str(\"hi\");
        let mut b = String.new();
        b.push_str(\"hi\");
        if a == b { 7 } else { 0 }
    }";
    assert_eq!(exit(src), 7);
}

#[test]
fn string_build_and_len() {
    let src = "fn main() -> i32 {
        let mut s = String.new();
        s.push_str(\"hi\");
        let n: u64 = s.len();
        if n == 2 { 0 } else { 1 }
    }";
    assert_eq!(exit(src), 0);
}

#[test]
fn string_dbg_and_concat() {
    let src = "fn main() -> i32 {
        let mut s = String.new();
        s.push_str(\"foo\");
        s.push_str(\"bar\");
        @dbg(s);
        0
    }";
    assert_eq!(run(src).stdout, "foobar\n");
}

#[test]
fn string_literal_dbg() {
    let src = "fn main() -> i32 {
        let s = \"hello\";
        @dbg(s);
        0
    }";
    assert_eq!(run(src).stdout, "hello\n");
}

// --- deterministic intrinsics & String primitives (RUE-341) ---

#[test]
fn target_arch_and_os_fold_to_host() {
    // `@target_arch()`/`@target_os()` are folded to a compile-time `EnumVariant`
    // in sema against `Target::host()`. The oracle runs on that same host, so the
    // folded discriminant must agree with this test binary's own host `cfg`.
    let src = "fn main() -> i32 {
        let a = match @target_arch() { Arch.X86_64 => 1, Arch.Aarch64 => 2 };
        let o = match @target_os() { Os.Linux => 10, Os.Macos => 20 };
        a + o
    }";
    let expected_arch = if cfg!(target_arch = "x86_64") { 1 } else { 2 };
    let expected_os = if cfg!(target_os = "linux") { 10 } else { 20 };
    assert_eq!(exit(src), expected_arch + expected_os);
}

#[test]
fn string_byte_indexing() {
    // `s[i]` lowers to `__rue_String_byte_at`. "café" is c=99, a=97, f=102, then
    // the two UTF-8 bytes of é: 0xC3=195, 0xA9=169 (matches the spec corpus case
    // `string_index_utf8_bytes`).
    let src = "fn main() -> i32 {
        let s = \"café\";
        @dbg(s[0]);
        @dbg(s[3]);
        @dbg(s[4]);
        0
    }";
    assert_eq!(run(src).stdout, "99\n195\n169\n");
}

#[test]
fn string_byte_index_out_of_bounds_traps() {
    // An in-`u8`-range but out-of-`len` byte index traps like array indexing.
    let src = "fn main() -> i32 {
        let s = \"café\";
        @dbg(s[5]);
        0
    }";
    let out = run(src);
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.panic.as_deref(), Some("index out of bounds"));
}

#[test]
fn string_byte_iteration_counts_bytes() {
    // Byte view (`for _b in s`): "café" is 5 bytes (é is 2 UTF-8 bytes). Uses
    // `__rue_String_byte_at` for the element and `__rue_String_len` for the bound.
    let src = "fn main() -> i32 {
        let s = \"café\";
        let mut count = 0;
        for _b in s {
            count = count + 1;
        }
        count
    }";
    assert_eq!(exit(src), 5);
}

#[test]
fn string_push_models_raw_non_utf8_byte() {
    // `String.push` appends one byte, not one Unicode scalar. 0xC3 alone is
    // not valid UTF-8, but the byte-string model permits storing it; byte
    // operations must still see the raw byte.
    let src = "fn main() -> i32 {
        let mut s = String.new();
        s.push(195);
        @dbg(s.len());
        @dbg(s[0]);
        0
    }";
    assert_eq!(run(src).stdout, "1\n195\n");
}

#[test]
fn string_chars_traps_on_raw_invalid_utf8_byte() {
    // Strict `.chars()` is the UTF-8 validation boundary for byte strings.
    let src = "fn main() -> i32 {
        let mut s = String.new();
        s.push(195);
        for c in s.chars() {
            @dbg(c);
        }
        0
    }";
    let out = run(src);
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.panic.as_deref(), Some("invalid UTF-8"));
}

#[test]
fn string_chars_lossy_replaces_raw_invalid_utf8_byte() {
    // The lossy view mirrors the runtime's replacement behavior: a lone invalid
    // lead byte becomes U+FFFD and advances by one byte.
    let src = "fn main() -> i32 {
        let mut s = String.new();
        s.push(195);
        let mut count = 0;
        for c in s.chars_lossy() {
            @dbg(c);
            count = count + 1;
        }
        count
    }";
    let out = run(src);
    assert_eq!(out.stdout, "65533\n");
    assert_eq!(out.exit_code, 1);
}

#[test]
fn string_chars_iteration_scalars() {
    // Scalar view (`for c in s.chars()`): yields Unicode scalars via
    // `__rue_String_char_scalar` and advances via `__rue_String_char_next`.
    // "café" → c=99, a=97, f=102, é=U+00E9=233; 4 scalars (matches the spec
    // corpus case `for_chars_scalars`).
    let src = "fn main() -> i32 {
        let s = \"café\";
        let mut count = 0;
        for c in s.chars() {
            @dbg(c);
            count = count + 1;
        }
        count
    }";
    let out = run(src);
    assert_eq!(out.stdout, "99\n97\n102\n233\n");
    assert_eq!(out.exit_code, 4);
}

#[test]
fn string_chars_lossy_matches_strict_over_valid_utf8() {
    // Over well-formed UTF-8 the lossy view decodes identically to `.chars()`.
    let src = "fn main() -> i32 {
        let s = \"az€\";
        let mut count = 0;
        for c in s.chars_lossy() {
            @dbg(c);
            count = count + 1;
        }
        count
    }";
    // a=97, z=122, €=U+20AC=8364; 3 scalars.
    let out = run(src);
    assert_eq!(out.stdout, "97\n122\n8364\n");
    assert_eq!(out.exit_code, 3);
}

#[test]
fn string_is_empty_and_clear() {
    let src = "fn main() -> i32 {
        let mut s = String.new();
        s.push_str(\"x\");
        let a: bool = s.is_empty();
        s.clear();
        let b: bool = s.is_empty();
        if !a { if b { 0 } else { 1 } } else { 2 }
    }";
    assert_eq!(exit(src), 0);
}

#[test]
fn zst_param_before_scalar_does_not_shift_slots() {
    // A zero-sized argument occupies zero ABI slots (abi_slot_count), so the
    // scalar after it lives at slot 0, not slot 1. The oracle used to force
    // every argument to at least one slot and read 0 here — a phantom
    // DISAGREE blaming codegen (rue-oracle component review, 2026-07-04).
    let src = "struct E {}
    fn pick(e: E, n: i32) -> i32 { n }
    fn main() -> i32 { let e = E {}; pick(e, 42) }";
    assert_eq!(exit(src), 42);
}

#[test]
fn zst_param_forwarded_through_two_calls() {
    // Forwarding the ZST through another call keeps the layout consistent at
    // every level.
    let src = "struct E {}
    fn pick(e: E, n: i32) -> i32 { n }
    fn wrap(e: E, n: i32) -> i32 { pick(e, n + 1) }
    fn main() -> i32 { let e = E {}; wrap(e, 41) }";
    assert_eq!(exit(src), 42);
}
