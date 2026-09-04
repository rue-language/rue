//! Provider-path body-analysis tests migrated from the retired whole-program
//! `Sema` drivers (RUE-1538): core expression, scoping, inference, and
//! diagnostic semantics.
//!
//! Every test drives [`super::analyze_provider_ordinary_body`] through
//! [`super::provider_fixture::ProviderFixture`], so each analyzed source
//! carries exactly one declaration and every other referenced declaration
//! crosses the provider boundary as an explicit durable fact.

use std::sync::Arc;

use rue_error::ErrorKind;

use super::DurableSignatureParameter;
use super::provider_fixture::{
    FixtureKey, FixtureModule, FixtureType, ProviderFixture, error_source_slice, value_param,
};
use crate::SemanticImportNominalKind;
use crate::inst::{AirInstData, AirRef};
use crate::types::Type;
use crate::{SemanticImportConstValue, SemanticImportType, SemanticParameterMode};

/// A comptime signature parameter fact (`comptime name: ty`), the comptime
/// counterpart of [`value_param`] for the declarations these tests seed.
fn comptime_param(
    name: &str,
    ty: FixtureType,
) -> DurableSignatureParameter<FixtureKey, FixtureModule> {
    DurableSignatureParameter {
        name: Arc::from(name),
        ty,
        mode: SemanticParameterMode::Value,
        is_comptime: true,
    }
}

// Migrated from `tests::test_analyze_simple_function`: the smallest ordinary
// body analyzes on the production provider path with the exact AIR shape.
#[test]
fn provider_body_analyzes_simple_function() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { 42 }", "main")
        .expect("simple body analyzes");

    let air = &body.function.air;
    assert_eq!(air.return_type(), Type::I32);
    assert_eq!(air.len(), 2); // Const + Ret
}

// Migrated from `tests::large_signature_parameter_uses_resolve_through_body_analysis`:
// indexed parameter lookup preserves ordinary body semantics for a
// nine-parameter signature. The callee body and the calling body are separate
// provider transactions; the callee's signature crosses as a durable fact.
#[test]
fn provider_body_resolves_large_signature_parameter_uses() {
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    let params = || {
        names
            .iter()
            .map(|name| value_param(name, SemanticImportType::I32))
            .collect()
    };

    let mut callee = ProviderFixture::new();
    callee.declare_function("sum", params(), SemanticImportType::I32);
    callee
        .analyze(
            "fn sum(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32, h: i32, i: i32) -> i32 {\n\
                 a + b + c + d + e + f + g + h + i\n\
             }",
            "sum",
        )
        .expect("indexed parameter lookup preserves ordinary body semantics");

    let mut caller = ProviderFixture::new();
    caller.declare_function("sum", params(), SemanticImportType::I32);
    caller.declare_function("main", Vec::new(), SemanticImportType::I32);
    caller
        .analyze(
            "fn main() -> i32 { sum(1, 2, 3, 4, 5, 6, 7, 8, 9) }",
            "main",
        )
        .expect("the nine-argument call analyzes against the durable signature");
}

// Migrated from `tests::reserved_looking_source_intrinsics_never_select_internal_operations`:
// reserved-looking source spellings stay unknown intrinsics on the provider
// path instead of selecting internal operations.
#[test]
fn provider_body_keeps_reserved_looking_source_intrinsics_unknown() {
    for (name, args) in [
        ("__rue_iter_len", "0"),
        ("__rue_char_scalar", "0, 0"),
        ("__rue_char_next", "0, 0"),
        ("__rue_char_scalar_lossy", "0, 0"),
        ("__rue_char_next_lossy", "0, 0"),
    ] {
        let mut fixture = ProviderFixture::new();
        fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
        let source = format!("fn main() {{ @{name}({args}) }}");
        let error = fixture
            .analyze(&source, "main")
            .map(|_| ())
            .expect_err("source spelling must stay unknown");
        assert!(
            matches!(&error.kind, ErrorKind::UnknownIntrinsic(actual) if actual == name),
            "{name}: {error:?}"
        );
    }
}

