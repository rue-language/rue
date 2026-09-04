//! Correctness corpus for the reference interpreter. Each case asserts the
//! oracle's observable behavior (exit code, stdout, stderr, trap cause) matches
//! the value the language semantics require — which is also what the compiled
//! binary must produce. Agreement here is the differential check in miniature;
//! `tests/differential.rs`-style wiring against the real binary across the whole
//! CLI/spec corpus is the next step (RUE-50).

use super::*;
use rue_compiler::PreviewFeature;

#[test]
fn oracle_uses_air_synthetic_type_identity_policy() {
    let source = include_str!("lib.rs");
    for peer in [
        ".strip_prefix(\"Str(\")",
        ".starts_with(\"Str(\")",
        ".starts_with('[')",
    ] {
        assert!(
            !source.contains(peer),
            "oracle regained handwritten synthetic-type identity policy: {peer}"
        );
    }
}

#[test]
fn oracle_intrinsic_contracts_and_evaluation_use_only_the_typed_operation() {
    let source = include_str!("lib.rs");
    assert!(source.contains(
        "fn unsupported_intrinsic_kind_for_operation(\n    operation: rue_air::IntrinsicOperation"
    ));
    assert!(source.contains("CfgInstData::Intrinsic { operation, .. }"));
    assert!(source.contains("match *operation {"));

    let pointer_eval = source
        .split("fn eval_pointer_intrinsic(")
        .nth(1)
        .and_then(|source| source.split("\n    fn expect_ptr(").next())
        .expect("typed pointer-intrinsic evaluator");
    assert!(pointer_eval.contains("match operation {"));
    assert!(!pointer_eval.contains("_ => Ok(None)"));

    let debug_eval = source
        .split("fn eval_debug_intrinsic(")
        .nth(1)
        .and_then(|source| source.split("\n    fn write_dbg(").next())
        .expect("typed debug-intrinsic evaluator");
    assert!(debug_eval.contains("operation.validate_call("));
    assert!(debug_eval.contains("match operation {"));
    assert!(!debug_eval.contains("_ =>"));

    for forbidden in [
        "IntrinsicSelector",
        "impl From<&str>",
        "fn unsupported_intrinsic_kind(name",
        "unsupported_intrinsic_kind(name",
        "intrinsic_operation_from_name",
        "operation_from_name",
        "match name.as_str()",
        "match name {\n            \"ptr_",
    ] {
        assert!(
            !source.contains(forbidden),
            "oracle regained intrinsic string selection or compatibility fallback: {forbidden}"
        );
    }
}

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
    expect_unsupported_with_preview(src, &PreviewFeatures::new())
}

