//! Provider-path body-analysis tests migrated from the retired whole-program
//! `Sema` drivers; populated by the RUE-1538 migration.
//!
//! This file carries the byref-argument, string, and ownership tests: each
//! drives [`super::analyze_provider_ordinary_body`] through the shared
//! [`super::provider_fixture::ProviderFixture`], so the analyzed source holds
//! exactly one declaration and every other fact crosses the provider boundary
//! as explicit durable data.

use std::sync::Arc;

use rue_error::ErrorKind;

use super::provider_fixture::{
    FixtureBody, FixtureType, MethodShape, ProviderFixture, StructShape, mode_param, value_param,
};
use crate::inst::{AirArgMode, AirInstData};
use crate::types::Type;
use crate::{
    SemanticImportConstValue, SemanticImportNominalKind, SemanticImportType, SemanticParameterMode,
    StableDefinitionKind,
};

/// The `str` view type as it crosses the provider boundary: a builtin nominal
/// carried by name and resolved by the body-local identity pool.
fn str_view() -> FixtureType {
    SemanticImportType::BuiltinNominal {
        name: Arc::from("str"),
        kind: SemanticImportNominalKind::Struct,
    }
}

/// A fixed string buffer type `Str(capacity)` as it crosses the provider
/// boundary.
fn fixed_str(capacity: u32) -> FixtureType {
    SemanticImportType::BuiltinNominal {
        name: Arc::from(crate::types::fixed_string_name(capacity.into())),
        kind: SemanticImportNominalKind::Struct,
    }
}

/// Every borrow-operand slot introduced by one analyzed body, paired with
/// whether it is registered as non-owning (the AIR signature of static
/// promotion). Provider-path counterpart of the whole-program helper in
/// `tests.rs`, reading one body's `ValidatedAir`.
fn borrow_operand_slots(body: &FixtureBody) -> Vec<(u32, bool)> {
    let air = &body.function.air;
    air.iter()
        .filter_map(|(_, inst)| match inst.data {
            AirInstData::StorageLive { slot } => Some((slot, air.is_borrow_slot(slot))),
            _ => None,
        })
        .collect()
}

// Migrated from `tests::inout_arguments_reject_non_places_during_air_analysis`:
// every non-place `inout` argument shape is rejected during body analysis. The
// literal shape is also covered by the fixture exemplar
// `provider_fixture_tests::provider_body_rejects_inout_argument_without_a_place`;
// the retired test kept its three shapes together, so this one does too.
#[test]
fn provider_body_rejects_every_non_place_inout_argument_shape() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_const(
        "VALUE",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(1),
    );
    fixture.declare_function("value", Vec::new(), SemanticImportType::I32);
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

    for source in [
        "fn main() -> i32 { take(inout VALUE); 0 }",
        "fn main() -> i32 { take(inout 1); 0 }",
        "fn main() -> i32 { take(inout value()); 0 }",
    ] {
        let error = fixture
            .analyze(source, "main")
            .map(|_| ())
            .expect_err("non-place must fail in sema");
        assert!(
            matches!(&error.kind, ErrorKind::InoutNonLvalue),
            "source: {source}\nerror: {error:?}"
        );
    }
}

