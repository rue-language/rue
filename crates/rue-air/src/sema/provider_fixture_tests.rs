//! Direct tests for the production provider body path.
//!
//! These tests drive [`analyze_provider_ordinary_body`] — the exact entry point
//! the compiler's body transaction uses — through the in-memory durable fact
//! source in [`super::provider_fixture`], so body analysis is exercised against
//! the production `ProviderBodyHost`/`OrdinaryBodyEngine` seam. A structural
//! guard at the bottom of this file keeps the fixture module and this file off
//! the retired whole-program `Sema` drivers by name.

use rue_error::ErrorKind;

use super::provider_fixture::{
    MethodShape, ProviderFixture, StructShape, error_source_slice, mode_param, value_param,
};
use crate::{SemanticImportConstValue, SemanticImportType, StableDefinitionKind};

// Migrated from `tests::test_analyze_addition`: ordinary expression typing on
// the production provider path.
#[test]
fn provider_body_types_integer_addition() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { 1 + 2 }", "main")
        .expect("addition body analyzes");

    let air = &body.function.air;
    assert_eq!(air.return_type(), crate::types::Type::I32);
    // Const(1) + Const(2) + Add + Ret = 4 instructions
    assert_eq!(air.len(), 4);
    let add = air.get(crate::AirRef::from_raw(2));
    assert!(matches!(add.data, crate::AirInstData::Add(_, _)));
    assert_eq!(add.ty, crate::types::Type::I32);
}

// Migrated from `tests::test_undefined_variable`: the diagnostic keeps its
// exact source span across the provider boundary.
#[test]
fn provider_body_reports_undefined_variable_with_exact_span() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let source = "fn main() -> i32 { missing + 1 }";
    let error = fixture
        .analyze(source, "main")
        .map(|_| ())
        .expect_err("undefined operand is rejected");

    assert!(
        matches!(&error.kind, ErrorKind::UndefinedVariable(name) if name == "missing"),
        "unexpected diagnostic: {error:?}"
    );
    assert_eq!(error_source_slice(source, &error), "missing");
}

// Migrated from `tests::test_use_after_move_error`: ownership diagnostics on
// the production provider path, with the callee crossing the boundary as an
// explicit durable signature fact.
#[test]
fn provider_body_reports_use_after_move() {
    let mut fixture = ProviderFixture::new();
    let non_copy = fixture.declare_struct("NonCopy", vec![("x", SemanticImportType::I32)], false);
    fixture.declare_function(
        "consume",
        vec![value_param("n", SemanticImportType::Nominal(non_copy))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let source = "fn main() -> i32 {
    let n = NonCopy { x: 42 };
    let consumed = consume(n);
    consumed + n.x
}";
    let error = fixture
        .analyze(source, "main")
        .map(|_| ())
        .expect_err("the moved value cannot be read again");

    assert!(
        matches!(&error.kind, ErrorKind::UseAfterMove { .. }),
        "unexpected diagnostic: {error:?}"
    );
    assert!(error.span().is_some(), "move diagnostic keeps its span");
}

// Migrated from `tests::test_copy_type_not_moved`: `@copy` metadata crosses
// the boundary inside the durable nominal fact.
#[test]
fn provider_body_copy_type_is_not_moved() {
    let mut fixture = ProviderFixture::new();
    let point = fixture.declare_struct("Point", vec![("x", SemanticImportType::I32)], true);
    fixture.declare_function(
        "give",
        vec![value_param("p", SemanticImportType::Nominal(point))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let p = Point { x: 7 };
    let a = give(p);
    let b = give(p);
    a + b + p.x
}",
            "main",
        )
        .expect("a copy value survives repeated calls");
    assert!(body.warnings.is_empty(), "no incidental warnings expected");
}

// Provider-path counterpart of `tests::test_struct_field_type_resolution`
// (which keeps its whole-program type-pool assertions): aggregate
// construction with nested nominal fields resolved from durable facts.
#[test]
fn provider_body_resolves_nested_struct_field_types() {
    let mut fixture = ProviderFixture::new();
    let inner = fixture.declare_struct("Inner", vec![("x", SemanticImportType::I32)], true);
    fixture.declare_struct(
        "Outer",
        vec![("inner", SemanticImportType::Nominal(inner))],
        true,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let o = Outer { inner: Inner { x: 42 } };
    o.inner.x
}",
            "main",
        )
        .expect("nested aggregate construction analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// Direct provider-path coverage of a method call: the member is resolved
// through the durable member lookup plus its durable method signature.
#[test]
fn provider_body_types_method_call() {
    let mut fixture = ProviderFixture::new();
    let point = fixture.declare_struct(
        "Point",
        vec![
            ("x", SemanticImportType::I32),
            ("y", SemanticImportType::I32),
        ],
        true,
    );
    fixture.declare_method(&point, "x_value", Vec::new(), SemanticImportType::I32);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let p = Point { x: 3, y: 4 };
    p.x_value()
}",
            "main",
        )
        .expect("method call analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// Provider-path counterpart of the body-local alias half of
// `tests::comptime_type_alias_filter_preserves_analysis_and_diagnostics`:
// a comptime type alias types a later binding on the production path.
#[test]
fn provider_body_reduces_local_comptime_type_alias() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let Direct = i32;
    let value: Direct = 40 + 2;
    value
}",
            "main",
        )
        .expect("comptime alias body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// A comptime value crossing the boundary as a durable const fact.
#[test]
fn provider_body_reads_durable_const_value() {
    let mut fixture = ProviderFixture::new();
    let limit = fixture.declare_const(
        "LIMIT",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(40),
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { LIMIT + 2 }", "main")
        .expect("durable const body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
    assert!(
        body.referenced_values.contains(&limit),
        "the consulted const is recorded as a referenced value"
    );
}

// Provider-path counterpart of the local-alias half of
// `tests::direct_anonymous_type_alias_and_const_receive_authoritative_
// producers`: a body-local anonymous nominal is produced with a durable
// identity, and its member initialization type-checks.
#[test]
fn provider_body_produces_anonymous_nominal_identity() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let T = struct { value: i32 };
    let holder: T = T { value: 42 };
    holder.value
}",
            "main",
        )
        .expect("anonymous nominal body analyzes");
    assert_eq!(body.produced_anonymous_nominals.len(), 1);
    let produced = &body.produced_anonymous_nominals[0];
    assert!(matches!(
        &produced.shape,
        super::provider_body_host::SemanticProducedAnonymousNominalShape::Struct { fields, .. }
            if fields.len() == 1 && &*fields[0].0 == "value"
    ));
}

