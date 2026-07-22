//! RUE-1089 producer-nominal anonymous-type identity — programmatic acceptance
//! tests.
//!
//! This module is the home for the acceptance criteria that cannot be expressed
//! as spec/CLI TOML cases, because they need programmatic assertions
//! (warm/fresh/cold parity, symbol-set comparison, execution of the linked ELF)
//! or a test-only anchor-transport fault-injection hook.
//!
//! Companion behavioral cases live in
//! `crates/rue-spec/cases/expressions/producer_nominal_acceptance.toml` and
//! `crates/rue-cli-tests/cases/producer_nominal_targets.toml`. The full
//! criterion → test map is in `docs/notes/rue-1089-acceptance-ledger.md`.
//!
//! The anchor-transport fix (RUE-1089) has landed: the frontend anonymous-type
//! anchor is transported exactly into the durable evaluator, so the Wrap payload
//! shape compiles and executes, and an injected anchor divergence fails closed.

#![cfg(test)]

use crate::*;
use std::collections::BTreeSet;
use std::sync::Arc;

use rue_target::Target;

/// The canonical RUE-1089 Wrap repro: a GENERIC struct producer whose method
/// reaches an anonymous-enum MEMBER (`self.inner`, of type `Option(T)`) under
/// the contextual (generic) anchor. This was the sole shape that hit the
/// fail-closed E9000 frontier before the anchor-transport fix. It now compiles
/// and exits 42, with the receiver field type, the match enum key, the payload
/// operation, and the enum layout all referring to ONE nominal identity.
const WRAP_REPRO: &str = r#"
fn Option(comptime T: type) -> type { enum { Some(T), None } }
fn Wrap(comptime T: type) -> type {
    struct {
        inner: Option(T),
        fn get_or(self, d: T) -> T {
            let O = Option(T);
            match self.inner { O.Some(v) => v, O.None => d }
        }
    }
}
fn main() -> i32 {
    let W = Wrap(i32);
    let O = Option(i32);
    let w: W = W { inner: O.Some(42) };
    w.get_or(0)
}
"#;

/// A methodful producer that mints several distinct anonymous types in
/// different positions. Used for the determinism / warm-fresh / identity
/// stability criteria (4 and 3).
const MULTI_ANON_PRODUCER: &str = r#"
fn Holder() -> type {
    struct {
        v: i32,
        fn first(self) -> i32 {
            let A = struct { x: i32 };
            let a: A = A { x: self.v };
            a.x
        }
        fn second(self) -> i32 {
            let B = struct { y: i32 };
            let b: B = B { y: self.v };
            b.y * 2
        }
    }
}
fn main() -> i32 {
    let H = Holder();
    let h: H = H { v: 14 };
    h.first() + h.second()
}
"#;

/// The same program as [`MULTI_ANON_PRODUCER`] with the sibling methods
/// REORDERED and an unrelated top-level declaration added. Producer-nominal
/// identity must be unchanged by these edits.
const MULTI_ANON_PRODUCER_REORDERED: &str = r#"
fn unrelated() -> i32 { 99 }
fn Holder() -> type {
    struct {
        v: i32,
        fn second(self) -> i32 {
            let B = struct { y: i32 };
            let b: B = B { y: self.v };
            b.y * 2
        }
        fn first(self) -> i32 {
            let A = struct { x: i32 };
            let a: A = A { x: self.v };
            a.x
        }
    }
}
fn main() -> i32 {
    let H = Holder();
    let h: H = H { v: 14 };
    h.first() + h.second()
}
"#;

/// Compile a single (import-free) source through a FRESH session and return its
/// canonical semantic output (or the collected errors).
fn fresh_semantic(
    source: &str,
    options: &CompileOptions,
) -> Result<Arc<CanonicalSemanticOutput>, CompileErrors> {
    let snapshot = SourceSnapshot::single("<acceptance>", source).map_err(CompileErrors::from)?;
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result()?;
    session.canonical_semantic(options)
}