// Migrated from `tests::panic_is_never_and_assert_is_unit_in_air`: `@panic`
// diverges (`!` result), `@assert` stays unit; the message operand's type
// never changes that result, and only unit trailing assertions synthesize a
// return. The `diverge` callee crosses as a durable fact.
//
// The two are different AIR shapes. `@panic` is one intrinsic. `@assert` is a
// branch with no else whose then-arm reports the failure on the ADR-0083 §5.1
// channel and aborts, so its result type is the branch's — and its failing arm
// is a pair of runtime calls, not an intrinsic.
#[test]
fn provider_body_panic_is_never_and_assert_is_unit() {
    for (name, body_source, expected) in [
        ("panic_no_message", "@panic()", Type::NEVER),
        ("panic_with_message", "@panic(\"boom\")", Type::NEVER),
        ("assertion", "@assert(true)", Type::UNIT),
        (
            "assertion_with_message",
            "@assert(true, \"ok\")",
            Type::UNIT,
        ),
        (
            "never assertion condition",
            "@assert(diverge())",
            Type::UNIT,
        ),
        ("never panic message", "@panic(diverge())", Type::NEVER),
    ] {
        let asserts = expected == Type::UNIT;
        let mut fixture = ProviderFixture::new();
        fixture.declare_function("diverge", Vec::new(), SemanticImportType::Never);
        fixture.declare_function("probe", Vec::new(), SemanticImportType::Unit);
        let source = format!("fn probe() {{ {body_source} }}");
        let body = fixture
            .analyze(&source, "probe")
            .unwrap_or_else(|error| panic!("{name} must analyze: {error:?}"));

        let result_types: Vec<_> = body
            .function
            .air
            .iter()
            .filter_map(|(_, inst)| {
                let selected = if asserts {
                    matches!(inst.data, AirInstData::Branch { .. })
                } else {
                    matches!(inst.data, AirInstData::Intrinsic { .. })
                };
                selected.then_some(inst.ty)
            })
            .collect();
        assert_eq!(
            result_types,
            vec![expected],
            "{name} result must agree with HM"
        );
        // A unit-valued trailing `@assert` still needs an implicit return; a
        // never-valued trailing `@panic` diverges, so no return is synthesized.
        let has_ret = body
            .function
            .air
            .iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::Ret(_)));
        if asserts {
            assert!(has_ret, "{name}: a unit trailing intrinsic needs a return");
        } else {
            assert!(
                !has_ret,
                "{name}: a diverging trailing intrinsic must not synthesize a return"
            );
        }
    }
}

// Migrated from `tests::panic_and_assert_reject_invalid_operand_types_at_the_operand`:
// the operand mismatch is the primary diagnostic and points at the offending
// operand; aggregate operand types cross as durable nominal facts.
#[test]
fn provider_body_panic_and_assert_reject_invalid_operand_types_at_the_operand() {
    fn no_facts(_: &mut ProviderFixture) {}
    fn fake_value_struct(fixture: &mut ProviderFixture) {
        fixture.declare_struct("Fake", vec![("value", SemanticImportType::I32)], false);
    }
    fn mode_enum(fixture: &mut ProviderFixture) {
        fixture.declare_enum("Mode", vec![("A", Vec::new())]);
    }
    fn fake_three_slot_struct(fixture: &mut ProviderFixture) {
        fixture.declare_struct(
            "Fake",
            vec![
                ("a", SemanticImportType::U64),
                ("b", SemanticImportType::U64),
                ("c", SemanticImportType::U64),
            ],
            false,
        );
    }

    let cases: [(&str, fn(&mut ProviderFixture), &str, &str, &str, &str, &str); 7] = [
        (
            "integer assertion condition",
            no_facts,
            "fn main() -> i32 { @assert(1); 0 }",
            "assert",
            "bool condition",
            "i32",
            "1",
        ),
        (
            "aggregate assertion condition",
            fake_value_struct,
            "fn main() -> i32 { let s = Fake { value: 1 }; @assert(s); 0 }",
            "assert",
            "bool condition",
            "Fake",
            "s",
        ),
        (
            "scalar panic message",
            no_facts,
            "fn main() -> i32 { @panic(1); 0 }",
            "panic",
            "text message",
            "i32",
            "1",
        ),
        (
            "scalar assertion message",
            no_facts,
            "fn main() -> i32 { @assert(false, 7); 0 }",
            "assert",
            "text message",
            "i32",
            "7",
        ),
        (
            "array message",
            no_facts,
            "fn main() -> i32 { let a = [1, 2, 3]; @panic(a); 0 }",
            "panic",
            "text message",
            "[i32; 3]",
            "a",
        ),
        (
            "enum message",
            mode_enum,
            "fn main() -> i32 { @assert(false, Mode.A); 0 }",
            "assert",
            "text message",
            "Mode",
            "Mode.A",
        ),
        (
            "three-slot struct impostor",
            fake_three_slot_struct,
            "fn main() -> i32 { @panic(Fake { a: 0, b: 0, c: 0 }); 0 }",
            "panic",
            "text message",
            "Fake",
            "Fake { a: 0, b: 0, c: 0 }",
        ),
    ];

    for (name, seed, source, intrinsic, expected, found, offending) in cases {
        let mut fixture = ProviderFixture::new();
        seed(&mut fixture);
        fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
        let error = match fixture.analyze(source, "main") {
            Ok(_) => panic!("{name} should fail"),
            Err(error) => error,
        };
        match &error.kind {
            ErrorKind::IntrinsicTypeMismatch(data) => {
                assert_eq!(data.name, intrinsic, "{name} intrinsic");
                assert_eq!(data.expected, expected, "{name} expected type");
                assert_eq!(data.found, found, "{name} found type");
            }
            other => panic!("{name} produced {other:?}, expected E0702"),
        }
        assert_eq!(
            error_source_slice(source, &error),
            offending,
            "{name} must point at the offending operand"
        );
    }
}