// Migrated from `tests::borrow_operands_that_name_no_place_are_elaborated`:
// every shape the old E0427 rejected now compiles (RUE-953).
#[test]
fn provider_body_elaborates_borrow_operands_that_name_no_place() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_const(
        "VALUE",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(1),
    );
    fixture.declare_function("value", Vec::new(), SemanticImportType::I32);
    fixture.declare_function(
        "take",
        vec![mode_param(
            "x",
            SemanticImportType::I32,
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::Unit,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    for source in [
        "fn main() -> i32 { take(borrow VALUE); 0 }",
        "fn main() -> i32 { take(borrow (1 + 2)); 0 }",
        "fn main() -> i32 { take(borrow value()); 0 }",
        "fn main() -> i32 { take(borrow 5); 0 }",
    ] {
        fixture.analyze(source, "main").unwrap_or_else(|error| {
            panic!("source: {source}\nerror: {error:?}");
        });
    }
}

// Migrated from `tests::promoted_borrow_operands_schedule_no_cleanup`: a
// comptime-evaluable, infallible operand loans a static image: its hidden
// binding owns nothing, so its slot is non-owning and drop elaboration
// schedules nothing for it.
#[test]
fn provider_body_promoted_borrow_operands_schedule_no_cleanup() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_const(
        "VALUE",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(1),
    );
    fixture.declare_function(
        "take",
        vec![mode_param(
            "x",
            SemanticImportType::I32,
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::Unit,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    let body = fixture
        .analyze(
            "fn main() -> i32 { take(borrow 5); take(borrow (1 + 2)); take(borrow VALUE); 0 }",
            "main",
        )
        .expect("promotable operands compile");
    let slots = borrow_operand_slots(&body);
    assert_eq!(slots.len(), 3, "one hidden binding per operand: {slots:?}");
    assert!(
        slots.iter().all(|(_, non_owning)| *non_owning),
        "every promoted operand's slot is non-owning: {slots:?}"
    );
}

// Migrated from `tests::runtime_borrow_operands_materialize_an_owning_temporary`:
// a runtime operand — and a form outside the promotion set, even with constant
// arguments — takes the temporary path, whose binding owns its value and is
// dropped by the ordinary scope-exit machinery.
#[test]
fn provider_body_runtime_borrow_operands_materialize_an_owning_temporary() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("value", Vec::new(), SemanticImportType::I32);
    fixture.declare_function(
        "take",
        vec![mode_param(
            "x",
            SemanticImportType::I32,
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::Unit,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    for source in [
        "fn main() -> i32 { take(borrow value()); 0 }",
        "fn main() -> i32 { take(borrow (6 / 2)); 0 }",
    ] {
        let body = fixture
            .analyze(source, "main")
            .expect("runtime operands compile");
        let slots = borrow_operand_slots(&body);
        assert_eq!(slots.len(), 1, "source: {source}, slots: {slots:?}");
        assert!(
            !slots[0].1,
            "source: {source}: a temporary's slot owns its value"
        );
    }
}

// Migrated from `tests::linear_borrow_operand_temporaries_are_rejected`: the
// linear marking crosses the boundary inside the durable nominal fact, and
// nothing can consume a hidden binding, so a linear one leaks.
#[test]
fn provider_body_rejects_linear_borrow_operand_temporaries() {
    let mut fixture = ProviderFixture::new();
    let token = fixture.declare_struct_with(
        "Token",
        vec![("id", SemanticImportType::I32)],
        false,
        StructShape {
            is_linear: true,
            ..StructShape::default()
        },
    );
    fixture.declare_function(
        "make",
        Vec::new(),
        SemanticImportType::Nominal(token.clone()),
    );
    fixture.declare_function(
        "peek",
        vec![mode_param(
            "t",
            SemanticImportType::Nominal(token),
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    let error = fixture
        .analyze("fn main() -> i32 { peek(borrow make()) }", "main")
        .map(|_| ())
        .expect_err("nothing can consume a hidden binding, so a linear one leaks");
    assert!(
        matches!(&error.kind, ErrorKind::LinearValueDiscarded { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::byref_arguments_accept_places_and_forwarded_projections`:
// the caller body accepts places and projections as byref arguments, and the
// forwarding body re-projects its own byref parameters; each body analyzes
// under the same durable callee facts.
#[test]
fn provider_body_byref_arguments_accept_places_and_forwarded_projections() {
    let mut fixture = ProviderFixture::new();
    let pair = fixture.declare_struct("Pair", vec![("value", SemanticImportType::I32)], false);
    fixture.declare_function(
        "edit",
        vec![mode_param(
            "x",
            SemanticImportType::I32,
            SemanticParameterMode::Inout,
        )],
        SemanticImportType::Unit,
    );
    fixture.declare_function(
        "read",
        vec![mode_param(
            "x",
            SemanticImportType::I32,
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::I32,
    );
    fixture.declare_function(
        "forward",
        vec![
            mode_param(
                "edit_pair",
                SemanticImportType::Nominal(pair.clone()),
                SemanticParameterMode::Inout,
            ),
            mode_param(
                "read_pair",
                SemanticImportType::Nominal(pair),
                SemanticParameterMode::Borrow,
            ),
        ],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    fixture
        .analyze(
            "fn forward(inout edit_pair: Pair, borrow read_pair: Pair) -> i32 {
    edit(inout edit_pair.value);
    read(borrow read_pair.value)
}",
            "forward",
        )
        .expect("forwarded byref projections analyze");
    fixture
        .analyze(
            "fn main() -> i32 {
    let mut pair = Pair { value: 1 };
    let other = Pair { value: 2 };
    let mut values = [1, 2];
    edit(inout pair.value);
    edit(inout values[0]);
    read(borrow pair.value) + read(borrow values[1]) + forward(inout pair, borrow other)
}",
            "main",
        )
        .expect("byref places and projections analyze");
}

// Migrated from `tests::byref_method_receiver_rejects_call_result_during_air_analysis`:
// a call result is not an addressable method receiver; the borrow receiver
// mode crosses the boundary as a durable method fact.
#[test]
fn provider_body_byref_method_receiver_rejects_call_result() {
    let mut fixture = ProviderFixture::new();
    let pair = fixture.declare_struct("Pair", vec![("value", SemanticImportType::I32)], false);
    fixture.declare_method_with(
        &pair,
        "read",
        Vec::new(),
        SemanticImportType::I32,
        MethodShape {
            self_mode: SemanticParameterMode::Borrow,
            ..MethodShape::default()
        },
    );
    fixture.declare_function("make", Vec::new(), SemanticImportType::Nominal(pair));
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    let error = fixture
        .analyze("fn main() -> i32 { make().read() }", "main")
        .map(|_| ())
        .expect_err("a call result is not an addressable method receiver");
    assert!(
        matches!(&error.kind, ErrorKind::BorrowNonLvalue),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::inout_param_assignment_is_constrained_and_store_typed_exactly`:
// parameter assignments participate in inference, so a literal takes the
// declared integer width instead of defaulting to i32. Sema then proves the
// same exact type again at the ParamStore chokepoint.
#[test]
fn provider_body_inout_param_assignment_is_constrained_and_store_typed_exactly() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "replace",
        vec![mode_param(
            "value",
            SemanticImportType::I64,
            SemanticParameterMode::Inout,
        )],
        SemanticImportType::Unit,
    );

    let body = fixture
        .analyze("fn replace(inout value: i64) { value = 42; }", "replace")
        .expect("constrained parameter assignment analyzes");
    let stored_value = body
        .function
        .air
        .iter()
        .find_map(|(_, inst)| match inst.data {
            AirInstData::ParamStore { value, .. } => Some(value),
            _ => None,
        })
        .expect("replace must contain a ParamStore");
    assert_eq!(body.function.air.get(stored_value).ty, Type::I64);

    // An arbitrary differently-typed RHS used to bypass inference and reach
    // ParamStore. It must now fail in the frontend.
    let error = fixture
        .analyze("fn replace(inout value: i64) { value = true; }", "replace")
        .map(|_| ())
        .expect_err("a bool RHS cannot satisfy the i64 parameter");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "i64" && found == "bool"
        ),
        "unexpected diagnostic: {error:?}"
    );

    // Constraining only mutable parameters preserves the primary target
    // diagnostic for normal and borrowed bindings; RHS type inference must
    // not mask these as E0206.
    let mut immutable_fixture = ProviderFixture::new();
    immutable_fixture.declare_function(
        "replace",
        vec![value_param("value", SemanticImportType::I64)],
        SemanticImportType::Unit,
    );
    let immutable = immutable_fixture
        .analyze("fn replace(value: i64) { value = true; }", "replace")
        .map(|_| ())
        .expect_err("a value parameter is immutable");
    assert!(
        matches!(&immutable.kind, ErrorKind::AssignToImmutable(_)),
        "unexpected diagnostic: {immutable:?}"
    );

    let mut borrowed_fixture = ProviderFixture::new();
    borrowed_fixture.declare_function(
        "replace",
        vec![mode_param(
            "value",
            SemanticImportType::I64,
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::Unit,
    );
    let borrowed = borrowed_fixture
        .analyze("fn replace(borrow value: i64) { value = true; }", "replace")
        .map(|_| ())
        .expect_err("a borrowed parameter cannot be mutated");
    assert!(
        matches!(&borrowed.kind, ErrorKind::MutateBorrowedValue { .. }),
        "unexpected diagnostic: {borrowed:?}"
    );
}

// Migrated from `tests::test_string_len_method`: `str.len()` returns u64.
#[test]
fn provider_body_string_len_method_returns_u64() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("length", Vec::new(), SemanticImportType::U64);
    let body = fixture
        .analyze(
            "fn length() -> u64 {
    let s = \"hello\";
    s.len()
}",
            "length",
        )
        .expect("string length body analyzes");
    assert_eq!(body.function.air.return_type(), Type::U64);
}

// Migrated from `tests::test_string_is_empty_method`: an emptiness probe built
// on `str.len()` returns bool.
#[test]
fn provider_body_string_emptiness_probe_returns_bool() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("empty", Vec::new(), SemanticImportType::Bool);
    let body = fixture
        .analyze(
            "fn empty() -> bool {
    let s = \"hello\";
    s.len() == 0
}",
            "empty",
        )
        .expect("string emptiness body analyzes");
    assert_eq!(body.function.air.return_type(), Type::BOOL);
}

// Migrated from `tests::non_string_tail_in_str_function_is_rejected_during_sema`:
// a bool tail cannot satisfy a `str` return type. The recursive callee
// signature crosses the boundary as a durable function fact.
#[test]
fn provider_body_rejects_non_string_tail_in_str_function() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "choose",
        vec![value_param("c", SemanticImportType::Bool)],
        str_view(),
    );
    // Parenthesized so the `if` is in operand position: in statement/tail
    // position a block-like expression is a complete statement and a following
    // `==` is a syntax error (RUE-918). The bool comparison tail still
    // mismatches `str`.
    let error = fixture
        .analyze(
            r#"fn choose(c: bool) -> str {
    (if c { "hello" } else { "world" }) == choose(true)
}"#,
            "choose",
        )
        .map(|_| ())
        .expect_err("a bool tail cannot satisfy a str return type");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "bool" && found == "str"
                    || expected == "str" && found == "bool"
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::str_literal_body_imports_fresh`, reduced to the body
// that owns the literal: the whole-program half asserted two exports and an
// epoch re-import, which does not apply per body. The per-body core remains:
// the literal is exported as a body-local string, and the `str` result stays a
// builtin identity — never a named-nominal token.
#[test]
fn provider_body_str_literal_strings_are_body_local() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("make", Vec::new(), str_view());
    let body = fixture
        .analyze("fn make() -> str { \"hello\" }", "make")
        .expect("str literal body analyzes");
    assert_eq!(body.strings, vec!["hello".to_owned()]);
    assert_eq!(
        body.function
            .air
            .return_type()
            .safe_name_with_pool(Some(&body.type_pool)),
        "str"
    );
    assert_eq!(
        body.export.body.strings.as_ref(),
        [Arc::<str>::from("hello")]
    );
    assert!(
        body.export
            .body
            .instructions
            .iter()
            .all(|inst| !matches!(inst.ty, SemanticImportType::Nominal(_))),
        "the str-typed body must export no named-nominal instruction types"
    );
}

// Migrated from `tests::inout_str_view_cannot_be_reassigned_as_a_whole_value`:
// an `inout str` view parameter rejects whole-value reassignment.
#[test]
fn provider_body_inout_str_view_cannot_be_reassigned_as_a_whole_value() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "replace",
        vec![
            mode_param("target", str_view(), SemanticParameterMode::Inout),
            value_param("replacement", str_view()),
        ],
        SemanticImportType::Unit,
    );
    let error = fixture
        .analyze(
            "fn replace(inout target: str, replacement: str) {
    target = replacement;
}",
            "replace",
        )
        .map(|_| ())
        .expect_err("a str view cannot be reassigned as a whole value");
    assert!(
        matches!(&error.kind, ErrorKind::StrViewReassignment),
        "unexpected diagnostic: {error:?}"
    );
}

/// The Probe declaration whose member bodies carry the physical-mode
/// assertions of `provider_bodies_method_and_assoc_view_calls_encode_physical_modes`.
const PROBE_VIEW_MODES_SOURCE: &str = r#"struct Probe {
    bias: i32,

    fn method(
        borrow self,
        borrow read: str,
        inout edit: str,
        borrow item: Item,
    ) -> i32 {
        self.bias + @intCast(read.len()) + @intCast(edit.len()) + item.value
    }

    fn assoc(borrow read: str, inout edit: str, borrow item: Item) -> i32 {
        11 + @intCast(read.len()) + @intCast(edit.len()) + item.value
    }
}"#;

// Migrated from `tests::method_and_assoc_view_calls_encode_physical_modes`:
// the caller encodes each argument's physical mode (a borrowed `str` view
// passes by value), and the member bodies flatten their parameter modes per
// ABI slot. The caller and each member body analyze separately under the same
// durable facts.
#[test]
fn provider_bodies_method_and_assoc_view_calls_encode_physical_modes() {
    let mut fixture = ProviderFixture::new();
    let item = fixture.declare_struct("Item", vec![("value", SemanticImportType::I32)], false);
    let probe = fixture.declare_struct("Probe", vec![("bias", SemanticImportType::I32)], false);
    let member_params = || {
        vec![
            mode_param("read", str_view(), SemanticParameterMode::Borrow),
            mode_param("edit", str_view(), SemanticParameterMode::Inout),
            mode_param(
                "item",
                SemanticImportType::Nominal(item.clone()),
                SemanticParameterMode::Borrow,
            ),
        ]
    };
    fixture.declare_method_with(
        &probe,
        "method",
        member_params(),
        SemanticImportType::I32,
        MethodShape {
            self_mode: SemanticParameterMode::Borrow,
            ..MethodShape::default()
        },
    );
    fixture.declare_method_with(
        &probe,
        "assoc",
        member_params(),
        SemanticImportType::I32,
        MethodShape {
            has_self: false,
            ..MethodShape::default()
        },
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    let main = fixture
        .analyze(
            r#"fn main() -> i32 {
    let probe = Probe { bias: 11 };
    let read: Str(8) = "read";
    let mut edit: Str(8) = "editor";
    let item = Item { value: 0 };
    probe.method(borrow read, inout edit, borrow item)
        + Probe.assoc(borrow read, inout edit, borrow item)
}"#,
            "main",
        )
        .expect("view-mode caller analyzes");
    let call_modes: Vec<Vec<AirArgMode>> = main
        .function
        .air
        .iter()
        .filter_map(|(_, inst)| match inst.data {
            AirInstData::Call { ref args, .. } => Some(
                main.function
                    .air
                    .get_call_args(args)
                    .map(|arg| arg.mode)
                    .collect(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        call_modes,
        vec![
            vec![
                AirArgMode::Borrow,
                AirArgMode::Normal,
                AirArgMode::Inout,
                AirArgMode::Borrow,
            ],
            vec![AirArgMode::Normal, AirArgMode::Inout, AirArgMode::Borrow],
        ]
    );

    let method = fixture
        .analyze_member(
            PROBE_VIEW_MODES_SOURCE,
            "Probe",
            "method",
            StableDefinitionKind::Method,
        )
        .expect("view-mode method body analyzes");
    assert_eq!(
        method.function.param_modes.by_ref(),
        &[true, false, false, true, true]
    );
    assert_eq!(
        method.function.param_modes.writable(),
        &[false, false, false, true, false]
    );

    let assoc = fixture
        .analyze_member(
            PROBE_VIEW_MODES_SOURCE,
            "Probe",
            "assoc",
            StableDefinitionKind::AssociatedFunction,
        )
        .expect("view-mode associated body analyzes");
    assert_eq!(
        assoc.function.param_modes.by_ref(),
        &[false, false, true, true]
    );
    assert_eq!(
        assoc.function.param_modes.writable(),
        &[false, false, true, false]
    );
}

// Migrated from `tests::method_and_assoc_fixed_string_literals_are_contextual`:
// a fixed-string parameter contextualizes branch-result literals at both
// method and associated call sites.
#[test]
fn provider_body_method_and_assoc_fixed_string_literals_are_contextual() {
    let mut fixture = ProviderFixture::new();
    let probe = fixture.declare_struct("Probe", Vec::new(), false);
    fixture.declare_method(
        &probe,
        "method",
        vec![value_param("value", fixed_str(8))],
        SemanticImportType::U64,
    );
    fixture.declare_method_with(
        &probe,
        "assoc",
        vec![value_param("value", fixed_str(8))],
        SemanticImportType::U64,
        MethodShape {
            has_self: false,
            ..MethodShape::default()
        },
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    fixture
        .analyze(
            r#"fn main() -> i32 {
    let probe = Probe {};
    @intCast(
        probe.method(if true { "hi" } else { "bye" })
            + Probe.assoc(if false { "long" } else { "four" })
    )
}"#,
            "main",
        )
        .expect("contextual fixed-string call sites analyze");
}

// Migrated from `tests::string_literal_default_is_stable_str`: an
// unconstrained string literal defaults to the Copy, two-slot `str` view.
#[test]
fn provider_body_string_literal_default_is_stable_str() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            r#"fn main() -> i32 {
    let value = "hello";
    let first = value;
    let second = value;
    @intCast(first.len() + second.len())
}"#,
            "main",
        )
        .expect("the stable str default must be Copy and reusable");
    let literal = body
        .function
        .air
        .iter()
        .find_map(|(_, inst)| matches!(inst.data, AirInstData::StringConst(_)).then_some(inst))
        .expect("main must materialize its literal");
    assert_eq!(literal.ty.safe_name_with_pool(Some(&body.type_pool)), "str");
    assert_eq!(body.type_pool.abi_slot_count(literal.ty), 2);
}

// Migrated from `tests::string_default_survives_control_flow_and_aggregate_joins`:
// literal-derived joins through branches, blocks, matches, and aggregate
// fields remain Copy first-class `str` values.
#[test]
fn provider_body_string_default_survives_control_flow_and_aggregate_joins() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_struct("Holder", vec![("value", str_view())], false);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            r#"fn main() -> i32 {
    let branch = if true { "a" } else { "bb" };
    let branch_first = branch;
    let branch_second = branch;

    let block = { let marker = 0; "ccc" };
    let block_first = block;
    let block_second = block;

    let matched = match true {
        true => "dddd",
        false => "eeeee",
    };
    let match_first = matched;
    let match_second = matched;

    let holder = Holder {
        value: if false { "ffffff" } else { "ggggggg" },
    };
    let field_first = holder.value;
    let field_second = holder.value;

    @intCast(
        branch_first.len() + branch_second.len()
            + block_first.len() + block_second.len()
            + match_first.len() + match_second.len()
            + field_first.len() + field_second.len()
    )
}"#,
            "main",
        )
        .expect("literal-derived joins must remain Copy first-class str values");

    let literal_types: Vec<Type> = body
        .function
        .air
        .iter()
        .filter_map(|(_, inst)| matches!(inst.data, AirInstData::StringConst(_)).then_some(inst.ty))
        .collect();
    assert_eq!(literal_types.len(), 7);
    assert!(literal_types.iter().all(|&ty| {
        ty.safe_name_with_pool(Some(&body.type_pool)) == "str"
            && body.type_pool.abi_slot_count(ty) == 2
            && ty.is_copy_in_pool(&body.type_pool)
    }));
}

// Migrated from `tests::string_literal_join_cannot_default_through_an_integer_literal`:
// integer and string literal branches must not share a default.
#[test]
fn provider_body_string_literal_join_cannot_default_through_an_integer_literal() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "choose",
        vec![value_param("cond", SemanticImportType::Bool)],
        SemanticImportType::Unit,
    );
    let error = fixture
        .analyze(
            r#"fn choose(cond: bool) {
    let mixed = if cond { 42 } else { "not an integer" };
}"#,
            "choose",
        )
        .map(|_| ())
        .expect_err("integer and string literal branches must not share a default");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "string type" && found == "{integer}"
        ),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::fixed_string_call_context_stops_at_nested_call_operands`:
// a fixed-string parameter's context reaches structural result positions of
// its own argument but never a nested call's operands. Each caller shape
// analyzes independently under the same durable callee facts.
#[test]
fn provider_body_fixed_string_call_context_stops_at_nested_call_operands() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "take",
        vec![value_param("value", fixed_str(8))],
        SemanticImportType::U64,
    );
    fixture.declare_function("make", Vec::new(), fixed_str(8));
    fixture.declare_function("with_assert", Vec::new(), fixed_str(8));
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    let cases = [
        (
            "conditional result",
            r#"fn main() -> i32 { @intCast(take(if true { "hi" } else { "bye" })) }"#,
        ),
        (
            "block result",
            r#"fn main() -> i32 { @intCast(take({ let _marker = 0; "block" })) }"#,
        ),
        (
            "declared fixed-string return",
            r#"fn main() -> i32 { @intCast(take(make())) }"#,
        ),
        (
            "intrinsic statement before a fixed-string block result",
            r#"fn main() -> i32 { @intCast(take(with_assert())) }"#,
        ),
        (
            "never-returning intrinsic",
            r#"fn main() -> i32 { @intCast(take(@panic("boom"))) }"#,
        ),
    ];
    for (case, source) in cases {
        fixture.analyze(source, "main").unwrap_or_else(|error| {
            panic!("{case} must compile independently: {error:?}");
        });
    }
}

// Migrated from `tests::expected_string_type_reaches_only_structural_result_positions`:
// comparison operands and a discarded loop-body value have no buffer context,
// so they use the first-class `str` default; if/match-style branches and block
// tails remain transparent and materialize as the declared Str(8). The retired
// whole-program counts (3 str / 4 Str(8)) split per body: choose holds 2 str +
// 3 Str(8), loop_probe holds 1 str + 1 Str(8).
#[test]
fn provider_bodies_expected_string_type_reaches_only_structural_result_positions() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "choose",
        vec![value_param("flag", SemanticImportType::Bool)],
        fixed_str(8),
    );
    fixture.declare_function("loop_probe", Vec::new(), fixed_str(8));

    let literal_type_names = |body: &FixtureBody| -> Vec<String> {
        body.function
            .air
            .iter()
            .filter(|(_, inst)| matches!(inst.data, AirInstData::StringConst(_)))
            .map(|(_, inst)| inst.ty.safe_name_with_pool(Some(&body.type_pool)))
            .collect()
    };

    let choose = fixture
        .analyze(
            r#"fn choose(flag: bool) -> Str(8) {
    if "left" == "right" {
        if flag { "yes" } else { "no" }
    } else {
        { let marker = 0; "fallback" }
    }
}"#,
            "choose",
        )
        .expect("structural result positions analyze");
    let choose_types = literal_type_names(&choose);
    assert_eq!(
        choose_types.iter().filter(|ty| *ty == "str").count(),
        2,
        "comparison operands default to str: {choose_types:?}"
    );
    assert_eq!(
        choose_types.iter().filter(|ty| *ty == "Str(8)").count(),
        3,
        "branch and block tails materialize the declared type: {choose_types:?}"
    );

    let loop_probe = fixture
        .analyze(
            r#"fn loop_probe() -> Str(8) {
    while false { "loop body"; }
    "tail"
}"#,
            "loop_probe",
        )
        .expect("loop probe analyzes");
    let loop_types = literal_type_names(&loop_probe);
    assert_eq!(
        loop_types.iter().filter(|ty| *ty == "str").count(),
        1,
        "a discarded loop-body value defaults to str: {loop_types:?}"
    );
    assert_eq!(
        loop_types.iter().filter(|ty| *ty == "Str(8)").count(),
        1,
        "the tail materializes the declared type: {loop_types:?}"
    );
}

