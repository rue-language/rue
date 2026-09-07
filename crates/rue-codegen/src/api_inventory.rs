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
fn integer_cast_and_arithmetic_post_op_policy_live_only_in_the_value_plan() {
    // A cast's range check and an arithmetic result's overflow/re-narrowing
    // rule are language semantics, not target encodings. Each backend used to
    // re-derive them from `(bits, signed)` — AArch64 in three copies — so a
    // width fixed in one table and missed in another silently gave the two
    // targets different arithmetic (RUE-31, RUE-647, RUE-1982).
    let value_plan = include_str!("value_plan.rs");
    for owner in [
        "pub enum IntCastCheckPlan",
        "pub fn int_cast_check_plan(",
        "pub enum PostOpPolicy",
        "pub enum SubWordCheck",
        "pub fn post_op_policy(",
        "pub fn width_extension(",
        "pub fn narrowing_extension(",
    ] {
        assert!(
            value_plan.contains(owner),
            "integer policy owner lost {owner}"
        );
    }
    // The plan asks AIR whether a value is representable rather than comparing
    // bit counts, so the cast the backends compile is the cast semantic
    // analysis and constant folding accept.
    assert!(value_plan.contains("target.fits_i128(source.min_i128())"));

    for (name, source) in [
        ("x86_64/cfg_lower", include_str!("x86_64/cfg_lower.rs")),
        ("aarch64/cfg_lower", include_str!("aarch64/cfg_lower.rs")),
    ] {
        let production = source
            .split("\n#[cfg(test)]\nmod ")
            .next()
            .expect("codegen production prefix");
        for removed in [
            "fn emit_subword_narrow(",
            "fn emit_wrap_narrow(",
            "fn emit_wrap_narrow_subword(",
            "fn emit_subword_range_check(",
            "fn emit_overflow_check(",
            "fn emit_overflow_check_add(",
            "fn emit_overflow_check_sub(",
            "fn emit_overflow_check_neg(",
        ] {
            assert!(
                !production.contains(removed),
                "backend {name} regained a per-target integer policy table: {removed}"
            );
        }
        for forbidden in [
            "type_bits",
            "type_is_signed",
            "from_signed",
            "to_signed",
            "to_width",
            "65535",
        ] {
            assert!(
                !production.contains(forbidden),
                "backend {name} re-derives integer policy from the width: {forbidden}"
            );
        }
        // One extension primitive per backend, and it is the only place an
        // extension instruction is selected from the shared enum.
        assert_eq!(
            production.matches("fn emit_extension(").count(),
            1,
            "backend {name} must expose exactly one integer-extension primitive"
        );
        assert_eq!(
            production.matches("IntegerExtension::Sign16").count(),
            1,
            "backend {name} selects an extension instruction outside emit_extension"
        );
    }
}