// Migrated from `tests::panic_and_assert_preserve_primary_operand_errors`:
// an undefined operand keeps its own primary diagnostic and span instead of
// cascading into an intrinsic-typing error.
#[test]
fn provider_body_panic_and_assert_preserve_primary_operand_errors() {
    for source in [
        "fn main() -> i32 { @panic(missing); 0 }",
        "fn main() -> i32 { @assert(missing); 0 }",
        "fn main() -> i32 { @assert(false, missing); 0 }",
    ] {
        let mut fixture = ProviderFixture::new();
        fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
        let error = fixture
            .analyze(source, "main")
            .map(|_| ())
            .expect_err("primary operand error must surface");
        assert!(
            matches!(&error.kind, ErrorKind::UndefinedVariable(name) if name == "missing"),
            "expected the operand's primary error, got {:?}",
            error.kind
        );
        assert_eq!(error_source_slice(source, &error), "missing");
    }
}

// Migrated from `tests::test_analyze_all_binary_ops`: every arithmetic binary
// operator analyzes on the provider path.
#[test]
fn provider_body_analyzes_all_binary_ops() {
    for source in [
        "fn main() -> i32 { 1 + 2 }",
        "fn main() -> i32 { 1 - 2 }",
        "fn main() -> i32 { 1 * 2 }",
        "fn main() -> i32 { 1 / 2 }",
        "fn main() -> i32 { 1 % 2 }",
    ] {
        let mut fixture = ProviderFixture::new();
        fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
        fixture
            .analyze(source, "main")
            .unwrap_or_else(|error| panic!("{source} must analyze: {error:?}"));
    }
}

// Migrated from `tests::test_analyze_negation`: unary negation keeps its exact
// AIR shape and result type.
#[test]
fn provider_body_types_negation() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { -42 }", "main")
        .expect("negation body analyzes");

    let air = &body.function.air;
    // Const(42) + Neg + Ret = 3 instructions
    assert_eq!(air.len(), 3);
    let neg_inst = air.get(AirRef::from_raw(1));
    assert!(matches!(neg_inst.data, AirInstData::Neg(_)));
    assert_eq!(neg_inst.ty, Type::I32);
}

// Migrated from `tests::test_analyze_complex_expr`: parenthesized operand
// grouping keeps its exact AIR shape.
#[test]
fn provider_body_types_complex_expression() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { (1 + 2) * 3 }", "main")
        .expect("complex expression analyzes");

    let air = &body.function.air;
    // Const(1) + Const(2) + Add + Const(3) + Mul + Ret = 6 instructions
    assert_eq!(air.len(), 6);
    let mul_inst = air.get(AirRef::from_raw(4));
    assert!(matches!(mul_inst.data, AirInstData::Mul(_, _)));
}

// Migrated from `tests::test_analyze_let_binding`: a let binding lowers to the
// exact storage/alloc/load/block instruction shape.
#[test]
fn provider_body_lowers_let_binding() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { let x = 42; x }", "main")
        .expect("let binding analyzes");

    assert_eq!(body.function.num_locals, 1);
    let air = &body.function.air;
    // Const(42) + StorageLive + Alloc + Block([StorageLive], Alloc) + Load
    // + Block([alloc block], Load) + Ret = 7 instructions
    assert_eq!(air.len(), 7);
    assert!(matches!(
        air.get(AirRef::from_raw(1)).data,
        AirInstData::StorageLive { slot: 0 }
    ));
    assert!(matches!(
        air.get(AirRef::from_raw(2)).data,
        AirInstData::Alloc { slot: 0, .. }
    ));
    assert!(matches!(
        air.get(AirRef::from_raw(4)).data,
        AirInstData::Load { slot: 0 }
    ));
    assert!(matches!(
        air.get(AirRef::from_raw(5)).data,
        AirInstData::Block { .. }
    ));
}

