//! Adversarial contracts for place metadata and inout storage.

use super::*;

fn expect_flow_unsupported<T>(result: Step<T>) -> Unsupported {
    match result {
        Err(Flow::Unsupported(unsupported)) => unsupported,
        Err(Flow::Panic(_)) => panic!("expected Unsupported, got a modeled panic"),
        Ok(_) => panic!("expected Unsupported, got a value"),
    }
}

#[test]
fn flattened_parameter_padding_is_a_semantic_gap_but_oob_is_a_contract_failure() {
    let state = query_cfg_state(
        "struct Pair { a: i32, b: i32 }
        fn take(p: Pair) -> i32 { p.a + p.b }
        fn main() -> i32 { take(Pair { a: 1, b: 2 }) }",
    )
    .expect("probe must compile");
    let cfg = state
        .functions
        .iter()
        .find(|function| function.is_source_named("take"))
        .map(|function| &function.cfg)
        .expect("take CFG");
    assert_eq!(cfg.num_params(), 2, "Pair is flattened into two ABI slots");
    let frame = Frame {
        params: vec![
            Some(Value::Aggregate(vec![Value::Int(1), Value::Int(2)])),
            None,
        ],
        locals: Vec::new(),
        cache: HashMap::new(),
        promoted: HashMap::new(),
        param_places: HashMap::new(),
        place_return: false,
    };

    let padding = expect_flow_unsupported(Interp::base_value(&frame, PlaceBase::Param(1)));
    assert_eq!(
        padding.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::FlattenedParameterSlot)
    );

    let out_of_bounds = expect_flow_unsupported(Interp::base_value(&frame, PlaceBase::Param(2)));
    assert_eq!(
        out_of_bounds.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::ParameterSlotOutOfBounds)
    );
}

#[test]
fn core_str_length_is_modeled_and_inout_forwarding_threads_through_nested_calls() {
    let text = run_source_with_preview_features(
        r#"fn main() -> i32 {
            let s: str = "hi";
            @intCast(s.len())
        }"#,
        &PreviewFeatures::new(),
    )
    .expect("canonical core str length must be modeled");
    assert_eq!(
        text,
        Outcome {
            exit_code: 2,
            stdout: String::new(),
            stdout_bytes: Vec::new(),
            stderr: String::new(),
            panic: None,
        }
    );

    // Forwarding a writable `inout` parameter as a nested call's `inout`
    // argument is now modeled (RUE-1010): `f` forwards its own `inout v` to
    // `g`, whose mutation threads back through both call boundaries. This is the
    // container `self`-chain (`push` -> `self.reserve()`) that previously
    // reached an oracle model gap.
    let forwarding = run_source_with_preview_features(
        "struct D { x: i32 }
        fn g(inout v: D) -> i32 { v.x = v.x + 1; v.x }
        fn f(inout v: D) -> i32 { g(inout v) }
        fn main() -> i32 { let mut d = D { x: 7 }; f(inout d) }",
        &PreviewFeatures::new(),
    )
    .expect("inout parameter forwarding must be modeled");
    assert_eq!(
        forwarding.exit_code, 8,
        "d.x incremented 7 -> 8 threads back"
    );
}

