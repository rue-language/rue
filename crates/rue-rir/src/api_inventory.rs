//! Structural guardrails for the RIR payload ownership boundary.

fn source_region<'a>(owner: &'a str, start: &str, end: &str) -> &'a str {
    owner
        .split(start)
        .nth(1)
        .and_then(|rest| rest.split(end).next())
        .unwrap_or_else(|| panic!("missing source inventory region {start:?}..{end:?}"))
}

#[test]
fn rir_instruction_layers_keep_their_canonical_owners() {
    let hub = include_str!("inst.rs");
    let payload = include_str!("inst/payload.rs");
    let schema = include_str!("inst/schema.rs");
    let spans = include_str!("inst/spans.rs");
    let validation = include_str!("inst/validation.rs");
    let editor = include_str!("inst/editor.rs");
    let printer = include_str!("inst/printer.rs");

    assert!(hub.contains("mod payload;\nmod printer;"));
    assert!(hub.contains("pub use payload::*;\npub use printer::*;"));
    assert!(
        payload
            .contains("#[path = \"schema.rs\"]\nmod schema;\n#[path = \"spans.rs\"]\nmod spans;")
    );
    assert!(payload.contains("pub use schema::*;\npub use spans::*;"));
    assert!(spans.contains("#[path = \"validation.rs\"]\nmod validation;"));
    assert!(spans.contains("pub use validation::*;"));
    assert!(validation.contains("#[path = \"editor.rs\"]\nmod editor;"));
    assert!(validation.contains("#[path = \"payload_support.rs\"]\nmod payload_support;"));
    assert!(validation.contains("pub use editor::*;"));
    assert!(editor.contains("#[path = \"packed.rs\"]\nmod packed;"));
    assert!(editor.contains("#[path = \"tests.rs\"]\nmod tests;"));
    assert!(!hub.contains("pub enum InstData"));
    assert!(!hub.contains("pub struct RirEditor"));
    assert!(!hub.contains("pub struct ValidatedRir"));
    assert!(!hub.contains("pub struct RirPrinter"));

    assert!(payload.contains("macro_rules! payload_family"));
    assert!(payload.contains("pub struct RirSlice<'a, T>"));
    assert!(schema.contains("pub enum InstData"));
    assert!(spans.contains("fn try_visit_validated_span_slots<E>("));
    assert!(validation.contains("pub struct ValidatedRir(Rir);"));
    assert!(editor.contains("pub struct RirEditor {"));
    assert!(printer.contains("pub struct RirPrinter<'a, 'b>"));

    assert!(editor.contains(
        "pub struct RirEditor {\n    rir: Rir,\n    type_syntax: RirTypeSyntaxBuilder<Spur>,\n}"
    ));

    for non_owner in [hub, payload, schema, spans, editor, printer] {
        assert!(
            !non_owner.contains("pub struct ValidatedRir(Rir);"),
            "ValidatedRir declaration escaped validation.rs"
        );
    }
}

#[test]
fn fn_decl_editor_api_keeps_flags_named() {
    let editor = include_str!("inst/editor.rs");
    let facade = include_str!("lib.rs");
    let flags = source_region(editor, "pub struct FnDeclFlags {", "fn remap_call_args");
    for field in [
        "pub is_pub: bool,",
        "pub is_unchecked: bool,",
        "pub is_extern: bool,",
        "pub is_c_export: bool,",
        "pub is_test: bool,",
        "pub has_self: bool,",
        "pub self_mode: RirParamMode,",
        "pub self_is_mut: bool,",
        "pub returns_borrow: bool,",
        "pub returns_inout: bool,",
    ] {
        assert!(flags.contains(field), "FnDeclFlags omits {field}");
    }
    assert!(editor.contains(
        "#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]\npub struct FnDeclFlags {"
    ));
    assert!(facade.contains("FnDeclFlags, Inst, InstData"));

    let constructor = source_region(
        editor,
        "pub fn add_fn_decl_with_return_modes(",
        ") -> Result<InstRef, RirPayloadBuildError> {",
    );
    assert!(constructor.contains("flags: FnDeclFlags,"));
    assert_eq!(constructor.matches("flags: FnDeclFlags,").count(), 1);
    assert!(
        !constructor.contains(": bool,"),
        "function declaration flags became positional booleans again"
    );

    let _: fn(
        &mut crate::RirEditor,
        &[crate::RirDirective],
        crate::FnDeclFlags,
        lasso::Spur,
        &[crate::RirParam],
        crate::RirTypeSyntaxRef,
        crate::InstRef,
        rue_span::Span,
    ) -> Result<crate::InstRef, crate::RirPayloadBuildError> =
        crate::RirEditor::add_fn_decl_with_return_modes;
}