// Migrated from `tests::test_analyze_let_mut_assignment`: mutation through a
// `let mut` binding lowers to the exact store/block shape.
#[test]
fn provider_body_lowers_let_mut_assignment() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { let mut x = 10; x = 20; x }", "main")
        .expect("mutable assignment analyzes");

    let air = &body.function.air;
    // Const(10) + StorageLive + Alloc + Block([StorageLive], Alloc) + Const(20)
    // + Store + Load + Block([alloc block, Store], Load) + Ret = 9 instructions
    assert_eq!(air.len(), 9);
    assert!(matches!(
        air.get(AirRef::from_raw(5)).data,
        AirInstData::Store { slot: 0, .. }
    ));
    assert!(matches!(
        air.get(AirRef::from_raw(7)).data,
        AirInstData::Block { .. }
    ));
}

// Migrated from `tests::test_assign_to_immutable`: assigning through an
// immutable binding is rejected with the primary mutability diagnostic.
#[test]
fn provider_body_rejects_assignment_to_immutable() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { let x = 10; x = 20; x }", "main")
        .map(|_| ())
        .expect_err("assignment to immutable binding is rejected");
    assert!(
        matches!(error.kind, ErrorKind::AssignToImmutable(_)),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::test_multiple_variables`: each binding takes its own
// local slot.
#[test]
fn provider_body_counts_multiple_variables() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { let x = 10; let y = 20; x + y }", "main")
        .expect("two bindings analyze");
    assert_eq!(body.function.num_locals, 2);
}

// Migrated from `tests::test_empty_block_evaluates_to_unit`: an empty block
// evaluates to `()` and produces a UnitConst.
#[test]
fn provider_body_empty_block_evaluates_to_unit() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::Unit);
    let body = fixture
        .analyze("fn main() { let _x: () = {}; }", "main")
        .expect("empty block analyzes");
    assert!(
        body.function
            .air
            .iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::UnitConst)),
        "empty block should produce UnitConst"
    );
}

// Migrated from `tests::test_single_error_no_cascade_simple`: the retired
// driver asserted exactly one collected error; the provider path returns one
// primary CompileError, which must be the original operand mismatch.
#[test]
fn provider_body_reports_single_primary_error_for_bool_operand() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { 1 + true }", "main")
        .map(|_| ())
        .expect_err("integer + bool is rejected");
    assert!(
        matches!(&error.kind, ErrorKind::TypeMismatch { expected, found }
            if expected.contains("integer") && found.contains("bool")),
        "error should mention integer and bool, got: {:?}",
        error.kind
    );
}

// Migrated from `tests::test_single_error_no_cascade_with_function_call`: the
// error-typed variable used as a call argument keeps the original mismatch as
// the primary diagnostic instead of cascading into an argument error.
#[test]
fn provider_body_error_does_not_cascade_into_function_call() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "foo",
        vec![
            value_param("a", SemanticImportType::I32),
            value_param("b", SemanticImportType::I32),
        ],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
                 let x = 1 + true;
                 foo(x, 1)
             }",
            "main",
        )
        .map(|_| ())
        .expect_err("the original mismatch is the primary error");
    assert!(
        matches!(&error.kind, ErrorKind::TypeMismatch { expected, found }
            if expected.contains("integer") && found.contains("bool")),
        "the primary error must be the original mismatch, got: {:?}",
        error.kind
    );
}

// Migrated from `tests::test_single_error_no_cascade_deep_chain`: a deep chain
// of uses of the error-typed value keeps the original mismatch primary.
#[test]
fn provider_body_error_does_not_cascade_through_deep_chain() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
                 let x = 1 + true;
                 let y = x + 1;
                 let z = y * 2;
                 let w = z - 3;
                 let v = w / 4;
                 v
             }",
            "main",
        )
        .map(|_| ())
        .expect_err("the original mismatch is the primary error");
    assert!(
        matches!(&error.kind, ErrorKind::TypeMismatch { expected, found }
            if expected.contains("integer") && found.contains("bool")),
        "the primary error must be the original mismatch, got: {:?}",
        error.kind
    );
}

// Migrated from `tests::test_bool_plus_int_error`: the reversed operand order
// is rejected with one primary type error.
#[test]
fn provider_body_rejects_bool_plus_int() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { true + 1 }", "main")
        .map(|_| ())
        .expect_err("bool + integer is rejected");
    assert!(
        matches!(&error.kind, ErrorKind::TypeMismatch { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::test_arithmetic_on_bool_type_error`: bool operands in
// arithmetic are rejected with one primary type error.
#[test]
fn provider_body_rejects_arithmetic_on_bool() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { true * true }", "main")
        .map(|_| ())
        .expect_err("bool arithmetic is rejected");
    assert!(
        matches!(&error.kind, ErrorKind::TypeMismatch { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::test_variable_shadowing_same_type`: shadowing with the
// same type allocates a fresh slot.
#[test]
fn provider_body_shadows_variable_with_same_type() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
                let x = 10;
                let x = 20;
                x
            }",
            "main",
        )
        .expect("shadowing body analyzes");
    assert_eq!(body.function.num_locals, 2);
}

// Migrated from `tests::test_variable_shadowing_different_type`: shadowing may
// change the binding's type; the shadowing body is analyzed directly.
#[test]
fn provider_body_shadows_variable_with_different_type() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("shadow", Vec::new(), SemanticImportType::Bool);
    let body = fixture
        .analyze(
            "fn shadow() -> bool {
                let x = 10;
                let x = true;
                x
            }",
            "shadow",
        )
        .expect("shadowing body analyzes");
    assert_eq!(body.function.num_locals, 2);
    assert_eq!(body.function.air.return_type(), Type::BOOL);
}

// Migrated from `tests::test_nested_scope_variable_not_visible_outside`: an
// inner-scope binding is not visible after the scope ends.
#[test]
fn provider_body_hides_nested_scope_variable_outside() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
                {
                    let x = 10;
                }
                x
            }",
            "main",
        )
        .map(|_| ())
        .expect_err("out-of-scope use is rejected");
    assert!(
        matches!(error.kind, ErrorKind::UndefinedVariable(_)),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::test_shadowed_variable_restored_after_scope`: the
// outer binding is visible again once the shadowing scope ends.
#[test]
fn provider_body_restores_shadowed_variable_after_scope() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
                let x = 10;
                {
                    let x = 20;
                }
                x
            }",
            "main",
        )
        .expect("restored shadowed binding analyzes");
    assert_eq!(body.function.num_locals, 2);
}

// Migrated from `tests::test_deeply_nested_scopes`: bindings across deeply
// nested scopes each keep their own slot and stay reachable.
#[test]
fn provider_body_tracks_deeply_nested_scopes() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
                let a = 1;
                {
                    let b = 2;
                    {
                        let c = 3;
                        {
                            let d = 4;
                            a + b + c + d
                        }
                    }
                }
            }",
            "main",
        )
        .expect("deeply nested scopes analyze");
    assert_eq!(body.function.num_locals, 4);
}