#[test]
fn matching_cfg_metadata_is_required_before_a_runtime_symptom_is_registrable() {
    let state = query_cfg_state(
        "struct Pair { a: i32, b: i32 }
        fn read(p: Pair) -> i32 { p.a }
        fn main() -> i32 { read(Pair { a: 1, b: 2 }) }",
    )
    .expect("probe must compile");
    let cfg = state
        .functions
        .iter()
        .find(|function| function.is_source_named("read"))
        .map(|function| &function.cfg)
        .expect("read CFG");
    let mut field_read = None;
    for value in cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
    {
        if matches!(cfg.get_inst(value).data, CfgInstData::PlaceRead { .. }) {
            field_read = Some(value);
        }
    }
    let field_value = field_read.expect("Pair field read");
    let CfgInstData::PlaceRead { place: field_place } = &cfg.get_inst(field_value).data else {
        unreachable!()
    };
    let mut interp = Interp {
        state: &state,
        stdout_trace: Vec::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
        small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
        heap_metadata_bytes: 0,
    };
    let mut frame = Frame {
        // Deliberately inject a non-aggregate value under Pair projection
        // metadata. A field projection of a non-aggregate must be a contract
        // violation, not a silently-read cell.
        params: vec![Some(Value::Ptr(None)), None],
        locals: vec![None; cfg.num_locals() as usize],
        cache: HashMap::new(),
        promoted: HashMap::new(),
        param_places: HashMap::new(),
        place_return: false,
    };
    let projection = expect_flow_unsupported(interp.place_read(cfg, &mut frame, field_place));
    assert_eq!(
        projection.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::NonAggregateProjectionRead)
    );
    let projected_inout = expect_flow_unsupported(interp.lvalue_of(cfg, field_value));
    assert_eq!(
        projected_inout.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::InoutArgumentNotLvalue),
        "a projected by-value parameter is not caller-writable storage"
    );

    let ordinary_state = query_cfg_state(
        "fn identity(value: i32) -> i32 { value }
        fn main() -> i32 { identity(7) }",
    )
    .expect("ordinary parameter probe must compile");
    let ordinary_cfg = ordinary_state
        .functions
        .iter()
        .find(|function| function.is_source_named("identity"))
        .map(|function| &function.cfg)
        .expect("identity CFG");
    let ordinary_param = ordinary_cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
        .find(|value| {
            matches!(
                ordinary_cfg.get_inst(*value).data,
                CfgInstData::Param { .. }
            )
        })
        .expect("ordinary by-value Param instruction");
    let ordinary_interp = Interp {
        state: &ordinary_state,
        stdout_trace: Vec::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
        small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
        heap_metadata_bytes: 0,
    };
    // A by-value parameter is the frame's own mutable storage, so passing it
    // `inout` to a nested call is an ordinary write-back to its slot
    // (RUE-2042); only a projection of one, above, is not caller-writable.
    let Ok(inout) = ordinary_interp.lvalue_of(ordinary_cfg, ordinary_param) else {
        panic!("a by-value parameter slot is an inout lvalue");
    };
    assert!(
        matches!(
            inout,
            WritebackPlace::Simple {
                base: PlaceBase::Param(0),
                ..
            }
        ),
        "the write-back place is the parameter slot itself"
    );
}

#[test]
fn validated_cfg_requires_the_complete_ordinary_nominal_chain_to_be_well_typed() {
    let source = r#"struct Pair { value: i32 }
        struct Header { pointer: u64, length: u64, capacity: u64 }
        fn read(p: Pair) -> i32 { p.value }
        fn main() -> i32 {
            let header = Header { pointer: 0, length: 0, capacity: 0 };
            read(Pair { value: @intCast(header.length) })
        }"#;

    for invalid_suffix in [false, true] {
        let mut state = query_cfg_state(source).expect("projection-metadata probe must compile");
        let header_struct = state
            .type_pool()
            .all_struct_ids()
            .into_iter()
            .find(|id| &*state.type_pool().struct_def(*id).name == "Header")
            .expect("ordinary Header nominal");
        let header_ty = Type::new_struct(header_struct);
        assert_eq!(state.type_pool().struct_def(header_struct).fields.len(), 3);
        let read_index = state
            .functions
            .iter()
            .position(|function| function.is_source_named("read"))
            .expect("read CFG");
        let (place_value, base) = {
            let cfg = &state.functions[read_index].cfg;
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .find_map(|value| {
                    let CfgInstData::PlaceRead { place } = &cfg.get_inst(value).data else {
                        return None;
                    };
                    Some((value, place.base))
                })
                .expect("Pair field PlaceRead")
        };
        let pair_projection = {
            let cfg = &state.functions[read_index].cfg;
            let CfgInstData::PlaceRead { place } = &cfg.get_inst(place_value).data else {
                unreachable!()
            };
            cfg.get_place_projections(place)[0]
        };
        let projections = if invalid_suffix {
            vec![
                Projection::Field {
                    struct_id: header_struct,
                    field_index: 0,
                },
                pair_projection,
            ]
        } else {
            vec![Projection::Field {
                struct_id: header_struct,
                field_index: 0,
            }]
        };
        let type_pool = state.type_pool().clone();
        let cfg = &mut state.functions[read_index].cfg;
        let error = cfg
            .try_edit(&type_pool, |editor| {
                editor.replace_place_read(place_value, base, header_ty, projections)
            })
            .expect_err("ValidatedCfg must reject a malformed nominal projection chain");
        assert!(matches!(
            error,
            rue_cfg::CfgEditTransactionError::Verification(_)
        ));
    }
}