/// Compile a single source WARM: publish an unrelated prior revision, compile
/// it, then publish and compile the target source in the same session. This
/// exercises the incremental path against the same session state a fresh
/// compile never sees.
fn warm_semantic(
    prior: &str,
    source: &str,
    options: &CompileOptions,
) -> Result<Arc<CanonicalSemanticOutput>, CompileErrors> {
    let mut session = CompilerSession::new();
    let prior_snapshot =
        SourceSnapshot::single("<acceptance>", prior).map_err(CompileErrors::from)?;
    session.update(&prior_snapshot).into_result()?;
    session.canonical_semantic(options).ok(); // prior revision is not oracled.
    let snapshot = SourceSnapshot::single("<acceptance>", source).map_err(CompileErrors::from)?;
    session.update(&snapshot).into_result()?;
    session.canonical_semantic(options)
}

/// The emitted symbol names of a semantic output: every struct/enum symbol and
/// every function machine name. Two independent cold compiles of one program
/// must produce identical sets.
fn symbol_names(semantic: &CanonicalSemanticOutput) -> BTreeSet<String> {
    let pool = semantic.type_pool();
    let mut names = BTreeSet::new();
    for id in pool.all_struct_ids() {
        names.insert(format!("struct:{}", pool.struct_symbol_name(id)));
    }
    for id in pool.all_enum_ids() {
        names.insert(format!("enum:{}", pool.enum_symbol_name(id)));
    }
    for function in semantic.functions() {
        names.insert(format!("fn:{}", function.machine_name));
    }
    names
}

/// The named (non-anonymous) type/function symbols of a semantic output. These
/// are invariant under reordering and unrelated edits. Anonymous synthetic
/// symbols are asserted separately via [`anonymous_symbols`], which since the
/// Stage-A stable-naming cut (ADR-0066, RUE-1089) are also invariant under those
/// edits — their disambiguating suffix is a digest of the producer identity, not
/// an allocation-order counter.
fn named_symbols(semantic: &CanonicalSemanticOutput) -> BTreeSet<String> {
    symbol_names(semantic)
        .into_iter()
        .filter(|name| !name.contains("__anon_"))
        .filter(|name| !name.contains("unrelated")) // the intentionally-added extra decl
        .collect()
}

/// The anonymous synthetic type symbols of a semantic output — the struct/enum
/// symbols whose spelling carries the `__anon_struct_`/`__anon_enum_` prefix.
/// Since the Stage-A cut (ADR-0066, RUE-1089) each spelling is a STABLE digest
/// of the producer identity, so this set is identical across independent cold
/// compiles and across warm/fresh, and unchanged by unrelated edits.
fn anonymous_symbols(semantic: &CanonicalSemanticOutput) -> BTreeSet<String> {
    symbol_names(semantic)
        .into_iter()
        .filter(|name| name.contains("__anon_struct_") || name.contains("__anon_enum_"))
        .collect()
}

/// Count the anonymous struct/enum types minted into the type pool.
fn anonymous_type_count(semantic: &CanonicalSemanticOutput) -> usize {
    let pool = semantic.type_pool();
    let structs = pool
        .all_struct_ids()
        .filter(|&id| pool.struct_def(id).name.starts_with("__anon_struct"))
        .count();
    let enums = pool
        .all_enum_ids()
        .filter(|&id| pool.enum_def(id).name.starts_with("__anon_enum"))
        .count();
    structs + enums
}

// ---------------------------------------------------------------------------
// Criterion 3 — identity stability under unrelated edits
// ---------------------------------------------------------------------------

