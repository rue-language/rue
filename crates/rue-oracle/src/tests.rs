//! Correctness corpus for the reference interpreter. Each case asserts the
//! oracle's observable behavior (exit code, stdout, stderr, trap cause) matches
//! the value the language semantics require — which is also what the compiled
//! binary must produce. Agreement here is the differential check in miniature;
//! `tests/differential.rs`-style wiring against the real binary across the whole
//! CLI/spec corpus is the next step (RUE-50).

use super::*;
use rue_compiler::PreviewFeature;

mod call_contracts;
mod place_contracts;

fn run(src: &str) -> Outcome {
    run_source(src).unwrap_or_else(|error| panic!("oracle failed: {error}"))
}

fn run_with_budget(src: &str, budget: u64) -> Result<Outcome, Unsupported> {
    let state = query_cfg_state(src).unwrap_or_else(|e| panic!("compile error: {e:#?}"));
    run_state_with_budget(state, budget)
}

fn run_with_stdout_cap(src: &str, stdout_cap: usize) -> Result<Outcome, Unsupported> {
    run_with_output_caps(src, stdout_cap, MAX_STDERR_BYTES)
}

fn run_with_output_caps(
    src: &str,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<Outcome, Unsupported> {
    let state = query_cfg_state(src).unwrap_or_else(|e| panic!("compile error: {e:#?}"));
    run_state_with_output_limits(state, STEP_BUDGET, stdout_cap, stderr_cap)
}

fn run_test_preview(src: &str) -> Outcome {
    let preview_features = PreviewFeatures::from([PreviewFeature::TestInfra]);
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
fn stable_str_equality_is_lowered_and_modeled_by_byte_content() {
    let source = r#"fn equal(left: str, right: str) -> bool { left == right }
        fn different(left: str, right: str) -> bool { left != right }
        fn main() -> i32 {
            if equal("same", "same") {
                if different("same", "other") { 0 } else { 1 }
            } else { 2 }
        }"#;
    let state = query_cfg_state(source).expect("stable str equality probe must compile");
    for (function, expected) in [("equal", "Eq"), ("different", "Ne")] {
        let cfg = state
            .functions
            .iter()
            .find(|candidate| candidate.is_source_named(function))
            .map(|candidate| &candidate.cfg)
            .unwrap_or_else(|| panic!("missing {function} CFG"));
        let (lhs, rhs) = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| match cfg.get_inst(value).data {
                CfgInstData::Eq(lhs, rhs) if expected == "Eq" => Some((lhs, rhs)),
                CfgInstData::Ne(lhs, rhs) if expected == "Ne" => Some((lhs, rhs)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {expected} in {function}"));
        for operand in [lhs, rhs] {
            let TypeKind::Struct(struct_id) = cfg.get_inst(operand).ty.kind() else {
                panic!("{function} operand is not the stable str nominal")
            };
            assert_eq!(&*state.type_pool.struct_def(struct_id).name, "str");
        }
    }
    let outcome = run_state(state).expect("stable str equality must be modeled");
    assert_eq!(outcome.exit_code, 0);
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
        run_with_budget(src, 1_000).unwrap_or_else(|u| panic!("bounded loop unsupported: {u}"));
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
    assert_eq!(out.stderr, "");
    assert_eq!(out.exit_code, 0);
}

#[test]
fn panic_and_assert_have_exact_observable_semantics() {
    let out = run("fn main() -> i32 { @panic(); 0 }");
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.stdout, "");
    assert_eq!(out.stderr, "panic\n");
    assert_eq!(out.panic, Some(TrapKind::UserPanic));

    let out = run(r#"fn main() -> i32 { @panic("boom"); 0 }"#);
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.stderr, "panic: boom\n");
    assert_eq!(out.panic, Some(TrapKind::UserPanic));

    let out = run(r#"fn main() -> i32 { @panic(""); 0 }"#);
    assert_eq!(out.stderr, "panic: \n");
    assert_eq!(out.panic, Some(TrapKind::UserPanic));

    let out = run("fn main() -> i32 { @assert(true); 42 }");
    assert_eq!(out.exit_code, 42);
    assert_eq!(out.stderr, "");
    assert_eq!(out.panic, None);

    let out = run("fn main() -> i32 { @assert(false); 0 }");
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.stderr, "assertion failed\n");
    assert_eq!(out.panic, Some(TrapKind::AssertionFailure));

    let out = run(r#"fn main() -> i32 { @assert(1 == 2, "not equal"); 0 }"#);
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.stderr, "panic: not equal\n");
    assert_eq!(out.panic, Some(TrapKind::UserPanic));
}

#[test]
fn assert_eagerly_evaluates_condition_then_message_even_when_true() {
    let out = run(r#"fn condition() -> bool { @dbg(1); true }
        fn message() -> str { @dbg(2); "unused" }
        fn main() -> i32 { @assert(condition(), message()); 42 }"#);
    assert_eq!(out.exit_code, 42);
    assert_eq!(out.stdout, "1\n2\n");
    assert_eq!(out.stderr, "");
    assert_eq!(out.panic, None);
}

#[test]
fn stdout_and_stderr_limits_are_independent_and_fail_closed() {
    let source = "fn main() -> i32 {
        @dbg(7);
        let max: i32 = 2147483647;
        max + 1
    }";
    let out = run_with_output_caps(source, 2, 24)
        .unwrap_or_else(|unsupported| panic!("exactly capped streams failed: {unsupported}"));
    assert_eq!(out.stdout, "7\n");
    assert_eq!(out.stderr, "error: integer overflow\n");

    let unsupported =
        run_with_output_caps(source, 1, 24).expect_err("stdout overflow must fail before the trap");
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes)
    );

    let unsupported = run_with_output_caps(source, 2, 23)
        .expect_err("the complete runtime diagnostic must exceed the stderr cap");
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StderrBytes)
    );
    assert_eq!(
        unsupported.detail(),
        "stderr byte limit exceeded (23-byte limit)"
    );
}