fn expect_unsupported_with_preview(src: &str, preview_features: &PreviewFeatures) -> Unsupported {
    match run_source_with_preview_features(src, preview_features) {
        Err(RunSourceError::Unsupported(unsupported)) => unsupported,
        Err(RunSourceError::Compile(errors)) => {
            panic!("expected oracle-unsupported source, but it failed to compile: {errors:#?}")
        }
        Err(RunSourceError::CfgTransformationDisagreement { .. }) => {
            panic!("expected one typed unsupported result, but CFG boundaries disagreed")
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
fn print_and_println_share_an_ordered_byte_trace() {
    let outcome = run(r#"fn main() -> i32 {
            print("hé");
            println("!");
            print("");
            0
        }"#);
    assert_eq!(outcome.stdout.as_bytes(), "hé!\n".as_bytes());
}

#[test]
fn print_output_respects_the_raw_stdout_bound() {
    let unsupported = run_with_stdout_cap("fn main() -> i32 { print(\"hello\"); 0 }", 3)
        .expect_err("output crossing the bound must fail closed");
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes)
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn stdout_write_syscall_is_modeled_only_for_fd_one() {
    let output = run(r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(3, 1) };
            checked {
                @ptr_write(p, 65);
                @ptr_write(@ptr_offset(p, 1), 66);
                @ptr_write(@ptr_offset(p, 2), 67);
            };
            let count: i64 = checked { @syscall(1, 1, @ptr_to_int(p), 3) };
            checked { @free(p, 3, 1); };
            @intCast(count)
        }"#);
    assert_eq!(output.stdout, "ABC");
    assert_eq!(output.exit_code, 3);

    // A shorter requested length is the only partial-write behavior the
    // deterministic model claims: it observes exactly that prefix and returns
    // the requested count. It does not pretend to simulate an OS short write.
    let requested_prefix = run(r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(3, 1) };
            checked {
                @ptr_write(p, 65);
                @ptr_write(@ptr_offset(p, 1), 66);
                @ptr_write(@ptr_offset(p, 2), 67);
            };
            let count: i64 = checked { @syscall(1, 1, @ptr_to_int(p), 2) };
            checked { @free(p, 3, 1); };
            @intCast(count)
        }"#);
    assert_eq!(requested_prefix.stdout, "AB");
    assert_eq!(requested_prefix.exit_code, 2);

    // Two short atomic requests retain their call order. This tests a
    // requested prefix, not an arbitrary OS short write.
    let sequential = run(r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(2, 1) };
            checked { @ptr_write(p, 65); @ptr_write(@ptr_offset(p, 1), 66); };
            let first: i64 = checked { @syscall(1, 1, @ptr_to_int(p), 1) };
            let second: i64 = checked { @syscall(1, 1, @ptr_to_int(@ptr_offset(p, 1)), 1) };
            checked { @free(p, 2, 1); };
            @intCast(first + second)
        }"#);
    assert_eq!(sequential.stdout, "AB");
    assert_eq!(sequential.exit_code, 2);

    let cap_crossing = run_with_stdout_cap(
        r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(2, 1) };
            checked { @ptr_write(p, 65); @ptr_write(@ptr_offset(p, 1), 66); };
            let _: i64 = checked { @syscall(1, 1, @ptr_to_int(p), 1) };
            let _: i64 = checked { @syscall(1, 1, @ptr_to_int(@ptr_offset(p, 1)), 1) };
            0
        }"#,
        1,
    )
    .expect_err("a valid write crossing only the stdout cap must fail closed");
    assert_eq!(
        cap_crossing.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes)
    );

    let oversized = expect_unsupported(
        r#"fn main() -> i32 {
            let _: i64 = checked { @syscall(1, 1, 0, 513) };
            0
        }"#,
    );
    assert_eq!(
        oversized.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::SystemCall)
    );

    let wrong_fd = expect_unsupported(
        r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(1, 1) };
            checked { @ptr_write(p, 65); };
            let _: i64 = checked { @syscall(1, 2, @ptr_to_int(p), 1) };
            0
        }"#,
    );
    assert_eq!(
        wrong_fd.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::SystemCall)
    );

    let wrong_number = expect_unsupported(
        r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(1, 1) };
            checked { @ptr_write(p, 65); };
            let _: i64 = checked { @syscall(39, 1, @ptr_to_int(p), 1) };
            0
        }"#,
    );
    assert_eq!(
        wrong_number.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::SystemCall)
    );

    let invalid_pointer = expect_unsupported(
        r#"fn main() -> i32 {
            let _: i64 = checked { @syscall(1, 1, 123, 1) };
            0
        }"#,
    );
    assert_eq!(
        invalid_pointer.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::SystemCall)
    );

    let freed_pointer = expect_unsupported(
        r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(1, 1) };
            checked { @ptr_write(p, 65); @free(p, 1, 1); };
            let _: i64 = checked { @syscall(1, 1, @ptr_to_int(p), 1) };
            0
        }"#,
    );
    assert_eq!(
        freed_pointer.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::SystemCall)
    );

    let uninitialized_pointer = expect_unsupported(
        r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(1, 1) };
            let _: i64 = checked { @syscall(1, 1, @ptr_to_int(p), 1) };
            0
        }"#,
    );
    assert_eq!(
        uninitialized_pointer.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::SystemCall)
    );

    let invalid = run(r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(1, 1) };
            checked { @byte_set(p, 255, 1); };
            let _: i64 = checked { @syscall(1, 1, @ptr_to_int(p), 1) };
            0
        }"#);
    let different_invalid = run(r#"fn main() -> i32 {
            let p: ptr mut u8 = checked { @alloc(1, 1) };
            checked { @byte_set(p, 254, 1); };
            let _: i64 = checked { @syscall(1, 1, @ptr_to_int(p), 1) };
            0
        }"#);
    assert_eq!(invalid.stdout, different_invalid.stdout);
    assert_ne!(invalid.stdout_bytes, different_invalid.stdout_bytes);
    assert_eq!(invalid.stdout_bytes, [255]);
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
))]
#[test]
fn modeled_stdout_syscall_crossing_cap_is_a_resource_failure() {
    let write_nr = host_write_syscall_number().expect("supported host write ABI");
    let source = format!(
        r#"fn main() -> i32 {{
            let p: ptr mut u8 = checked {{ @alloc(2, 1) }};
            checked {{ @ptr_write(p, 65); @ptr_write(@ptr_offset(p, 1), 66); }};
            let _: i64 = checked {{ @syscall({write_nr}, 1, @ptr_to_int(p), 1) }};
            let _: i64 = checked {{ @syscall({write_nr}, 1, @ptr_to_int(@ptr_offset(p, 1)), 1) }};
            0
        }}"#
    );
    let unsupported = run_with_stdout_cap(&source, 1)
        .expect_err("a valid write crossing only the stdout cap must fail closed");
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes)
    );
}

#[test]
fn allocator_reuses_freed_small_blocks_by_size_class() {
    let outcome = run(r#"fn main() -> i32 {
            let first: ptr mut u8 = checked { @alloc(4006, 1) };
            let first_address: u64 = checked { @ptr_to_int(first) };
            checked { @free(first, 4006, 1); };

            // 4005 and 4006 both round to the runtime allocator's 4096-byte
            // small class, so this allocation reuses the freed block.
            let same_class: ptr mut u8 = checked { @alloc(4005, 1) };
            let same_address: u64 = checked { @ptr_to_int(same_class) };
            let different_class: ptr mut u8 = checked { @alloc(8, 1) };
            let different_address: u64 = checked { @ptr_to_int(different_class) };
            checked {
                @free(same_class, 4005, 1);
                @free(different_class, 8, 1);
            };
            if first_address == same_address && first_address != different_address { 42 } else { 1 }
        }"#);
    assert_eq!(outcome.exit_code, 42);
}

#[test]
fn recycled_storage_keeps_stale_pointer_provenance_dead() {
    let unsupported = expect_unsupported(
        r#"fn main() -> i32 {
        let old: ptr mut u8 = checked { @alloc(8, 1) };
        checked { @ptr_write(old, 65); @free(old, 8, 1); };
        let fresh: ptr mut u8 = checked { @alloc(8, 1) };
        checked { @ptr_read(old); };
        let old_address: u64 = checked { @ptr_to_int(old) };
        let fresh_address: u64 = checked { @ptr_to_int(fresh) };
        if old_address == fresh_address { 0 } else { 1 }
    }"#,
    );
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerRead,
        ))
    );
    assert_eq!(
        unsupported.detail(),
        "pointer has stale allocation provenance"
    );
}