/// Reordering sibling methods and adding an unrelated declaration does not
/// change which anonymous identities exist, nor the named symbol surface.
#[test]
fn producer_nominal_identity_is_stable_under_unrelated_edits() {
    let options = CompileOptions::default();
    let baseline = fresh_semantic(MULTI_ANON_PRODUCER, &options)
        .expect("baseline multi-anon producer compiles");
    let reordered = fresh_semantic(MULTI_ANON_PRODUCER_REORDERED, &options)
        .expect("reordered multi-anon producer compiles");

    // The same set of anonymous identities exists in both orderings.
    assert_eq!(
        anonymous_type_count(&baseline),
        anonymous_type_count(&reordered),
        "reordering methods / adding an unrelated decl changed the anonymous identity count",
    );
    assert!(
        anonymous_type_count(&baseline) >= 2,
        "the Holder producer should mint at least the two written anonymous struct identities \
         (found {})",
        anonymous_type_count(&baseline),
    );

    // The named symbol surface (Holder's methods, main, drop glue) is unchanged.
    assert_eq!(
        named_symbols(&baseline),
        named_symbols(&reordered),
        "reordering methods / adding an unrelated decl changed the named symbol surface",
    );

    // Stage A (RUE-1089): the ANONYMOUS symbol surface is likewise unchanged.
    // Each anonymous symbol's suffix is a digest of its producer identity
    // (method name + definition-relative anchor), both preserved when sibling
    // methods are reordered and an unrelated top-level decl is added, so the
    // allocation-order-independent spellings match exactly.
    let baseline_anon = anonymous_symbols(&baseline);
    assert!(
        !baseline_anon.is_empty(),
        "the Holder producer must emit at least one anonymous symbol to assert stability over",
    );
    assert_eq!(
        baseline_anon,
        anonymous_symbols(&reordered),
        "reordering methods / adding an unrelated decl changed the anonymous symbol spellings \
         (Stage-A stable naming must make them allocation-order-independent)",
    );
}

// ---------------------------------------------------------------------------
// Criterion 4 — warm / fresh / cold parity: identical semantic bodies,
// layouts, and symbols
// ---------------------------------------------------------------------------

/// Two independent COLD compiles of the same program produce byte-identical
/// semantic output (bodies, layouts, type pool, dependencies) and an identical
/// emitted symbol set. This is the determinism half of the parity oracle,
/// asserted through the same `unstable_parity_snapshot` projection the scaling
/// harness uses.
#[test]
fn producer_nominal_semantic_output_is_deterministic_across_cold_compiles() {
    let options = CompileOptions::default();
    let first = fresh_semantic(MULTI_ANON_PRODUCER, &options).expect("first cold compile");
    let second = fresh_semantic(MULTI_ANON_PRODUCER, &options).expect("second cold compile");

    assert_eq!(
        first.unstable_parity_snapshot(),
        second.unstable_parity_snapshot(),
        "two cold compiles of the same program diverged in semantic parity snapshot",
    );
    assert_eq!(
        symbol_names(&first),
        symbol_names(&second),
        "two cold compiles of the same program emitted different symbol names",
    );

    // Stage A (RUE-1089): assert the ANONYMOUS symbols specifically, not merely
    // that the full set matches. Their spellings are stable digests of the
    // producer identity, so two independent cold compiles must agree on every
    // `__anon_struct_`/`__anon_enum_` symbol exactly — the property that made
    // the prior allocation-order counter unsound for incremental linking and
    // parallel compilation.
    let first_anon = anonymous_symbols(&first);
    assert!(
        !first_anon.is_empty(),
        "the acceptance producer must emit anonymous symbols to assert determinism over",
    );
    assert_eq!(
        first_anon,
        anonymous_symbols(&second),
        "two cold compiles emitted different anonymous symbol spellings",
    );
}

/// Distinct producers minting same-shape anonymous types receive DISTINCT stable
/// anonymous symbols; the same producer key receives the same symbol. This is
/// the identity half of the Stage-A naming property (a digest that both
/// disambiguates producers and is allocation-order-independent).
#[test]
fn distinct_producers_receive_distinct_stable_anonymous_symbols() {
    // Two separate producers `L` and `R` each mint an anonymous struct of the
    // SAME shape (`{ x: i32 }`). Producer-nominal identity makes them distinct
    // types, so their stable symbols must differ.
    let source = r#"
fn L() -> type { struct { x: i32 } }
fn R() -> type { struct { x: i32 } }
fn main() -> i32 {
    let TL = L();
    let TR = R();
    let a: TL = TL { x: 40 };
    let b: TR = TR { x: 2 };
    a.x + b.x
}
"#;
    let options = CompileOptions::default();
    let first = fresh_semantic(source, &options).expect("distinct-producer program compiles");
    let anon = anonymous_symbols(&first);
    assert_eq!(
        anon.len(),
        2,
        "two distinct same-shape producers must yield two distinct anonymous symbols, got {anon:?}",
    );

    // The same program compiled again yields the SAME two symbols (stability),
    // and they are the same set (determinism) — never a re-numbered pair.
    let second = fresh_semantic(source, &options).expect("second compile");
    assert_eq!(
        anon,
        anonymous_symbols(&second),
        "distinct-producer anonymous symbols were not stable across cold compiles",
    );
}

