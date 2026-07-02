//! Correctness corpus for the reference interpreter. Each case asserts the
//! oracle's observable behavior (exit code, `@dbg` output, panic-or-not) matches
//! the value the language semantics require — which is also what the compiled
//! binary must produce. Agreement here is the differential check in miniature;
//! `tests/differential.rs`-style wiring against the real binary across the whole
//! CLI/spec corpus is the next step (RUE-50).

use super::*;

fn run(src: &str) -> Outcome {
    run_source(src).unwrap_or_else(|u| panic!("unsupported: {}", u.0))
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
fn loop_via_recursion_sum() {
    // Gauss sum 1..=10 = 55, exercised through recursion + branch joins.
    let src = "fn sum(n: i32, acc: i32) -> i32 {
        if n == 0 { acc } else { sum(n - 1, acc + n) }
    }
    fn main() -> i32 { sum(10, 0) }";
    assert_eq!(exit(src), 55);
}

#[test]
fn unsupported_is_reported_not_panicked() {
    // Structs are a later slice; the oracle must say so cleanly, not crash.
    let src = "struct P { x: i32, y: i32 }
    fn main() -> i32 { let p = P { x: 1, y: 2 }; p.x + p.y }";
    assert!(run_source(src).is_err());
}