#[test]
fn recycled_storage_rejects_stale_pointer_derived_integer() {
    let unsupported = expect_unsupported(
        r#"fn main() -> i32 {
        let old: ptr mut u8 = checked { @alloc(8, 1) };
        let old_address: u64 = checked { @ptr_to_int(old) };
        checked { @free(old, 8, 1); };
        let fresh: ptr mut u8 = checked { @alloc(8, 1) };
        let recovered: ptr mut u8 = checked { @int_to_ptr(old_address) };
        checked { @ptr_read(recovered); };
        let fresh_address: u64 = checked { @ptr_to_int(fresh) };
        if fresh_address == old_address { 0 } else { 1 }
    }"#,
    );
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::IntToPointer,
        ))
    );
}

#[test]
fn small_free_lists_follow_free_order_and_realloc_releases_old_blocks() {
    let outcome = run(r#"fn main() -> i32 {
        let a: ptr mut u8 = checked { @alloc(8, 1) };
        let b: ptr mut u8 = checked { @alloc(8, 1) };
        let a_address: u64 = checked { @ptr_to_int(a) };
        let b_address: u64 = checked { @ptr_to_int(b) };
        checked { @free(b, 8, 1); @free(a, 8, 1); };
        let first: ptr mut u8 = checked { @alloc(8, 1) };
        let first_address: u64 = checked { @ptr_to_int(first) };

        let moved: ptr mut u8 = checked { @realloc(first, 8, 1, 16) };
        let b_again: ptr mut u8 = checked { @alloc(8, 1) };
        let b_again_address: u64 = checked { @ptr_to_int(b_again) };
        checked { @free(moved, 16, 1); @free(b_again, 8, 1); };
        if first_address == a_address
            && first_address != b_address
            && b_again_address == first_address
        { 42 } else { 1 }
    }"#);
    assert_eq!(outcome.exit_code, 42);
}

#[test]
fn recycled_extent_growth_is_chargeable_heap_metadata() {
    let state = query_cfg_state("fn main() -> i32 { 0 }").expect("test state compiles");
    let mut interp = Interp {
        state: &state,
        stdout_trace: Vec::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: vec![Allocation {
            bytes: vec![0; 9],
            initialized: vec![false; 9],
            provenance: vec![None; 9],
            root_ty: None,
            freed: true,
            generation: 1,
            free_list_next: None,
            origin: AllocationOrigin::Heap,
            declared_alignment: 1,
            owner_depth: None,
        }],
        small_free_heads: [
            None,
            Some(0),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ],
        heap_metadata_bytes: 128 * 1024 * 1024 - 1,
    };
    let error = interp
        .do_alloc(16, false, 1)
        .expect_err("reused extent growth must charge metadata");
    assert!(matches!(
        error,
        Flow::Unsupported(Unsupported {
            kind: UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps),
            ..
        })
    ));
}

#[test]
fn cfg_boundary_differential_is_deterministic() {
    let source = "fn square(x: i32) -> i32 { x * x } fn main() -> i32 { square(3) }";
    let first = run_source_cfg_differential(source).expect("CFG boundaries agree");
    let second = run_source_cfg_differential(source).expect("repeated CFG boundaries agree");
    assert_eq!(first, second);
}

#[test]
fn cfg_boundary_differential_catches_a_planted_transformation_fault() {
    let source = r#"struct Inner {
        x: i32,
        fn value(borrow self) -> borrow i32 { yield self.x; }
    }
    fn seven() -> i32 { 7 }
    fn main() -> i32 {
        let inner = Inner { x: 7 };
        if inner.value() == seven() { 0 } else { 1 }
    }"#;
    let snapshot = SourceSnapshot::single("<oracle-fault>", source).unwrap();
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().unwrap();
    assert!(rue_compiler::unstable::inject_stale_query_for_oracle(
        &mut session,
        rue_compiler::unstable::DifferentialOracleFault::CfgTransformation,
    ));
    let error = run_session_cfg_differential_inner(&mut session, &PreviewFeatures::new())
        .expect_err("the planted post-CFG fault must disagree");
    assert!(
        matches!(error, RunSourceError::CfgTransformationDisagreement { .. }),
        "unexpected fault result: {error:?}"
    );
}

#[test]
fn cfg_boundary_differential_preserves_accessor_materialization() {
    let source = r#"struct Inner {
        x: i64,
        fn value(borrow self) -> borrow i64 { yield self.x; }
    }
    fn main() -> i32 {
        let inner = Inner { x: 7 };
        if inner.value() == 7 { 0 } else { 1 }
    }"#;
    assert_eq!(
        run_source_cfg_differential(source)
            .expect("pre/post CFGs preserve accessor semantics")
            .exit_code,
        0
    );
}