// Migrated from `tests::test_if_else_scope_isolation`: a binding introduced in
// one branch is not visible in the other.
#[test]
fn provider_body_isolates_if_else_scopes() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
                if true {
                    let x = 10;
                    x
                } else {
                    y
                }
            }",
            "main",
        )
        .map(|_| ())
        .expect_err("cross-branch use is rejected");
    assert!(
        matches!(&error.kind, ErrorKind::UndefinedVariable(name) if name == "y"),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::test_loop_scope_isolation`: loop-body bindings do not
// leak past the loop.
#[test]
fn provider_body_isolates_loop_scope() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    fixture
        .analyze(
            "fn main() -> i32 {
                let mut i = 0;
                loop {
                    let inner = 1;
                    i = i + inner;
                    if i > 5 {
                        break;
                    }
                }
                i
            }",
            "main",
        )
        .expect("loop-scoped binding analyzes");

    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
                let mut i = 0;
                loop {
                    let inner = 1;
                    i = i + inner;
                    if i > 5 {
                        break;
                    }
                }
                inner
            }",
            "main",
        )
        .map(|_| ())
        .expect_err("post-loop use of the loop-local binding is rejected");
    assert!(
        matches!(&error.kind, ErrorKind::UndefinedVariable(name) if name == "inner"),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::test_struct_field_type_resolution`: nested nominal
// field types resolve from durable facts; the retired whole-program
// `struct_count == 3` becomes a per-body pool containment check.
#[test]
fn provider_body_resolves_struct_field_types_into_the_pool() {
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
        .expect("nested field resolution analyzes");

    assert_eq!(body.function.air.return_type(), Type::I32);
    for expected in ["Inner", "Outer"] {
        assert!(
            body.type_pool
                .all_struct_ids()
                .into_iter()
                .map(|id| body.type_pool.struct_def(id))
                .any(|def| &*def.name == expected),
            "the body's type pool must intern {expected}"
        );
    }
}

#[test]
fn provider_body_struct_field_mismatch_points_at_the_value() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_struct(
        "Record",
        vec![
            ("count", SemanticImportType::I32),
            (
                "text",
                SemanticImportType::BuiltinNominal {
                    name: Arc::from("str"),
                    kind: SemanticImportNominalKind::Struct,
                },
            ),
        ],
        false,
    );
    fixture.declare_function(
        "id",
        vec![value_param("n", SemanticImportType::I32)],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let source = "fn main() -> i32 {\n\
        let x: i32 = 1;\n\
        let r = Record { count: 0, text: id(x) };\n\
        0\n\
    }";
    let error = fixture
        .analyze(source, "main")
        .map(|_| ())
        .expect_err("the str field rejects an i32 expression");

    assert!(
        matches!(&error.kind, ErrorKind::TypeMismatch { expected, found }
            if expected == "str" && found == "i32"),
        "the diagnostic wording must preserve its expected/found direction: {error:?}"
    );
    assert_eq!(error_source_slice(source, &error), "id(x)");
    let [label] = error.diagnostic().labels.as_slice() else {
        panic!("the mismatch must carry exactly one field label: {error:?}");
    };
    assert_eq!(label.message, "field 'text' expects type str");
    assert_eq!(
        &source[label.span.start as usize..label.span.end as usize],
        "id(x)"
    );
}

// Migrated from `tests::test_copy_struct_with_copy_fields`: a `@copy` struct
// with Copy fields is copied (not moved) on rebinding, and its pool entry
// keeps the copy marking from the durable fact.
#[test]
fn provider_body_copy_struct_with_copy_fields_is_copyable() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_struct(
        "Point",
        vec![
            ("x", SemanticImportType::I32),
            ("y", SemanticImportType::I32),
        ],
        true,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
                let p = Point { x: 1, y: 2 };
                let q = p;
                p.x + q.x
             }",
            "main",
        )
        .expect("a copy value survives rebinding");
    assert!(
        body.type_pool
            .all_struct_ids()
            .into_iter()
            .map(|id| body.type_pool.struct_def(id))
            .any(|def| &*def.name == "Point" && def.is_copy),
        "Point must be interned as a copy struct"
    );
}

