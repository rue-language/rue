//! Structural guardrails for the CFG payload ownership boundary.

#[test]
fn cfg_payload_stores_and_codegen_boundary_stay_typed() {
    let owner = include_str!("inst.rs");
    let schemas = include_str!("payload.rs");
    let facade = include_str!("lib.rs");

    let cfg = owner
        .split("pub struct Cfg {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("CFG owner declaration");
    for store in [
        "values",
        "extra",
        "call_args",
        "switch_cases",
        "projections",
    ] {
        assert!(
            !cfg.contains(&format!("pub {store}:")),
            "CFG exposed {store} store"
        );
    }

    let family = schemas
        .split("macro_rules! family")
        .nth(1)
        .and_then(|rest| rest.split("family!(\n    CfgIntrinsicArgs").next())
        .expect("CFG payload-family declaration");
    assert!(family.contains("pub(crate) struct $name"));
    assert!(!family.contains("pub struct $name"));
    assert!(!facade.contains("CfgIntrinsicArgs"));
    assert_eq!(schemas.matches("family!(").count(), 10);
    assert_eq!(crate::CFG_PAYLOAD_FAMILY_NAMES.len(), 10);

    for raw_api in [
        "pub fn payload_store_mut(",
        "pub fn values_mut(",
        "pub fn call_args_mut(",
        "pub fn switch_cases_mut(",
        "pub fn projections_mut(",
    ] {
        assert!(
            !owner.contains(raw_api),
            "CFG exposed raw payload API: {raw_api}"
        );
    }
}

#[test]
fn production_cfg_does_not_call_frozen_declaration_test_adapters() {
    let source = include_str!("build.rs");
    let production = source
        .split("\n#[cfg(test)]\nmod ")
        .next()
        .expect("CFG production prefix");
    for adapter in [
        ".predeclare_declaration_shells_for_test()",
        ".bind_declarations_for_test()",
        ".analyze_all_for_test()",
        ".resolve_declarations_for_test()",
        ".resolve_declarations_with_work_for_test()",
    ] {
        assert!(
            !production.contains(adapter),
            "production CFG called frozen declaration test adapter {adapter}"
        );
    }
}

#[test]
fn cfg_uses_air_synthetic_type_identity_policy() {
    for (name, source) in [
        ("build.rs", include_str!("build.rs")),
        ("verify.rs", include_str!("verify.rs")),
    ] {
        for peer in [
            ".strip_prefix(\"Str(\")",
            ".starts_with(\"Str(\")",
            ".starts_with('[')",
        ] {
            assert!(
                !source.contains(peer),
                "{name} regained handwritten synthetic-type identity policy: {peer}"
            );
        }
    }
}

#[test]
fn cfg_abi_width_checks_have_one_frozen_pool_authority() {
    let source = include_str!("verify.rs");
    assert_eq!(
        source.matches("    fn abi_slot_count(").count(),
        1,
        "CFG verifier must have exactly one ABI-width decision helper"
    );
    assert_eq!(
        source.matches("pool.try_abi_slot_count(ty)").count(),
        1,
        "CFG verifier must contain exactly one frozen-pool ABI-width query"
    );
    assert!(source.contains("fn verify_slot_ranges_consume_the_frozen_pool_authority()"));
    let width_check = source
        .split("    fn abi_slot_count(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("CFG verifier ABI-width helper");
    assert!(width_check.contains("pool.try_abi_slot_count(ty)"));
    for local_authority in [
        "abi_slot_cache",
        "try_struct_def(struct_id)",
        "try_array_def(array_id)",
        "try_enum_def(enum_id)",
        "saturating_add(self.abi_slot_count",
        "compute_abi_slot_count",
    ] {
        assert!(
            !width_check.contains(local_authority),
            "CFG verifier regained a local ABI-width authority: {local_authority}"
        );
    }
}

#[test]
fn slot_write_classification_has_one_owner() {
    // The RUE-521 knowledge of which local slots may be store-to-load
    // forwarded (write counting, address escapes, by-ref call arguments,
    // projected writes, the RUE-194 out-of-range skip) is owned by
    // opt/slot_facts.rs. Its consumers must go through the shared
    // classifier, not restate the scan.
    let owner = include_str!("opt/slot_facts.rs");
    assert!(owner.contains("pub(super) fn classify_slot_writes("));
    assert!(owner.contains("enum SlotWrites"));

    for (name, source) in [
        ("opt/constopt.rs", include_str!("opt/constopt.rs")),
        ("opt/forward.rs", include_str!("opt/forward.rs")),
    ] {
        assert!(
            source.contains("slot_facts::classify_slot_writes("),
            "{name} no longer consumes the shared slot-write classifier"
        );
        for reimpl in [
            "enum SlotWrite",
            "enum SlotClass",
            "fn record_write(",
            "fn classify_slot_writes(",
        ] {
            assert!(
                !source.contains(reimpl),
                "{name} regained a local slot-write classification: {reimpl}"
            );
        }
    }
}

#[test]
fn constant_folding_uses_the_air_integer_semantics_kernel() {
    let source = include_str!("opt/constfold.rs");
    assert!(source.contains("integer_semantics()"));
    assert!(source.contains("compare_u64"));
    for helper in [
        "fn type_bits(",
        "fn is_signed(",
        "fn sign_extend(",
        "fn fits_in_signed_type(",
        "fn fits_in_unsigned_type(",
    ] {
        assert!(
            !source.contains(helper),
            "const folding regained local helper {helper}"
        );
    }
}

#[test]
fn dce_liveness_roots_only_reachable_block_instructions() {
    let source = include_str!("opt/dce.rs");
    let production = source
        .split("\n#[cfg(test)]\nmod tests")
        .next()
        .expect("DCE production prefix");
    let liveness = production
        .split_once("fn compute_live_values(")
        .and_then(|(_, rest)| rest.split_once("/// Check whether an instruction"))
        .map(|(body, _)| body)
        .expect("DCE liveness helper");

    assert!(liveness.contains("for block in cfg.blocks()"));
    assert!(liveness.contains("reachable.contains(block.id.as_u32())"));
    assert!(
        !liveness.contains("0..cfg.value_count()"),
        "DCE liveness must not seed roots from the whole value arena"
    );
}

#[test]
fn validated_cfg_consuming_editor_conversion_does_not_copy_payloads() {
    let owner = include_str!("inst.rs");
    let conversion = owner
        .split_once("pub fn into_editor(self) -> CfgEditor {")
        .expect("validated CFG consuming editor conversion")
        .1
        .split_once("    }")
        .unwrap()
        .0;
    assert!(conversion.contains("self.0"));
    assert!(!conversion.contains("clone("));
}