// A fact that was never seeded fails closed: the miss surfaces as an ordinary
// visible diagnostic, never as an invented fact.
#[test]
fn provider_body_missing_function_fact_fails_closed() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { helper() }", "main")
        .map(|_| ())
        .expect_err("an unseeded callee cannot resolve");
    assert!(
        matches!(&error.kind, ErrorKind::UndefinedFunction(name) if name == "helper"),
        "unexpected diagnostic: {error:?}"
    );
}

// Warnings survive the provider boundary alongside the analyzed body.
#[test]
fn provider_body_preserves_unused_variable_warning() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { let unused = 1; 2 }", "main")
        .expect("body with unused binding analyzes");
    assert!(
        body.warnings.iter().any(|warning| matches!(
            &warning.kind,
            rue_error::WarningKind::UnusedVariable(name) if name == "unused"
        )),
        "unexpected warnings: {:?}",
        body.warnings
    );
}

// Referenced definitions are reported exactly, so the compiler can register
// its dependency edges from the analysis result.
#[test]
fn provider_body_records_referenced_callee_definitions() {
    let mut fixture = ProviderFixture::new();
    let helper = fixture.declare_function("helper", Vec::new(), SemanticImportType::I32);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { helper() }", "main")
        .expect("callee body analyzes");
    assert!(
        body.referenced_definitions.contains(&helper),
        "the resolved callee is a referenced definition"
    );
    assert!(
        body.referenced_specializations.is_empty(),
        "an ordinary call requests no specialization"
    );
}

// Structural guard: the fixture helper drives the production provider entry
// point and never re-enters a retired whole-program `Sema` driver, and that
// entry point runs the one canonical ordinary-body engine.
#[test]
fn fixture_helper_drives_only_the_production_provider_entry_point() {
    let fixture_source = include_str!("provider_fixture.rs");
    let entry = concat!("analyze_provider_", "ordinary_body(");
    assert!(
        fixture_source.contains(entry),
        "the fixture helper must call the production provider entry point"
    );
    for source in [fixture_source, include_str!("provider_fixture_tests.rs")] {
        for retired in [
            concat!("Sema::", "new_synthetic"),
            concat!("new_", "synthetic("),
            concat!("bind_declarations", "_for_test"),
            concat!("analyze_all", "_for_test"),
            concat!("analyze_", "all("),
            concat!("bind_", "declarations("),
        ] {
            assert!(
                !source.contains(retired),
                "the fixture must not re-enter a retired Sema driver: {retired}"
            );
        }
    }
    // The entry point the helper calls runs the one canonical ordinary-body
    // engine. Matching the constructor and the resolved-signature entry
    // separately keeps the guard insensitive to formatting of the call chain.
    let provider_host = include_str!("provider_body_host.rs");
    assert!(
        provider_host.contains("OrdinaryBodyEngine::new"),
        "the provider host must construct the canonical ordinary-body engine"
    );
    assert!(
        provider_host.contains(".analyze_single_function_resolved("),
        "the provider entry point must run the engine's resolved ordinary-body analysis"
    );
}