/// A WARM (incremental) compile of the acceptance program — reached after the
/// session already compiled an unrelated prior revision — produces semantic
/// output identical to a FRESH compile of the same program. This reuses the
/// scaling harness's parity machinery (`unstable_parity_snapshot`) test-side.
#[test]
fn producer_nominal_warm_and_fresh_semantic_output_agree() {
    let options = CompileOptions::default();
    let warm = warm_semantic("fn main() -> i32 { 0 }", MULTI_ANON_PRODUCER, &options)
        .expect("warm compile of multi-anon producer");
    let fresh = fresh_semantic(MULTI_ANON_PRODUCER, &options).expect("fresh compile");

    assert_eq!(
        warm.unstable_parity_snapshot(),
        fresh.unstable_parity_snapshot(),
        "warm/fresh semantic parity snapshot diverged for the acceptance producer",
    );
    assert_eq!(
        symbol_names(&warm),
        symbol_names(&fresh),
        "warm/fresh emitted symbol names diverged for the acceptance producer",
    );

    // Stage A (RUE-1089): the warm incremental session and the fresh session
    // assign different session-local token issuers, so an allocation-order name
    // could diverge here. The stable digest resolves each token to its
    // request-independent endpoint content first, so every anonymous symbol
    // agrees exactly.
    let warm_anon = anonymous_symbols(&warm);
    assert!(
        !warm_anon.is_empty(),
        "the acceptance producer must emit anonymous symbols to compare warm vs fresh",
    );
    assert_eq!(
        warm_anon,
        anonymous_symbols(&fresh),
        "warm/fresh emitted different anonymous symbol spellings",
    );
}

// ---------------------------------------------------------------------------
// Criterion 5 — the Wrap repro's single-nominal-identity check
// (currently fail-closed E9000)
// ---------------------------------------------------------------------------

/// Execute a linked Rue program and return its process output. Mirrors
/// `pipeline_tests::execute_compiled_output`.
#[cfg(unix)]
fn execute_wrap(output: &CompileOutput, label: &str) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "rue-producer-nominal-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::write(&path, &output.elf).expect("write linked Rue executable");
    let mut permissions = std::fs::metadata(&path)
        .expect("read executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("make executable runnable");
    let result = std::process::Command::new(&path).output();
    std::fs::remove_file(&path).expect("remove executable after execution");
    result.expect("execute linked Rue program")
}

/// Count the anonymous ENUM identities minted into the pool.
fn anonymous_enum_count(semantic: &CanonicalSemanticOutput) -> usize {
    let pool = semantic.type_pool();
    pool.all_enum_ids()
        .filter(|&id| pool.enum_def(id).name.starts_with("__anon_enum"))
        .count()
}