#[test]
fn raw_accessor_whole_receiver_reassignment_matches_spliced_cfg() {
    let source = r#"struct P {
        x: i32,
        fn value(inout self) -> inout i32 {
            self = P { x: 9 };
            yield self.x;
        }
    }
    fn main() -> i32 {
        let mut p = P { x: 1 };
        p.value() = 11;
        if p.x == 11 { 0 } else { 1 }
    }"#;
    assert_eq!(
        run_source_cfg_differential(source)
            .expect("raw accessor parameter redirection matches canonical splicing")
            .exit_code,
        0
    );
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
            assert_eq!(&*state.type_pool().struct_def(struct_id).name, "str");
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
        RunSourceError::CfgTransformationDisagreement { .. } => {
            panic!("compile failure was misclassified as a CFG disagreement");
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
    use rue_air::IntrinsicOperation as Operation;

    for (operation, intrinsic) in [
        (Operation::ParseI32, Intrinsic::ParseI32),
        (Operation::ParseI64, Intrinsic::ParseI64),
        (Operation::ParseU32, Intrinsic::ParseU32),
        (Operation::ParseU64, Intrinsic::ParseU64),
        (Operation::PtrRead, Intrinsic::PointerRead),
        (Operation::PtrReadUnaligned, Intrinsic::PointerRead),
        (Operation::PtrWrite, Intrinsic::PointerWrite),
        (Operation::PtrWriteUnaligned, Intrinsic::PointerWrite),
        (Operation::PtrOffset, Intrinsic::PointerOffset),
        (Operation::PtrToInt, Intrinsic::PointerToInt),
        (Operation::IntToPtr, Intrinsic::IntToPointer),
        (Operation::Raw, Intrinsic::RawAddress),
        (Operation::RawMut, Intrinsic::RawMutableAddress),
        (Operation::FieldPtr, Intrinsic::FieldPointer),
        (Operation::Alloc, Intrinsic::Allocate),
        (Operation::Free, Intrinsic::Free),
        (Operation::Realloc, Intrinsic::Reallocate),
        (Operation::AllocZeroed, Intrinsic::AllocateZeroed),
        (Operation::Resize, Intrinsic::Resize),
        (Operation::ByteMove, Intrinsic::ByteMove),
        (Operation::ByteCopy, Intrinsic::ByteCopy),
        (Operation::ByteSet, Intrinsic::ByteSet),
        (Operation::IntToFloat, Intrinsic::IntToFloat),
        (Operation::FloatToInt, Intrinsic::FloatToInt),
        (Operation::FloatCast, Intrinsic::FloatCast),
    ] {
        let expected = UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(intrinsic));
        assert_eq!(
            unsupported_intrinsic_kind_for_operation(operation),
            expected,
            "{operation:?}"
        );
        assert!(
            expected.model_gap().is_some(),
            "{operation:?} must be registrable"
        );
    }

    for (operation, dependency) in [
        (Operation::ReadLine, ExternalDependencyKind::StandardInput),
        (Operation::RandomU32, ExternalDependencyKind::RandomU32),
        (Operation::RandomU64, ExternalDependencyKind::RandomU64),
        (Operation::Syscall, ExternalDependencyKind::SystemCall),
        (Operation::ArgCount, ExternalDependencyKind::ArgCount),
        (Operation::ArgPtr, ExternalDependencyKind::ArgPtr),
        (Operation::ArgLen, ExternalDependencyKind::ArgLen),
        (Operation::EnvCount, ExternalDependencyKind::EnvCount),
        (Operation::EnvPtr, ExternalDependencyKind::EnvPtr),
        (Operation::EnvLen, ExternalDependencyKind::EnvLen),
    ] {
        assert_eq!(
            unsupported_intrinsic_kind_for_operation(operation),
            UnsupportedKind::ExternalDependency(dependency),
            "{operation:?}"
        );
    }

    for operation in [
        Operation::PanicNoMessage,
        Operation::Panic,
        Operation::AssertFailed,
        Operation::AssertWithMessage,
        Operation::BoundsCheck,
        Operation::DebugI64,
        Operation::DebugU64,
        Operation::DebugBool,
        Operation::DebugStr,
        Operation::TotalCmp,
        Operation::BitCast,
    ] {
        assert_eq!(
            unsupported_intrinsic_kind_for_operation(operation),
            UnsupportedKind::ContractViolation(ContractViolationKind::UnexpectedIntrinsic),
            "{operation:?} must fail closed if it unexpectedly reaches gap classification"
        );
    }

    assert_eq!(Operation::ALL.len(), 46);
}

#[test]
fn float_arithmetic_is_a_typed_model_gap() {
    let unsupported = expect_unsupported_with_preview(
        r#"fn main() -> i32 {
            let zero: f64 = 0.0;
            if zero / zero == zero { 0 } else { 1 }
        }"#,
        &PreviewFeatures::from([PreviewFeature::Floats]),
    );
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::FloatArithmetic)
    );
}

#[test]
fn scalar_float_comparison_is_a_typed_model_gap() {
    let unsupported = expect_unsupported_with_preview(
        "fn main() -> i32 { let left: f64 = 1.0; let right: f64 = 2.0; if left < right { 0 } else { 1 } }",
        &PreviewFeatures::from([PreviewFeature::Floats]),
    );
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::FloatArithmetic)
    );
}

#[test]
fn aggregate_float_equality_is_a_typed_model_gap() {
    let unsupported = expect_unsupported_with_preview(
        r#"struct Outer { nested: [f64; 1] }
        fn main() -> i32 {
            let left = Outer { nested: [1.0] };
            let right = Outer { nested: [1.0] };
            if left == right { 0 } else { 1 }
        }"#,
        &PreviewFeatures::from([PreviewFeature::Floats]),
    );
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::FloatArithmetic)
    );
}

