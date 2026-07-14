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
        .map(|function| &function.cfg)
        .find(|cfg| cfg.fn_name() == "take")
        .expect("take CFG");
    assert_eq!(cfg.num_params(), 2, "Pair is flattened into two ABI slots");
    let frame = Frame {
        params: vec![
            Some(Value::Aggregate(vec![Value::Int(1), Value::Int(2)])),
            None,
        ],
        locals: Vec::new(),
        cache: HashMap::new(),
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
fn core_str_length_is_modeled_but_inout_forwarding_remains_a_program_model_gap() {
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
            stderr: String::new(),
            panic: None,
        }
    );

    let forwarding = expect_unsupported(
        "struct D { x: i32 }
        fn g(inout v: D) -> i32 { v.x = v.x + 1; v.x }
        fn f(inout v: D) -> i32 { g(inout v) }
        fn main() -> i32 { let mut d = D { x: 7 }; f(inout d) }",
    );
    assert_eq!(
        forwarding.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::InoutParameterForwarding)
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
        .map(|function| &function.cfg)
        .find(|cfg| cfg.fn_name() == "read")
        .expect("read CFG");
    let mut field_read = None;
    for value in cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
    {
        if let CfgInstData::PlaceRead { place } = &cfg.get_inst(value).data {
            field_read = Some((value, place.clone()));
        }
    }
    let (field_value, field_place) = field_read.expect("Pair field read");
    let mut interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    let mut frame = Frame {
        // Deliberately inject the oracle's text runtime representation under
        // non-text Pair projection metadata. Value shape alone must not turn
        // this broken state into a registrable TextProjection gap.
        params: vec![Some(Value::string("not a Pair")), None],
        locals: vec![None; cfg.num_locals() as usize],
        cache: HashMap::new(),
    };
    let projection = expect_flow_unsupported(interp.place_read(cfg, &mut frame, &field_place));
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
        .map(|function| &function.cfg)
        .find(|cfg| cfg.fn_name() == "identity")
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
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    let inout = expect_flow_unsupported(ordinary_interp.lvalue_of(ordinary_cfg, ordinary_param));
    assert_eq!(
        inout.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::InoutArgumentNotLvalue)
    );
}