#[test]
fn dynamic_panic_stderr_obeys_the_shared_raw_byte_limit() {
    let source = r#"fn main() -> i32 { @panic("x"); 0 }"#;
    let out = run_with_output_caps(source, MAX_STDOUT_BYTES, 9)
        .unwrap_or_else(|unsupported| panic!("exactly capped panic failed: {unsupported}"));
    assert_eq!(out.stderr, "panic: x\n");

    let unsupported = run_with_output_caps(source, MAX_STDOUT_BYTES, 8)
        .expect_err("panic prefix, body, and newline must all count");
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StderrBytes)
    );
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
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes)
    );
    assert_eq!(
        unsupported.detail(),
        "stdout byte limit exceeded (3-byte limit)"
    );
}

#[test]
fn oversized_string_dbg_is_unsupported() {
    let unsupported = run_with_stdout_cap("fn main() -> i32 { let s = \"hello\"; @dbg(s); 0 }", 5)
        .expect_err("the String plus its newline must exceed the cap");
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes)
    );
    assert_eq!(
        unsupported.detail(),
        "stdout byte limit exceeded (5-byte limit)"
    );
}

#[test]
fn stdout_cap_counts_utf8_bytes_not_scalar_values() {
    let source = "fn main() -> i32 { @dbg(\"é\"); 0 }";
    let out = run_with_stdout_cap(source, 3)
        .unwrap_or_else(|unsupported| panic!("two UTF-8 bytes plus newline failed: {unsupported}"));
    assert_eq!(out.stdout, "é\n");
    let unsupported = run_with_stdout_cap(source, 2)
        .expect_err("two UTF-8 bytes plus newline must exceed a two-byte cap");
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes)
    );
}

