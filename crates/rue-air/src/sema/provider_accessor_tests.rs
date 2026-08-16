//! Provider-path body-analysis tests for `-> borrow` accessors (ADR-0062),
//! migrated from the retired whole-program `Sema` accessor suite (RUE-1538).
//!
//! Production splits the accessor rules across two planes. Declaration
//! legality — the 6.6:3 preview gate, the 6.6:4/6.6:5 signature shape, the
//! 6.6:6/6.6:7 body shape, and the 6.6:14 `self`-call cycle — is decided at
//! the declaration query seam in `rue-compiler`
//! (`revisioned_query_database.rs`, through
//! `rue_air::declaration_validation`), with no body demanded. Body analysis
//! (the `OrdinaryBodyEngine` these tests drive through the production
//! provider seam) re-enforces the body-shape rules over a demanded accessor
//! body and owns every caller-side rule: the call-site loan and escape
//! contract (6.6:9/6.6:10) and the marked `AccessorCall` that mandatory CFG
//! splicing consumes (RUE-1208).
//!
//! Declaration-plane rejections the provider body path never re-checks for a
//! member — a non-`borrow self` receiver, a non-value parameter mode, the
//! associated-function form, and the cross-declaration accessor cycle — are
//! covered by their producers' own tests: the signature-query projection test
//! `parsed_accessor_signature_uses_exact_owner_facts` in
//! `rue-compiler/src/revisioned_query_database.rs` and the spec suite
//! `rue-spec/cases/items/borrow-accessors.toml` (6.6:3-6.6:14).

use rue_error::{ErrorKind, PreviewFeature, PreviewFeatures};

use super::provider_fixture::{FixtureKey, MethodShape, ProviderFixture, mode_param, value_param};
use crate::{AirInstData, SemanticImportType, SemanticParameterMode, StableDefinitionKind};

fn accessor_preview() -> PreviewFeatures {
    let mut features = PreviewFeatures::new();
    features.insert(PreviewFeature::BorrowAccessors);
    features
}

/// The durable method shape of a phase-1 accessor: `borrow self` receiver
/// with the accessor flag set.
fn accessor_shape() -> MethodShape {
    MethodShape {
        has_self: true,
        self_mode: SemanticParameterMode::Borrow,
        is_accessor: true,
    }
}

/// Durable facts of the retired `GRID_ACCESSOR` program: a non-copy `Grid`
/// holding four cells plus its `-> borrow i64` accessor `at`. Callers analyze
/// against these facts alone — caller-side accessor analysis establishes the
/// place/loan contract from the durable signature and never reads the callee
/// body (RUE-1208).
fn grid_fixture(preview: PreviewFeatures) -> (ProviderFixture, FixtureKey) {
    let mut fixture = ProviderFixture::with_preview(preview);
    let grid = fixture.declare_struct(
        "Grid",
        vec![(
            "cells",
            SemanticImportType::Array {
                element: Box::new(SemanticImportType::I64),
                len: 4,
            },
        )],
        false,
    );
    fixture.declare_method_with(
        &grid,
        "at",
        vec![value_param("i", SemanticImportType::U64)],
        SemanticImportType::I64,
        accessor_shape(),
    );
    (fixture, grid)
}

/// The accessor's own declaration, for tests that analyze the accessor body
/// itself (the single declaration the member body plan carries).
const GRID_ACCESSOR_DECL: &str = r#"struct Grid {
    cells: [i64; 4],

    fn at(borrow self, i: u64) -> borrow i64 {
        if i >= 4 {
            @panic("index out of bounds");
        }
        yield self.cells[i];
    }
}"#;

fn accessor_call_count(air: &crate::ValidatedAir) -> usize {
    air.iter()
        .filter(|(_, inst)| matches!(inst.data, AirInstData::AccessorCall { .. }))
        .count()
}