// Migrated from `tests::declared_enum_payload_and_ptr_write_pointee_contextualize_fixed_strings`.
// A durable enum payload's fixed-string identity must contextualize the literal
// passed to its variant constructor even when the provider materializes that
// generated nominal during constraint generation.
#[test]
fn provider_body_enum_fixed_string_payload_contextualizes_literal() {
    let mut fixture = ProviderFixture::new();
    let message = fixture.declare_enum(
        "Message",
        vec![("Text", vec![fixed_str(8)]), ("Empty", vec![])],
    );
    fixture.declare_function("make", Vec::new(), SemanticImportType::Nominal(message));

    fixture
        .analyze(
            r#"fn make() -> Message {
    Message.Text("hello")
}"#,
            "make",
        )
        .expect("enum fixed-string payload contextualizes its construction literal");
}

#[test]
fn provider_body_source_nominal_cannot_counterfeit_fixed_string_payload() {
    let mut fixture = ProviderFixture::new();
    let counterfeit = fixture.declare_struct("Str(8)", Vec::new(), true);
    let message = fixture.declare_enum(
        "Message",
        vec![("Text", vec![SemanticImportType::Nominal(counterfeit)])],
    );
    fixture.declare_function("make", Vec::new(), SemanticImportType::Nominal(message));

    let error = fixture
        .analyze(
            r#"fn make() -> Message {
    Message.Text("hello")
}"#,
            "make",
        )
        .map(|_| ())
        .expect_err("a source-defined Str(8) nominal must not accept a string literal");
    assert!(
        matches!(
            &error.kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "string type" && found == "Str(8)"
        ),
        "unexpected counterfeit fixed-string diagnostic: {error:?}"
    );
}