#[test]
fn preview_features_reach_oracle_frontend() {
    let src = r#"fn main() -> i32 {
        @test_preview_gate();
        0
    }"#;

    assert!(
        matches!(run_source(src), Err(RunSourceError::Compile(_))),
        "test-only intrinsic should stay gated by default with a compile error"
    );
    assert_eq!(run_test_preview(src).exit_code, 0);
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
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::RandomU32)
    );
    assert_eq!(unsupported.detail(), "intrinsic @random_u32");
}

#[test]
fn every_known_unsupported_intrinsic_has_a_closed_kind() {
    use UnsupportedIntrinsicKind as Intrinsic;

    for (name, intrinsic) in [
        ("parse_i32", Intrinsic::ParseI32),
        ("parse_i64", Intrinsic::ParseI64),
        ("parse_u32", Intrinsic::ParseU32),
        ("parse_u64", Intrinsic::ParseU64),
        ("ptr_read", Intrinsic::PointerRead),
        ("ptr_write", Intrinsic::PointerWrite),
        ("ptr_offset", Intrinsic::PointerOffset),
        ("ptr_to_int", Intrinsic::PointerToInt),
        ("int_to_ptr", Intrinsic::IntToPointer),
        ("raw", Intrinsic::RawAddress),
        ("raw_mut", Intrinsic::RawMutableAddress),
        ("field_ptr", Intrinsic::FieldPointer),
        ("alloc", Intrinsic::Allocate),
        ("free", Intrinsic::Free),
        ("realloc", Intrinsic::Reallocate),
        ("alloc_zeroed", Intrinsic::AllocateZeroed),
        ("resize", Intrinsic::Resize),
        ("byte_move", Intrinsic::ByteMove),
        ("byte_copy", Intrinsic::ByteCopy),
        ("byte_set", Intrinsic::ByteSet),
    ] {
        let expected = UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(intrinsic));
        assert_eq!(unsupported_intrinsic_kind(name), expected, "@{name}");
        assert!(
            expected.model_gap().is_some(),
            "@{name} must be registrable"
        );
    }

    for (name, dependency) in [
        ("read_line", ExternalDependencyKind::StandardInput),
        ("random_u32", ExternalDependencyKind::RandomU32),
        ("random_u64", ExternalDependencyKind::RandomU64),
        ("syscall", ExternalDependencyKind::SystemCall),
        ("arg_count", ExternalDependencyKind::ArgCount),
        ("arg_ptr", ExternalDependencyKind::ArgPtr),
        ("arg_len", ExternalDependencyKind::ArgLen),
        ("env_count", ExternalDependencyKind::EnvCount),
        ("env_ptr", ExternalDependencyKind::EnvPtr),
        ("env_len", ExternalDependencyKind::EnvLen),
    ] {
        assert_eq!(
            unsupported_intrinsic_kind(name),
            UnsupportedKind::ExternalDependency(dependency),
            "@{name}"
        );
    }

    for name in [
        "dbg",
        "panic",
        "assert",
        "drop",
        "intCast",
        "cast",
        "to_string",
        "test_preview_gate",
        "import",
        "target_arch",
        "target_os",
        "__rue_char_next",
        "unknown_future_intrinsic",
    ] {
        assert_eq!(
            unsupported_intrinsic_kind(name),
            UnsupportedKind::ContractViolation(ContractViolationKind::UnexpectedIntrinsic),
            "@{name} must fail closed if it unexpectedly survives lowering"
        );
    }
}

#[test]
fn every_known_missing_runtime_call_has_a_closed_kind() {
    use UnsupportedRuntimeCallKind as RuntimeCall;

    for (kind, expected) in [
        (RuntimeCallKind::StrPrintAggregate, RuntimeCall::Print),
        (RuntimeCallKind::StrPrintProjected, RuntimeCall::Print),
        (RuntimeCallKind::StrPrintlnAggregate, RuntimeCall::Println),
        (RuntimeCallKind::StrPrintlnProjected, RuntimeCall::Println),
    ] {
        assert_eq!(
            unsupported_runtime_call_kind(kind),
            Some(expected),
            "{kind:?}"
        );
    }
    for kind in [
        RuntimeCallKind::StrByteAt,
        RuntimeCallKind::DebugI64,
        RuntimeCallKind::Alloc,
    ] {
        assert_eq!(unsupported_runtime_call_kind(kind), None, "{kind:?}");
    }
}