#[test]
fn rir_payload_storage_and_raw_ranges_stay_owner_private() {
    let owner = include_str!("inst.rs");
    let payload = include_str!("inst/payload.rs");
    let editor = include_str!("inst/editor.rs");
    let producer = include_str!("astgen.rs");
    let facade = include_str!("lib.rs");

    let rir = source_region(owner, "pub struct Rir {", "impl PartialEq for Rir");
    for private_field in [
        "    instructions: Vec<Inst>,",
        "    extra: Vec<u32>,",
        "    type_syntax: RirTypeSyntaxArena<Spur>,",
        "    instruction_limit_exceeded: bool,",
        "    views_validated: bool,",
    ] {
        assert!(
            rir.lines().any(|line| line == private_field),
            "Rir storage field changed identity or visibility: {private_field}"
        );
    }

    assert!(payload.contains(
        "#[repr(C)]\n#[derive(Clone, PartialEq, Eq)]\nstruct PayloadRange<Family> {\n    start: u32,\n    extent: u32,\n    family: PhantomData<fn() -> Family>,\n}"
    ));

    let range = source_region(
        payload,
        "macro_rules! payload_family",
        "pub(crate) trait PayloadFallback",
    );
    assert!(range.contains(
        "        #[repr(transparent)]\n        #[derive(Clone, PartialEq, Eq)]\n        pub struct $name(PayloadRange<$marker>);"
    ));
    assert!(range.contains("\n            const fn from_parts(start: u32, extent: u32) -> Self {"));
    assert!(range.contains("\n            const fn start(&self) -> u32 {"));
    assert!(range.contains("\n            const fn extent(&self) -> u32 {"));

    for raw_api in [
        "pub fn append_payload(",
        "pub fn payload_words(",
        "pub fn get_extra(",
        "pub fn extra_mut(",
    ] {
        assert!(
            !payload.contains(raw_api),
            "RIR exposed raw payload API: {raw_api}"
        );
        assert!(
            !facade.contains(raw_api),
            "RIR facade exposed raw payload API: {raw_api}"
        );
    }

    assert_eq!(payload.matches("payload_family!(").count(), 17);
    assert_eq!(crate::RIR_PAYLOAD_FAMILY_NAMES.len(), 17);
    assert!(editor.contains("fn atomic<T>("));
    assert!(editor.contains("self.rir.instructions.truncate(instruction_len)"));
    assert!(editor.contains("self.rir.extra.truncate(extra_len)"));
    assert!(!producer.contains(".extra["));
    assert!(!producer.contains(".extra."));
    assert!(!producer.contains("from_parts("));
}