#[test]
fn provider_body_ptr_write_pointee_contextualizes_fixed_strings() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    fixture
        .analyze(
            r#"fn main() -> i32 {
    let mut value: Str(8) = "old";
    checked {
        let p: ptr mut Str(8) = @raw_mut(value);
        @ptr_write(p, "new");
    };
    @intCast(value.len())
}"#,
            "main",
        )
        .expect("pointer-write pointee context analyzes");
}

// Migrated from `tests::inout_fixed_string_assignment_materializes_the_destination_type`:
// a fixed-string `inout` parameter assignment materializes the destination
// type at the ParamStore chokepoint, keeps never coercion legal, bounds the
// expected type away from intrinsic message operands, and rejects capacity
// and width mismatches.
#[test]
fn provider_body_inout_fixed_string_assignment_materializes_the_destination_type() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function(
        "replace",
        vec![mode_param(
            "value",
            fixed_str(8),
            SemanticParameterMode::Inout,
        )],
        SemanticImportType::Unit,
    );

    let body = fixture
        .analyze(
            r#"fn replace(inout value: Str(8)) { value = "hi"; }"#,
            "replace",
        )
        .expect("fixed-string parameter assignment analyzes");
    let stored_value = body
        .function
        .air
        .iter()
        .find_map(|(_, inst)| match inst.data {
            AirInstData::ParamStore { value, .. } => Some(value),
            _ => None,
        })
        .expect("replace must contain a ParamStore");
    assert_eq!(
        body.function
            .air
            .get(stored_value)
            .ty
            .safe_name_with_pool(Some(&body.type_pool)),
        "Str(8)"
    );

    // Never coercion remains legal. The outer Str(8) expectation belongs to
    // the assignment result and must not leak into @panic's text message
    // operand.
    for rhs in ["@panic()", "@panic(\"boom\")"] {
        fixture
            .analyze(
                &format!("fn replace(inout value: Str(8)) {{ value = {rhs}; }}"),
                "replace",
            )
            .unwrap_or_else(|error| panic!("{rhs} must coerce from never: {error:?}"));
    }

    // The same expected-type boundary applies to @assert: its message is
    // text, and the assignment is rejected for the unit result rather than
    // misdiagnosing the message as Str(8).
    let assertion = fixture
        .analyze(
            r#"fn replace(inout value: Str(8)) { value = @assert(false, "boom"); }"#,
            "replace",
        )
        .map(|_| ())
        .expect_err("a unit intrinsic result cannot satisfy Str(8)");
    assert!(
        matches!(
            &assertion.kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "Str(8)" && found == "()"
        ),
        "unexpected diagnostic: {assertion:?}"
    );

    let mut mismatch_fixture = ProviderFixture::new();
    mismatch_fixture.declare_function(
        "replace",
        vec![
            mode_param("value", fixed_str(8), SemanticParameterMode::Inout),
            value_param("other", fixed_str(16)),
        ],
        SemanticImportType::Unit,
    );
    let mismatch = mismatch_fixture
        .analyze(
            r#"fn replace(inout value: Str(8), other: Str(16)) { value = other; }"#,
            "replace",
        )
        .map(|_| ())
        .expect_err("a wider fixed string cannot satisfy Str(8)");
    assert!(
        matches!(
            &mismatch.kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "Str(8)" && found == "Str(16)"
        ),
        "unexpected diagnostic: {mismatch:?}"
    );

    let too_long = fixture
        .analyze(
            r#"fn replace(inout value: Str(8)) { value = "123456789"; }"#,
            "replace",
        )
        .map(|_| ())
        .expect_err("an overlong literal cannot satisfy Str(8)");
    assert!(
        matches!(
            &too_long.kind,
            ErrorKind::StrFixedCapacityExceeded {
                capacity: 8,
                byte_len: 9
            }
        ),
        "unexpected diagnostic: {too_long:?}"
    );
}