#[test]
fn place_contract_requires_the_complete_ordinary_nominal_chain_to_be_well_typed() {
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
            .type_pool
            .all_struct_ids()
            .into_iter()
            .find(|id| state.type_pool.struct_def(*id).name == "Header")
            .expect("ordinary Header nominal");
        let header_ty = Type::new_struct(header_struct);
        assert_eq!(state.type_pool.struct_def(header_struct).fields.len(), 3);
        let read_index = state
            .functions
            .iter()
            .position(|function| function.cfg.fn_name() == "read")
            .expect("read CFG");
        let (place_value, original_place) = {
            let cfg = &state.functions[read_index].cfg;
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .find_map(|value| {
                    let CfgInstData::PlaceRead { place } = &cfg.get_inst(value).data else {
                        return None;
                    };
                    Some((value, place.clone()))
                })
                .expect("Pair field PlaceRead")
        };
        let pair_projection = state.functions[read_index]
            .cfg
            .get_place_projections(&original_place)[0];
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
        let cfg = &mut state.functions[read_index].cfg;
        let place = cfg.make_place(original_place.base, header_ty, projections);
        cfg.get_inst_mut(place_value).data = CfgInstData::PlaceRead { place };

        let cfg = &state.functions[read_index].cfg;
        let mut interp = Interp {
            state: &state,
            stdout: String::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
        };
        let mut frame = Frame {
            params: vec![Some(Value::string("not a Header"))],
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
        };
        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, place_value));
        assert_eq!(
            unsupported.kind(),
            UnsupportedKind::ContractViolation(ContractViolationKind::PlaceProjectionMetadata),
            "the malformed complete place chain must fail before its injected text runtime shape; invalid_suffix={invalid_suffix}"
        );
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
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    let place_read = |name: &str| {
        let cfg = state
            .functions
            .iter()
            .map(|function| &function.cfg)
            .find(|cfg| cfg.fn_name() == name)
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
fn text_projection_gaps_require_exact_representation_metadata() {
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
        .map(|function| &function.cfg)
        .find(|cfg| cfg.fn_name() == "main")
        .expect("main CFG");
    let view_place = view_cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
        .find_map(|value| match &view_cfg.get_inst(value).data {
            CfgInstData::PlaceRead { place } => Some(place.clone()),
            _ => None,
        })
        .expect("str field projection");
    let PlaceBase::Local(view_slot) = view_place.base else {
        panic!("str probe must be rooted in a local")
    };
    let mut view_frame = Frame {
        params: Vec::new(),
        locals: vec![None; view_cfg.num_locals() as usize],
        cache: HashMap::new(),
    };
    view_frame.locals[view_slot as usize] = Some(Value::string("wrong three-slot value"));
    let mut view_interp = Interp {
        state: &view_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    let width =
        expect_flow_unsupported(view_interp.place_read(view_cfg, &mut view_frame, &view_place));
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
        .position(|function| function.cfg.fn_name() == "main")
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
    let cfg = &mut owned_state.functions[main_index].cfg;
    let cap_place = cfg.make_place(
        PlaceBase::Local(0),
        owned_ty,
        [Projection::Field {
            struct_id: owned_struct,
            field_index: 2,
        }],
    );
    let index_place = cfg.make_place(
        PlaceBase::Local(0),
        owned_ty,
        [Projection::Index {
            array_type: owned_ty,
            index: zero,
        }],
    );
    let owned_cfg = &owned_state.functions[main_index].cfg;
    let owned_interp = Interp {
        state: &owned_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    for place in [cap_place, index_place] {
        let mut frame = Frame {
            params: Vec::new(),
            locals: vec![Some(Value::string("owned"))],
            cache: HashMap::new(),
        };
        let unsupported = expect_flow_unsupported({
            let mut interp = Interp {
                state: owned_interp.state,
                stdout: String::new(),
                stdout_bytes: 0,
                stdout_cap: MAX_STDOUT_BYTES,
                stderr_cap: MAX_STDERR_BYTES,
                budget: STEP_BUDGET,
                depth: 0,
            };
            interp.place_read(owned_cfg, &mut frame, &place)
        });
        assert_eq!(
            unsupported.kind(),
            UnsupportedKind::ContractViolation(ContractViolationKind::NonAggregateProjectionRead)
        );
    }
}

#[test]
fn place_write_contracts_precede_rhs_model_gaps() {
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
            .position(|function| function.cfg.fn_name() == "write")
            .expect("write CFG");
        let (write_value, original_place, rhs) = {
            let cfg = &state.functions[write_index].cfg;
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .find_map(|value| {
                    let CfgInstData::PlaceWrite { place, value: rhs } = cfg.get_inst(value).data
                    else {
                        return None;
                    };
                    (cfg.get_place_projections(&place).len() == 2).then_some((value, place, rhs))
                })
                .expect("nested PlaceWrite")
        };
        let projections = state.functions[write_index]
            .cfg
            .get_place_projections(&original_place)
            .to_vec();
        let expected = match corruption {
            Corruption::LocalBase | Corruption::ParamBase => {
                UnsupportedKind::ContractViolation(ContractViolationKind::PlaceBaseOutOfBounds)
            }
            Corruption::RootType | Corruption::SuffixType | Corruption::FinalType => {
                UnsupportedKind::ContractViolation(ContractViolationKind::PlaceProjectionMetadata)
            }
        };
        let cfg = &mut state.functions[write_index].cfg;
        let invalid_place = match corruption {
            Corruption::LocalBase => Place {
                base: PlaceBase::Local(cfg.num_locals()),
                ..original_place
            },
            Corruption::ParamBase => Place {
                base: PlaceBase::Param(cfg.num_params()),
                ..original_place
            },
            Corruption::RootType => Place {
                base_type: Type::I32,
                ..original_place
            },
            Corruption::SuffixType => cfg.make_place(
                original_place.base,
                original_place.base_type,
                [projections[0], projections[0]],
            ),
            Corruption::FinalType => cfg.make_place(
                original_place.base,
                original_place.base_type,
                [projections[0]],
            ),
        };
        cfg.get_inst_mut(write_value).data = CfgInstData::PlaceWrite {
            place: invalid_place,
            value: rhs,
        };

        let cfg = &state.functions[write_index].cfg;
        let mut interp = Interp {
            state: &state,
            stdout: String::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
        };
        let mut frame = Frame {
            params: vec![None; cfg.num_params() as usize],
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
        };
        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, write_value));
        assert_eq!(unsupported.kind(), expected, "{corruption:?}");
    }
}

