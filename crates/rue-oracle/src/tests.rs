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

#[test]
fn string_still_unsupported() {
    // Strings (heap builtin) are a later slice; must report cleanly, not crash.
    let src = "fn main() -> i32 {
        let mut s = String::new();
        s.push_str(\"hi\");
        let n: u64 = s.len();
        if n == 2 { 0 } else { 1 }
    }";
    assert!(run_source(src).is_err());
}
