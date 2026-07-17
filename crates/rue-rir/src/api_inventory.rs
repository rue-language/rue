//! Structural guardrails for the RIR payload ownership boundary.

#[test]
fn rir_payload_storage_and_raw_ranges_stay_owner_private() {
    let owner = include_str!("inst.rs");
    let producer = include_str!("astgen.rs");
    let facade = include_str!("lib.rs");

    let rir = owner
        .split("pub struct Rir {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("RIR owner declaration");
    assert!(rir.contains("instructions: Vec<Inst>"));
    assert!(rir.contains("extra: Vec<u32>"));
    assert!(!rir.contains("pub instructions"));
    assert!(!rir.contains("pub extra"));

    let range = owner
        .split("macro_rules! payload_family")
        .nth(1)
        .and_then(|rest| rest.split("pub(crate) trait PayloadFallback").next())
        .expect("RIR payload-family declaration");
    assert!(range.contains("const fn from_parts("));
    assert!(!range.contains("pub fn from_parts("));
    assert!(!range.contains("pub const fn from_parts("));
    assert!(!range.contains("pub start"));
    assert!(!range.contains("pub extent"));
    assert!(range.contains("#[derive(PartialEq, Eq)]\n        pub struct $name"));
    assert!(!range.contains("#[derive(Clone, Copy, PartialEq, Eq)]\n        pub struct $name"));

    for raw_api in [
        "pub fn append_payload(",
        "pub fn payload_words(",
        "pub fn get_extra(",
        "pub fn extra_mut(",
    ] {
        assert!(
            !owner.contains(raw_api),
            "RIR exposed raw payload API: {raw_api}"
        );
        assert!(
            !facade.contains(raw_api),
            "RIR facade exposed raw payload API: {raw_api}"
        );
    }

    assert_eq!(owner.matches("payload_family!(").count(), 17);
    assert_eq!(crate::RIR_PAYLOAD_FAMILY_NAMES.len(), 17);
    assert!(!producer.contains(".extra["));
    assert!(!producer.contains(".extra."));
    assert!(!producer.contains("from_parts("));
}