#[test]
fn rir_payload_schema_and_decoders_have_one_owner() {
    let payload = include_str!("inst/payload.rs");
    let schema = include_str!("inst/schema.rs");
    let spans = include_str!("inst/spans.rs");
    let validation = include_str!("inst/validation.rs");
    let editor = include_str!("inst/editor.rs");
    let printer = include_str!("inst/printer.rs");
    let packed = include_str!("inst/packed.rs");

    for identity in [
        "const REF_SCHEMA: FixedPayloadSchema",
        "const SYMBOL_SCHEMA: FixedPayloadSchema",
        "const CALL_ARG_SCHEMA: FixedPayloadSchema",
        "const PARAM_SCHEMA: FixedPayloadSchema",
        "const FIELD_INIT_SCHEMA: FixedPayloadSchema",
        "const FIELD_DECL_SCHEMA: FixedPayloadSchema",
        "pub enum PatternKind",
        "fn decoded_match_record_extent(",
        "fn decoded_directive_record_extent(",
        "fn decode_match_record(",
        "fn decode_directive_record(",
        "fn enum_payload_record(",
    ] {
        assert!(payload.contains(identity), "payload owner omits {identity}");
        for non_owner in [schema, spans, validation, editor, printer, packed] {
            assert!(
                !non_owner.contains(identity),
                "payload schema identity {identity} escaped payload.rs"
            );
        }
    }

    for (owner, representation) in [
        (
            schema,
            "#[repr(u32)]\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]\npub enum RirParamMode {",
        ),
        (
            schema,
            "#[repr(u32)]\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]\npub enum RirArgMode {",
        ),
        (
            payload,
            "#[repr(u32)]\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub enum PatternKind {",
        ),
    ] {
        assert!(
            owner.contains(representation),
            "encoded payload representation changed: {representation}"
        );
    }
}