#[test]
fn place_read_base_contract_precedes_index_model_gap() {
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
            .position(|function| function.cfg.fn_name() == "read")
            .expect("read CFG");
        let (read_value, original_place) = {
            let cfg = &state.functions[read_index].cfg;
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .find_map(|value| {
                    let CfgInstData::PlaceRead { place } = cfg.get_inst(value).data else {
                        return None;
                    };
                    matches!(
                        cfg.get_place_projections(&place),
                        [Projection::Index { .. }]
                    )
                    .then_some((value, place))
                })
                .expect("indexed PlaceRead")
        };
        let cfg = &mut state.functions[read_index].cfg;
        let invalid_place = Place {
            base: if param_base {
                PlaceBase::Param(cfg.num_params())
            } else {
                PlaceBase::Local(cfg.num_locals())
            },
            ..original_place
        };
        cfg.get_inst_mut(read_value).data = CfgInstData::PlaceRead {
            place: invalid_place,
        };

        let cfg = &state.functions[read_index].cfg;
        let mut interp = Interp {
            state: &state,
            stdout: String::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
        };
        let mut frame = Frame {
            params: vec![None; cfg.num_params() as usize],
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
        };
        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, read_value));
        assert_eq!(
            unsupported.kind(),
            UnsupportedKind::ContractViolation(ContractViolationKind::PlaceBaseOutOfBounds),
            "param_base={param_base}"
        );
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
        .position(|function| function.cfg.fn_name() == "main")
        .expect("main CFG");
    let (read_value, original_place) = {
        let cfg = &state.functions[main_index].cfg;
        cfg.blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| {
                let CfgInstData::PlaceRead { place } = cfg.get_inst(value).data else {
                    return None;
                };
                (!cfg.get_place_projections(&place).is_empty()).then_some((value, place))
            })
            .expect("zero-sized projected PlaceRead")
    };
    {
        let cfg = &state.functions[main_index].cfg;
        let interp = Interp {
            state: &state,
            stdout: String::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
        };
        assert_eq!(
            interp
                .state
                .type_pool
                .abi_slot_count(original_place.base_type),
            0
        );
        assert_eq!(original_place.base, PlaceBase::Local(cfg.num_locals()));
        assert_eq!(
            interp.place_base_violation(cfg, &original_place, PlaceAccess::Read),
            None,
            "a zero-slot root may live at the canonical one-past boundary"
        );
    }

    let cfg = &mut state.functions[main_index].cfg;
    let invalid_place = Place {
        base: PlaceBase::Local(cfg.num_locals() + 1),
        ..original_place
    };
    cfg.get_inst_mut(read_value).data = CfgInstData::PlaceRead {
        place: invalid_place,
    };
    let cfg = &state.functions[main_index].cfg;
    let mut interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    let mut frame = Frame {
        params: Vec::new(),
        locals: vec![None; cfg.num_locals() as usize],
        cache: HashMap::new(),
    };
    let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, read_value));
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::PlaceBaseOutOfBounds)
    );
}