// Migrated from `tests::break_path_move_is_moved_after_loop`: a loop's exit
// ownership state is the union of the at-break states (RUE-1293, formal core
// §5.7, (Loop-Break)), so a value moved on a break path is moved after the
// loop. Before the fix the exit state was the fall-through state — which
// never reaches the exit — and this use-after-move was accepted (observable
// double-drop).
#[test]
fn provider_body_break_path_move_is_moved_after_loop() {
    let mut fixture = ProviderFixture::new();
    let non_copy = fixture.declare_struct("NonCopy", vec![("x", SemanticImportType::I32)], false);
    fixture.declare_function(
        "consume",
        vec![value_param("n", SemanticImportType::Nominal(non_copy))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    let error = fixture
        .analyze(
            "fn main() -> i32 {
    let n = NonCopy { x: 1 };
    let mut i = 0;
    loop {
        i = i + 1;
        if i > 0 { let v = consume(n); break; }
    }
    consume(n)
}",
            "main",
        )
        .map(|_| ())
        .expect_err("a break-path move poisons the loop exit state");
    assert!(
        matches!(&error.kind, ErrorKind::UseAfterMove { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

// Migrated from `tests::break_path_move_of_shadow_leaves_outer_owned`: the
// exit join is per-binding, not per-name (RUE-522 × RUE-1293): the break
// snapshot names the loop-local shadow, and pop_scope's restoration replayed
// onto it must resume the outer binding's own state, not poison it with the
// shadow's move.
#[test]
fn provider_body_break_path_move_of_shadow_leaves_outer_owned() {
    let mut fixture = ProviderFixture::new();
    let non_copy = fixture.declare_struct("NonCopy", vec![("x", SemanticImportType::I32)], false);
    fixture.declare_function(
        "consume",
        vec![value_param("n", SemanticImportType::Nominal(non_copy))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    fixture
        .analyze(
            "fn main() -> i32 {
    let n = NonCopy { x: 1 };
    let mut i = 0;
    loop {
        i = i + 1;
        let n = NonCopy { x: 2 };
        if i > 0 { let v = consume(n); break; }
    }
    consume(n)
}",
            "main",
        )
        .expect("outer binding is untouched by the shadow's break-path move");
}

// Migrated from `tests::test_partial_move_sibling_still_valid`: after moving
// one field, sibling fields should still be usable. Inner is non-copy, so
// Outer is also non-copy (it can't be @copy with a non-copy field).
#[test]
fn provider_body_partial_move_leaves_sibling_field_valid() {
    let mut fixture = ProviderFixture::new();
    let inner = fixture.declare_struct("Inner", vec![("x", SemanticImportType::I32)], false);
    fixture.declare_struct(
        "Outer",
        vec![
            ("a", SemanticImportType::Nominal(inner.clone())),
            ("b", SemanticImportType::I32),
        ],
        false,
    );
    fixture.declare_function(
        "consume",
        vec![value_param("i", SemanticImportType::Nominal(inner))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);

    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let o = Outer { a: Inner { x: 1 }, b: 2 };
    let x = consume(o.a);
    o.b
}",
            "main",
        )
        .expect("a partial move leaves the sibling field usable");
    assert_eq!(body.function.air.return_type(), Type::I32);
}

/// `H { a: [P; 2], b: NonCopy }` with `P { c: i64, s: NonCopy }`: an array
/// field of move type beside a sibling field of move type (RUE-2344).
fn dynamic_index_field_array_fixture() -> ProviderFixture {
    let mut fixture = ProviderFixture::new();
    let non_copy = fixture.declare_struct("NonCopy", vec![("x", SemanticImportType::I64)], false);
    let p = fixture.declare_struct(
        "P",
        vec![
            ("c", SemanticImportType::I64),
            ("s", SemanticImportType::Nominal(non_copy.clone())),
        ],
        false,
    );
    fixture.declare_struct(
        "H",
        vec![
            (
                "a",
                SemanticImportType::Array {
                    element: Arc::new(SemanticImportType::Nominal(p)),
                    len: 2,
                },
            ),
            ("b", SemanticImportType::Nominal(non_copy)),
        ],
        false,
    );
    fixture.declare_function(
        "f",
        vec![value_param("i", SemanticImportType::I64)],
        SemanticImportType::I64,
    );
    fixture
}

const DYNAMIC_INDEX_H: &str = "H { a: [P { c: 1, s: NonCopy { x: 10 } }, P { c: 3, s: NonCopy { x: 30 } }], b: NonCopy { x: 50 } }";

fn assert_use_after_move_of(error: &rue_error::CompileError, place: &str) {
    assert!(
        matches!(&error.kind, ErrorKind::UseAfterMove(name) if name == place),
        "expected use of moved value '{place}', got: {error:?}"
    );
}

// RUE-2344: the move path of a place below a dynamic index names the array
// that index selects from. Before the fix the path restarted after the index,
// so `h.a[i].c` was checked as `h.c`: a read through the moved `h.a` passed.
#[test]
fn provider_body_dynamic_index_read_below_moved_field_array_rejected() {
    let fixture = dynamic_index_field_array_fixture();
    let source = format!(
        "fn f(i: i64) -> i64 {{
    let h = {DYNAMIC_INDEX_H};
    let t = h.a;
    h.a[i].c
}}"
    );
    let error = fixture
        .analyze(&source, "f")
        .map(|_| ())
        .expect_err("a read below a dynamic index into a moved array is a use-after-move");
    assert_use_after_move_of(&error, "h.a");
}

// RUE-2344: the write form, which ran the overwrite-drop of a destroyed
// element and leaked the written value.
#[test]
fn provider_body_dynamic_index_write_below_moved_field_array_rejected() {
    let fixture = dynamic_index_field_array_fixture();
    let source = format!(
        "fn f(i: i64) -> i64 {{
    let mut h = {DYNAMIC_INDEX_H};
    let t = h.a;
    h.a[i].s = NonCopy {{ x: 99 }};
    0
}}"
    );
    let error = fixture
        .analyze(&source, "f")
        .map(|_| ())
        .expect_err("a write below a dynamic index into a moved array is a use-after-move");
    assert_use_after_move_of(&error, "h.a");
}

// RUE-2344: `h.a[i].c` is not `h.c`, and `h.a[i].s.x` is not `h.s.x`; a moved
// sibling of the array leaves both the read and the write legal.
#[test]
fn provider_body_dynamic_index_beside_moved_sibling_field_accepted() {
    let fixture = dynamic_index_field_array_fixture();
    let source = format!(
        "fn f(i: i64) -> i64 {{
    let mut h = {DYNAMIC_INDEX_H};
    let t = h.b;
    h.a[i].c = 7;
    h.a[i].c + h.a[i].s.x
}}"
    );
    fixture
        .analyze(&source, "f")
        .expect("a moved sibling field does not affect a place below a dynamic index");
}