// Migrated from `tests::test_recursive_struct_via_array`: a simple
// non-recursive struct analyzes cleanly. Per-body, the nominal crosses as a
// durable fact, so the body constructs it to observe the resolution the
// retired driver observed at declaration time.
#[test]
fn provider_body_constructs_simple_struct() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_struct("Node", vec![("value", SemanticImportType::I32)], false);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    fixture
        .analyze(
            "fn main() -> i32 {
                let n = Node { value: 0 };
                n.value
             }",
            "main",
        )
        .expect("simple struct construction analyzes");
}

// Migrated from `tests::test_function_signature_resolution`: a callee's
// parameters and return type resolve through its durable signature fact, and
// the resolved callee is recorded as a referenced definition.
#[test]
fn provider_body_resolves_function_signature() {
    let mut fixture = ProviderFixture::new();
    let add = fixture.declare_function(
        "add",
        vec![
            value_param("a", SemanticImportType::I32),
            value_param("b", SemanticImportType::I32),
        ],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { add(1, 2) }", "main")
        .expect("call against the durable signature analyzes");
    assert_eq!(body.function.air.return_type(), Type::I32);
    assert!(
        body.referenced_definitions.contains(&add),
        "the resolved callee is a referenced definition"
    );
}

// Migrated from `tests::test_strbuf_is_not_injected`: a plain string literal
// body never interns the source-defined `StrBuf` nominal.
#[test]
fn provider_body_does_not_inject_strbuf() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
                let s = \"hello\";
                0
            }",
            "main",
        )
        .expect("string literal body analyzes");
    assert!(
        !body
            .type_pool
            .all_struct_ids()
            .into_iter()
            .map(|id| body.type_pool.struct_def(id))
            .any(|def| &*def.name == "StrBuf"),
        "StrBuf must not be injected"
    );
}

// Migrated from `tests::test_string_literal_type_inference`: string literals
// type as the builtin string and take local storage.
#[test]
fn provider_body_infers_string_literal_locals() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("has_content", Vec::new(), SemanticImportType::Bool);
    let body = fixture
        .analyze(
            "fn has_content() -> bool {
                let s = \"hello\";
                let t = \"world\";
                s.len() != 0
            }",
            "has_content",
        )
        .expect("string literal bindings analyze");
    assert!(body.function.num_locals >= 2);
}

// Migrated from `tests::test_integer_literal_infers_i32_by_default`: an
// unconstrained integer literal defaults to i32.
#[test]
fn provider_body_integer_literal_defaults_to_i32() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
                let x = 42;
                x
            }",
            "main",
        )
        .expect("default-typed literal analyzes");
    assert_eq!(body.function.air.return_type(), Type::I32);
}

// Migrated from `tests::test_integer_literal_infers_from_context`: an
// annotated binding constrains the literal's type.
#[test]
fn provider_body_integer_literal_infers_from_context() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("value", Vec::new(), SemanticImportType::I64);
    let body = fixture
        .analyze(
            "fn value() -> i64 {
                let x: i64 = 42;
                x
            }",
            "value",
        )
        .expect("annotated literal analyzes");
    assert_eq!(body.function.air.return_type(), Type::I64);
}