#[test]
fn logical_inout_writability_is_distinct_from_the_by_reference_abi() {
    let state = query_cfg_state(
        "struct Pair { a: i32, b: i32 }
        fn borrowed(borrow p: Pair) -> i32 { p.a }
        fn writable(inout p: Pair) -> i32 { p.a }
        fn main() -> i32 {
            let mut p = Pair { a: 1, b: 2 };
            borrowed(borrow p) + writable(inout p)
        }",
    )
    .expect("borrow/inout metadata probe must compile");
    let interp = Interp {
        state: &state,
        stdout_trace: Vec::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
        small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
        heap_metadata_bytes: 0,
    };
    let place_read = |name: &str| {
        let cfg = state
            .functions
            .iter()
            .find(|function| function.is_source_named(name))
            .map(|function| &function.cfg)
            .unwrap_or_else(|| panic!("missing {name} CFG"));
        let value = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| matches!(cfg.get_inst(*value).data, CfgInstData::PlaceRead { .. }))
            .unwrap_or_else(|| panic!("missing projected parameter read in {name}"));
        (cfg, value)
    };

    let (borrow_cfg, borrow_value) = place_read("borrowed");
    assert!(borrow_cfg.is_param_by_ref(0), "borrow uses the by-ref ABI");
    assert!(
        !borrow_cfg.is_param_writable(0),
        "borrow is not logically writable"
    );
    let borrow = expect_flow_unsupported(interp.lvalue_of(borrow_cfg, borrow_value));
    assert_eq!(
        borrow.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::InoutArgumentNotLvalue)
    );

    let (inout_cfg, inout_value) = place_read("writable");
    assert!(inout_cfg.is_param_by_ref(0), "inout uses the by-ref ABI");
    assert!(
        inout_cfg.is_param_writable(0),
        "inout is logically writable"
    );
    assert!(interp.lvalue_of(inout_cfg, inout_value).is_ok());
}

#[test]
fn validated_cfg_rejects_invalid_owned_text_projection_metadata() {
    let preview_features = PreviewFeatures::new();
    let view_state = query_cfg_state_with_preview_features(
        r#"fn main() -> i32 {
            let view: str = "view";
            @intCast(view.len())
        }"#,
        &preview_features,
    )
    .expect("str projection probe must compile");
    let view_cfg = view_state
        .functions
        .iter()
        .find(|function| function.is_source_named("main"))
        .map(|function| &function.cfg)
        .expect("main CFG");
    let view_value = view_cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
        .find(|&value| {
            matches!(
                &view_cfg.get_inst(value).data,
                CfgInstData::PlaceRead { .. }
            )
        })
        .expect("str field projection");
    let CfgInstData::PlaceRead { place: view_place } = &view_cfg.get_inst(view_value).data else {
        unreachable!()
    };
    let PlaceBase::Local(view_slot) = view_place.base else {
        panic!("str probe must be rooted in a local")
    };
    let mut view_frame = Frame {
        params: Vec::new(),
        locals: vec![None; view_cfg.num_locals() as usize],
        cache: HashMap::new(),
        promoted: HashMap::new(),
        param_places: HashMap::new(),
        place_return: false,
    };
    // A non-aggregate value under str projection metadata: reading a field of
    // it must be a contract violation, not a silent success.
    view_frame.locals[view_slot as usize] = Some(Value::Ptr(None));
    let mut view_interp = Interp {
        state: &view_state,
        stdout_trace: Vec::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
        small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
        heap_metadata_bytes: 0,
    };
    let width =
        expect_flow_unsupported(view_interp.place_read(view_cfg, &mut view_frame, view_place));
    assert_eq!(
        width.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::NonAggregateProjectionRead)
    );

    let mut owned_state = query_cfg_state(
        r#"fn main() -> i32 {
            let s = "owned";
            @intCast(s[0])
        }"#,
    )
    .expect("owned-string projection metadata probe must compile");
    let main_index = owned_state
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
        .expect("main CFG");
    let (owned_ty, zero) = {
        let cfg = &owned_state.functions[main_index].cfg;
        let mut owned_ty = None;
        let mut zero = None;
        for value in cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
        {
            let inst = cfg.get_inst(value);
            if matches!(inst.data, CfgInstData::StringConst(_)) {
                owned_ty = Some(inst.ty);
            }
            if matches!(inst.data, CfgInstData::Const(0)) {
                zero = Some(value);
            }
        }
        (owned_ty.expect("String type"), zero.expect("index zero"))
    };
    let TypeKind::Struct(owned_struct) = owned_ty.kind() else {
        panic!("String must be a struct type")
    };
    let type_pool = owned_state.type_pool().clone();
    let cfg = &mut owned_state.functions[main_index].cfg;
    let span = cfg.get_inst(zero).span;
    let error = cfg
        .try_edit(&type_pool, |editor| {
            editor.append_place_read(
                editor.entry,
                PlaceBase::Local(0),
                owned_ty,
                [Projection::Field {
                    struct_id: owned_struct,
                    field_index: 2,
                }],
                Type::U64,
                span,
            )?;
            editor.append_place_read(
                editor.entry,
                PlaceBase::Local(0),
                owned_ty,
                [Projection::Index {
                    array_type: owned_ty,
                    index: zero,
                }],
                Type::U64,
                span,
            )?;
            Ok::<_, rue_cfg::CfgEditError>(())
        })
        .expect_err("ValidatedCfg must reject invalid owned-text projections");
    assert!(matches!(
        error,
        rue_cfg::CfgEditTransactionError::Verification(_)
    ));
}

