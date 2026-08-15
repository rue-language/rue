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