// A named method body analyzed directly: the single declaration is the owning
// nominal, and the member is selected exactly as the compiler's member body
// transaction selects it.
#[test]
fn provider_member_body_types_receiver_field_read() {
    let mut fixture = ProviderFixture::new();
    let point = fixture.declare_struct("Point", vec![("x", SemanticImportType::I32)], true);
    fixture.declare_method(&point, "double", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze_member(
            "struct Point {
    x: i32,

    fn double(self) -> i32 {
        self.x + self.x
    }
}",
            "Point",
            "double",
            StableDefinitionKind::Method,
        )
        .expect("method body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// A destructor body analyzed directly: the single declaration is the
// `drop fn` item, and the owning nominal crosses as a durable fact.
#[test]
fn provider_destructor_body_analyzes_against_durable_owner() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_struct_with(
        "Resource",
        vec![("handle", SemanticImportType::I32)],
        false,
        StructShape {
            has_destructor: true,
            ..StructShape::default()
        },
    );
    let body = fixture
        .analyze_destructor(
            "drop fn Resource(self) { let _open = self.handle; }",
            "Resource",
        )
        .expect("destructor body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::UNIT);
}

// A RIR edit between lowering and validation probes analysis behavior on
// instruction shapes the frontend cannot produce: a malformed internal
// intrinsic arity is diagnosed instead of panicking.
#[test]
fn provider_body_reports_malformed_internal_intrinsic_arity() {
    use rue_rir::{InstData, InternalIntrinsic};

    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let error = fixture
        .analyze_edited("fn main() { @dbg(1); }", "main", |rir| {
            let intrinsic_ref = rir
                .iter()
                .find_map(|(inst_ref, inst)| match inst.data {
                    InstData::Intrinsic { .. } => Some(inst_ref),
                    _ => None,
                })
                .expect("lowered body contains the probed intrinsic");
            rir.replace_internal_intrinsic(intrinsic_ref, InternalIntrinsic::IterLen, &[])
                .expect("intrinsic replacement applies");
        })
        .map(|_| ())
        .expect_err("malformed compiler RIR must be diagnosed");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::InternalError(message)
                if message.contains("`__rue_iter_len` expects 1 argument, found 0")
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// An `inout` argument that names no place is rejected during body analysis;
// the callee's parameter mode crosses the boundary as a durable signature
// fact.
#[test]
fn provider_body_rejects_inout_argument_without_a_place() {
    use crate::SemanticParameterMode;

    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "take",
        vec![mode_param(
            "x",
            SemanticImportType::I32,
            SemanticParameterMode::Inout,
        )],
        SemanticImportType::Unit,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { take(inout 1); 0 }", "main")
        .map(|_| ())
        .expect_err("non-place must fail in sema");
    assert!(
        matches!(&error.kind, ErrorKind::InoutNonLvalue),
        "unexpected diagnostic: {error:?}"
    );
}

// A declared enum's variant is constructed from its durable nominal fact.
#[test]
fn provider_body_constructs_declared_enum_variant() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_enum("Color", vec![("Red", Vec::new()), ("Green", Vec::new())]);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let c = Color.Red;
    0
}",
            "main",
        )
        .expect("enum construction analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// A legal accessor body analyzed directly under the preview gate: the
// accessor flag and receiver mode cross as durable method facts.
#[test]
fn provider_accessor_body_analyzes_under_preview_gate() {
    use crate::SemanticParameterMode;
    use rue_error::PreviewFeature;

    let mut preview = rue_error::PreviewFeatures::new();
    preview.insert(PreviewFeature::BorrowAccessors);
    let mut fixture = ProviderFixture::with_preview(preview);
    let holder = fixture.declare_struct("Holder", vec![("x", SemanticImportType::I64)], true);
    fixture.declare_method_with(
        &holder,
        "xr",
        Vec::new(),
        SemanticImportType::I64,
        MethodShape {
            has_self: true,
            self_mode: SemanticParameterMode::Borrow,
            is_accessor: true,
        },
    );
    let body = fixture
        .analyze_member(
            "struct Holder {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        yield self.x;
    }
}",
            "Holder",
            "xr",
            StableDefinitionKind::Method,
        )
        .expect("legal accessor body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I64);
}

// Durable body exports anchor spans relative to the body, so surrounding
// source relocation leaves the exported body identical.
#[test]
fn provider_body_export_ignores_surrounding_source_relocation() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let original = fixture
        .analyze("fn main() -> i32 { 42 }", "main")
        .expect("original body analyzes");
    let relocated = fixture
        .analyze("\n\nfn main() -> i32 { 42 }\n", "main")
        .expect("relocated body analyzes");
    assert_eq!(original.export.body, relocated.export.body);
}