#[test]
fn rir_span_storage_is_tied_to_the_canonical_slot_visitor() {
    let schema = include_str!("inst/schema.rs");
    let spans = include_str!("inst/spans.rs");
    let editor = include_str!("inst/editor.rs");

    assert!(spans.contains(
        "pub struct RirSpanSlot {\n    instruction: InstRef,\n    field: RirSpanField,\n}"
    ));
    assert!(spans.contains(
        "\n    const fn new(instruction: InstRef, field: RirSpanField) -> Self {\n        Self { instruction, field }\n    }"
    ));

    let directive = source_region(schema, "pub struct RirDirective {", "pub enum RirParamMode");
    assert_eq!(
        directive.matches("Span").count(),
        2,
        "directive span schema changed"
    );
    assert!(directive.contains("pub span: Span"));

    let parameter = source_region(schema, "pub struct RirParam {", "pub enum RirArgMode");
    assert_eq!(
        parameter.matches("Span").count(),
        2,
        "parameter span schema changed"
    );
    assert!(parameter.contains("pub span: Span"));

    let pattern = source_region(schema, "pub enum RirPattern {", "impl RirPattern");
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

    let instruction = source_region(schema, "pub struct Inst {", "pub enum RepeatCount");
    assert_eq!(
        instruction.matches("Span").count(),
        1,
        "instruction span schema changed"
    );
    assert!(instruction.contains("pub span: Span"));

    let instruction_data = source_region(schema, "pub enum InstData {", "\n}");
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

    let visitor = source_region(
        spans,
        "\n    fn try_visit_validated_span_slots<E>(",
        "\n    fn try_rewrite_validated_span_slots<E>(",
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

    let validation = include_str!("inst/validation.rs");
    let public_visitor = source_region(
        validation,
        "\n    pub fn try_visit_span_slots<E>(",
        "/// Consume this validated owner and rewrite every canonical span slot",
    );
    assert!(public_visitor.contains("self.0.try_visit_validated_span_slots(checkpoint, visit)"));

    let remapper = source_region(
        editor,
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
    let schema = include_str!("inst/schema.rs");
    let validation = include_str!("inst/validation.rs");
    let instruction_data = source_region(schema, "pub enum InstData {", "\n}");
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
    assert!(
        !include_str!("lib.rs").contains("RirDeferredStructuralAnchor,"),
        "producer-private deferred anchors must not enter the public hub"
    );

    let charge = source_region(
        validation,
        "pub fn retained_allocation_charge(&self) -> u64 {",
        "impl std::ops::Deref for ValidatedRir",
    );
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

#[test]
fn fixed_payload_views_keep_the_validated_boundary_and_direct_indexing() {
    let payload = include_str!("inst/payload.rs");
    let validation = include_str!("inst/validation.rs");
    let printer = include_str!("inst/printer.rs");
    let packed = include_str!("inst/packed.rs");

    let slice = source_region(
        payload,
        "impl<'a, T: 'a> RirSlice<'a, T> {",
        "impl<'a, T> IntoIterator for RirSlice",
    );
    let get = slice
        .split("pub fn get(&self, index: usize) -> Option<T> {")
        .nth(1)
        .and_then(|rest| rest.split("\n    }").next())
        .expect("RirSlice::get implementation");
    assert!(get.contains("checked_mul"));
    assert!(get.contains("checked_add"));
    assert!(!get.contains("nth"));

    for accessor in [
        "fn ref_view<R>(",
        "fn call_arg_view<R>(",
        "pub fn params(",
        "pub fn field_inits(",
        "fn field_decl_view<R>(",
        "fn symbol_view<R>(",
    ] {
        let body = payload
            .split(accessor)
            .nth(1)
            .and_then(|rest| rest.split("\n    }").next())
            .unwrap_or_else(|| panic!("missing fixed accessor {accessor}"));
        assert!(
            body.contains("fixed_view"),
            "{accessor} bypasses fixed_view"
        );
    }

    let match_decoder = payload
        .split("fn decode_match_record(")
        .nth(1)
        .and_then(|rest| rest.split("fn decode_directive_record(").next())
        .expect("match decoder");
    let match_bindings = match_decoder
        .split("x if x == PatternKind::Path")
        .nth(1)
        .expect("path-pattern binding decoder");
    assert!(match_bindings.contains("validated"));
    assert!(match_bindings.contains("RirSlice::new_validated"));
    assert!(match_bindings.contains("RirSlice::new_unvalidated"));

    let directive_decoder = payload
        .split("fn decode_directive_record(")
        .nth(1)
        .and_then(|rest| rest.split("/// Stored representation of directive").next())
        .expect("directive decoder");
    assert!(directive_decoder.contains("validated"));
    assert!(directive_decoder.contains("RirSlice::new_validated"));
    assert!(directive_decoder.contains("RirSlice::new_unvalidated"));

    let enum_decoder = payload
        .split("impl<'a> Iterator for RirEnumPayloads")
        .nth(1)
        .and_then(|rest| rest.split("impl ExactSizeIterator").next())
        .expect("enum payload decoder");
    assert!(enum_decoder.contains("validated"));
    assert!(enum_decoder.contains("RirSlice::new_validated"));
    assert!(enum_decoder.contains("RirSlice::new_unvalidated"));

    for accessor in ["pub fn match_arms(", "pub fn directives("] {
        let body = payload
            .split(accessor)
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n\n    pub fn").next())
            .unwrap_or_else(|| panic!("missing variable accessor {accessor}"));
        assert!(
            body.contains("validated: self.views_validated"),
            "{accessor} does not propagate validation state"
        );
    }

    let enum_view = payload
        .split("fn enum_payload_view<'a, R>(")
        .nth(1)
        .and_then(|rest| rest.split("    pub fn enum_payloads(").next())
        .expect("enum payload view");
    assert!(enum_view.contains("validated: self.views_validated"));

    let publication = source_region(
        validation,
        "pub fn finish(",
        "/// Visit every span-bearing RIR slot",
    );
    assert!(publication.contains("rir.validate_payloads()?"));
    assert!(publication.contains("rir.validate_context(context)?"));
    assert!(publication.contains("Self::from_prevalidated(rir)"));

    let validated_boundary = source_region(
        validation,
        "\n    fn from_prevalidated(mut rir: Rir) -> Self {",
        "/// Consume and validate an editor",
    );
    assert!(validated_boundary.contains("rir.views_validated = true"));
    assert!(validated_boundary.contains("Self(rir)"));
    assert_eq!(validation.matches("views_validated = true").count(), 1);
    assert!(!packed.contains("views_validated = true"));
    assert!(!packed.contains("ValidatedRir(rir)"));
    assert!(packed.contains("ValidatedRir::from_prevalidated(rir)"));

    assert!(!printer.contains(".extra["));
    assert!(!printer.contains(".extra."));
    assert!(!printer.contains("payload_words("));
    assert!(printer.contains("self.rir.anon_struct_methods(methods)"));
}
