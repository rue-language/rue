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