#[test]
fn whole_place_write_requires_exact_type_and_writable_storage() {
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
            .position(|function| function.cfg.fn_name() == "write")
            .expect("write CFG");
        let (write_value, original_place, rhs) = {
            let cfg = &state.functions[write_index].cfg;
            cfg.blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .find_map(|value| {
                    let CfgInstData::Store { slot, value: rhs } = cfg.get_inst(value).data else {
                        return None;
                    };
                    let CfgInstData::Intrinsic { name, .. } = cfg.get_inst(rhs).data else {
                        return None;
                    };
                    (state.interner.resolve(&name) == "random_u32").then_some((
                        value,
                        Place::local(slot, cfg.get_inst(rhs).ty),
                        rhs,
                    ))
                })
                .expect("whole-variable store with random RHS")
        };
        let cfg = &mut state.functions[write_index].cfg;
        assert_eq!(cfg.num_params(), 1);
        assert!(!cfg.is_param_writable(0));
        let invalid_place = if nonwritable_param {
            Place {
                base: PlaceBase::Param(0),
                ..original_place
            }
        } else {
            Place {
                base_type: Type::I32,
                ..original_place
            }
        };
        cfg.get_inst_mut(write_value).data = CfgInstData::PlaceWrite {
            place: invalid_place,
            value: rhs,
        };

        let cfg = &state.functions[write_index].cfg;
        let mut interp = Interp {
            state: &state,
            stdout: String::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
        };
        let mut frame = Frame {
            params: vec![Some(Value::Int(1))],
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
        };
        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, write_value));
        let expected = if nonwritable_param {
            ContractViolationKind::PlaceBaseNotWritable
        } else {
            ContractViolationKind::PlaceProjectionMetadata
        };
        assert_eq!(
            unsupported.kind(),
            UnsupportedKind::ContractViolation(expected),
            "nonwritable_param={nonwritable_param}"
        );
    }
}

#[test]
fn whole_place_read_allows_only_the_explicit_str_view_coercion() {
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
        .position(|function| function.cfg.fn_name() == "probe")
        .expect("probe CFG");
    let other_fixed_type = state.functions[probe_index]
        .cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
        .map(|value| state.functions[probe_index].cfg.get_inst(value).ty)
        .find(|ty| {
            ty.as_struct()
                .is_some_and(|struct_id| state.type_pool.struct_def(struct_id).name == "Str(3)")
        })
        .expect("Str(3) parameter type");
    let (read_value, original_place, read_type) = {
        let cfg = &state.functions[probe_index].cfg;
        cfg.blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| {
                let inst = cfg.get_inst(value);
                let CfgInstData::PlaceRead { place } = inst.data else {
                    return None;
                };
                (cfg.get_place_projections(&place).is_empty() && place.base_type != inst.ty)
                    .then_some((value, place, inst.ty))
            })
            .expect("Str(N)-to-str whole PlaceRead")
    };
    {
        let cfg = &state.functions[probe_index].cfg;
        let interp = Interp {
            state: &state,
            stdout: String::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
        };
        assert!(interp.is_str_like_type(original_place.base_type));
        assert!(interp.is_str_like_type(read_type));
        assert!(interp.is_bare_str_type(read_type));
        assert!(interp.place_projection_metadata_is_valid(
            cfg,
            &original_place,
            read_type,
            PlaceAccess::Read,
        ));
        assert!(interp.is_str_like_type(other_fixed_type));
        assert!(!interp.is_bare_str_type(other_fixed_type));
        assert!(!interp.place_projection_metadata_is_valid(
            cfg,
            &original_place,
            other_fixed_type,
            PlaceAccess::Read,
        ));
    }

    state.functions[probe_index]
        .cfg
        .get_inst_mut(read_value)
        .data = CfgInstData::PlaceRead {
        place: Place {
            base_type: Type::I32,
            ..original_place
        },
    };
    let cfg = &state.functions[probe_index].cfg;
    let mut interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    let mut frame = Frame {
        params: Vec::new(),
        locals: vec![None; cfg.num_locals() as usize],
        cache: HashMap::new(),
    };
    let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, read_value));
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::PlaceProjectionMetadata)
    );
}