/// FLIPPED-POST-ANCHOR-FIX (RUE-1089). The generic `Wrap` whose `get_or` method
/// matches its anonymous-enum field `Option(T)` now compiles and executes to the
/// payload value 42. astgen and the durable fragment evaluator agree on the
/// anonymous-type anchor: the receiver field type, the `Option(T)` inside the
/// match, the match enum key, the payload operation, and the enum layout all
/// resolve to ONE `Option$…` nominal identity — observable as exactly one
/// anonymous enum in the type pool.
#[cfg(unix)]
#[test]
fn wrap_single_nominal_identity_executes_to_the_payload() {
    let options = CompileOptions::default();
    let semantic = fresh_semantic(WRAP_REPRO, &options).expect("Wrap repro compiles");

    // A single anonymous Option identity backs every reach of `self.inner`.
    assert_eq!(
        anonymous_enum_count(&semantic),
        1,
        "the Wrap repro must resolve to exactly one anonymous Option enum identity",
    );

    let snapshot = SourceSnapshot::single("<wrap>", WRAP_REPRO).expect("snapshot");
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().expect("publish");
    let output = crate::queries::compile_with_session(&mut session, &snapshot, &options)
        .expect("Wrap repro links");
    let execution = execute_wrap(&output, "single");
    assert_eq!(
        execution.status.code(),
        Some(42),
        "Wrap repro must execute to the payload value 42: {execution:?}",
    );
}

// ---------------------------------------------------------------------------
// Criterion 6 — both backends execute the Wrap payload regression
// (currently fail-closed on both backend targets)
// ---------------------------------------------------------------------------

