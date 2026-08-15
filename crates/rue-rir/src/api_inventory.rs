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
    assert!(range.contains("#[derive(Clone, PartialEq, Eq)]\n        pub struct $name"));
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

#[test]
fn rir_span_storage_is_tied_to_the_canonical_slot_visitor() {
    let owner = include_str!("inst.rs");

    let declaration = |start: &str, end: &str| {
        owner
            .split(start)
            .nth(1)
            .and_then(|rest| rest.split(end).next())
            .unwrap_or_else(|| panic!("missing source inventory region {start:?}..{end:?}"))
    };

    let directive = declaration("pub struct RirDirective {", "pub enum RirParamMode");
    assert_eq!(
        directive.matches("Span").count(),
        2,
        "directive span schema changed"
    );
    assert!(directive.contains("pub span: Span"));

    let parameter = declaration("pub struct RirParam {", "pub enum RirArgMode");
    assert_eq!(
        parameter.matches("Span").count(),
        2,
        "parameter span schema changed"
    );
    assert!(parameter.contains("pub span: Span"));

    let pattern = declaration("pub enum RirPattern {", "impl RirPattern");
    assert_eq!(
        pattern.matches("Span").count(),
        6,
        "pattern span schema changed"
    );
    for shape in [
        "Wildcard(Span)",
        "span: Span",
        "Bool(bool, Span)",
        "span: Span",
    ] {
        assert!(pattern.contains(shape));
    }

    let instruction = declaration("pub struct Inst {", "pub enum RepeatCount");
    assert_eq!(
        instruction.matches("Span").count(),
        1,
        "instruction span schema changed"
    );
    assert!(instruction.contains("pub span: Span"));

    let instruction_data = declaration("pub enum InstData {", "impl fmt::Display for InstRef");
    assert_eq!(
        instruction_data.matches("Span").count(),
        2,
        "embedded instruction span schema changed"
    );
    assert!(instruction_data.contains("shorthand_span: Option<Span>"));

    assert_eq!(
        crate::RIR_SPAN_FIELD_FAMILY_NAMES,
        [
            "instruction",
            "directive",
            "parameter",
            "match pattern",
            "struct-init shorthand",
        ]
    );

    let visitor = declaration(
        "fn try_visit_validated_span_slots<E>(",
        "/// Add an instruction and return its reference.",
    );
    for tag in [
        "RirSpanField::Instruction",
        "RirSpanField::MatchPattern",
        "RirSpanField::FunctionDirective",
        "RirSpanField::FunctionParameter",
        "RirSpanField::ConstDirective",
        "RirSpanField::AllocDirective",
        "RirSpanField::StructDirective",
        "RirSpanField::StructInitShorthand",
    ] {
        assert!(visitor.contains(tag), "canonical visitor omits {tag}");
    }

    let remapper = declaration(
        "pub fn try_append_remapped_with_span_slots<E>(",
        "/// Atomically replace an instruction with a compiler-internal intrinsic.",
    );
    for tag in [
        "RirSpanField::Instruction",
        "RirSpanField::MatchPattern",
        "RirSpanField::FunctionDirective",
        "RirSpanField::FunctionParameter",
        "RirSpanField::ConstDirective",
        "RirSpanField::AllocDirective",
        "RirSpanField::StructDirective",
        "RirSpanField::StructInitShorthand",
    ] {
        assert!(remapper.contains(tag), "slot-aware remapper omits {tag}");
    }
}

#[test]
fn rir_structural_anchor_storage_is_tied_to_retained_charge() {
    let owner = include_str!("inst.rs");
    let instruction_data = owner
        .split("pub enum InstData {")
        .nth(1)
        .and_then(|rest| rest.split("impl fmt::Display for InstRef").next())
        .expect("InstData declaration");
    assert_eq!(
        instruction_data
            .matches("anchor: RirStructuralAnchor")
            .count(),
        3,
        "direct structural-anchor storage changed"
    );
    assert_eq!(
        instruction_data
            .matches("anchor: Option<RirStructuralAnchor>")
            .count(),
        1,
        "optional structural-anchor storage changed"
    );

    let charge = owner
        .split("pub fn retained_allocation_charge(&self) -> u64 {")
        .nth(1)
        .and_then(|rest| rest.split("impl std::ops::Deref for ValidatedRir").next())
        .expect("ValidatedRir retained allocation charge");
    for variant in [
        "InstData::StringConst",
        "InstData::VarRef",
        "InstData::AnonStructType",
        "InstData::AnonEnumType",
    ] {
        assert!(
            charge.contains(variant),
            "retained charge omits anchor-bearing {variant}"
        );
    }
    assert!(
        charge.contains("type_syntax().retained_allocation_charge()"),
        "retained charge omits the structured type-syntax arena"
    );
}