#[test]
fn only_model_gaps_are_registrable() {
    let semantic = UnsupportedKind::SemanticGap(SemanticGapKind::FlattenedParameterSlot);
    assert_eq!(
        semantic.model_gap(),
        Some(ModelGapKind::Semantic(
            SemanticGapKind::FlattenedParameterSlot
        ))
    );
    assert_eq!(semantic.class(), UnsupportedClass::SemanticGap);

    let resource = UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps);
    assert_eq!(resource.model_gap(), None);
    assert_eq!(resource.class(), UnsupportedClass::ResourceLimit);

    let contract = UnsupportedKind::ContractViolation(ContractViolationKind::MissingTerminator);
    assert_eq!(contract.model_gap(), None);
    assert_eq!(contract.class(), UnsupportedClass::ContractViolation);
}

#[test]
fn str_arguments_use_two_abi_slots() {
    let src = r#"fn take(s: str, n: i32) -> i32 {
        n
    }
    fn main() -> i32 {
        take("hi", 7)
    }"#;

    assert_eq!(run(src).exit_code, 7);
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
    assert_eq!(out.stderr, "error: integer overflow\n");
    assert_eq!(out.panic, Some(TrapKind::ArithmeticOverflow));
}

#[test]
fn divide_and_remainder_by_zero_share_a_trap_kind() {
    let out = run("fn main() -> i32 { let z = 0; 10 / z }");
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.stderr, "error: division by zero\n");
    assert_eq!(out.panic, Some(TrapKind::DivisionByZero));

    let out = run("fn main() -> i32 { let z = 0; 10 % z }");
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.stderr, "error: division by zero\n");
    assert_eq!(out.panic, Some(TrapKind::DivisionByZero));
}

#[test]
fn trap_kind_display_spellings_are_stable() {
    let cases = [
        (TrapKind::ArithmeticOverflow, "arithmetic overflow"),
        (TrapKind::DivisionByZero, "division by zero"),
        (TrapKind::IntegerCastOverflow, "integer cast overflow"),
        (TrapKind::IndexOutOfBounds, "index out of bounds"),
        (TrapKind::InvalidUtf8, "invalid UTF-8"),
        (TrapKind::UserPanic, "user panic"),
        (TrapKind::AssertionFailure, "assertion failure"),
        (TrapKind::Unreachable, "reached unreachable"),
    ];

    for (kind, expected) in cases {
        assert_eq!(kind.to_string(), expected);
    }
}

#[test]
fn normal_return_101_is_not_a_trap() {
    let out = run("fn main() -> i32 { 101 }");
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.panic, None);
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
    assert_eq!(out.stderr, "error: integer cast overflow\n");
    assert_eq!(out.panic, Some(TrapKind::IntegerCastOverflow));
}

#[test]
fn unbounded_recursion_is_unsupported() {
    // A recursive function with no base case would recurse forever and overflow
    // the Rust stack (an uncatchable abort). The shared recursion-depth bound
    // (MAX_DEPTH) must turn it into a typed resource failure instead (RUE-340).
    let src = "fn r(n: i32) -> i32 { r(n) }
    fn main() -> i32 { r(0) }";
    let err = expect_unsupported(src);
    assert_eq!(
        err.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::RecursionDepth),
        "expected a depth-budget failure, got: {err}"
    );
}