#[test]
fn validated_cfg_rejects_malformed_place_writes_before_oracle_model_gaps() {
    const SOURCE: &str = "struct Inner { value: u32 }
        struct Outer { inner: Inner }
        fn write() -> u32 {
            let mut outer = Outer { inner: Inner { value: 1 } };
            outer.inner.value = @random_u32();
            outer.inner.value
        }
        fn main() { write(); }";

    let gap = expect_unsupported(SOURCE);
    assert_eq!(
        gap.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::RandomU32),
        "the valid write's RHS must establish the model gap this test could otherwise hide"
    );

    #[derive(Clone, Copy, Debug)]
    enum Corruption {
        LocalBase,
        ParamBase,
        RootType,
        SuffixType,
        FinalType,
    }

    for corruption in [
        Corruption::LocalBase,
        Corruption::ParamBase,
        Corruption::RootType,
        Corruption::SuffixType,
        Corruption::FinalType,
    ] {
        let mut state = query_cfg_state(SOURCE).expect("PlaceWrite contract probe must compile");
        let write_index = state
            .functions
            .iter()
            .position(|function| function.is_source_named("write"))
            .expect("write CFG");
        state.select_source_function("write");
        let (write_value, base, base_type, rhs, projections) = {
            let cfg = &state.functions[write_index].cfg;
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .find_map(|value| {
                    let CfgInstData::PlaceWrite { place, value: rhs } = &cfg.get_inst(value).data
                    else {
                        return None;
                    };
                    (cfg.get_place_projections(place).len() == 2).then_some((
                        value,
                        place.base,
                        place.base_type,
                        *rhs,
                        cfg.get_place_projections(place).to_vec(),
                    ))
                })
                .expect("nested PlaceWrite")
        };
        let type_pool = state.type_pool().clone();
        let cfg = &mut state.functions[write_index].cfg;
        let (base, base_type, projections) = match corruption {
            Corruption::LocalBase => (PlaceBase::Local(cfg.num_locals()), base_type, projections),
            Corruption::ParamBase => (PlaceBase::Param(cfg.num_params()), base_type, projections),
            Corruption::RootType => (base, Type::I32, projections),
            Corruption::SuffixType => (base, base_type, vec![projections[0], projections[0]]),
            Corruption::FinalType => (base, base_type, vec![projections[0]]),
        };
        let before = cfg.to_string();
        let error = cfg
            .try_edit(&type_pool, |editor| {
                editor.replace_place_write(write_value, base, base_type, projections, rhs)
            })
            .expect_err("ValidatedCfg must reject a malformed place write");
        assert!(matches!(
            error,
            rue_cfg::CfgEditTransactionError::Edit(_)
                | rue_cfg::CfgEditTransactionError::Verification(_)
        ));
        assert_eq!(cfg.to_string(), before, "{corruption:?}");
    }
}