#[test]
fn zero_length_float_array_equality_remains_modeled() {
    let source = r#"struct EmptyFloats { values: [f64; 0] }
        fn main() -> i32 {
            let left = EmptyFloats { values: [] };
            let right = EmptyFloats { values: [] };
            if left == right { 0 } else { 1 }
        }"#;
    let outcome =
        run_source_with_preview_features(source, &PreviewFeatures::from([PreviewFeature::Floats]))
            .expect("zero-length arrays have no reachable float value to compare");
    assert_eq!(outcome.exit_code, 0);
}

#[test]
fn malformed_float_arithmetic_shape_is_a_contract_violation() {
    let state = query_cfg_state(
        "fn add(left: i32, right: i32) -> i32 { left + right } fn main() -> i32 { add(1, 2) }",
    )
    .expect("integer addition probe compiles");
    let cfg = &state
        .functions
        .iter()
        .find(|function| function.is_source_named("add"))
        .expect("add function remains reachable")
        .cfg;
    let (left, right) = cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
        .find_map(|value| match cfg.get_inst(value).data {
            CfgInstData::Add(left, right) => Some((left, right)),
            _ => None,
        })
        .expect("integer Add remains in the parameterized function");
    let interp = Interp {
        state: &state,
        stdout_trace: Vec::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
        small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
        heap_metadata_bytes: 0,
    };
    let error = interp
        .is_well_typed_float_operation(cfg, &CfgInstData::Add(left, right), Type::F64)
        .expect_err("integer operands with a float result violate the CFG contract");
    assert!(matches!(
        error,
        Flow::Unsupported(Unsupported {
            kind: UnsupportedKind::ContractViolation(
                ContractViolationKind::NonIntegerOperationType
            ),
            ..
        })
    ));
}

#[test]
fn malformed_float_aggregate_ordering_is_a_contract_violation() {
    let previews = PreviewFeatures::from([PreviewFeature::Floats]);
    let state = query_cfg_state_with_preview_features(
        r#"struct Boxed { value: f64 }
        fn equal(left: Boxed, right: Boxed) -> bool { left == right }
        fn main() -> i32 {
            if equal(Boxed { value: 1.0 }, Boxed { value: 1.0 }) { 0 } else { 1 }
        }"#,
        &previews,
    )
    .expect("aggregate equality probe compiles");
    let cfg = &state
        .functions
        .iter()
        .find(|function| function.is_source_named("equal"))
        .expect("equal function remains reachable")
        .cfg;
    let (left, right) = cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
        .find_map(|value| match cfg.get_inst(value).data {
            CfgInstData::Eq(left, right) => Some((left, right)),
            _ => None,
        })
        .expect("aggregate Eq remains in the parameterized function");
    let interp = Interp {
        state: &state,
        stdout_trace: Vec::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
        small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
        heap_metadata_bytes: 0,
    };
    let error = interp
        .is_well_typed_float_operation(cfg, &CfgInstData::Lt(left, right), Type::BOOL)
        .expect_err("ordering a float-bearing aggregate violates the CFG contract");
    assert!(matches!(
        error,
        Flow::Unsupported(Unsupported {
            kind: UnsupportedKind::ContractViolation(
                ContractViolationKind::NonIntegerOperationType
            ),
            ..
        })
    ));
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
fn slice_bounds_check_uses_index_trap_category_without_changing_assertions() {
    let slice_oob = r#"
        fn get(borrow s: [i64], i: u64) -> i32 { @intCast(s[i]) }
        fn main() -> i32 {
            let a: [i64; 3] = [10, 20, 30];
            get(borrow a, 5)
        }
    "#;
    let out = run(slice_oob);
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.stderr, "error: index out of bounds\n");
    assert_eq!(out.panic, Some(TrapKind::IndexOutOfBounds));

    let slice_negative = r#"
        fn get(borrow s: [i64], i: i32) -> i32 { @intCast(s[i]) }
        fn main() -> i32 {
            let a: [i64; 3] = [10, 20, 30];
            get(borrow a, -1)
        }
    "#;
    let out = run(slice_negative);
    assert_eq!(out.exit_code, 101);
    assert_eq!(out.stderr, "error: index out of bounds\n");
    assert_eq!(out.panic, Some(TrapKind::IndexOutOfBounds));

    let assertion = run("fn main() -> i32 { @assert(false); 0 }");
    assert_eq!(assertion.stderr, "assertion failed\n");
    assert_eq!(assertion.panic, Some(TrapKind::AssertionFailure));
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

#[test]
fn byte_move_overlapping_forward_and_backward_matches_memmove() {
    let forward = r#"fn main() -> i32 {
        checked {
            let p: ptr mut u8 = @alloc(6, 1);
            let mut i: u64 = 0;
            while i < 6 { let b: u8 = @intCast(i + 1); @ptr_write(@ptr_offset(p, i), b); i = i + 1; }
            @byte_move(@ptr_offset(p, 1), p, 5);
            let mut sum: i32 = 0; i = 0;
            while i < 6 { sum = sum + @intCast(@ptr_read(@ptr_offset(p, i))); i = i + 1; }
            @free(p, 6, 1); sum
        }
    }"#;
    let backward = r#"fn main() -> i32 {
        checked {
            let p: ptr mut u8 = @alloc(6, 1);
            let mut i: u64 = 0;
            while i < 6 { let b: u8 = @intCast(i + 1); @ptr_write(@ptr_offset(p, i), b); i = i + 1; }
            @byte_move(p, @ptr_offset(p, 1), 5);
            let mut sum: i32 = 0; i = 0;
            while i < 6 { sum = sum + @intCast(@ptr_read(@ptr_offset(p, i))); i = i + 1; }
            @free(p, 6, 1); sum
        }
    }"#;
    assert_eq!(exit(forward), 16);
    assert_eq!(exit(backward), 26);
}