fn assert_partially_moved_array(error: &rue_error::CompileError, array: &str) {
    assert!(
        matches!(&error.kind, ErrorKind::AssignToPartiallyMovedArray { array: name } if name == array),
        "expected E0480 naming '{array}', got: {error:?}"
    );
}

// RUE-2341: the partially-moved-array write rule keyed on the root binding
// being an array, so a write into a field-reached array with a moved-out part
// was accepted, ran the moved part's destructor again and leaked the written
// value. It now keys on the outermost array the destination steps into.
#[test]
fn provider_body_dynamic_index_write_into_partially_moved_field_array_rejected() {
    let fixture = dynamic_index_field_array_fixture();
    let source = format!(
        "fn f(i: i64) -> i64 {{
    let mut h = {DYNAMIC_INDEX_H};
    let t = h.a[0].s;
    h.a[i].s = NonCopy {{ x: 99 }};
    0
}}"
    );
    let error = fixture
        .analyze(&source, "f")
        .map(|_| ())
        .expect_err("a write into a field-reached array with a moved-out part is E0480");
    assert_partially_moved_array(&error, "h.a");
}

// RUE-2341: the constant forms, at the moved element and at a sibling one.
#[test]
fn provider_body_constant_index_writes_into_partially_moved_field_array_rejected() {
    let fixture = dynamic_index_field_array_fixture();
    for write in [
        "h.a[0].s = NonCopy { x: 99 };",
        "h.a[0] = P { c: 7, s: NonCopy { x: 99 } };",
        "h.a[1].c = 7;",
    ] {
        let source = format!(
            "fn f(i: i64) -> i64 {{
    let mut h = {DYNAMIC_INDEX_H};
    let t = h.a[0].s;
    {write}
    0
}}"
        );
        let error = fixture
            .analyze(&source, "f")
            .map(|_| ())
            .expect_err("a write into a field-reached array with a moved-out part is E0480");
        assert_partially_moved_array(&error, "h.a");
    }
}

