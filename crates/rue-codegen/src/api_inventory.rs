//! Guard the validated CFG boundary at public backend entry points.

#[test]
fn production_generate_entry_points_require_validated_cfg() {
    let contract = include_str!("backend.rs");
    assert!(contract.contains("pub(crate) trait Backend"));
    for required in [
        "type Mir;",
        "type Reg;",
        "const ARCH",
        "const ARG_REG_COUNT",
        "const RETURN_REG_COUNT",
        "fn lower(",
        "fn allocate(",
        "fn peephole(",
        "fn schedule(",
        "fn verify(",
        "fn referenced_string_ids(",
        "fn remap_string_ids(",
        "fn emit(",
    ] {
        assert!(
            contract.contains(required),
            "Backend contract lost {required}"
        );
    }

    for backend in [
        include_str!("x86_64/mod.rs"),
        include_str!("aarch64/mod.rs"),
    ] {
        assert!(backend.contains("use crate::backend::Backend;"));
        assert!(backend.contains("impl Backend for"));
        assert!(backend.contains("const ARCH: rue_target::Arch"));
        assert!(backend.contains("generate_with_backend::<"));
        for removed in ["fn generate_inner(", "fn prepare_backend_with_artifacts("] {
            assert!(
                !backend.contains(removed),
                "backend-local orchestration helper returned: {removed}"
            );
        }
        for entry_point in [
            "generate",
            "generate_with_symbols",
            "generate_with_symbols_and_atoms",
            "generate_product_with_symbols_and_atoms",
        ] {
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
            let target = signature
                .find("target: Target")
                .or_else(|| signature.find("target: rue_target::Target"));
            assert!(
                target.is_some(),
                "{entry_point} must carry the target in both backends"
            );
            assert!(signature.find("interner:").unwrap() < target.unwrap());
        }
        for removed in [
            "pub fn generate_with_asm(",
            "pub fn generate_regalloc_info(",
        ] {
            assert!(
                !backend.contains(removed),
                "presentation-only backend entry point returned: {removed}"
            );
        }
    }

    let root = include_str!("lib.rs");
    for removed in [
        "pub use x86_64::generate;",
        "pub use x86_64::{Operand, Reg, X86Inst, X86Mir};",
    ] {
        assert!(
            !root.contains(removed),
            "crate-root x86 facade returned: {removed}"
        );
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

#[test]
fn production_codegen_does_not_call_frozen_declaration_test_adapters() {
    for (name, source) in [
        ("x86_64/cfg_lower", include_str!("x86_64/cfg_lower.rs")),
        ("aarch64/cfg_lower", include_str!("aarch64/cfg_lower.rs")),
        ("place_lower", include_str!("place_lower.rs")),
        ("types", include_str!("types.rs")),
        ("stack_frame", include_str!("stack_frame.rs")),
    ] {
        let production = source
            .split("\n#[cfg(test)]\nmod ")
            .next()
            .expect("codegen production prefix");
        for adapter in [
            ".predeclare_declaration_shells_for_test()",
            ".bind_declarations_for_test()",
            ".analyze_all_for_test()",
            ".resolve_declarations_for_test()",
            ".resolve_declarations_with_work_for_test()",
        ] {
            assert!(
                !production.contains(adapter),
                "production codegen module {name} called frozen declaration test adapter {adapter}"
            );
        }
    }
}

#[test]
fn intrinsic_codegen_dispatch_is_typed_and_has_no_name_fallback() {
    let value_plan = include_str!("value_plan.rs");
    assert!(value_plan.contains("let operation = *operation;"));
    assert!(value_plan.contains("operation == IntrinsicOperation::BitCast"));
    assert!(!value_plan.contains("pub enum IntrinsicOperation"));
    for (name, source) in [
        ("value_plan", value_plan),
        ("x86_64/cfg_lower", include_str!("x86_64/cfg_lower.rs")),
        ("aarch64/cfg_lower", include_str!("aarch64/cfg_lower.rs")),
        ("local_storage", include_str!("local_storage.rs")),
        ("types", include_str!("types.rs")),
        ("place_lower", include_str!("place_lower.rs")),
        ("stack_frame", include_str!("stack_frame.rs")),
        ("cfg_lower", include_str!("cfg_lower.rs")),
    ] {
        for forbidden in [
            "IntrinsicSelector",
            "IntrinsicKind",
            "resolve_intrinsic_symbol",
            "expected_spelling",
            "intrinsic_operation_from_name",
            "unsupported intrinsic",
            "match self.interner.resolve(&name)",
            "match interner.resolve(&name)",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} contains removed intrinsic name dispatch: {forbidden}"
            );
        }
    }
    assert!(value_plan.contains("pub operation: IntrinsicOperation"));
    assert!(value_plan.contains("operation.runtime_call_kind()"));
    assert!(value_plan.contains("match operation {"));
    for forbidden in [
        "if values.len() == 0",
        "if values.len() == 1",
        "if args.len() == 0",
        "if args.len() == 1",
        "match values.len()",
        "match args.len()",
    ] {
        assert!(
            !value_plan.contains(forbidden),
            "value planning regained call-shape intrinsic selection: {forbidden}"
        );
    }
}