#[test]
fn deep_bounded_recursion_is_unsupported() {
    // Recursion that is bounded but far deeper than MAX_DEPTH must also fail
    // cleanly (hitting the depth bound) rather than overflow the stack.
    let src = "fn r(n: i32) -> i32 { if n <= 0 { 0 } else { r(n - 1) } }
    fn main() -> i32 { r(100000) }";
    let err = expect_unsupported(src);
    assert_eq!(
        err.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::RecursionDepth),
        "expected a depth-budget failure, got: {err}"
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
    assert_eq!(
        err.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
        "expected a step-budget failure, got: {err}"
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
    assert_eq!(out.panic, Some(TrapKind::IndexOutOfBounds));
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
fn inout_param_forwarded_to_nested_inout_call() {
    // Forwarding a writable `inout` parameter as a *nested* call's `inout`
    // argument. This is the container `self`-chain — `ArrayBuf::push` calls
    // `self.reserve()`, forwarding its own `inout self` — and before RUE-1010 it
    // was an oracle model gap. The mutation must thread through both call
    // boundaries back to the original caller.
    let src = "fn add(inout x: i32, k: i32) { x = x + k; }
    fn bump(inout x: i32) { add(inout x, 1); }
    fn main() -> i32 {
        let mut n = 40;
        bump(inout n);
        bump(inout n);
        n
    }";
    assert_eq!(exit(src), 42);
}

#[test]
fn inout_aggregate_param_forwarded_preserves_all_fields() {
    // The forwarded `inout` value is a whole aggregate mutated through a nested
    // `inout` call: the copy-back must rewrite the entire header, so an
    // untouched sibling field survives the round trip (the `{buf, len, cap}`
    // header-forwarding shape the container mutators rely on).
    let src = "struct P { x: i32, y: i32 }
    fn set_x(inout p: P, v: i32) { p.x = v; }
    fn relabel(inout p: P) { set_x(inout p, 99); }
    fn main() -> i32 {
        let mut p = P { x: 1, y: 7 };
        relabel(inout p);
        p.x + p.y
    }";
    // p.x becomes 99, p.y stays 7 -> 106
    assert_eq!(exit(src), 106);
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
    // Core `str` indexing reads packed UTF-8 bytes. "café" is c=99, a=97,
    // f=102, then the two UTF-8 bytes of é: 0xC3=195, 0xA9=169 (matches the spec
    // corpus case `string_index_utf8_bytes`).
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
    assert_eq!(out.panic, Some(TrapKind::IndexOutOfBounds));
}

#[test]
fn zst_param_before_scalar_does_not_shift_slots() {
    // A zero-sized argument occupies zero ABI slots (abi_slot_count), so the
    // scalar after it lives at slot 0, not slot 1. The oracle must use that
    // same slot contract to avoid a phantom DISAGREE blaming codegen.
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

#[test]
fn enum_parameter_width_comes_from_its_static_type() {
    let src = r#"enum Shape { Empty, One(i32), Pair(i32, i32) }
        fn marker(shape: Shape, n: i32) -> i32 { n }
        fn main() -> i32 {
            marker(Shape.Empty, 10)
                + marker(Shape.One(1), 20)
                + marker(Shape.Pair(1, 2), 12)
        }"#;
    assert_eq!(exit(src), 42);
}

#[test]
fn by_ref_aggregate_width_comes_from_its_physical_mode() {
    let src = r#"struct Pair { left: i32, right: i32 }
        fn borrowed_marker(borrow pair: Pair, n: i32) -> i32 {
            n + pair.left - pair.left
        }
        fn inout_marker(inout pair: Pair, n: i32) -> i32 {
            pair.right = pair.right;
            n
        }
        fn main() -> i32 {
            let mut pair = Pair { left: 1, right: 2 };
            borrowed_marker(borrow pair, 20) + inout_marker(inout pair, 22)
        }"#;
    assert_eq!(exit(src), 42);
}

#[test]
fn borrowed_aggregate_before_inout_uses_the_physical_writeback_slot() {
    let src = r#"struct Pair { left: i32, right: i32 }
        fn set_marker(borrow pair: Pair, inout marker: i32) {
            marker = 40 + pair.right;
        }
        fn main() -> i32 {
            let pair = Pair { left: 1, right: 2 };
            let mut marker = 0;
            set_marker(borrow pair, inout marker);
            marker
        }"#;
    assert_eq!(exit(src), 42);
}

#[test]
fn inout_zst_uses_a_physical_slot_without_copying_back() {
    let src = r#"struct Empty {}
        fn reset(inout value: Empty, n: i32) -> i32 {
            value = Empty {};
            n
        }
        fn main() -> i32 {
            let mut value = Empty {};
            let answer = 42;
            let observed = reset(inout value, answer);
            if answer == 42 { observed } else { 1 }
        }"#;
    assert_eq!(exit(src), 42);
}

#[test]
fn payloadless_enum_before_inout_uses_the_static_writeback_slot() {
    let src = r#"enum Maybe { Some(i32), None }
        fn set_marker(value: Maybe, inout marker: i32) { marker = 42; }
        fn main() -> i32 {
            let mut marker = 0;
            set_marker(Maybe.None, inout marker);
            marker
        }"#;
    assert_eq!(exit(src), 42);
}

// ---- abstract heap & pointer intrinsics (RUE heap model) -----------------

/// `@raw` + `@ptr_read` round-trips a local's value through a const pointer.
#[test]
fn raw_pointer_read_roundtrip() {
    let src = r#"fn main() -> i32 {
        let x: i32 = 123;
        let v: i32 = checked {
            let p: ptr const i32 = @raw(x);
            @ptr_read(p)
        };
        @dbg(v);
        0
    }"#;
    assert_eq!(run(src).stdout, "123\n");
}

/// A write through a `ptr mut` from `@raw_mut` mutates the source local.
#[test]
fn raw_mut_write_mutates_local() {
    let src = r#"fn main() -> i32 {
        let mut x: i32 = 10;
        checked {
            let p: ptr mut i32 = @raw_mut(x);
            @ptr_write(p, 77);
        };
        @dbg(x);
        0
    }"#;
    assert_eq!(run(src).stdout, "77\n");
}

/// An address round-trips through `@ptr_to_int` / `@int_to_ptr` back to the
/// same allocation.
#[test]
fn ptr_int_roundtrip_reads_same_cell() {
    let src = r#"fn main() -> i32 {
        let x: i32 = 42;
        let addr: u64 = checked {
            let p: ptr const i32 = @raw(x);
            @ptr_to_int(p)
        };
        let v: i32 = checked {
            let p2: ptr mut i32 = @int_to_ptr(addr);
            @ptr_read(p2)
        };
        @dbg(v);
        0
    }"#;
    assert_eq!(run(src).stdout, "42\n");
}