/// FLIPPED-POST-ANCHOR-FIX (RUE-1089). The Wrap payload regression now compiles
/// on BOTH backend targets: the unified anchor is a frontend fact reached before
/// backend selection, so both `x86-64-linux` and `aarch64-linux` link. The
/// native x86-64 ELF executes to exit 42; aarch64 stays a structural
/// cross-compile check off its native host (mirroring `cli.abi_conformance`).
#[cfg(unix)]
#[test]
fn wrap_payload_executes_on_both_backend_targets() {
    for target in [Target::X86_64Linux, Target::Aarch64Linux] {
        let options = CompileOptions {
            target,
            ..CompileOptions::default()
        };
        fresh_semantic(WRAP_REPRO, &options).unwrap_or_else(|errors| {
            panic!("target {target:?}: Wrap repro must compile: {errors}")
        });

        let snapshot = SourceSnapshot::single("<wrap>", WRAP_REPRO).expect("snapshot");
        let mut session = CompilerSession::new();
        session.update(&snapshot).into_result().expect("publish");
        let output = crate::queries::compile_with_session(&mut session, &snapshot, &options)
            .unwrap_or_else(|errors| panic!("target {target:?}: Wrap repro must link: {errors}"));

        if target == Target::X86_64Linux {
            let execution = execute_wrap(&output, "x86");
            assert_eq!(
                execution.status.code(),
                Some(42),
                "target {target:?}: Wrap payload must execute to 42: {execution:?}",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Criterion 7 — an artificial anchor-transport disagreement fails closed
// ---------------------------------------------------------------------------

/// Render every error's code and message into one string for substring checks.
fn rendered_errors(errors: &CompileErrors) -> String {
    errors
        .iter()
        .map(|error| format!("[{}] {}", error.kind.code(), error.kind))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Wrap shape (a generic struct producer whose method reaches its
/// anonymous-enum field), carrying a test-only marker inside the producer whose
/// durable identity a reached member consumes. The marker rides into the
/// reparsed fragment source and drives the evaluator's fault-injection hook,
/// corrupting the transported anchor table for that declaration exactly as a
/// real transport bug would. This is the shape where the durable identity is
/// load-bearing, so a fail-closed transport error must sink the whole request.
fn fault_probe_program(marker: &str) -> String {
    format!(
        r#"
fn Option(comptime T: type) -> type {{ enum {{ Some(T), None }} }}
fn Wrap(comptime T: type) -> type {{
    // {marker}
    struct {{
        inner: Option(T),
        fn get_or(self, d: T) -> T {{
            let O = Option(T);
            match self.inner {{ O.Some(v) => v, O.None => d }}
        }}
    }}
}}
fn main() -> i32 {{
    let W = Wrap(i32);
    let O = Option(i32);
    let w: W = W {{ inner: O.Some(42) }};
    w.get_or(0)
}}
"#
    )
}

/// A DIVERGENT transported anchor — a wrong-but-present anchor published for the
/// producer whose durable identity a reached member consumes — reproduces the
/// exact pre-fix hazard. It must fail closed LOUD: the reached `get_or` member
/// cannot match its owner terminal, so a typed E9000-class internal diagnostic
/// surfaces and the request returns `Err`. Never a silent miscompile.
#[test]
fn divergent_anchor_transport_fails_closed_loud() {
    let options = CompileOptions::default();
    let program = fault_probe_program("__RUE1089_FAULT_DIVERGE__");
    let errors = match fresh_semantic(&program, &options) {
        Err(errors) => errors,
        Ok(_) => {
            panic!("a divergent transported anchor must fail closed, but the program compiled")
        }
    };
    let rendered = rendered_errors(&errors);
    assert!(
        errors
            .iter()
            .any(|error| error.kind.code() == rue_error::ErrorCode::INTERNAL_ERROR),
        "expected a fail-closed E9000 internal diagnostic, got:\n{rendered}",
    );
}

/// HARDENED (RUE-1089 Stage D). The resolve-level corruptions — a missing
/// locator, a duplicate locator, or a kind mismatch — are each an invariant
/// violation of the atomic `{source, anonymous_sites}` anchored artifact once
/// the raw fragment terminal exists. They must fail closed LOUD, exactly like a
/// divergent anchor: the committed E9000-class internal error is the sole
/// authority and must NEVER be downgraded to a retryable abort that AIR rescues
/// by recomputing the identity from RIR.
///
/// Previously these three modes were "frontend-recoverable" (the durable
/// producer's failure was masked by a live AIR mint), which created a second
/// identity authority and hid transport defects. The reviewer ruled that
/// unacceptable; every mode now sinks the request with a typed internal
/// diagnostic and publishes NO semantic output.
#[test]
fn resolve_level_transport_corruptions_fail_closed_loud() {
    let options = CompileOptions::default();
    for marker in [
        "__RUE1089_FAULT_MISSING__",
        "__RUE1089_FAULT_DUPLICATE__",
        "__RUE1089_FAULT_WRONG_KIND__",
    ] {
        let program = fault_probe_program(marker);
        // Zero publication: the request yields NO `CanonicalSemanticOutput`, so
        // no nominal/member/alias terminal reached the caller.
        let errors = match fresh_semantic(&program, &options) {
            Err(errors) => errors,
            Ok(_) => panic!(
                "corruption mode {marker} must fail closed with no published semantic output, \
                 but the program compiled",
            ),
        };
        let rendered = rendered_errors(&errors);
        assert!(
            errors
                .iter()
                .any(|error| error.kind.code() == rue_error::ErrorCode::INTERNAL_ERROR),
            "corruption mode {marker} must raise a typed E9000 internal diagnostic, got:\n{rendered}",
        );
    }
}

/// Without a fault marker the same probe compiles and runs — proving the hook is
/// inert by default and the fault behavior above is caused by the injection.
#[cfg(unix)]
#[test]
fn fault_probe_compiles_and_runs_cleanly_without_a_marker() {
    let options = CompileOptions::default();
    let program = fault_probe_program("no fault here");
    fresh_semantic(&program, &options).expect("the unmarked probe must compile");
    let snapshot = SourceSnapshot::single("<fault>", &program).expect("snapshot");
    let mut session = CompilerSession::new();
    session.update(&snapshot).into_result().expect("publish");
    let output = crate::queries::compile_with_session(&mut session, &snapshot, &options)
        .expect("unmarked probe links");
    let execution = execute_wrap(&output, "clean");
    assert_eq!(
        execution.status.code(),
        Some(42),
        "unmarked probe: {execution:?}"
    );
}

// ---------------------------------------------------------------------------
// Evaluator correspondence — two same-kind sites in one producer, reversed
// order, must not swap identities.
// ---------------------------------------------------------------------------

/// A single comptime producer binds two same-kind anonymous structs with
/// DIFFERENT fields, then selects one by a comptime flag. Each site must map to
/// its own frontend anchor: a span→anchor mix-up would give the selected local
/// the other site's anchor, which the runtime reference (under AstGen's real
/// anchor) could not resolve. Both selections, in both source orders, must
/// compile and run correctly — a set-equality check alone would miss a swap.
#[test]
fn evaluator_correspondence_two_same_kind_sites_do_not_swap() {
    let options = CompileOptions::default();
    // `A` bound first, `B` second; the field names differ so a swap changes the
    // constructed field and fails to compile or returns the wrong value.
    let forward = r#"
fn Choose(comptime pick_a: bool) -> type {
    let A = struct { a: i32 };
    let B = struct { b: i32 };
    if pick_a { A } else { B }
}
fn main() -> i32 {
    let TA = Choose(true);
    let TB = Choose(false);
    let a: TA = TA { a: 40 };
    let b: TB = TB { b: 2 };
    a.a + b.b
}
"#;
    // The two bindings in the opposite source order (byte offsets shift, anchors
    // must not).
    let reversed = r#"
fn Choose(comptime pick_a: bool) -> type {
    let B = struct { b: i32 };
    let A = struct { a: i32 };
    if pick_a { A } else { B }
}
fn main() -> i32 {
    let TA = Choose(true);
    let TB = Choose(false);
    let a: TA = TA { a: 40 };
    let b: TB = TB { b: 2 };
    a.a + b.b
}
"#;
    for (label, source) in [("forward", forward), ("reversed", reversed)] {
        let snapshot = SourceSnapshot::single("<choose>", source).expect("snapshot");
        let mut session = CompilerSession::new();
        session.update(&snapshot).into_result().expect("publish");
        let output = crate::queries::compile_with_session(&mut session, &snapshot, &options)
            .unwrap_or_else(|errors| panic!("{label}: Choose must compile: {errors}"));
        #[cfg(unix)]
        {
            let execution = execute_wrap(&output, label);
            assert_eq!(
                execution.status.code(),
                Some(42),
                "{label}: two same-kind sites must keep their own identities: {execution:?}",
            );
        }
        #[cfg(not(unix))]
        let _ = output;
    }
}

/// Only one of two syntactic anonymous sites is ever evaluated (a comptime `if`
/// picks a branch), so runtime consumption is a strict SUBSET of the transported
/// table. This must compile — the fail-closed rule requires every CONSUMED
/// locator to resolve, never every transported entry to be observed.
#[test]
fn selected_branch_consumes_a_subset_of_the_transported_table() {
    let options = CompileOptions::default();
    let source = r#"
fn Pick(comptime take_first: bool) -> type {
    if take_first { struct { first: i32 } } else { struct { second: i32 } }
}
fn main() -> i32 {
    let T = Pick(true);
    let t: T = T { first: 42 };
    t.first
}
"#;
    fresh_semantic(source, &options).expect("only the selected branch's site is consumed");
}

/// Trivia and unrelated declarations before the producer shift every module and
/// fragment byte offset, but the transported anchor is definition-relative, so
/// behavior is unchanged.
#[test]
fn anchor_transport_survives_trivia_and_unrelated_shifts() {
    let options = CompileOptions::default();
    let baseline = r#"
fn Box(comptime T: type) -> type {
    struct {
        v: T,
        fn get(self) -> T { self.v }
    }
}
fn main() -> i32 {
    let B = Box(i32);
    let b: B = B { v: 42 };
    b.get()
}
"#;
    let shifted = r#"
// an unrelated leading comment that shifts every byte offset below
fn unrelated_helper() -> i32 { 7 }

fn Box(comptime T: type) -> type {
    // trivia inside the producer body
    struct {
        v: T,
        fn get(self) -> T { self.v }
    }
}
fn main() -> i32 {
    let B = Box(i32);
    let b: B = B { v: 42 };
    b.get()
}
"#;
    let base = fresh_semantic(baseline, &options).expect("baseline compiles");
    let shift = fresh_semantic(shifted, &options).expect("shifted compiles");
    assert_eq!(
        anonymous_type_count(&base),
        anonymous_type_count(&shift),
        "trivia/unrelated shifts changed the anonymous identity count",
    );
}