#[test]
fn value_planning_uses_the_air_integer_semantics_kernel() {
    let source = include_str!("value_plan.rs");
    assert!(source.contains("ty.integer_semantics().map(Into::into)"));
    assert!(source.contains("IntegerType::new"));
    assert!(source.contains(".shift_count_mask()"));
    assert!(!source.contains("TypeKind::I8 | TypeKind::U8 => 8"));
    assert!(!source.contains("(8, true) => (i8::MIN"));
    assert!(!source.contains("type_bits(ty) - 1"));
}

#[test]
fn foreign_call_and_mir_state_have_one_shared_authority() {
    let foreign = include_str!("foreign_call.rs");
    assert!(foreign.contains("pub(crate) struct ForeignCallPlan"));
    assert!(foreign.contains("pub(crate) trait ForeignCallLoweringBackend"));
    assert!(foreign.contains("pub(crate) fn lower_foreign_call<B: ForeignCallLoweringBackend>"));
    assert!(foreign.contains("backend.foreign_reserve_sret"));
    assert!(foreign.contains("backend.foreign_emit_stack_args"));
    assert!(foreign.contains("backend.foreign_cleanup_byref"));
    assert!(foreign.contains("backend.foreign_register_result"));
    let driver = foreign
        .split("pub(crate) fn lower_foreign_call<B: ForeignCallLoweringBackend>")
        .nth(1)
        .expect("shared foreign-call driver");
    let event_order = [
        "foreign_reserve_sret",
        "foreign_emit_stack_args",
        "foreign_emit_register_args",
        "foreign_assign_sret",
        "foreign_issue_call",
        "foreign_cleanup_stack",
        "foreign_cleanup_byref",
        "foreign_zero_result",
        "foreign_scalar_result",
        "foreign_register_result",
        "foreign_sret_result",
    ];
    let mut previous = 0;
    for event in event_order {
        let offset = driver
            .find(event)
            .unwrap_or_else(|| panic!("driver event {event}"));
        assert!(offset >= previous, "driver event order changed at {event}");
        previous = offset;
    }

    for lowering in [
        include_str!("x86_64/cfg_lower.rs"),
        include_str!("aarch64/cfg_lower.rs"),
    ] {
        let production = lowering
            .split("\n#[cfg(test)]\nmod ")
            .next()
            .expect("foreign lowering production prefix");
        assert_eq!(
            production.matches("fn emit_foreign_call(").count(),
            1,
            "each backend must expose exactly one foreign-call adapter"
        );
        let emit_start = production
            .find("fn emit_foreign_call(")
            .expect("foreign-call adapter signature");
        let emit_end = production[emit_start..]
            .find("fn emit_runtime_call(")
            .map(|offset| emit_start + offset)
            .expect("foreign-call adapter terminator");
        let emit = &production[emit_start..emit_end];
        let compact = |source: &str| source.split_whitespace().collect::<String>();
        let expected_emit = concat!(
            "fn emit_foreign_call(\n",
            "    &mut self,\n",
            "    inputs: crate::foreign_call::ForeignCallInputs,\n",
            "    result: VReg,\n",
            ") -> crate::value_plan::ValueResult {\n",
            "    crate::value_plan::ValueResult::Materialized(",
            "crate::foreign_call::lower_foreign_call(self, inputs, result,)",
            ")\n",
            "}\n"
        );
        assert_eq!(
            compact(emit),
            compact(expected_emit),
            "backend emit_foreign_call must remain an exact shared-driver wrapper"
        );
        assert_eq!(
            emit.matches("crate::foreign_call::lower_foreign_call(")
                .count(),
            1,
            "the backend adapter must directly delegate to the shared driver"
        );
        for forbidden in [
            "ForeignArg",
            "ForeignReturn",
            "ForeignCallPlan",
            "TargetCCallAbi",
            "ForeignArgPlacement",
            "used_registers",
            "register_budget",
            "stack_cells",
            "int_ops",
            "stack_ops",
        ] {
            assert!(
                !emit.contains(forbidden),
                "backend emit_foreign_call contains shared foreign-call sequencing or placement: {forbidden}"
            );
        }
        assert!(
            !production.contains("fn lower_foreign_call("),
            "backend must not define a local foreign-call sequencer"
        );
        for forbidden in ["ForeignArg", "ForeignReturn", "TargetCCallAbi"] {
            assert!(
                !production.contains(forbidden),
                "backend production source must not own foreign-call classification: {forbidden}"
            );
        }
        let adapter = production
            .split("impl crate::foreign_call::ForeignCallLoweringBackend")
            .next()
            .expect("foreign lowering adapter split");
        assert!(adapter.contains("crate::foreign_call::lower_foreign_call("));
    }

    let state = include_str!("vreg.rs");
    assert!(state.contains("pub struct MirState"));
    for mir in [
        include_str!("x86_64/mir.rs"),
        include_str!("aarch64/mir.rs"),
    ] {
        assert!(mir.contains("state: MirState"));
        assert!(!mir.contains("symbol_index: AHashMap"));
        assert!(!mir.contains("next_vreg: u32"));
    }
}