/// `@ptr_offset` strides by whole elements, forward and backward.
#[test]
fn ptr_offset_strides_by_element() {
    let src = r#"fn main() -> i32 {
        let arr: [i64; 3] = [10, 20, 30];
        let v: i64 = checked {
            let base: ptr const i64 = @raw(arr[0]);
            @ptr_read(@ptr_offset(base, 1))
        };
        @dbg(v);
        let w: i64 = checked {
            let base: ptr const i64 = @raw(arr[2]);
            @ptr_read(@ptr_offset(base, -1))
        };
        @dbg(w);
        0
    }"#;
    assert_eq!(run(src).stdout, "20\n20\n");
}

/// Use-after-free is undefined and stays a typed oracle gap rather than being
/// assigned the stale bytes that happened to remain in the old bump allocator.
#[test]
fn alloc_read_after_free_is_unsupported() {
    let src = r#"fn main() -> i32 {
        checked {
            let bytes: u64 = 3 * @intCast(@size_of(i32));
            let align: u64 = @intCast(@align_of(i32));
            let raw: ptr mut u8 = @alloc(bytes, align);
            let p: ptr mut i32 = @int_to_ptr(@ptr_to_int(raw));
            @ptr_write(p, 10);
            @ptr_write(@ptr_offset(p, 1), 20);
            @ptr_write(@ptr_offset(p, 2), 30);
            @dbg(@ptr_read(p));
            @dbg(@ptr_read(@ptr_offset(p, 1)));
            @dbg(@ptr_read(@ptr_offset(p, 2)));
            @free(raw, bytes, align);
            @ptr_read(p) + @ptr_read(@ptr_offset(p, 1)) + @ptr_read(@ptr_offset(p, 2))
        }
    }"#;
    let unsupported = expect_unsupported(src);
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerRead,
        ))
    );
    assert_eq!(unsupported.detail(), "pointer read after free");
}