// Migrated from `tests::accessor_call_inlines_with_no_call_shape`: a call
// `g.at(2)` compiles with no ordinary call shape (ADR-0062 §3). The caller's
// AIR carries a marked `AccessorCall` — the dependency mandatory CFG splicing
// consumes (RUE-1208) — plus a `PlaceRead` of the accessor result; no `Call`
// instruction exists.
#[test]
fn accessor_call_inlines_with_no_call_shape() {
    let (mut fixture, _) = grid_fixture(accessor_preview());
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let g = Grid { cells: [10, 20, 30, 40] };
    if g.at(2) == 30 { 0 } else { 1 }
}",
            "main",
        )
        .expect("accessor call compiles");
    let air = &body.function.air;
    assert!(
        !air.iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::Call { .. })),
        "an accessor call must not lower to an ordinary AIR call"
    );
    assert!(
        air.iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::PlaceRead { .. })),
        "the accessor result place is read in the caller"
    );
    assert_eq!(
        accessor_call_count(air),
        1,
        "the marked place-producing call survives for CFG splicing"
    );
}

// Migrated from `tests::accessor_calls_remain_marked_for_mandatory_cfg_splicing`:
// semantic analysis preserves the exact accessor call for the per-function
// CFG query to splice.
#[test]
fn accessor_calls_remain_marked_for_mandatory_cfg_splicing() {
    let (mut fixture, _) = grid_fixture(accessor_preview());
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let g = Grid { cells: [10, 20, 30, 40] };
    if g.at(3) == 40 { 0 } else { 1 }
}",
            "main",
        )
        .expect("accessor call compiles");
    assert_eq!(accessor_call_count(&body.function.air), 1);
}