#[test]
fn byte_copy_overlap_is_a_typed_gap() {
    let source = r#"fn main() -> i32 {
        checked {
            let p: ptr mut u8 = @alloc(4, 1);
            @byte_copy(@ptr_offset(p, 1), p, 3);
            0
        }
    }"#;
    assert_eq!(
        expect_unsupported(source).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::ByteCopy
        ))
    );
}

#[test]
fn byte_copy_cross_stride_copies_representation_bytes() {
    let source = r#"fn main() -> i32 {
        checked {
            let src: ptr mut u8 = @alloc(8, 1);
            let dst: ptr mut u8 = @alloc(8, 1);
            @byte_set(src, 0xA5, 8);
            @byte_copy(dst, src, 8);
            let wide: ptr mut i64 = @int_to_ptr(@ptr_to_int(dst));
            let value: i64 = @ptr_read(wide);
            @free(src, 8, 1); @free(dst, 8, 1);
            if value == -6510615555426900571 { 0 } else { 1 }
        }
    }"#;
    assert_eq!(exit(source), 0);
}

#[test]
fn zero_length_byte_operations_accept_null_without_dereference() {
    let source = r#"fn main() -> i32 {
        checked {
            let zero: u64 = 0;
            let null: ptr mut u8 = @int_to_ptr(zero);
            @byte_set(null, 0, 0);
            @byte_copy(null, null, 0);
            @byte_move(null, null, 0);
        };
        0
    }"#;
    assert_eq!(exit(source), 0);
}

#[test]
fn byte_set_bounds_and_misaligned_typed_read_are_typed_gaps() {
    let bounds = r#"fn main() -> i32 {
        checked { let p: ptr mut u8 = @alloc(4, 1); @byte_set(p, 1, 5); };
        0
    }"#;
    assert_eq!(
        expect_unsupported(bounds).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::ByteSet
        ))
    );

    let misaligned = r#"fn main() -> i32 {
        checked {
            let raw: ptr mut u8 = @alloc(8, 1);
            let p: ptr mut i32 = @int_to_ptr(@ptr_to_int(raw) + 1);
            @ptr_read(p)
        };
        0
    }"#;
    assert_eq!(
        expect_unsupported(misaligned).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerRead
        ))
    );
}

#[test]
fn partial_initialization_is_gap_but_zeroed_aggregate_padding_is_ignored() {
    let partial = r#"fn main() -> i32 {
        checked {
            let raw: ptr mut u8 = @alloc(8, 8);
            let p: ptr mut i64 = @int_to_ptr(@ptr_to_int(raw));
            let value: i64 = @ptr_read(p);
            let result: i32 = @intCast(value);
            result
        };
        0
    }"#;
    assert_eq!(
        expect_unsupported(partial).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerRead
        ))
    );

    let aggregate = r#"struct P { a: u8, b: u64 }
    fn main() -> i32 {
        checked {
            let size: u64 = @intCast(@size_of(P));
            let align: u64 = @intCast(@align_of(P));
            let raw: ptr mut u8 = @alloc_zeroed(size, align);
            let p: ptr mut P = @int_to_ptr(@ptr_to_int(raw));
            let value: P = @ptr_read(p);
            @free(raw, size, align);
            if value.a == 0 && value.b == 0 { 0 } else { 1 }
        }
    }"#;
    assert_eq!(exit(aggregate), 0);
}

#[test]
fn realloc_shrink_drops_the_tail_and_rejects_bad_contracts() {
    let shrink = r#"fn main() -> i32 {
        let result: i32 = checked {
            let mut raw: ptr mut u8 = @alloc(8, 1);
            @byte_set(raw, 7, 8);
            raw = @realloc(raw, 8, 1, 4);
            let p: ptr mut i32 = @int_to_ptr(@ptr_to_int(raw));
            @ptr_read(@ptr_offset(p, 1))
        };
        result
    }"#;
    assert_eq!(
        expect_unsupported(shrink).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerRead
        ))
    );

    let bad_size = r#"fn main() -> i32 {
        checked { let p: ptr mut u8 = @alloc(4, 1); @realloc(p, 3, 1, 8); };
        0
    }"#;
    assert_eq!(
        expect_unsupported(bad_size).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::Reallocate
        ))
    );
}

#[test]
fn nested_enum_aggregate_representation_round_trips_deterministically() {
    let source = r#"enum Choice { Empty, One(i32), Pair(i32, i32) }
    struct Boxed { choice: Choice, tail: i16 }
    fn main() -> i32 {
        checked {
            let size: u64 = @intCast(@size_of(Boxed));
            let align: u64 = @intCast(@align_of(Boxed));
            let raw: ptr mut u8 = @alloc(size, align);
            let p: ptr mut Boxed = @int_to_ptr(@ptr_to_int(raw));
            @ptr_write(p, Boxed { choice: Choice.Pair(7, 35), tail: 2 });
            let value: Boxed = @ptr_read(p);
            @free(raw, size, align);
            if value.choice == Choice.Pair(7, 35) && value.tail == 2 { 0 } else { 1 }
        }
    }"#;
    let first = run(source);
    let second = run(source);
    assert_eq!(first, second);
    assert_eq!(first.exit_code, 0);
}