// RUE-2341: `h.a[0].s.x = ...` after `h.a[0].s` moved is both a write under a
// moved place (E0205) and a write into a partially moved array (E0480); the
// array rule runs first.
#[test]
fn provider_body_partially_moved_field_array_rule_precedes_moved_base_rule() {
    let fixture = dynamic_index_field_array_fixture();
    let source = format!(
        "fn f(i: i64) -> i64 {{
    let mut h = {DYNAMIC_INDEX_H};
    let t = h.a[0].s;
    h.a[0].s.x = 99;
    0
}}"
    );
    let error = fixture
        .analyze(&source, "f")
        .map(|_| ())
        .expect_err("a write under a moved part of an array is rejected");
    assert_partially_moved_array(&error, "h.a");
}

// RUE-2341: reinitializing the whole array field is the recovery (7.1:46),
// and afterwards writes into it are legal again.
#[test]
fn provider_body_partially_moved_field_array_whole_reinit_accepted() {
    let fixture = dynamic_index_field_array_fixture();
    let source = format!(
        "fn f(i: i64) -> i64 {{
    let mut h = {DYNAMIC_INDEX_H};
    let t = h.a[0].s;
    h.a = [P {{ c: 5, s: NonCopy {{ x: 50 }} }}, P {{ c: 6, s: NonCopy {{ x: 60 }} }}];
    h.a[i].c = 7;
    h.a[0].c
}}"
    );
    fixture
        .analyze(&source, "f")
        .expect("reinitializing the whole array field re-arms writes into it");
}