// Migrated from `tests::test_integer_literal_infers_from_return_type`: the
// declared return type constrains a trailing literal.
#[test]
fn provider_body_integer_literal_infers_from_return_type() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("value", Vec::new(), SemanticImportType::U8);
    let body = fixture
        .analyze("fn value() -> u8 { 42 }", "value")
        .expect("return-typed literal analyzes");
    assert_eq!(body.function.air.return_type(), Type::U8);
}

// Migrated from `tests::test_integer_literal_infers_from_binary_op`: a binary
// operand context constrains the literal's type.
#[test]
fn provider_body_integer_literal_infers_from_binary_op() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("value", Vec::new(), SemanticImportType::I64);
    let body = fixture
        .analyze(
            "fn value() -> i64 {
                let x: i64 = 10;
                x + 5
            }",
            "value",
        )
        .expect("binary-op-typed literal analyzes");
    assert_eq!(body.function.air.return_type(), Type::I64);
}

// Migrated from `tests::test_array_type_inference`: an annotated array binding
// types its elements and indexes as i32.
#[test]
fn provider_body_infers_array_types() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                arr[0]
            }",
            "main",
        )
        .expect("array body analyzes");
    assert_eq!(body.function.air.return_type(), Type::I32);
}

#[test]
fn provider_array_type_allocation_is_stable_across_expr_map_layouts() {
    fn snapshot(seeds: [u64; 4], reverse_insertion: bool) -> Vec<(u32, u32, u64)> {
        let body = crate::inference::with_expr_types_test_layout(seeds, reverse_insertion, || {
            let mut fixture = ProviderFixture::new();
            fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
            fixture.analyze(
                "fn main() -> i32 {
                        let flags = [true, false, true];
                        let pairs = [[1, 2], [3, 4]];
                        let values = [5, 6, 7, 8];
                        if flags[0] { pairs[0][0] + values[0] } else { 0 }
                    }",
                "main",
            )
        })
        .expect("array inference succeeds through the provider body path");
        assert!(
            body.function.air.iter().any(|(_, inst)| inst.ty.is_array()),
            "the semantic artifact must retain array-typed instructions"
        );
        body.type_pool
            .all_array_ids()
            .into_iter()
            .map(|id| {
                let (element, length) = body.type_pool.array_def(id);
                (Type::new_array(id).as_u32(), element.as_u32(), length)
            })
            .collect()
    }

    let configurations = [
        ([1, 2, 3, 4], false),
        ([91, 73, 55, 37], true),
        ([0, 0, 0, 0], false),
        ([u64::MAX, 0, u64::MAX, 0], true),
        ([13, 21, 34, 55], false),
        ([89, 144, 233, 377], true),
        ([0x1234, 0x5678, 0x9abc, 0xdef0], false),
        ([0xfedc, 0xba98, 0x7654, 0x3210], true),
    ];
    let baseline = snapshot(configurations[0].0, configurations[0].1);
    for (seeds, reverse_insertion) in configurations.into_iter().skip(1) {
        assert_eq!(
            snapshot(seeds, reverse_insertion),
            baseline,
            "array allocation changed for seeds {seeds:?}, reverse insertion={reverse_insertion}"
        );
    }
    assert_eq!(baseline.len(), 4);
    let mut lengths = baseline
        .iter()
        .map(|(_, _, length)| *length)
        .collect::<Vec<_>>();
    lengths.sort_unstable();
    assert_eq!(lengths, [2, 2, 3, 4]);
}

// Migrated from `tests::test_array_index_signed_type_is_accepted`: any integer
// type indexes an array (spec 7.1:7); range violations trap at runtime.
#[test]
fn provider_body_accepts_signed_array_index() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                let i: i32 = 1;
                arr[i]
            }",
            "main",
        )
        .expect("signed index analyzes");
    assert_eq!(body.function.air.return_type(), Type::I32);
}

// Migrated from `tests::test_array_index_non_integer_is_rejected`: a bool
// index is still a type error.
#[test]
fn provider_body_rejects_non_integer_array_index() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                let b: bool = true;
                arr[b]
            }",
            "main",
        )
        .map(|_| ())
        .expect_err("a non-integer index is rejected");
    assert!(error.span().is_some(), "index diagnostic keeps its span");
}

// Migrated from `tests::test_array_index_literal_infers_integer`: a literal
// index compiles under the integer-literal default.
#[test]
fn provider_body_array_index_literal_infers_integer() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                arr[1]
            }",
            "main",
        )
        .expect("literal index analyzes");
    assert_eq!(body.function.air.return_type(), Type::I32);
}