/// Keep the representation authority honest against test-only mutations: a
/// little-endian byte change must be visible to decode, and changing the
/// encoded value must not be hidden by a parallel typed-cell cache.
#[test]
fn representation_encode_decode_detects_planted_byte_mutation() {
    let state = query_cfg_state("fn main() -> i32 { 0 }").expect("test state compiles");
    let interp = Interp {
        state: &state,
        stdout_trace: Vec::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
        small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
        heap_metadata_bytes: 0,
    };
    let (mut bytes, initialized, provenance) =
        match interp.encode_value(&Value::Int(0x0102_0304), Type::I32) {
            Ok(encoded) => encoded,
            Err(_) => panic!("i32 has a target representation"),
        };
    assert_eq!(bytes, [0x04, 0x03, 0x02, 0x01]);
    bytes[0] = 0x05;
    let decoded = match interp.decode_value(&bytes, &initialized, &provenance, Type::I32) {
        Ok(value) => value,
        Err(_) => panic!("mutated initialized bytes still decode"),
    };
    assert_eq!(decoded, Value::Int(0x0102_0305));
}

#[test]
fn pointer_bytes_retype_only_the_view_and_preserve_provenance() {
    let source = r#"fn main() -> i32 {
        checked {
            let raw: ptr mut u8 = @alloc(8, 8);
            let p: ptr mut i64 = @int_to_ptr(@ptr_to_int(raw));
            @ptr_write(p, 42);
            let ptr_size: u64 = @intCast(@size_of(ptr mut i64));
            let ptr_align: u64 = @intCast(@align_of(ptr mut i64));
            let bytes: ptr mut u8 = @alloc(ptr_size, ptr_align);
            let stored: ptr mut ptr mut u8 = @int_to_ptr(@ptr_to_int(bytes));
            @ptr_write(stored, @int_to_ptr(@ptr_to_int(p)));
            let retyped: ptr mut ptr mut i64 = @int_to_ptr(@ptr_to_int(bytes));
            let recovered: ptr mut i64 = @ptr_read(retyped);
            let answer: i64 = @ptr_read(recovered);
            @free(bytes, ptr_size, ptr_align);
            @free(raw, 8, 8);
            @intCast(answer)
        }
    }"#;
    assert_eq!(exit(source), 42);
}

#[test]
fn partial_pointer_bytes_gap_but_complete_zero_fill_is_null() {
    let partial = r#"fn main() -> i32 {
        checked {
            let target: ptr mut u8 = @alloc(1, 1);
            let source: ptr mut u8 = @alloc(8, 8);
            let stored: ptr mut ptr mut u8 = @int_to_ptr(@ptr_to_int(source));
            @ptr_write(stored, target);
            let copy: ptr mut u8 = @alloc(8, 8);
            @byte_copy(copy, source, 4);
            let view: ptr mut ptr mut u8 = @int_to_ptr(@ptr_to_int(copy));
            @intCast(@ptr_to_int(@ptr_read(view)))
        }
    }"#;
    assert_eq!(
        expect_unsupported(partial).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerRead
        ))
    );

    let zeroed = r#"fn main() -> i32 {
        checked {
            let raw: ptr mut u8 = @alloc(8, 8);
            @byte_set(raw, 0, 8);
            let view: ptr mut ptr mut u8 = @int_to_ptr(@ptr_to_int(raw));
            @intCast(@ptr_to_int(@ptr_read(view)))
        }
    }"#;
    assert_eq!(exit(zeroed), 0);
}

#[test]
fn allocator_contracts_reject_non_heap_and_mismatched_handles() {
    for (source, kind) in [
        (
            r#"fn main() -> i32 { checked { let p: ptr mut u8 = @alloc(4, 8); @free(p, 4, 1); }; 0 }"#,
            UnsupportedIntrinsicKind::Free,
        ),
        (
            r#"fn main() -> i32 { checked { let p: ptr mut u8 = @alloc(4, 1); @free(@ptr_offset(p, 1), 3, 1); }; 0 }"#,
            UnsupportedIntrinsicKind::Free,
        ),
        (
            r#"fn main() -> i32 { checked { let p: ptr mut u8 = @alloc(4, 1); @free(p, 4, 1); @free(p, 4, 1); }; 0 }"#,
            UnsupportedIntrinsicKind::Free,
        ),
        (
            r#"fn main() -> i32 { let mut p: ptr mut u8 = checked { @alloc(4, 1) }; checked { @resize(p, 3, 1, 4); }; 0 }"#,
            UnsupportedIntrinsicKind::Resize,
        ),
        (
            r#"fn main() -> i32 { checked { let zero: u64 = 0; let null: ptr mut u8 = @int_to_ptr(zero); @resize(null, 0, 1, 4); }; 0 }"#,
            UnsupportedIntrinsicKind::Resize,
        ),
    ] {
        assert_eq!(
            expect_unsupported(source).kind(),
            UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(kind))
        );
    }
}

#[test]
fn allocator_rejects_overaligned_layouts_before_classification() {
    let unsupported = expect_unsupported(
        r#"fn main() -> i32 {
            checked { let p: ptr mut u8 = @alloc(8, 8192); };
            0
        }"#,
    );
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::Allocate,
        ))
    );
    assert_eq!(unsupported.detail(), "invalid allocation alignment");

    let realloc = expect_unsupported(
        r#"fn main() -> i32 {
            checked {
                let p: ptr mut u8 = @alloc(8, 1);
                let q: ptr mut u8 = @realloc(p, 8, 8192, 16);
            };
            0
        }"#,
    );
    assert_eq!(
        realloc.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::Reallocate,
        ))
    );
}