#[test]
fn target_independent_lowering_drivers_live_only_in_the_shared_cfg_lowering() {
    // The drivers over the shared plans — the terminator emitter, the parameter
    // reader, the entry preamble, the block-parameter helpers, the edge moves,
    // and the drop plan — decide *what* a lowering does; only the instruction
    // spelling is per target. They were hand-mirrored between the two backends
    // until RUE-1981, and a driver written twice is a driver that can differ
    // twice: that is how a multi-slot register return came to be written in one
    // order on x86-64 and the opposite order on AArch64 with nothing stating
    // why.
    let shared = include_str!("cfg_lower.rs");
    let drivers = [
        "emit_terminator_plan",
        "lower_param_value",
        "materialize_register_params",
        "preload_by_ref_param_ptrs",
        "preload_by_ref_params",
        "ensure_by_ref_param_ptr",
        "emit_edge_moves",
        "lower_drop_plan",
        "get_vreg",
        "materialize_block_param",
        "prepare_block_param",
    ];
    for driver in drivers {
        assert!(
            shared.contains(&format!("pub(crate) fn {driver}")),
            "the shared CFG lowering lost the {driver} driver"
        );
    }

    for (name, source) in [
        ("x86_64/cfg_lower", include_str!("x86_64/cfg_lower.rs")),
        ("aarch64/cfg_lower", include_str!("aarch64/cfg_lower.rs")),
    ] {
        let production = source
            .split("\n#[cfg(test)]\nmod ")
            .next()
            .expect("codegen production prefix");
        // Every driver a backend still names is reached through the shared
        // module, never re-implemented beside it.
        for driver in [
            "emit_terminator_plan",
            "lower_param_value",
            "ensure_by_ref_param_ptr",
            "lower_drop_plan",
            "get_vreg",
            "materialize_block_param",
            "preload_by_ref_params",
            "prepare_block_param",
        ] {
            assert_eq!(
                production
                    .matches(&format!("crate::cfg_lower::{driver}("))
                    .count(),
                1,
                "backend {name} must reach the shared {driver} driver exactly once"
            );
        }
        // The entry preamble and the edge moves have no backend spelling at
        // all: the shared driver reaches the machine through the leaves.
        for internal in [
            "fn materialize_register_params(",
            "fn preload_by_ref_param_ptrs(",
            "fn emit_edge_moves(",
        ] {
            assert!(
                !production.contains(internal),
                "backend {name} regained a hand-mirrored {internal}"
            );
        }
        // Inline labels come from the MIR's own allocator on both targets, so
        // the two label namespaces cannot drift apart (RUE-1981).
        assert!(
            !production.contains("next_label"),
            "backend {name} keeps a second inline-label counter beside the MIR's"
        );
        assert!(
            production.contains("alloc_label()"),
            "backend {name} must allocate inline labels through its MIR"
        );
        // The multi-eightbyte return order is a shared decision read from a
        // per-target predicate, not a hand-placed `.rev()`.
        assert!(
            production.contains("RETURN_SCRATCH_OVERLAP"),
            "backend {name} must name its scratch/result-register overlap"
        );
        assert!(
            production.contains("registers.write_order(RETURN_SCRATCH_OVERLAP)"),
            "backend {name} must spend its result registers in the shared order"
        );
    }

    // The order itself, and the reason it exists, live with the return plan.
    let call_plan = include_str!("call_plan.rs");
    assert!(call_plan.contains("pub fn return_register_write_order("));
    assert!(call_plan.contains("pub enum ScratchOverlap"));
}

#[test]
fn codegen_consults_air_for_aggregate_and_switch_compare_policy() {
    // The call planner and the value materializer must classify aggregates
    // identically, or a call passes a value in a shape the other side never
    // expects; AIR owns the predicate both consume.
    let types = include_str!("types.rs");
    assert!(types.contains("rue_air::is_multislot_aggregate(ty, type_slot_count(type_pool, ty))"));
    assert!(!types.contains("ty.is_enum() && type_slot_count(type_pool, ty) > 1"));

    // Switch case matching is one rule shared with CFG simplification's
    // constant folding, so the arm a fold selects is the arm a backend
    // compiles.
    let value_plan = include_str!("value_plan.rs");
    assert!(value_plan.contains("bits: ty.switch_compare_width(),"));
    let terminator = include_str!("terminator_plan.rs");
    assert!(terminator.contains("value_plan::switch_compare_width(ty)"));
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
        for forbidden in ["ForeignArg", "ForeignReturn"] {
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

/// The frame raw-pointer gate asks the same layout question the
/// fixed-array-to-slice coercion's E0908 refusal asks, through the same
/// canonical predicate (RUE-2097).
///
/// The coercion synthesizes `@raw(arr[0])` and hands the result to this gate,
/// so a gate that judged element layout by its own rule would either reject
/// what semantic analysis accepted — an internal error on a legal program — or
/// pass what semantic analysis would have refused. `rue-air`'s inventory guards
/// the other half (`slice_coercion_and_layout_predicates_come_from_one_walk`).
#[test]
fn frame_raw_aggregate_pointer_gate_uses_the_shared_stride_predicate() {
    let types = include_str!("types.rs");
    let gate = types
        .split("fn frame_raw_aggregate_pointer_unsupported(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("the frame raw-pointer gate");
    assert!(
        gate.contains("compact_stride_matches_slot_stride(type_pool, ty)"),
        "the frame raw-pointer gate must judge layout with the canonical \
         stride predicate"
    );
    assert!(
        !gate.contains("is_slot_identical_layout"),
        "the frame raw-pointer gate must not fold the call-ABI access-kind \
         question into its layout answer"
    );
    // The local name is a thin delegation to the one authority in `rue-air`,
    // not a second judgment.
    assert!(
        types.contains("rue_air::compact_stride_matches_slot_stride(type_pool, ty)"),
        "the codegen wrapper must delegate to the canonical layout authority"
    );
    assert_eq!(
        types
            .matches("pub(crate) fn compact_stride_matches_slot_stride")
            .count(),
        1,
        "the codegen wrapper has exactly one definition"
    );
}
