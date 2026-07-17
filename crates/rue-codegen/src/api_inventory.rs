//! Guard the validated CFG boundary at public backend entry points.

#[test]
fn production_generate_entry_points_require_validated_cfg() {
    for backend in [
        include_str!("x86_64/mod.rs"),
        include_str!("aarch64/mod.rs"),
    ] {
        for entry_point in ["generate", "generate_with_asm", "generate_regalloc_info"] {
            let signature = backend
                .split(&format!("pub fn {entry_point}("))
                .nth(1)
                .and_then(|rest| rest.split(')').next())
                .unwrap_or_else(|| panic!("backend {entry_point} signature"));
            assert!(
                signature.contains("cfg: &ValidatedCfg"),
                "{entry_point} accepts an unvalidated CFG"
            );
            assert!(!signature.contains("cfg: &Cfg,"));
        }
    }

    for lowering in [
        include_str!("x86_64/cfg_lower.rs"),
        include_str!("aarch64/cfg_lower.rs"),
    ] {
        let constructor = lowering
            .split("pub fn new(")
            .nth(1)
            .and_then(|rest| rest.split(") -> Self").next())
            .expect("public CFG lowering constructor");
        assert!(constructor.contains("cfg: &'a ValidatedCfg"));
        assert!(!constructor.contains("cfg: &'a Cfg"));
    }

    let shared = include_str!("cfg_lower.rs");
    assert!(shared.contains("pub(crate) struct CfgLowerContext<'a>"));
    assert!(!shared.contains("pub struct CfgLowerContext<'a>"));
    assert!(!shared.contains("pub cfg: &'a Cfg"));
    for planning in [
        include_str!("value_plan.rs"),
        include_str!("terminator_plan.rs"),
    ] {
        for raw_entry in [
            "pub fn for_value(",
            "pub fn lower_value",
            "pub fn by_ref_param_slots(",
            "pub fn plan_terminator",
            "pub fn lower_cfg",
        ] {
            assert!(
                !planning.contains(raw_entry),
                "public raw planning entry: {raw_entry}"
            );
        }
    }
}
