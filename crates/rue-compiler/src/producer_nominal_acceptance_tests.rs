//! RUE-1089 producer-nominal anonymous-type identity — programmatic acceptance
//! tests.
//!
//! This module is the home for the acceptance criteria that cannot be expressed
//! as spec/CLI TOML cases, either because they need programmatic assertions
//! (warm/fresh/cold parity, symbol-set comparison) or because they currently
//! hit the deliberately fail-closed E9000 blocker — which the spec/CLI runners
//! reject as an ICE that can never satisfy a `compile_fail` case
//! (`rue-test-runner::ice_message`).
//!
//! Companion behavioral cases live in
//! `crates/rue-spec/cases/expressions/producer_nominal_acceptance.toml`. The
//! full criterion → test map is in
//! `docs/notes/rue-1089-acceptance-ledger.md`.
//!
//! Two-state design: every test passes against the CURRENT tree (asserting the
//! present behavior, E9000 fail-closed included). The E9000 tests carry
//! `FLIPS-POST-ANCHOR-FIX` notes describing the exact edit that turns them into
//! the exit-42 execution regression once astgen and the durable fragment
//! evaluator mint anonymous-type anchors consistently.

#![cfg(test)]

use crate::*;
use std::collections::BTreeSet;
use std::sync::Arc;

use rue_target::Target;

/// The canonical RUE-1089 Wrap repro: a GENERIC struct producer whose method
/// reaches an anonymous-enum MEMBER (`self.inner`, of type `Option(T)`) under
/// the contextual (generic) anchor. This is the sole shape that currently hits
/// the fail-closed E9000 frontier. Post-anchor-fix it must compile and exit 42,
/// with the receiver field type, the match enum key, the payload operation, and
/// the enum layout all referring to ONE nominal identity.
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
/// names (`__anon_struct_N` / `__anon_enum_N`) are excluded because their base
/// display name is presently allocation-order-derived (Stage-4 note); their
/// COUNT is asserted separately via [`anonymous_type_count`].
fn named_symbols(semantic: &CanonicalSemanticOutput) -> BTreeSet<String> {
    symbol_names(semantic)
        .into_iter()
        .filter(|name| !name.contains("__anon_"))
        .filter(|name| !name.contains("unrelated")) // the intentionally-added extra decl
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