// Migrated from `tests::accessor_result_cannot_be_returned`: the borrowed
// place is scoped to its full expression, so `return g.at(0)` is an escape
// (6.6:9).
#[test]
fn accessor_result_cannot_be_returned() {
    let (mut fixture, grid) = grid_fixture(accessor_preview());
    fixture.declare_function(
        "read",
        vec![mode_param(
            "g",
            SemanticImportType::Nominal(grid),
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::I64,
    );
    let error = fixture
        .analyze(
            "fn read(borrow g: Grid) -> i64 {
    return g.at(0);
}",
            "read",
        )
        .map(|_| ())
        .expect_err("return escape");
    assert!(
        matches!(&error.kind, ErrorKind::AccessorResultReturned { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::accessor_result_cannot_be_tail_returned`: the tail
// value is an implicit return, so the same escape rule applies.
#[test]
fn accessor_result_cannot_be_tail_returned() {
    let (mut fixture, grid) = grid_fixture(accessor_preview());
    fixture.declare_function(
        "read",
        vec![mode_param(
            "g",
            SemanticImportType::Nominal(grid),
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::I64,
    );
    let error = fixture
        .analyze(
            "fn read(borrow g: Grid) -> i64 {
    g.at(0)
}",
            "read",
        )
        .map(|_| ())
        .expect_err("tail-return escape");
    assert!(
        matches!(&error.kind, ErrorKind::AccessorResultReturned { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::accessor_result_cannot_be_let_bound`: a `let`
// binding would outlive the full-expression loan (6.6:9).
#[test]
fn accessor_result_cannot_be_let_bound() {
    let (mut fixture, _) = grid_fixture(accessor_preview());
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
    let g = Grid { cells: [1, 2, 3, 4] };
    let b = g.at(0);
    0
}",
            "main",
        )
        .map(|_| ())
        .expect_err("let escape");
    assert!(
        matches!(&error.kind, ErrorKind::AccessorResultBound { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::accessor_result_cannot_be_stored`: assignment is an
// escape site too (6.6:9).
#[test]
fn accessor_result_cannot_be_stored() {
    let (mut fixture, _) = grid_fixture(accessor_preview());
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
    let g = Grid { cells: [1, 2, 3, 4] };
    let mut x = 0;
    x = g.at(0);
    0
}",
            "main",
        )
        .map(|_| ())
        .expect_err("store escape");
    assert!(
        matches!(&error.kind, ErrorKind::AccessorResultStored { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::accessor_result_cannot_be_captured_in_aggregate`:
// an aggregate literal captures its elements past the full expression
// (6.6:9).
#[test]
fn accessor_result_cannot_be_captured_in_aggregate() {
    let (mut fixture, _) = grid_fixture(accessor_preview());
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
    let g = Grid { cells: [1, 2, 3, 4] };
    let a = [g.at(0)];
    0
}",
            "main",
        )
        .map(|_| ())
        .expect_err("capture escape");
    assert!(
        matches!(&error.kind, ErrorKind::AccessorResultCaptured { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::accessor_loan_conflicts_with_inout_in_same_expression`:
// the (Accessor-Call) loan spans the enclosing full expression, so
// `using(g.at(0), bump(inout g))` overlaps a shared accessor loan with an
// exclusive `inout` loan on the same root (6.6:10, ADR-0062 §2).
#[test]
fn accessor_loan_conflicts_with_inout_in_same_expression() {
    let (mut fixture, grid) = grid_fixture(accessor_preview());
    fixture.declare_function(
        "using",
        vec![
            value_param("a", SemanticImportType::I64),
            value_param("b", SemanticImportType::I64),
        ],
        SemanticImportType::I64,
    );
    fixture.declare_function(
        "bump",
        vec![mode_param(
            "g",
            SemanticImportType::Nominal(grid),
            SemanticParameterMode::Inout,
        )],
        SemanticImportType::I64,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
    let mut g = Grid { cells: [1, 2, 3, 4] };
    using(g.at(0), bump(inout g));
    0
}",
            "main",
        )
        .map(|_| ())
        .expect_err("exclusivity conflict");
    assert!(
        matches!(&error.kind, ErrorKind::AccessorLoanConflict { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::accessor_body_requires_trailing_yield` and its
// declaration-seam twin `tests::uncalled_accessor_body_requires_a_trailing_yield`
// (identical accessor body): the engine decides the trailing-exit half of
// 6.6:6 at body entry, before any instruction is analyzed. The no-call-site
// half of the retired pair is the declaration plane's, tested at
// `rue-compiler` (`rules::accessor_body_error`) and by spec case
// `uncalled_accessor_body_requires_trailing_yield` (6.6:6).
#[test]
fn accessor_body_requires_trailing_yield() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    let p = fixture.declare_struct("P", vec![("x", SemanticImportType::I64)], false);
    fixture.declare_method_with(
        &p,
        "xr",
        Vec::new(),
        SemanticImportType::I64,
        accessor_shape(),
    );
    let error = fixture
        .analyze_member(
            "struct P {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        self.x
    }
}",
            "P",
            "xr",
            StableDefinitionKind::Method,
        )
        .map(|_| ())
        .expect_err("missing yield");
    assert!(
        matches!(&error.kind, ErrorKind::AccessorBodyMissingYield),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::uncalled_accessor_body_rejects_a_second_yield`: only
// the trailing instruction may be a `yield`; any earlier one is a second,
// bypassing exit (6.6:6).
#[test]
fn accessor_body_rejects_a_second_yield() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    let p = fixture.declare_struct("P", vec![("x", SemanticImportType::I64)], false);
    fixture.declare_method_with(
        &p,
        "xr",
        Vec::new(),
        SemanticImportType::I64,
        accessor_shape(),
    );
    let error = fixture
        .analyze_member(
            "struct P {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        yield self.x;
        yield self.x;
    }
}",
            "P",
            "xr",
            StableDefinitionKind::Method,
        )
        .map(|_| ())
        .expect_err("second yield");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::AccessorBodyOtherExit { found } if found == "a second `yield`"
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::uncalled_accessor_body_rejects_return`: a `return`
// is a non-diverging exit that bypasses the trailing `yield` (6.6:6).
#[test]
fn accessor_body_rejects_return() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    let p = fixture.declare_struct("P", vec![("x", SemanticImportType::I64)], false);
    fixture.declare_method_with(
        &p,
        "xr",
        vec![value_param("k", SemanticImportType::I64)],
        SemanticImportType::I64,
        accessor_shape(),
    );
    let error = fixture
        .analyze_member(
            "struct P {
    x: i64,

    fn xr(borrow self, k: i64) -> borrow i64 {
        if k == 0 {
            return 1;
        }
        yield self.x;
    }
}",
            "P",
            "xr",
            StableDefinitionKind::Method,
        )
        .map(|_| ())
        .expect_err("return in accessor");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::AccessorBodyOtherExit { found } if found == "a `return`"
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::accessor_yield_must_root_at_receiver` and its
// declaration-seam twin `tests::uncalled_accessor_yield_must_root_at_receiver`
// (identical accessor body): the yielded projection chain must bottom out at
// the receiver parameter `self` (6.6:7).
#[test]
fn accessor_yield_must_root_at_receiver() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    let p = fixture.declare_struct("P", vec![("x", SemanticImportType::I64)], false);
    fixture.declare_method_with(
        &p,
        "xr",
        vec![value_param("other", SemanticImportType::I64)],
        SemanticImportType::I64,
        accessor_shape(),
    );
    let error = fixture
        .analyze_member(
            "struct P {
    x: i64,

    fn xr(borrow self, other: i64) -> borrow i64 {
        yield other;
    }
}",
            "P",
            "xr",
            StableDefinitionKind::Method,
        )
        .map(|_| ())
        .expect_err("non-receiver yield");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::AccessorYieldNotReceiverRooted { found } if found.contains("`other`")
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::accessor_cannot_yield_a_call_to_itself`: the call is
// the inlined body, so a self-call in yield position has no finite expansion
// (6.6:14, E0261) rather than a compiler stack overflow (RUE-1211).
#[test]
fn accessor_cannot_yield_a_call_to_itself() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    let p = fixture.declare_struct("P", vec![("x", SemanticImportType::I64)], false);
    fixture.declare_method_with(
        &p,
        "xr",
        Vec::new(),
        SemanticImportType::I64,
        accessor_shape(),
    );
    let error = fixture
        .analyze_member(
            "struct P {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        yield self.xr();
    }
}",
            "P",
            "xr",
            StableDefinitionKind::Method,
        )
        .map(|_| ())
        .expect_err("self-recursive accessor");
    assert!(
        matches!(&error.kind, ErrorKind::AccessorRecursion { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::accessor_cannot_call_itself_from_a_guard`: the cycle
// is rejected wherever the re-entrant call appears, not only in yield
// position (RUE-1211).
#[test]
fn accessor_cannot_call_itself_from_a_guard() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    let p = fixture.declare_struct("P", vec![("x", SemanticImportType::I64)], false);
    fixture.declare_method_with(
        &p,
        "xr",
        Vec::new(),
        SemanticImportType::I64,
        accessor_shape(),
    );
    let error = fixture
        .analyze_member(
            "struct P {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        let _ = self.xr();
        yield self.x;
    }
}",
            "P",
            "xr",
            StableDefinitionKind::Method,
        )
        .map(|_| ())
        .expect_err("self-recursive accessor guard");
    assert!(
        matches!(&error.kind, ErrorKind::AccessorRecursion { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

/// The mutual-recursion pair whose `a -> b` link runs through a by-value
/// guard receiver rather than `self`, so the declaration-seam 6.6:14 rule
/// cannot see the cycle and semantic analysis must preserve the marked calls
/// for the canonical CFG dependency graph to reject.
const MUTUAL_ACCESSORS_DECL: &str = "struct P {
    x: i64,

    fn a(borrow self, other: P) -> borrow i64 {
        let _ = other.b();
        yield self.x;
    }

    fn b(borrow self) -> borrow i64 {
        yield self.a(P { x: 3 });
    }
}";

// Migrated from
// `tests::mutually_recursive_accessors_retain_marked_calls_for_cfg_cycle_rejection`:
// cross-body cycle rejection belongs to the canonical CFG dependency graph;
// each analyzed body must preserve its exact accessor call. The retired test
// counted three marked calls across the whole program; here the same three
// bodies are analyzed as production analyzes them — one demanded body per
// transaction — and each retains its one marked call.
#[test]
fn mutually_recursive_accessors_retain_marked_calls_for_cfg_cycle_rejection() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    let p = fixture.declare_struct("P", vec![("x", SemanticImportType::I64)], false);
    fixture.declare_method_with(
        &p,
        "a",
        vec![value_param("other", SemanticImportType::Nominal(p.clone()))],
        SemanticImportType::I64,
        accessor_shape(),
    );
    fixture.declare_method_with(
        &p,
        "b",
        Vec::new(),
        SemanticImportType::I64,
        accessor_shape(),
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    let a = fixture
        .analyze_member(
            MUTUAL_ACCESSORS_DECL,
            "P",
            "a",
            StableDefinitionKind::Method,
        )
        .expect("sema preserves accessor body `a`");
    assert_eq!(accessor_call_count(&a.function.air), 1);

    let b = fixture
        .analyze_member(
            MUTUAL_ACCESSORS_DECL,
            "P",
            "b",
            StableDefinitionKind::Method,
        )
        .expect("sema preserves accessor body `b`");
    assert_eq!(accessor_call_count(&b.function.air), 1);

    let main = fixture
        .analyze(
            "fn main() -> i32 {
    let p = P { x: 1 };
    if p.a(P { x: 2 }) == 1 { 0 } else { 1 }
}",
            "main",
        )
        .expect("the caller preserves its accessor call");
    assert_eq!(accessor_call_count(&main.function.air), 1);
}

// Migrated from `tests::yield_outside_accessor_is_rejected`: a `yield` in an
// ordinary function body is rejected during that body's analysis (E0256).
#[test]
fn yield_outside_accessor_is_rejected() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { yield 1; }", "main")
        .map(|_| ())
        .expect_err("stray yield");
    assert!(
        matches!(&error.kind, ErrorKind::YieldOutsideAccessor),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::free_function_cannot_be_an_accessor`: a free
// function has no receiver to project from (6.6:4). The provider body path
// re-checks the free-function declaration shape at body entry
// (`reject_free_function_accessor`), so this rejection stays on the seam.
#[test]
fn free_function_cannot_be_an_accessor() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    fixture.declare_function(
        "first",
        vec![mode_param(
            "v",
            SemanticImportType::I64,
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::I64,
    );
    let error = fixture
        .analyze(
            "fn first(borrow v: i64) -> borrow i64 {
    yield v;
}",
            "first",
        )
        .map(|_| ())
        .expect_err("free-fn accessor");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::AccessorRequiresBorrowSelf { found } if found == "a free function"
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::free_function_accessor_without_yield_is_rejected`:
// the accessor identity is the declaration's `-> borrow` result, not a
// trailing `yield` in the body (6.6:4, RUE-1213).
#[test]
fn free_function_accessor_without_yield_is_rejected() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    fixture.declare_function("make", Vec::new(), SemanticImportType::I64);
    let error = fixture
        .analyze(
            "fn make() -> borrow i64 {
    5
}",
            "make",
        )
        .map(|_| ())
        .expect_err("free-fn accessor");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::AccessorRequiresBorrowSelf { found } if found == "a free function"
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::plain_free_function_is_not_an_accessor`: only the
// result-position `borrow` marks an accessor, so an ordinary `-> T` free
// function keeps compiling with the preview enabled — as does a caller
// reaching it through its durable signature fact.
#[test]
fn plain_free_function_is_not_an_accessor() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    fixture.declare_function("make", Vec::new(), SemanticImportType::I64);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    fixture
        .analyze(
            "fn make() -> i64 {
    5
}",
            "make",
        )
        .expect("an ordinary free function is unaffected");
    fixture
        .analyze(
            "fn main() -> i32 {
    if make() == 5 { 0 } else { 1 }
}",
            "main",
        )
        .expect("an ordinary free-function call is unaffected");
}

// Migrated from `tests::uncalled_nested_accessor_yield_compiles`: a
// method-call link in the yielded chain is a legal projection when the
// callee is itself an accessor (6.6:7), which body analysis decides from the
// callee's resolved `returns_borrow` fact.
#[test]
fn nested_accessor_yield_compiles() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    let p = fixture.declare_struct("P", vec![("x", SemanticImportType::I64)], false);
    fixture.declare_method_with(
        &p,
        "inner",
        Vec::new(),
        SemanticImportType::I64,
        accessor_shape(),
    );
    fixture.declare_method_with(
        &p,
        "outer",
        Vec::new(),
        SemanticImportType::I64,
        accessor_shape(),
    );
    fixture
        .analyze_member(
            "struct P {
    x: i64,

    @allow(unused_function)
    fn inner(borrow self) -> borrow i64 { yield self.x; }

    @allow(unused_function)
    fn outer(borrow self) -> borrow i64 { yield self.inner(); }
}",
            "P",
            "outer",
            StableDefinitionKind::Method,
        )
        .expect("a legal nested accessor chain compiles");
}

// Migrated from `tests::uncalled_legal_accessor_declaration_compiles`: the
// control — a well-formed accessor body analyzes cleanly, so none of the
// body-shape rules misfire on the legal form.
#[test]
fn legal_accessor_body_compiles() {
    let mut fixture = ProviderFixture::with_preview(accessor_preview());
    let p = fixture.declare_struct("P", vec![("x", SemanticImportType::I64)], false);
    fixture.declare_method_with(
        &p,
        "xr",
        Vec::new(),
        SemanticImportType::I64,
        accessor_shape(),
    );
    let body = fixture
        .analyze_member(
            "struct P {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        yield self.x;
    }
}",
            "P",
            "xr",
            StableDefinitionKind::Method,
        )
        .expect("a legal accessor compiles");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I64);
}

// Migrated from `tests::accessor_declaration_requires_preview_gate` (and its
// uncalled twin, which the fixture cannot distinguish): with the gate off,
// the accessor's demanded body is rejected at its `yield` exit
// (`require_preview` in `analyze_yield`). The declaration-plane 6.6:3 gate —
// which fires with no body demanded — is `rules::accessor_preview_gate` at
// the `rue-compiler` signature query, covered by spec cases
// `accessor_requires_preview_gate` and `uncalled_accessor_requires_preview_gate`.
#[test]
fn accessor_body_requires_preview_gate() {
    let (fixture, _) = grid_fixture(PreviewFeatures::new());
    let error = fixture
        .analyze_member(
            GRID_ACCESSOR_DECL,
            "Grid",
            "at",
            StableDefinitionKind::Method,
        )
        .map(|_| ())
        .expect_err("the gate is off");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::PreviewFeatureRequired { feature, .. }
                if *feature == PreviewFeature::BorrowAccessors
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::free_function_accessor_without_yield_requires_preview`:
// 6.6:3 is disjunctive — the `-> borrow` result position alone demands the
// preview, whether or not the body contains a `yield` (RUE-1213). The
// provider body path enforces this through the free-function declaration
// shape check at body entry.
#[test]
fn free_function_accessor_without_yield_requires_preview() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("make", Vec::new(), SemanticImportType::I64);
    let error = fixture
        .analyze(
            "fn make() -> borrow i64 {
    5
}",
            "make",
        )
        .map(|_| ())
        .expect_err("the gate is off");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::PreviewFeatureRequired { feature, .. }
                if *feature == PreviewFeature::BorrowAccessors
        ),
        "unexpected diagnostic: {error:?}"
    );
}