/// `@alloc` round-trips an aggregate element written and read whole.
#[test]
fn alloc_read_write_aggregate() {
    let src = r#"struct P { x: i32, y: i32 }
    struct AosBox { a: [P; 2] }
    fn main() -> i32 {
        checked {
            let bytes: u64 = @intCast(@size_of(AosBox));
            let align: u64 = @intCast(@align_of(AosBox));
            let raw: ptr mut u8 = @alloc(bytes, align);
            let wp: ptr mut AosBox = @int_to_ptr(@ptr_to_int(raw));
            @ptr_write(wp, AosBox { a: [P { x: 1, y: 2 }, P { x: 3, y: 4 }] });
            let v = @ptr_read(wp);
            @dbg(v.a[0].x); @dbg(v.a[1].y);
            @free(raw, bytes, align);
        };
        0
    }"#;
    assert_eq!(run(src).stdout, "1\n4\n");
}

/// `@field_ptr` reads and writes a field in place, observable by direct access.
#[test]
fn field_ptr_reads_and_writes_field() {
    let src = r#"struct Pair { a: i32, b: i32 }
    fn main() -> i32 {
        let mut p = Pair { a: 7, b: 9 };
        checked {
            let fp: ptr mut i32 = @field_ptr(p.b);
            @ptr_write(fp, 100);
        };
        @dbg(p.a);
        @dbg(p.b);
        p.b
    }"#;
    let outcome = run(src);
    assert_eq!(outcome.stdout, "7\n100\n");
    assert_eq!(outcome.exit_code, 100);
}

/// `@field_ptr` addresses a by-value struct parameter's own storage.
#[test]
fn field_ptr_on_param_struct() {
    let src = r#"struct Point { x: i32, y: i32 }
    fn second(p: Point) -> i32 {
        checked { @ptr_read(@field_ptr(p.y)) }
    }
    fn main() -> i32 {
        let pt = Point { x: 3, y: 44 };
        @dbg(second(pt));
        0
    }"#;
    assert_eq!(run(src).stdout, "44\n");
}

/// The full byte family round-trips through `@alloc`, the bulk `@byte_*`
/// helpers, per-byte `@ptr_read`/`@ptr_write` over a `ptr u8`, and `@realloc`,
/// with contents preserved across a grow.
#[test]
fn byte_family_roundtrip() {
    let src = r#"fn main() -> i32 {
        checked {
            let p: ptr mut u8 = @alloc(6, 1);
            @byte_set(p, 0, 6);
            @ptr_write(@ptr_offset(p, 0), 10);
            @ptr_write(@ptr_offset(p, 1), 20);
            @ptr_write(@ptr_offset(p, 2), 30);
            let q: ptr mut u8 = @realloc(p, 6, 1, 8);
            @ptr_write(@ptr_offset(q, 3), 40);
            let dst: ptr mut u8 = @alloc(8, 1);
            @byte_copy(dst, q, 8);
            let sum: i32 = @intCast(@ptr_read(@ptr_offset(dst, 0))) + @intCast(@ptr_read(@ptr_offset(dst, 1)))
                + @intCast(@ptr_read(@ptr_offset(dst, 2))) + @intCast(@ptr_read(@ptr_offset(dst, 3)))
                + @intCast(@ptr_read(@ptr_offset(dst, 4)));
            @dbg(sum);
            @free(q, 8, 1);
            @free(dst, 8, 1);
        };
        0
    }"#;
    assert_eq!(run(src).stdout, "100\n");
}