/// `H { a: [P; 2], p: P, q: Q }` with `P { c: i64, s: NonCopy }` and
/// `Q { p: P }`, plus by-reference functions and receivers over `P`
/// (RUE-2350).
fn loan_of_partially_moved_place_fixture() -> ProviderFixture {
    let mut fixture = ProviderFixture::new();
    let non_copy = fixture.declare_struct("NonCopy", vec![("x", SemanticImportType::I64)], false);
    let p = fixture.declare_struct(
        "P",
        vec![
            ("c", SemanticImportType::I64),
            ("s", SemanticImportType::Nominal(non_copy)),
        ],
        false,
    );
    let q = fixture.declare_struct(
        "Q",
        vec![("p", SemanticImportType::Nominal(p.clone()))],
        false,
    );
    fixture.declare_struct(
        "H",
        vec![
            (
                "a",
                SemanticImportType::Array {
                    element: Arc::new(SemanticImportType::Nominal(p.clone())),
                    len: 2,
                },
            ),
            ("p", SemanticImportType::Nominal(p.clone())),
            ("q", SemanticImportType::Nominal(q.clone())),
        ],
        false,
    );
    for (name, mode) in [
        ("setp", SemanticParameterMode::Inout),
        ("readp", SemanticParameterMode::Borrow),
    ] {
        fixture.declare_function(
            name,
            vec![mode_param(
                "p",
                SemanticImportType::Nominal(p.clone()),
                mode,
            )],
            SemanticImportType::I64,
        );
    }
    fixture.declare_function(
        "readq",
        vec![mode_param(
            "q",
            SemanticImportType::Nominal(q),
            SemanticParameterMode::Borrow,
        )],
        SemanticImportType::I64,
    );
    for (name, mode) in [
        ("set", SemanticParameterMode::Inout),
        ("get", SemanticParameterMode::Borrow),
    ] {
        fixture.declare_method_with(
            &p,
            name,
            Vec::new(),
            SemanticImportType::I64,
            MethodShape {
                self_mode: mode,
                ..MethodShape::default()
            },
        );
    }
    fixture.declare_function("f", Vec::new(), SemanticImportType::I64);
    fixture
}

const LOAN_H: &str = "H { a: [P { c: 1, s: NonCopy { x: 10 } }, P { c: 2, s: NonCopy { x: 11 } }], p: P { c: 3, s: NonCopy { x: 17 } }, q: Q { p: P { c: 4, s: NonCopy { x: 18 } } } }";

// RUE-2350: a `borrow`/`inout` argument, method receiver or equality operand
// loans the whole place, so a moved part below it is a use of that part
// (core §5.4, 3.8:26). The by-reference path checked only the place and its
// ancestors: a borrow read the destroyed part and an inout write dropped it a
// second time. The diagnostic names the moved sub-place, and its help does
// not suggest a borrow, which is what fails.
#[test]
fn provider_body_loan_of_place_with_moved_part_below_rejected() {
    let fixture = loan_of_partially_moved_place_fixture();
    for (moved, loan) in [
        ("h.p.s", "readp(borrow h.p)"),
        ("h.p.s", "setp(inout h.p)"),
        ("h.a[0].s", "readp(borrow h.a[0])"),
        ("h.a[0].s", "setp(inout h.a[0])"),
        ("h.q.p.s", "readq(borrow h.q)"),
        ("h.a[1].s", "h.a[1].set()"),
        ("h.p.s", "h.p.get()"),
        ("h.a[0].s", "if h.a[0] == h.a[1] { 1 } else { 0 }"),
    ] {
        let source = format!(
            "fn f() -> i64 {{
    let mut h = {LOAN_H};
    let t = {moved};
    {loan}
}}"
        );
        let error = fixture
            .analyze(&source, "f")
            .map(|_| ())
            .expect_err("a loan of a place with a moved part below it is a use-after-move");
        assert_use_after_move_of(&error, moved);
        let helps = &error.diagnostic().helps;
        assert!(
            helps
                .iter()
                .all(|help| !help.to_string().contains("borrow"))
                && helps
                    .iter()
                    .any(|help| help.to_string().contains("reinitialize")),
            "`{loan}`: the help must suggest reinitializing, not borrowing: {helps:?}"
        );
    }
}

// RUE-2350: the controls. A moved sibling (`h.q.p.s` beside `h.p`, `h.a[1].s`
// beside `h.a[0]`) leaves the loan legal, as does reinitializing the moved
// part first (3.8:55), and a Copy leaf beside the hole is still readable.
#[test]
fn provider_body_loan_beside_or_after_reinit_of_moved_part_accepted() {
    let fixture = loan_of_partially_moved_place_fixture();
    for (moved, loan) in [
        ("h.q.p.s", "readp(borrow h.p)"),
        ("h.a[1].s", "setp(inout h.a[0])"),
        ("h.p.s", "h.p.s = NonCopy { x: 99 }; readp(borrow h.p)"),
        ("h.q.p.s", "h.q.p.s = NonCopy { x: 99 }; readq(borrow h.q)"),
        ("h.p.s", "h.p.c"),
    ] {
        let source = format!(
            "fn f() -> i64 {{
    let mut h = {LOAN_H};
    let t = {moved};
    {loan}
}}"
        );
        fixture
            .analyze(&source, "f")
            .unwrap_or_else(|error| panic!("`{loan}` after `{moved}` moved: {error:?}"));
    }
}