#[test]
fn validated_cfg_rejects_out_of_bounds_place_read_bases() {
    const SOURCE: &str = "fn read() -> u32 {
            let values: [u32; 2] = [1, 2];
            values[@random_u32()]
        }
        fn main() { read(); }";

    let gap = expect_unsupported(SOURCE);
    assert_eq!(
        gap.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::RandomU32),
        "the valid read's index must establish the model gap this test could otherwise hide"
    );

    for param_base in [false, true] {
        let mut state = query_cfg_state(SOURCE).expect("PlaceRead base probe must compile");
        let read_index = state
            .functions
            .iter()
            .position(|function| function.is_source_named("read"))
            .expect("read CFG");
        let (read_value, base_type, projections) = {
            let cfg = &state.functions[read_index].cfg;
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .find_map(|value| {
                    let CfgInstData::PlaceRead { place } = &cfg.get_inst(value).data else {
                        return None;
                    };
                    matches!(cfg.get_place_projections(place), [Projection::Index { .. }])
                        .then_some((
                            value,
                            place.base_type,
                            cfg.get_place_projections(place).to_vec(),
                        ))
                })
                .expect("indexed PlaceRead")
        };
        let type_pool = state.type_pool().clone();
        let cfg = &mut state.functions[read_index].cfg;
        let base = if param_base {
            PlaceBase::Param(cfg.num_params())
        } else {
            PlaceBase::Local(cfg.num_locals())
        };
        let before = cfg.to_string();
        let error = cfg
            .try_edit(&type_pool, |editor| {
                editor.replace_place_read(read_value, base, base_type, projections)
            })
            .expect_err("ValidatedCfg must reject an out-of-bounds place base");
        assert!(matches!(error, rue_cfg::CfgEditTransactionError::Edit(_)));
        assert_eq!(cfg.to_string(), before, "param_base={param_base}");
    }
}

#[test]
fn zero_sized_place_base_uses_the_canonical_boundary_slot() {
    const SOURCE: &str = "struct UnitBox { value: () }
        fn main() -> i32 {
            let boxed = UnitBox { value: () };
            if boxed.value == () { 42 } else { 0 }
        }";
    let mut state = query_cfg_state(SOURCE).expect("zero-sized place probe must compile");
    let main_index = state
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
        .expect("main CFG");
    let (read_value, base_type, projections) = {
        let cfg = &state.functions[main_index].cfg;
        cfg.blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| {
                let CfgInstData::PlaceRead { place } = &cfg.get_inst(value).data else {
                    return None;
                };
                (!cfg.get_place_projections(place).is_empty()).then_some((
                    value,
                    place.base_type,
                    cfg.get_place_projections(place).to_vec(),
                ))
            })
            .expect("zero-sized projected PlaceRead")
    };
    {
        let cfg = &state.functions[main_index].cfg;
        let interp = Interp {
            state: &state,
            stdout_trace: Vec::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
            heap: Vec::new(),
            small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
            heap_metadata_bytes: 0,
        };
        let CfgInstData::PlaceRead { place } = &cfg.get_inst(read_value).data else {
            unreachable!()
        };
        assert_eq!(interp.state.type_pool().abi_slot_count(place.base_type), 0);
        assert_eq!(place.base, PlaceBase::Local(cfg.num_locals()));
        assert_eq!(
            interp.place_base_violation(cfg, place, PlaceAccess::Read),
            None,
            "a zero-slot root may live at the canonical one-past boundary"
        );
    }

    let type_pool = state.type_pool().clone();
    let cfg = &mut state.functions[main_index].cfg;
    let invalid_slot = cfg.num_locals() + 1;
    let before = cfg.to_string();
    let error = cfg
        .try_edit(&type_pool, |editor| {
            editor.replace_place_read(
                read_value,
                PlaceBase::Local(invalid_slot),
                base_type,
                projections,
            )
        })
        .expect_err("ValidatedCfg must reject a slot beyond the canonical ZST boundary");
    assert!(matches!(error, rue_cfg::CfgEditTransactionError::Edit(_)));
    assert_eq!(cfg.to_string(), before);
}

