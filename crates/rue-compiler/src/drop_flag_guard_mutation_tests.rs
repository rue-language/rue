//! The drop-flag guard mutation (RUE-2367): delete a drop-flag guard from a
//! lowered CFG and expect the CFG verifier to reject the result.
//!
//! A guard `if flag != 0 { drop }` protects the drop of a place that may have
//! been moved out. Deleting it drops a moved-out value on some path, which is
//! exactly the builder bug family of RUE-2319, RUE-2356 and RUE-2378. The
//! verifier sees it through the CFG's `MoveOut` markers.

use crate::*;

/// Lower `source` as an ordinary executable request at `opt_level` and
/// return its CFGs.
fn rooted_cfg_at(
    source: &str,
    opt_level: OptLevel,
) -> Result<crate::session::RootedCfgOutput, CompileErrors> {
    let snapshot = SourceSnapshot::single("main.rue", source).map_err(CompileErrors::from)?;
    let options = CompileOptions {
        opt_level,
        ..CompileOptions::default()
    };
    let (_, semantic, _) = crate::test_frontend_snapshot(&snapshot, &options)?;
    Ok(semantic)
}

/// The CFGs as the builder lowers them, before any optimization.
fn rooted_cfg(source: &str) -> Result<crate::session::RootedCfgOutput, CompileErrors> {
    rooted_cfg_at(source, OptLevel::O0)
}

/// Mutation counts. A guard is *harmful* to delete when its flag may be zero
/// at the test, so the deletion drops a moved-out value on some path; the
/// builder also emits guards whose flag is always set there, and deleting
/// one of those changes nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Mutations {
    /// Guards found across every function of the program.
    guards: usize,
    /// Of those, the guards harmful to delete.
    harmful: usize,
    /// Guards whose deletion, one at a time, the verifier rejects.
    rejected: usize,
    /// Harmful guards whose deletion the verifier rejects.
    harmful_rejected: usize,
    /// Guards not harmful to delete whose deletion the verifier rejects
    /// anyway (the harmfulness scan is a lower bound: it only follows
    /// constant stores to the flag).
    benign_rejected: usize,
    /// Functions with at least one harmful guard.
    guarded_functions: usize,
    /// Of those, the functions the verifier rejects once every guard is gone.
    rejected_functions: usize,
}

fn mutate(output: &crate::session::RootedCfgOutput) -> (Mutations, Vec<String>) {
    let mut counts = Mutations::default();
    let mut accepted = Vec::new();
    for unit in output.functions() {
        let cfg = unit.cfg();
        let guards = cfg.drop_flag_guard_blocks();
        let harmful = guards
            .iter()
            .filter(|&&guard| cfg.drop_flag_guard_may_skip(guard))
            .count();
        if harmful > 0 {
            counts.guarded_functions += 1;
            if cfg
                .verify_without_drop_flag_guards(unit.type_pool(), &guards)
                .is_err()
            {
                counts.rejected_functions += 1;
            }
        }
        for &guard in &guards {
            let is_harmful = cfg.drop_flag_guard_may_skip(guard);
            counts.guards += 1;
            counts.harmful += usize::from(is_harmful);
            if let Err(error) = cfg.verify_without_drop_flag_guards(unit.type_pool(), &[guard]) {
                counts.rejected += 1;
                counts.harmful_rejected += usize::from(is_harmful);
                if !is_harmful {
                    counts.benign_rejected += 1;
                    eprintln!("benign mutant rejected: {error}");
                }
            } else if is_harmful {
                accepted.push(format!("{} guard {guard}", unit.source_name()));
            }
        }
    }
    (counts, accepted)
}