// A body that only warns still exports, with the warning preserved both in
// the analysis result and inside the durable export.
#[test]
fn provider_body_with_warning_exports_and_keeps_the_warning() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let body = fixture
        .analyze("fn main() { let unused = 1; }", "main")
        .expect("warning-only body analyzes");
    assert_eq!(body.export.body.warnings.len(), 1);
    assert!(body.warnings.iter().any(|warning| {
        matches!(warning.kind, rue_error::WarningKind::UnusedVariable(ref name) if name == "unused")
    }));
    assert_eq!(body.export.body.instructions.len(), body.function.air.len());
    assert_eq!(
        body.export.body.places.len(),
        body.function.air.places().len()
    );
    assert!(body.export.body.strings.is_empty());
}

// The durable export of a supported body imports into a fresh AIR epoch
// byte-for-byte: instruction stream, types, spans, places, projections,
// param drops, slot counts, borrow slots, and strings all round-trip.
#[test]
fn provider_body_export_round_trips_through_a_fresh_air_epoch_exactly() {
    use std::cell::Cell;

    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let source = "fn main() -> i32 { if 1 < 2 { (3 + 4) * 5 } else { 6 - 7 } }";
    let body_span = Cell::new(None);
    let body = fixture
        .analyze_edited(source, "main", |rir| {
            body_span.set(rir.iter().find_map(|(_, inst)| match inst.data {
                rue_rir::InstData::FnDecl { body, .. } => Some(rir.get(body).span),
                _ => None,
            }));
        })
        .expect("supported body analyzes");
    let body_span = body_span.get().expect("main body span");

    let epoch = crate::SemanticImportEpoch::<
        crate::SemanticDefinitionToken,
        crate::SemanticModuleToken,
    >::new(vec![], vec![], vec![])
    .expect("empty import epoch constructs");
    let imported = epoch
        .import_body(&body.export.body, body_span)
        .expect("exported body imports");
    let source_function = &body.function;

    assert_eq!(
        source_function.air.return_type(),
        imported.air.return_type()
    );
    assert_eq!(source_function.air.len(), imported.air.len());
    for ((source_ref, source_inst), (imported_ref, imported_inst)) in
        source_function.air.iter().zip(imported.air.iter())
    {
        assert_eq!(source_ref.as_u32(), imported_ref.as_u32());
        assert_eq!(
            format!("{:?}", source_inst.data),
            format!("{:?}", imported_inst.data)
        );
        assert_eq!(source_inst.ty, imported_inst.ty);
        assert_eq!(source_inst.span, imported_inst.span);
    }
    assert_eq!(
        format!("{:?}", source_function.air.places()),
        format!("{:?}", imported.air.places())
    );
    assert_eq!(
        format!("{:?}", source_function.air.projections()),
        format!("{:?}", imported.air.projections())
    );
    assert_eq!(
        source_function.air.param_drops(),
        imported.air.param_drops()
    );
    assert_eq!(source_function.num_locals, imported.num_locals);
    assert_eq!(source_function.num_param_slots, imported.num_param_slots);
    assert_eq!(source_function.param_modes, imported.param_modes);
    assert_eq!(
        source_function.allow_unreachable_code,
        imported.allow_unreachable_code
    );
    assert_eq!(body.strings, imported.strings);
    assert!(imported.warnings.is_empty());
    for slot in 0..source_function.num_locals {
        assert_eq!(
            source_function.air.is_borrow_slot(slot),
            imported.air.is_borrow_slot(slot)
        );
    }
}