#[test]
fn validated_cfg_requires_exact_type_and_writable_storage_for_whole_place_writes() {
    const SOURCE: &str = "fn write(input: u32) -> u32 {
            let mut value: u32 = input;
            value = @random_u32();
            value
        }
        fn main() { write(1); }";

    let gap = expect_unsupported(SOURCE);
    assert_eq!(
        gap.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::RandomU32)
    );

    for nonwritable_param in [false, true] {
        let mut state = query_cfg_state(SOURCE).expect("whole PlaceWrite probe must compile");
        let write_index = state
            .functions
            .iter()
            .position(|function| function.is_source_named("write"))
            .expect("write CFG");
        let (write_value, base, base_type, rhs) = {
            let cfg = &state.functions[write_index].cfg;
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .find_map(|value| {
                    let CfgInstData::Store { slot, value: rhs } = cfg.get_inst(value).data else {
                        return None;
                    };
                    let CfgInstData::Intrinsic {
                        operation: rue_air::IntrinsicOperation::RandomU32,
                        ..
                    } = cfg.get_inst(rhs).data
                    else {
                        return None;
                    };
                    Some((value, PlaceBase::Local(slot), cfg.get_inst(rhs).ty, rhs))
                })
                .unwrap_or_else(|| {
                    panic!(
                        "whole-variable store with random RHS in optimized CFG:\n{}",
                        &*state.functions[write_index].cfg
                    )
                })
        };
        let type_pool = state.type_pool().clone();
        let cfg = &mut state.functions[write_index].cfg;
        assert_eq!(cfg.num_params(), 1);
        assert!(!cfg.is_param_writable(0));
        let (base, base_type) = if nonwritable_param {
            (PlaceBase::Param(0), base_type)
        } else {
            (base, Type::I32)
        };
        let before = cfg.to_string();
        let error = cfg
            .try_edit(&type_pool, |editor| {
                editor.replace_place_write(write_value, base, base_type, [], rhs)
            })
            .expect_err("ValidatedCfg must reject an invalid whole-place write");
        assert!(matches!(
            error,
            rue_cfg::CfgEditTransactionError::Edit(_)
                | rue_cfg::CfgEditTransactionError::Verification(_)
        ));
        assert_eq!(
            cfg.to_string(),
            before,
            "nonwritable_param={nonwritable_param}"
        );
    }
}

#[test]
fn validated_cfg_allows_only_the_explicit_str_view_whole_place_read_coercion() {
    const SOURCE: &str = "fn take(borrow value: str) -> u64 { value.len() }
        fn probe() -> u64 {
            let value: Str(2) = \"hi\";
            let other: Str(3) = \"hey\";
            take(borrow value) + other.len()
        }
        fn main() { probe(); }";
    let preview_features = PreviewFeatures::new();
    let mut state = query_cfg_state_with_preview_features(SOURCE, &preview_features)
        .expect("whole PlaceRead probe must compile");
    let probe_index = state
        .functions
        .iter()
        .position(|function| function.is_source_named("probe"))
        .expect("probe CFG");
    state.select_source_function("probe");
    let other_fixed_type = state.functions[probe_index]
        .cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
        .map(|value| state.functions[probe_index].cfg.get_inst(value).ty)
        .find(|ty| {
            ty.as_struct()
                .is_some_and(|struct_id| &*state.type_pool().struct_def(struct_id).name == "Str(3)")
        })
        .expect("Str(3) parameter type");
    let (read_value, read_type) = {
        let cfg = &state.functions[probe_index].cfg;
        cfg.blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| {
                let inst = cfg.get_inst(value);
                let CfgInstData::PlaceRead { place } = &inst.data else {
                    return None;
                };
                (cfg.get_place_projections(place).is_empty() && place.base_type != inst.ty)
                    .then_some((value, inst.ty))
            })
            .expect("Str(N)-to-str whole PlaceRead")
    };
    {
        let cfg = &state.functions[probe_index].cfg;
        let CfgInstData::PlaceRead {
            place: original_place,
        } = &cfg.get_inst(read_value).data
        else {
            unreachable!()
        };
        let interp = Interp {
            state: &state,
            stdout_trace: Vec::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
            heap: Vec::new(),
            small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
            heap_metadata_bytes: 0,
        };
        assert!(interp.is_str_like_type(original_place.base_type));
        assert!(interp.is_str_like_type(read_type));
        assert!(interp.is_bare_str_type(read_type));
        assert!(interp.place_projection_metadata_is_valid(
            cfg,
            original_place,
            read_type,
            PlaceAccess::Read,
        ));
        assert!(interp.is_str_like_type(other_fixed_type));
        assert!(!interp.is_bare_str_type(other_fixed_type));
        assert!(!interp.place_projection_metadata_is_valid(
            cfg,
            original_place,
            other_fixed_type,
            PlaceAccess::Read,
        ));
    }

    let base = {
        let cfg = &state.functions[probe_index].cfg;
        let CfgInstData::PlaceRead { place } = &cfg.get_inst(read_value).data else {
            unreachable!()
        };
        place.base
    };
    let type_pool = state.type_pool().clone();
    let before = state.functions[probe_index].cfg.to_string();
    let error = state.functions[probe_index]
        .cfg
        .try_edit(&type_pool, |editor| {
            editor.replace_place_read(read_value, base, Type::I32, [])
        })
        .expect_err("ValidatedCfg must reject a non-str whole-place read coercion");
    assert!(matches!(
        error,
        rue_cfg::CfgEditTransactionError::Verification(_)
    ));
    assert_eq!(state.functions[probe_index].cfg.to_string(), before);
}