#[test]
fn address_literals_need_pointer_integer_provenance() {
    let source = r#"fn main() -> i32 {
        checked {
            let address: u64 = 17592186044416;
            let fake: ptr mut i32 = @int_to_ptr(address);
            @ptr_read(fake)
        }
    }"#;
    assert_eq!(
        expect_unsupported(source).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::IntToPointer
        ))
    );
}

#[test]
fn matching_literal_stays_unprovenanced_after_pointer_address_exposure() {
    let source = r#"fn main() -> i32 {
        checked {
            let x: i32 = 7;
            let p: ptr mut i32 = @raw_mut(x);
            let exposed: u64 = @ptr_to_int(p);
            @dbg(exposed);
            let matching_literal: u64 = 17592186044416;
            let forged: ptr mut i32 = @int_to_ptr(matching_literal);
            @ptr_read(forged)
        }
    }"#;
    assert_eq!(
        expect_unsupported(source).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::IntToPointer
        ))
    );
}

#[test]
fn address_provenance_is_not_preserved_through_reverse_subtraction() {
    let source = r#"fn main() -> i32 {
        checked {
            let x: i32 = 7;
            let p: ptr mut i32 = @raw_mut(x);
            let address: u64 = @ptr_to_int(p);
            let candidate: u64 = address * 2 + 1;
            let forged: ptr mut i32 = @int_to_ptr(candidate - address);
            @ptr_read(forged)
        }
    }"#;
    assert_eq!(
        expect_unsupported(source).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::IntToPointer
        ))
    );
}

#[test]
fn nested_integer_equality_ignores_address_provenance() {
    let source = r#"struct AddressPair { values: [u64; 2] }
    fn main() -> i32 {
        checked {
            let x: i32 = 7;
            let p: ptr mut i32 = @raw_mut(x);
            let address: u64 = @ptr_to_int(p);
            let ordinary: u64 = address * 1;
            let with_provenance: AddressPair = AddressPair { values: [address, address] };
            let without_provenance: AddressPair = AddressPair { values: [ordinary, ordinary] };
            if with_provenance == without_provenance { 0 } else { 1 }
        }
    }"#;
    assert_eq!(exit(source), 0);
}

#[test]
fn pointer_offsets_allow_one_past_but_reject_outside_live_extent() {
    let before_base = r#"fn main() -> i32 {
        checked {
            let p: ptr mut u8 = @alloc(4, 1);
            let bad: ptr mut u8 = @ptr_offset(@ptr_offset(p, 1), -2);
            @intCast(@ptr_read(bad))
        }
    }"#;
    assert_eq!(
        expect_unsupported(before_base).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerOffset
        ))
    );

    let one_past = r#"fn main() -> i32 {
        checked {
            let p: ptr mut u8 = @alloc(4, 1);
            let end: ptr mut u8 = @ptr_offset(p, 4);
            @intCast(@ptr_read(end))
        }
    }"#;
    assert_eq!(
        expect_unsupported(one_past).kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerRead
        ))
    );
}

#[test]
fn heap_metadata_budget_fails_before_materializing_large_allocations() {
    let source = r#"fn main() -> i32 {
        checked {
            let n: u64 = 16777216;
            let p: ptr mut u8 = @alloc(n, 1);
            @ptr_to_int(p);
        };
        0
    }"#;
    assert_eq!(
        expect_unsupported(source).kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::InterpreterSteps)
    );
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

#[test]
fn realloc_same_small_class_keeps_address() {
    let src = r#"fn main() -> i32 {
        checked {
            let p: ptr mut u8 = @alloc(9, 1);
            let before: u64 = @ptr_to_int(p);
            @byte_set(p, 65, 9);
            let q: ptr mut u8 = @realloc(p, 9, 1, 10);
            let after: u64 = @ptr_to_int(q);
            let value: u8 = @ptr_read(q);
            @free(q, 10, 1);
            if before == after && value == 65 { 42 } else { 1 }
        }
    }"#;
    assert_eq!(run(src).exit_code, 42);
}

#[test]
fn realloc_different_small_class_moves_and_preserves_prefix() {
    let src = r#"fn main() -> i32 {
        checked {
            let p: ptr mut u8 = @alloc(16, 1);
            let before: u64 = @ptr_to_int(p);
            @byte_set(p, 7, 16);
            let q: ptr mut u8 = @realloc(p, 16, 1, 8);
            let after: u64 = @ptr_to_int(q);
            let value: u8 = @ptr_read(@ptr_offset(q, 7));
            @free(q, 8, 1);
            if before != after && value == 7 { 42 } else { 1 }
        }
    }"#;
    assert_eq!(run(src).exit_code, 42);
}

#[test]
fn realloc_destination_uses_freed_small_class_head() {
    let src = r#"fn main() -> i32 {
        checked {
            let released: ptr mut u8 = @alloc(16, 1);
            let released_address: u64 = @ptr_to_int(released);
            @free(released, 16, 1);

            let narrow: ptr mut u8 = @alloc(8, 1);
            let wide: ptr mut u8 = @realloc(narrow, 8, 1, 16);
            let wide_address: u64 = @ptr_to_int(wide);
            @free(wide, 16, 1);
            if released_address == wide_address { 42 } else { 1 }
        }
    }"#;
    assert_eq!(run(src).exit_code, 42);
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