/// Programs covering each way the builder guards a maybe-moved place: a
/// whole binding, a field path at depth 1 to 3, a by-value parameter, a
/// `match` scrutinee, an overwrite of a maybe-moved place, and loop heads
/// reached with the place moved on the back edge (RUE-2356).
const GUARDED_PROGRAMS: &[&str] = &[
    r#"
struct S { v: i64 }
drop fn S(self) { @dbg(self.v); }
fn take(s: S) -> i64 { s.v }
fn main() -> i32 {
    let x = S { v: 1 };
    let c = 3;
    if c > 2 { take(x); }
    0
}
"#,
    r#"
struct S { v: i64 }
drop fn S(self) { @dbg(self.v); }
fn take(s: S) -> i64 { s.v }
fn run(s: S, c: bool) {
    if c { take(s); }
}
fn main() -> i32 { run(S { v: 1 }, true); run(S { v: 2 }, false); 0 }
"#,
    r#"
struct S { v: i64 }
drop fn S(self) { @dbg(self.v); }
struct Q { w: S }
struct R { q: Q }
struct H { a: S, w: S, q: Q, r: R }
fn take(s: S) -> i64 { s.v }
fn run(c: i64) {
    let mut h = H { a: S { v: 1 }, w: S { v: 2 }, q: Q { w: S { v: 3 } }, r: R { q: Q { w: S { v: 4 } } } };
    if c > 0 { take(h.w); }
    if c > 1 { take(h.q.w); }
    if c > 2 { take(h.r.q.w); }
    if c > 3 { h.w = S { v: 5 }; }
}
fn main() -> i32 { run(0); run(4); 0 }
"#,
    r#"
struct S { v: i64 }
drop fn S(self) { @dbg(self.v); }
struct Q { w: S }
struct R { q: Q }
struct H { a: S, w: S, q: Q, r: R }
fn run(n: i64) {
    let mut h = H { a: S { v: 1 }, w: S { v: 2 }, q: Q { w: S { v: 3 } }, r: R { q: Q { w: S { v: 4 } } } };
    let mut i: i64 = 0;
    while i < n {
        h.w = S { v: 10 + i };
        let x: S = h.w;
        @drop(x);
        h.q.w = S { v: 20 + i };
        let y: S = h.q.w;
        @drop(y);
        h.r.q.w = S { v: 30 + i };
        let z: S = h.r.q.w;
        @drop(z);
        i = i + 1;
    }
}
fn main() -> i32 { run(2); run(0); 0 }
"#,
    r#"
struct S { v: i64 }
drop fn S(self) { @dbg(self.v); }
fn take(s: S) -> i64 { s.v }
fn run(n: i64) {
    let mut x = S { v: 1 };
    let mut i: i64 = 0;
    while i < n {
        if i == 1 { take(x); x = S { v: 2 }; }
        if i == 3 { take(x); break; }
        i = i + 1;
    }
}
fn main() -> i32 { run(0); run(2); run(5); 0 }
"#,
    r#"
struct S { v: i64 }
drop fn S(self) { @dbg(self.v); }
enum E { A(S), B }
fn take(s: S) -> i64 { s.v }
fn run(e: E, c: bool) -> i64 {
    if c {
        match e {
            E.A(s) => take(s),
            E.B => 0,
        }
    } else {
        1
    }
}
fn main() -> i32 { run(E.A(S { v: 1 }), true); run(E.A(S { v: 2 }), false); 0 }
"#,
];

#[test]
fn verifier_rejects_every_deleted_drop_flag_guard() {
    for source in GUARDED_PROGRAMS {
        let output = rooted_cfg(source)
            .unwrap_or_else(|errors| panic!("the guarded program compiles: {errors:?}\n{source}"));
        let (counts, accepted) = mutate(&output);
        assert!(
            counts.harmful > 0,
            "the program has a drop-flag guard whose flag may be clear:\n{source}"
        );
        assert!(
            accepted.is_empty(),
            "the verifier accepted deleted drop-flag guards {accepted:?} in:\n{source}"
        );
    }
}

/// The corpus survey behind RUE-2367's before and after counts. Set
/// `RUE_2367_CORPUS` to a comma-separated list of absolute spec-case TOML
/// paths and run this ignored test by name; `RUE_2367_O2=1` surveys the
/// CFGs after `-O2` optimization instead of the builder's.
#[test]
#[ignore]
fn drop_flag_guard_mutation_corpus_survey() {
    let Ok(paths) = std::env::var("RUE_2367_CORPUS") else {
        eprintln!("RUE_2367_CORPUS is unset; nothing to survey");
        return;
    };
    let opt_level = if std::env::var_os("RUE_2367_O2").is_some() {
        OptLevel::O2
    } else {
        OptLevel::O0
    };
    let mut total = Mutations::default();
    let (mut cases, mut guarded_cases, mut rejected_cases, mut skipped) = (0, 0, 0, 0);
    for path in paths.split(',') {
        let text = std::fs::read_to_string(path).expect("corpus file is readable");
        for case in text.split("[[case]]").skip(1) {
            if case.contains("compile_fail") {
                continue;
            }
            let name = case
                .split("name = \"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or("?");
            let Some(source) = case
                .split("source = \"\"\"")
                .nth(1)
                .and_then(|rest| rest.split("\"\"\"").next())
            else {
                continue;
            };
            cases += 1;
            let Ok(output) = rooted_cfg_at(source, opt_level) else {
                skipped += 1;
                continue;
            };
            let (counts, accepted) = mutate(&output);
            for entry in accepted {
                eprintln!("accepted: {name}: {entry}");
            }
            if counts.benign_rejected > 0 {
                eprintln!(
                    "note: {name}: {} deleted guards rejected although their flag is never clear",
                    counts.benign_rejected
                );
            }
            total.guards += counts.guards;
            total.harmful += counts.harmful;
            total.rejected += counts.rejected;
            total.harmful_rejected += counts.harmful_rejected;
            total.guarded_functions += counts.guarded_functions;
            total.rejected_functions += counts.rejected_functions;
            if counts.guarded_functions > 0 {
                guarded_cases += 1;
                if counts.rejected_functions == counts.guarded_functions {
                    rejected_cases += 1;
                }
            }
        }
    }
    eprintln!(
        "RUE-2367 survey: {cases} cases ({skipped} not lowered here), {guarded_cases} with a \
         harmful guard; every guard deleted: {rejected_cases}/{guarded_cases} cases and {}/{} \
         functions rejected; one guard deleted at a time: {}/{} harmful rejected, {}/{} in all",
        total.rejected_functions,
        total.guarded_functions,
        total.harmful_rejected,
        total.harmful,
        total.rejected,
        total.guards
    );
}