// Migrated from `tests::test_array_length_mismatch`: the annotated length must
// match the initializer.
#[test]
fn provider_body_rejects_array_length_mismatch() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2];
                arr[0]
            }",
            "main",
        )
        .map(|_| ())
        .expect_err("length mismatch is rejected");
    assert!(error.span().is_some(), "length diagnostic keeps its span");
}

// Migrated from `tests::fallible_intrinsic_rejects_local_option_context`
// (RUE-1112): a fallible intrinsic's result IS the exact trusted std `Option`;
// with no trusted well-known Option facts seeded, the intrinsic fails closed
// instead of adopting a local same-shape lookalike from context. The original
// spelled the lookalike through a comptime constructor `Option(T)`; a
// cross-declaration comptime call cannot reduce through the fixture's durable
// source (it carries no comptime reducer), so the lookalike is spelled as the
// equivalent body-local anonymous enum, preserving the fail-closed assertion.
#[test]
fn provider_body_fallible_intrinsic_rejects_local_option_context() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            r#"fn main() -> i32 {
                let Opt = enum { Some(i64), None };
                let parsed: Opt = @parse_i64("42");
                match parsed {
                    Opt.Some(value) => @intCast(value),
                    Opt.None => 0,
                }
            }"#,
            "main",
        )
        .map(|_| ())
        .expect_err("a local-Option annotation is not accepted");
    assert!(
        error.to_string().contains("parse_i64"),
        "expected fail-closed missing-registry diagnostics on @parse_i64: {error:?}"
    );
}

// Migrated from `tests::comptime_type_alias_filter_preserves_analysis_and_diagnostics`:
// the alias filter keeps body-local comptime type aliases usable while
// ordinary diagnostics survive unchanged. The original's `Call = Make()` leg
// is a cross-declaration comptime call, which cannot reduce through the
// fixture's durable source (no comptime reducer); the body-local alias legs
// carry the filter behavior here.
#[test]
fn provider_body_comptime_type_alias_filter_preserves_analysis_and_diagnostics() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    fixture
        .analyze(
            "fn main() -> i32 {
                let runtime = 40 + 2;
                let Direct = i32;
                let Name = Direct;

                let direct: Direct = runtime;
                let name: Name = direct;
                name
            }",
            "main",
        )
        .expect("body-local aliases analyze");

    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 { let runtime = missing + 1; runtime }",
            "main",
        )
        .map(|_| ())
        .expect_err("ordinary diagnostics survive the alias filter");
    assert!(
        matches!(&error.kind, ErrorKind::UndefinedVariable(name) if name == "missing"),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::runtime_parameter_prevents_false_local_type_alias_precomputation`:
// a runtime parameter lexically shadows the same-named file-level type-valued
// constant (which crosses as a durable const fact), so a binding initialized
// from it is not a type alias.
#[test]
fn provider_body_runtime_parameter_prevents_false_local_type_alias_precomputation() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_const(
        "value",
        SemanticImportType::ComptimeType,
        SemanticImportConstValue::Type(SemanticImportType::I32),
    );
    fixture.declare_function(
        "use",
        vec![value_param("value", SemanticImportType::I32)],
        SemanticImportType::I32,
    );
    let error = fixture
        .analyze(
            "fn use(value: i32) -> i32 {
                 let local = value;
                 let result: local = 1;
                 result
             }",
            "use",
        )
        .map(|_| ())
        .expect_err("a runtime value cannot be used as a type alias");
    assert!(
        matches!(&error.kind, ErrorKind::UnknownType(name) if name == "local"),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::array_length_nested_type_argument_preserves_unknown_type_error`:
// an unknown type argument nested inside an array-length call keeps its exact
// unknown-type diagnostic.
#[test]
fn provider_body_array_length_nested_type_argument_preserves_unknown_type_error() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "Width",
        vec![comptime_param("T", SemanticImportType::ComptimeType)],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze(
            "fn main() -> i32 {
                 let values: [i32; Width(Unknown)] = [1, 2];
                 values[0]
             }",
            "main",
        )
        .map(|_| ())
        .expect_err("the unknown nested type argument is rejected");
    assert!(
        matches!(&error.kind, ErrorKind::UnknownType(name) if name == "Unknown"),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::unknown_type_constructor_diagnostic_preserves_placeholder_call_spelling`:
// an unknown type constructor keeps the placeholder call spelling in its
// diagnostic.
#[test]
fn provider_body_unknown_type_constructor_preserves_placeholder_call_spelling() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { @size_of(Foo(i32)) }", "main")
        .map(|_| ())
        .expect_err("the unknown constructor is rejected");
    assert!(
        matches!(&error.kind, ErrorKind::UnknownType(syntax) if syntax == "Foo(...)"),
        "unexpected diagnostic: {error:?}"
    );
}