/// Byte offsets folded into the address via `@ptr_to_int`/`@int_to_ptr`
/// (never `@ptr_offset`) address a byte-aliased sub-range.
#[test]
fn byte_address_arithmetic_roundtrip() {
    let src = r#"fn main() -> i32 {
        checked {
            let src: ptr mut u8 = @alloc(5, 1);
            let mut i: u64 = 0;
            while i < 5 {
                let b: u8 = @intCast(i + 1);
                @ptr_write(@ptr_offset(src, i), b);
                i = i + 1;
            }
            let dst: ptr mut u8 = @alloc(5, 1);
            @byte_set(dst, 0, 5);
            let sp: ptr mut u8 = @int_to_ptr(@ptr_to_int(src) + 1);
            let dp: ptr mut u8 = @int_to_ptr(@ptr_to_int(dst) + 2);
            @byte_copy(dp, sp, 3);
            let mut sum: i32 = 0;
            let mut j: u64 = 0;
            while j < 5 {
                sum = sum + @intCast(@ptr_read(@ptr_offset(dst, j)));
                j = j + 1;
            }
            @dbg(sum);
            @free(src, 5, 1);
            @free(dst, 5, 1);
        };
        0
    }"#;
    assert_eq!(run(src).stdout, "9\n");
}

/// `@realloc` grows a block and preserves the earlier contents.
#[test]
fn realloc_grows_and_preserves() {
    let src = r#"fn main() -> i32 {
        checked {
            let unit: u64 = @intCast(@size_of(i32));
            let align: u64 = @intCast(@align_of(i32));
            let mut raw: ptr mut u8 = @alloc(2 * unit, align);
            let mut p: ptr mut i32 = @int_to_ptr(@ptr_to_int(raw));
            @ptr_write(p, 5);
            @ptr_write(@ptr_offset(p, 1), 7);
            raw = @realloc(raw, 2 * unit, align, 16 * unit);
            p = @int_to_ptr(@ptr_to_int(raw));
            @ptr_write(@ptr_offset(p, 8), 100);
            let sum: i32 = @ptr_read(p) + @ptr_read(@ptr_offset(p, 1)) + @ptr_read(@ptr_offset(p, 8));
            @free(raw, 16 * unit, align);
            sum
        }
    }"#;
    assert_eq!(run(src).exit_code, 112);
}

/// A `@realloc` too large to satisfy returns null; the original allocation and
/// its contents remain valid (spec 8.6:3/8.6:4).
#[test]
fn realloc_failure_returns_null_and_preserves_original() {
    let src = r#"fn main() -> i32 {
        checked {
            let p: ptr mut u8 = @alloc(4, 1);
            @ptr_write(p, 10);
            @ptr_write(@ptr_offset(p, 1), 20);
            @ptr_write(@ptr_offset(p, 2), 30);
            @ptr_write(@ptr_offset(p, 3), 40);
            let q: ptr mut u8 = @realloc(p, 4, 1, 2305843009213693951);
            if @ptr_to_int(q) == 0 {
                let sum: i32 = @intCast(@ptr_read(p))
                    + @intCast(@ptr_read(@ptr_offset(p, 1)))
                    + @intCast(@ptr_read(@ptr_offset(p, 2)))
                    + @intCast(@ptr_read(@ptr_offset(p, 3)));
                @free(p, 4, 1);
                sum
            } else {
                @free(q, 2305843009213693951, 1);
                1
            }
        }
    }"#;
    assert_eq!(run(src).exit_code, 100);
}

/// A null pointer from `@int_to_ptr(0)` round-trips to integer zero.
#[test]
fn int_to_ptr_zero_is_null() {
    let src = r#"fn main() -> i32 {
        let zero: u64 = 0;
        let z: u64 = checked {
            let p: ptr mut i32 = @int_to_ptr(zero);
            @ptr_to_int(p)
        };
        @intCast(z)
    }"#;
    assert_eq!(run(src).exit_code, 0);
}
