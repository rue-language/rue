//! Adversarial contracts for calls, intrinsics, and pointer provenance.

use super::*;

fn expect_flow_unsupported<T>(result: Step<T>) -> Unsupported {
    match result {
        Err(Flow::Unsupported(unsupported)) => unsupported,
        Err(Flow::Panic(_)) => panic!("expected Unsupported, got a modeled panic"),
        Ok(_) => panic!("expected Unsupported, got a value"),
    }
}

fn find_call_metadata(
    state: &CompileState,
    expected_name: &str,
) -> (Vec<Type>, Vec<CfgArgMode>, Type) {
    let mut found = None;
    for function in &state.functions {
        let cfg = &function.cfg;
        for block in cfg.blocks() {
            for &value in &block.insts {
                let inst = cfg.get_inst(value);
                let CfgInstData::Call {
                    name,
                    args_start,
                    args_len,
                } = &inst.data
                else {
                    continue;
                };
                if state.interner.resolve(name) != expected_name {
                    continue;
                }
                let args = cfg.get_call_args(*args_start, *args_len);
                let metadata = (
                    args.iter().map(|arg| cfg.get_inst(arg.value).ty).collect(),
                    args.iter().map(|arg| arg.mode).collect(),
                    inst.ty,
                );
                assert!(
                    found.replace(metadata).is_none(),
                    "expected exactly one call to {expected_name}"
                );
            }
        }
    }
    found.unwrap_or_else(|| panic!("missing call to {expected_name}"))
}

fn find_intrinsic_in_function<'a>(
    state: &'a CompileState,
    function_name: &str,
    expected_name: &str,
) -> (&'a Cfg, CfgValue, Vec<CfgValue>, Type) {
    let cfg = state
        .functions
        .iter()
        .map(|function| &function.cfg)
        .find(|cfg| cfg.fn_name() == function_name)
        .unwrap_or_else(|| panic!("missing CFG for {function_name}"));
    let mut found = None;
    for value in cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
    {
        let inst = cfg.get_inst(value);
        let CfgInstData::Intrinsic {
            name,
            args_start,
            args_len,
        } = &inst.data
        else {
            continue;
        };
        if state.interner.resolve(name) != expected_name {
            continue;
        }
        let item = (
            value,
            cfg.get_extra(*args_start, *args_len).to_vec(),
            inst.ty,
        );
        assert!(
            found.replace(item).is_none(),
            "expected exactly one @{expected_name} in {function_name}"
        );
    }
    let (value, args, ty) =
        found.unwrap_or_else(|| panic!("missing @{expected_name} in {function_name}"));
    (cfg, value, args, ty)
}

#[test]
fn runtime_call_classification_requires_exact_metadata() {
    use SemanticGapKind as Semantic;
    use UnsupportedRuntimeCallKind as RuntimeCall;

    let mut preview_features = PreviewFeatures::new();
    preview_features.insert(rue_compiler::PreviewFeature::StringTrio);
    let state = compile_to_cfg_with_preview_features(
        r#"fn probe_concat() -> i32 { @intCast(("a" + "b").len()) }
        fn probe_contains() -> i32 { if "abc".contains("b") { 1 } else { 0 } }
        fn probe_starts() -> i32 { if "abc".starts_with("a") { 1 } else { 0 } }
        fn probe_substring() -> i32 { @intCast("abc".substring(0, 1).len()) }
        fn probe_print() -> i32 { print("x"); 0 }
        fn probe_println() -> i32 { println("x"); 0 }
        fn probe_view() -> i32 { let view: str = "view"; @intCast(view.len()) }
        fn main() -> i32 {
            probe_concat() + probe_contains() + probe_starts() + probe_substring()
                + probe_print() + probe_println() + probe_view()
        }"#,
        &preview_features,
    )
    .expect("runtime-call and str-view probes must compile");
    let interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    let view = state
        .functions
        .iter()
        .flat_map(|function| {
            function
                .cfg
                .blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
                .map(|value| function.cfg.get_inst(value))
        })
        .find_map(|inst| {
            (matches!(inst.data, CfgInstData::StringConst(_)) && interp.is_str_like_type(inst.ty))
                .then_some(inst.ty)
        })
        .expect("str-view constant type");

    let cases = [
        ("__rue_String_concat", RuntimeCall::StringConcat),
        ("__rue_String_contains", RuntimeCall::StringContains),
        ("__rue_String_starts_with", RuntimeCall::StringStartsWith),
        ("__rue_String_substring", RuntimeCall::StringSubstring),
        ("__rue_print", RuntimeCall::Print),
        ("__rue_println", RuntimeCall::Println),
    ];
    let values_for = |types: &[Type]| {
        types
            .iter()
            .map(|ty| {
                if interp.is_owned_string_type(*ty) {
                    Value::string("probe")
                } else {
                    Value::Int(0)
                }
            })
            .collect::<Vec<_>>()
    };

    for (name, runtime_call) in cases {
        let (arg_types, arg_modes, result_ty) = find_call_metadata(&state, name);
        let args = values_for(&arg_types);
        assert_eq!(
            interp
                .classify_unsupported_runtime_call(name, &args, &arg_types, &arg_modes, result_ty,),
            UnsupportedKind::SemanticGap(Semantic::RuntimeCall(runtime_call)),
            "{name}"
        );
    }
    for name in [
        "__rue_str_eq",
        "__rue_fn_user_function",
        "__rue_new_runtime_helper",
    ] {
        assert_eq!(
            interp.classify_unsupported_runtime_call(name, &[], &[], &[], Type::UNIT),
            UnsupportedKind::ContractViolation(ContractViolationKind::MissingFunctionBody),
            "{name}"
        );
    }
    let (concat_types, concat_modes, concat_result) =
        find_call_metadata(&state, "__rue_String_concat");
    let concat_values = values_for(&concat_types);
    assert_eq!(
        interp.classify_unsupported_runtime_call(
            "__rue_String_concat",
            &concat_values[..1],
            &concat_types[..1],
            &concat_modes[..1],
            concat_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity)
    );

    assert!(interp.is_str_like_type(view));
    assert!(!interp.is_owned_string_type(view));
    assert_eq!(
        interp.classify_unsupported_runtime_call(
            "__rue_String_concat",
            &concat_values,
            &[view, concat_types[1]],
            &concat_modes,
            concat_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
    );
    assert_eq!(
        interp.classify_unsupported_runtime_call(
            "__rue_String_concat",
            &concat_values,
            &concat_types,
            &concat_modes[..1],
            concat_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity),
        "argument-mode metadata length is independently checked"
    );
    for (name, wrong_result) in [
        ("__rue_String_concat", Type::UNIT),
        ("__rue_String_contains", Type::UNIT),
        ("__rue_String_substring", Type::UNIT),
        ("__rue_print", Type::BOOL),
    ] {
        let (arg_types, arg_modes, result_ty) = find_call_metadata(&state, name);
        let args = values_for(&arg_types);
        assert_ne!(
            wrong_result, result_ty,
            "{name} negative must change the result"
        );
        assert_eq!(
            interp.classify_unsupported_runtime_call(
                name,
                &args,
                &arg_types,
                &arg_modes,
                wrong_result,
            ),
            UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature),
            "{name} result type is exact"
        );
    }
    let (substring_types, substring_modes, substring_result) =
        find_call_metadata(&state, "__rue_String_substring");
    let substring_values = values_for(&substring_types);
    for index in [1, 2] {
        let mut wrong_types = substring_types.clone();
        wrong_types[index] = Type::I32;
        assert_eq!(
            interp.classify_unsupported_runtime_call(
                "__rue_String_substring",
                &substring_values,
                &wrong_types,
                &substring_modes,
                substring_result,
            ),
            UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature),
            "substring index {index} is exactly u64, not merely any integer"
        );
    }
    assert_eq!(
        interp.classify_unsupported_runtime_call(
            "__rue_String_concat",
            &concat_values,
            &concat_types,
            &[CfgArgMode::Borrow, CfgArgMode::Normal],
            concat_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
    );
    let mut wrong_runtime_shape = concat_values.clone();
    wrong_runtime_shape[0] = Value::str_view("two-slot view");
    assert_eq!(
        interp.classify_unsupported_runtime_call(
            "__rue_String_concat",
            &wrong_runtime_shape,
            &concat_types,
            &concat_modes,
            concat_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature),
        "an owned-string CFG type requires the owned three-slot runtime shape"
    );
}

#[test]
fn capacity_and_intrinsic_signature_drift_are_contract_failures() {
    let capacity_state = compile_to_cfg(
        r#"fn main() -> i32 {
            @intCast("probe".capacity())
        }"#,
    )
    .expect("probe must compile");
    let (capacity_types, capacity_modes, capacity_result_ty) =
        find_call_metadata(&capacity_state, "__rue_String_capacity");
    let capacity_interp = Interp {
        state: &capacity_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    let exact_capacity = expect_flow_unsupported(capacity_interp.string_builtin(
        "__rue_String_capacity",
        &[Value::string("probe")],
        &capacity_types,
        &capacity_modes,
        capacity_result_ty,
    ));
    assert_eq!(
        exact_capacity.kind(),
        UnsupportedKind::ImplementationDefined(ImplementationDefinedKind::StringCapacityValue)
    );
    let capacity_arity = expect_flow_unsupported(capacity_interp.string_builtin(
        "__rue_String_capacity",
        &[],
        &[],
        &[],
        Type::U64,
    ));
    assert_eq!(
        capacity_arity.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArity)
    );
    let capacity_type = expect_flow_unsupported(capacity_interp.string_builtin(
        "__rue_String_capacity",
        &[Value::string("probe")],
        &[Type::I32],
        &capacity_modes,
        capacity_result_ty,
    ));
    assert_eq!(
        capacity_type.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType)
    );
    let capacity_value = expect_flow_unsupported(capacity_interp.string_builtin(
        "__rue_String_capacity",
        &[Value::Int(0)],
        &capacity_types,
        &capacity_modes,
        capacity_result_ty,
    ));
    assert_eq!(
        capacity_value.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType)
    );
    let capacity_width = expect_flow_unsupported(capacity_interp.string_builtin(
        "__rue_String_capacity",
        &[Value::str_view("probe")],
        &capacity_types,
        &capacity_modes,
        capacity_result_ty,
    ));
    assert_eq!(
        capacity_width.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType)
    );
    let capacity_mode = expect_flow_unsupported(capacity_interp.string_builtin(
        "__rue_String_capacity",
        &[Value::string("probe")],
        &capacity_types,
        &[CfgArgMode::Borrow],
        capacity_result_ty,
    ));
    assert_eq!(
        capacity_mode.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentMode)
    );
    let capacity_result = expect_flow_unsupported(capacity_interp.string_builtin(
        "__rue_String_capacity",
        &[Value::string("probe")],
        &capacity_types,
        &capacity_modes,
        Type::BOOL,
    ));
    assert_eq!(
        capacity_result.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinResultType)
    );

    let random_state = compile_to_cfg(
        "fn main() -> i32 {
            let n: u32 = @random_u32();
            if n == 0 { 0 } else { 1 }
        }",
    )
    .expect("probe must compile");
    let random_cfg = random_state
        .functions
        .iter()
        .map(|function| &function.cfg)
        .find(|cfg| cfg.fn_name() == "main")
        .expect("main CFG");
    let random_inst = random_cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
        .find(|value| {
            matches!(
                random_cfg.get_inst(*value).data,
                CfgInstData::Intrinsic { .. }
            )
        })
        .expect("random intrinsic");
    let random_interp = Interp {
        state: &random_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    assert_eq!(
        random_interp.classify_unsupported_intrinsic(
            random_cfg,
            random_inst,
            "random_u32",
            &[],
            Type::U32,
        ),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::RandomU32)
    );
    assert_eq!(
        random_interp.classify_unsupported_intrinsic(
            random_cfg,
            random_inst,
            "random_u32",
            &[random_inst],
            Type::U32
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity)
    );
    assert_eq!(
        random_interp.classify_unsupported_intrinsic(
            random_cfg,
            random_inst,
            "random_u32",
            &[],
            Type::U64,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature)
    );
}

#[test]
fn panic_never_signature_is_an_oracle_contract_for_both_arities() {
    let state = compile_to_cfg(
        r#"fn panic_no_message() { @panic() }
        fn panic_with_message() { @panic("boom") }
        fn main() -> i32 {
            panic_no_message();
            panic_with_message();
            0
        }"#,
    )
    .expect("both panic arities must compile with the never contract");
    let interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    for function_name in ["panic_no_message", "panic_with_message"] {
        let (cfg, _intrinsic, args, result_ty) =
            find_intrinsic_in_function(&state, function_name, "panic");
        // `@panic` diverges, so the compiler types it `!` (never) (RUE-512).
        assert_eq!(result_ty, Type::NEVER, "{function_name} compiler metadata");
        // The abort preflight (the oracle's panic contract since RUE-589)
        // accepts exactly the never-typed shape...
        assert!(
            matches!(
                interp.preflight_abort_intrinsic(cfg, "panic", &args, result_ty),
                Ok(Some(AbortIntrinsic::Panic))
            ),
            "{function_name} never signature must pass preflight"
        );
        // ...and rejects stale unit-typed metadata as a contract violation.
        match interp.preflight_abort_intrinsic(cfg, "panic", &args, Type::UNIT) {
            Err(Flow::Unsupported(unsupported)) => assert_eq!(
                unsupported.kind(),
                UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
                "{function_name} must reject the stale unit-typed metadata"
            ),
            _ => panic!("{function_name}: stale unit-typed metadata must be a contract violation"),
        }
    }
}

#[test]
fn malformed_outer_calls_are_rejected_before_unmodeled_operands_run() {
    let mut preview_features = PreviewFeatures::new();
    preview_features.insert(rue_compiler::PreviewFeature::StringTrio);
    let source = r#"fn main() -> i32 {
        let entropy: u32 = @random_u32();
        let text = String.new();
        @intCast(text.capacity()) + @intCast((text + "suffix").len())
    }"#;

    for (call_name, expected) in [
        (
            "__rue_String_capacity",
            UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType),
        ),
        (
            "__rue_String_concat",
            UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature),
        ),
    ] {
        let mut state = compile_to_cfg_with_preview_features(source, &preview_features)
            .expect("call-preflight probe must compile");
        let main_index = state
            .functions
            .iter()
            .position(|function| function.cfg.fn_name() == "main")
            .expect("main CFG");
        let (random, outer_call) = {
            let cfg = &state.functions[main_index].cfg;
            let mut random = None;
            let mut outer_call = None;
            for value in cfg
                .blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
            {
                match &cfg.get_inst(value).data {
                    CfgInstData::Intrinsic { name, .. }
                        if state.interner.resolve(name) == "random_u32" =>
                    {
                        random = Some(value);
                    }
                    CfgInstData::Call { name, .. } if state.interner.resolve(name) == call_name => {
                        outer_call = Some(value);
                    }
                    _ => {}
                }
            }
            (
                random.expect("random intrinsic"),
                outer_call.unwrap_or_else(|| panic!("{call_name} call")),
            )
        };
        let cfg = &mut state.functions[main_index].cfg;
        let (args_start, args_len) = match cfg.get_inst(outer_call).data {
            CfgInstData::Call {
                args_start,
                args_len,
                ..
            } => (args_start, args_len),
            _ => unreachable!("selected a Call"),
        };
        let mut args = cfg.get_call_args(args_start, args_len).to_vec();
        args[0].value = random;
        let (args_start, args_len) = cfg.push_call_args(args);
        let CfgInstData::Call {
            args_start: stored_start,
            args_len: stored_len,
            ..
        } = &mut cfg.get_inst_mut(outer_call).data
        else {
            unreachable!("selected a Call")
        };
        *stored_start = args_start;
        *stored_len = args_len;

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
        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, outer_call));
        assert_eq!(unsupported.kind(), expected, "{call_name}");
    }
}

#[test]
fn user_call_layout_is_rejected_before_unmodeled_operands_run() {
    let source = r#"struct Pair { left: u32, right: u32 }
        fn normal(value: u32) -> u32 { value }
        fn borrowed(borrow value: u32) -> u32 { value }
        fn writable(inout value: u32) -> u32 { value }
        fn pair(value: Pair) -> u32 { value.left + value.right }
        fn main() -> i32 {
            let entropy: u32 = @random_u32();
            let mut value: u32 = 1;
            let values = Pair { left: 2, right: 3 };
            @intCast(
                normal(value)
                    + borrowed(borrow value)
                    + writable(inout value)
                    + pair(values)
                    + entropy
            )
        }"#;

    // Every source/target mode mismatch is one slot wide for this scalar, so
    // a total-width-only guard cannot distinguish it. The Pair probe instead
    // replaces a valid two-slot argument with the one-slot entropy value.
    for (callee_name, replacement_mode) in [
        ("normal", CfgArgMode::Borrow),
        ("normal", CfgArgMode::Inout),
        ("borrowed", CfgArgMode::Normal),
        ("borrowed", CfgArgMode::Inout),
        ("writable", CfgArgMode::Normal),
        ("writable", CfgArgMode::Borrow),
        ("pair", CfgArgMode::Normal),
    ] {
        let mut state = compile_to_cfg(source).expect("call-layout probe must compile");
        let main_index = state
            .functions
            .iter()
            .position(|function| function.cfg.fn_name() == "main")
            .expect("main CFG");
        let (random, call) = {
            let cfg = &state.functions[main_index].cfg;
            let mut random = None;
            let mut call = None;
            for value in cfg
                .blocks()
                .iter()
                .flat_map(|block| block.insts.iter().copied())
            {
                match &cfg.get_inst(value).data {
                    CfgInstData::Intrinsic { name, .. }
                        if state.interner.resolve(name) == "random_u32" =>
                    {
                        random = Some(value);
                    }
                    CfgInstData::Call { name, .. }
                        if state.interner.resolve(name) == callee_name =>
                    {
                        assert!(
                            call.replace(value).is_none(),
                            "expected exactly one call to {callee_name}"
                        );
                    }
                    _ => {}
                }
            }
            (
                random.expect("random intrinsic"),
                call.unwrap_or_else(|| panic!("missing call to {callee_name}")),
            )
        };

        let cfg = &mut state.functions[main_index].cfg;
        let (args_start, args_len) = match cfg.get_inst(call).data {
            CfgInstData::Call {
                args_start,
                args_len,
                ..
            } => (args_start, args_len),
            _ => unreachable!("selected a Call"),
        };
        let mut args = cfg.get_call_args(args_start, args_len).to_vec();
        assert_eq!(args.len(), 1, "{callee_name} probe arity");
        args[0].value = random;
        args[0].mode = replacement_mode;
        let (args_start, args_len) = cfg.push_call_args(args);
        let CfgInstData::Call {
            args_start: stored_start,
            args_len: stored_len,
            ..
        } = &mut cfg.get_inst_mut(call).data
        else {
            unreachable!("selected a Call")
        };
        *stored_start = args_start;
        *stored_len = args_len;

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
        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, call));
        assert_eq!(
            unsupported.kind(),
            UnsupportedKind::ContractViolation(ContractViolationKind::CallParameterLayout),
            "{callee_name} with {replacement_mode:?} must reject malformed static layout before @random_u32"
        );
    }
}

#[test]
fn abort_intrinsic_static_contracts_precede_unmodeled_operands() {
    let source = r#"fn main() -> i32 {
        let entropy: u32 = @random_u32();
        @assert(true, "ok");
        @panic("stop");
        if entropy == 0 { 0 } else { 1 }
    }"#;

    for probe in 0..5 {
        let mut state = compile_to_cfg(source).expect("abort-preflight probe must compile");
        let main_index = state
            .functions
            .iter()
            .position(|function| function.cfg.fn_name() == "main")
            .expect("main CFG");
        let (random, panic, panic_args, assertion, assert_args) = {
            let cfg = &state.functions[main_index].cfg;
            let (_, random, random_args, _) =
                find_intrinsic_in_function(&state, "main", "random_u32");
            assert!(random_args.is_empty());
            let (_, panic, panic_args, _) = find_intrinsic_in_function(&state, "main", "panic");
            let (_, assertion, assert_args, _) =
                find_intrinsic_in_function(&state, "main", "assert");
            assert!(cfg.get_inst(random).ty == Type::U32);
            (random, panic, panic_args, assertion, assert_args)
        };

        let cfg = &mut state.functions[main_index].cfg;
        let (outer, replacement_args, replacement_ty, expected) = match probe {
            0 => (
                panic,
                vec![panic_args[0], random],
                PANIC_CFG_RESULT_TYPE,
                ContractViolationKind::IntrinsicArity,
            ),
            1 => (
                panic,
                vec![random],
                PANIC_CFG_RESULT_TYPE,
                ContractViolationKind::IntrinsicSignature,
            ),
            2 => (
                assertion,
                vec![assert_args[0], assert_args[1], random],
                Type::UNIT,
                ContractViolationKind::IntrinsicArity,
            ),
            3 => (
                assertion,
                vec![random, assert_args[1]],
                Type::UNIT,
                ContractViolationKind::IntrinsicSignature,
            ),
            4 => (
                assertion,
                assert_args.clone(),
                Type::NEVER,
                ContractViolationKind::IntrinsicSignature,
            ),
            _ => unreachable!(),
        };
        let (args_start, args_len) = cfg.push_extra(replacement_args);
        let CfgInstData::Intrinsic {
            args_start: stored_start,
            args_len: stored_len,
            ..
        } = &mut cfg.get_inst_mut(outer).data
        else {
            unreachable!("selected an intrinsic")
        };
        *stored_start = args_start;
        *stored_len = args_len;
        cfg.get_inst_mut(outer).ty = replacement_ty;

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
        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, outer));
        assert_eq!(
            unsupported.kind(),
            UnsupportedKind::ContractViolation(expected),
            "probe {probe} must reject the malformed outer intrinsic before @random_u32"
        );
    }
}

#[test]
fn abort_intrinsics_require_exact_runtime_value_shapes() {
    let source = r#"fn main() -> i32 {
        @assert(true, "ok");
        @panic("stop");
        0
    }"#;

    for name in ["panic", "assert"] {
        let state = compile_to_cfg(source).expect("abort value-shape probe must compile");
        let (cfg, intrinsic, args, _) = find_intrinsic_in_function(&state, "main", name);
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
        let corrupted = if name == "panic" { args[0] } else { args[1] };
        frame
            .cache
            .insert(corrupted.as_u32(), Value::str_view("not owned"));

        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, intrinsic));
        assert_eq!(
            unsupported.kind(),
            UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
            "@{name} must reject a value whose runtime shape contradicts its owned-string type"
        );
    }

    let state = compile_to_cfg(source).expect("assert condition-shape probe must compile");
    let (cfg, assertion, args, _) = find_intrinsic_in_function(&state, "main", "assert");
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
    frame.cache.insert(args[0].as_u32(), Value::Int(1));
    let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, assertion));
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "@assert condition must be a runtime Bool, not merely truthy"
    );
}

#[test]
fn pointer_intrinsic_gaps_require_exact_signature_and_synthesized_provenance() {
    let mut preview_features = PreviewFeatures::new();
    preview_features.insert(rue_compiler::PreviewFeature::Slices);
    let source = r#"const ZERO: u64 = 0;
        fn slice_len(borrow s: [i32]) -> u64 { s.len() }
        fn user_pointer() -> u64 {
            checked {
                let p: ptr mut i32 = @int_to_ptr(ZERO);
                @ptr_to_int(p)
            }
        }
        fn offset_probe() -> u64 {
            let a = [1, 2];
            checked { @ptr_to_int(@ptr_offset(@raw(a[0]), 1)) }
        }
        fn main() -> i32 {
            let empty: [i32; 0] = [];
            @intCast(slice_len(borrow empty) + user_pointer() + offset_probe())
        }"#;
    let state = compile_to_cfg_with_preview_features(source, &preview_features)
        .expect("pointer provenance probe must compile");
    let interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };

    let (slice_cfg, slice_inst, slice_args, slice_result) =
        find_intrinsic_in_function(&state, "main", "int_to_ptr");
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            slice_cfg,
            slice_inst,
            "int_to_ptr",
            &slice_args,
            slice_result,
        ),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::EmptySlicePointer,
        ))
    );

    let (user_cfg, user_inst, user_args, user_result) =
        find_intrinsic_in_function(&state, "user_pointer", "int_to_ptr");
    assert!(matches!(
        user_cfg.get_inst(user_args[0]).data,
        CfgInstData::Const(0)
    ));
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            user_cfg,
            user_inst,
            "int_to_ptr",
            &user_args,
            user_result,
        ),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::IntToPointer,
        ))
    );
    let (offset_cfg, offset_inst, offset_args, offset_result) =
        find_intrinsic_in_function(&state, "offset_probe", "ptr_offset");
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            offset_cfg,
            offset_inst,
            "ptr_offset",
            &offset_args,
            offset_result,
        ),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerOffset,
        ))
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            offset_cfg,
            offset_inst,
            "ptr_offset",
            &offset_args[..1],
            offset_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity)
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            offset_cfg,
            offset_inst,
            "ptr_offset",
            &[offset_args[0], offset_args[0]],
            offset_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature)
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            offset_cfg,
            offset_inst,
            "ptr_offset",
            &offset_args,
            Type::U64,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "ptr_offset must return the exact input pointer type"
    );

    // Simulate compiler signature drift faithfully: mutate the instruction's
    // own result type, rather than only passing an inconsistent classifier
    // argument, then verify that a user-authored zero still lacks the exact
    // downstream slice-StructInit provenance.
    let mut drift_state = compile_to_cfg_with_preview_features(source, &preview_features)
        .expect("pointer provenance drift probe must compile");
    let (_, _, _, drift_slice_result) =
        find_intrinsic_in_function(&drift_state, "main", "int_to_ptr");
    let (_, drift_user_inst, _, _) =
        find_intrinsic_in_function(&drift_state, "user_pointer", "int_to_ptr");
    let drift_user_index = drift_state
        .functions
        .iter()
        .position(|function| function.cfg.fn_name() == "user_pointer")
        .expect("user_pointer CFG");
    drift_state.functions[drift_user_index]
        .cfg
        .get_inst_mut(drift_user_inst)
        .ty = drift_slice_result;
    let (drift_cfg, drift_inst, drift_args, drift_result) =
        find_intrinsic_in_function(&drift_state, "user_pointer", "int_to_ptr");
    let drift_interp = Interp {
        state: &drift_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    assert_eq!(
        drift_interp.classify_unsupported_intrinsic(
            drift_cfg,
            drift_inst,
            "int_to_ptr",
            &drift_args,
            drift_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "a user-authored zero is not the synthesized empty-slice pointer"
    );

    let mut extra_use_state = compile_to_cfg_with_preview_features(source, &preview_features)
        .expect("empty-slice exclusive-use probe must compile");
    let (_, extra_slice_inst, _, _) =
        find_intrinsic_in_function(&extra_use_state, "main", "int_to_ptr");
    let extra_main = extra_use_state
        .functions
        .iter()
        .position(|function| function.cfg.fn_name() == "main")
        .expect("main CFG");
    let outer_cast = extra_use_state.functions[extra_main]
        .cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
        .find(|value| {
            matches!(
                extra_use_state.functions[extra_main]
                    .cfg
                    .get_inst(*value)
                    .data,
                CfgInstData::IntCast { .. }
            )
        })
        .expect("outer result cast");
    extra_use_state.functions[extra_main]
        .cfg
        .get_inst_mut(outer_cast)
        .data = CfgInstData::IntCast {
        value: extra_slice_inst,
        from_ty: drift_slice_result,
    };
    let (extra_cfg, extra_inst, extra_args, extra_result) =
        find_intrinsic_in_function(&extra_use_state, "main", "int_to_ptr");
    let extra_interp = Interp {
        state: &extra_use_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    assert_eq!(
        extra_interp.classify_unsupported_intrinsic(
            extra_cfg,
            extra_inst,
            "int_to_ptr",
            &extra_args,
            extra_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "an extra use cannot borrow synthesized empty-slice provenance"
    );

    let mut extra_init_use_state = compile_to_cfg_with_preview_features(source, &preview_features)
        .expect("empty-slice StructInit exclusive-use probe must compile");
    let (_, extra_init_pointer, _, _) =
        find_intrinsic_in_function(&extra_init_use_state, "main", "int_to_ptr");
    let extra_init_main = extra_init_use_state
        .functions
        .iter()
        .position(|function| function.cfg.fn_name() == "main")
        .expect("main CFG");
    let (slice_init, slice_ty, outer_cast) = {
        let cfg = &extra_init_use_state.functions[extra_init_main].cfg;
        let slice_init = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| {
                let CfgInstData::StructInit {
                    fields_start,
                    fields_len,
                    ..
                } = cfg.get_inst(*value).data
                else {
                    return false;
                };
                cfg.get_extra(fields_start, fields_len).first() == Some(&extra_init_pointer)
            })
            .expect("empty-slice StructInit");
        let outer_cast = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| matches!(cfg.get_inst(*value).data, CfgInstData::IntCast { .. }))
            .expect("outer result cast");
        (slice_init, cfg.get_inst(slice_init).ty, outer_cast)
    };
    extra_init_use_state.functions[extra_init_main]
        .cfg
        .get_inst_mut(outer_cast)
        .data = CfgInstData::IntCast {
        value: slice_init,
        from_ty: slice_ty,
    };
    let (extra_init_cfg, extra_init_inst, extra_init_args, extra_init_result) =
        find_intrinsic_in_function(&extra_init_use_state, "main", "int_to_ptr");
    assert_eq!(extra_init_cfg.value_use_count(extra_init_inst), 1);
    assert_eq!(extra_init_cfg.value_use_count(slice_init), 2);
    let extra_init_interp = Interp {
        state: &extra_init_use_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    assert_eq!(
        extra_init_interp.classify_unsupported_intrinsic(
            extra_init_cfg,
            extra_init_inst,
            "int_to_ptr",
            &extra_init_args,
            extra_init_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "an extra slice-StructInit use cannot borrow synthesized empty-slice provenance"
    );

    let mut wrong_consumer_state = compile_to_cfg_with_preview_features(source, &preview_features)
        .expect("empty-slice consumer-mode probe must compile");
    let (_, wrong_consumer_pointer, _, _) =
        find_intrinsic_in_function(&wrong_consumer_state, "main", "int_to_ptr");
    let wrong_consumer_main = wrong_consumer_state
        .functions
        .iter()
        .position(|function| function.cfg.fn_name() == "main")
        .expect("main CFG");
    let (wrong_consumer_init, consumer, mut consumer_args) = {
        let cfg = &wrong_consumer_state.functions[wrong_consumer_main].cfg;
        let init = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| {
                let CfgInstData::StructInit {
                    fields_start,
                    fields_len,
                    ..
                } = cfg.get_inst(*value).data
                else {
                    return false;
                };
                cfg.get_extra(fields_start, fields_len).first() == Some(&wrong_consumer_pointer)
            })
            .expect("empty-slice StructInit");
        let (consumer, args) = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| {
                let CfgInstData::Call {
                    args_start,
                    args_len,
                    ..
                } = cfg.get_inst(value).data
                else {
                    return None;
                };
                let args = cfg.get_call_args(args_start, args_len);
                args.iter()
                    .any(|arg| arg.value == init)
                    .then(|| (value, args.to_vec()))
            })
            .expect("empty-slice call consumer");
        (init, consumer, args)
    };
    let slice_arg = consumer_args
        .iter_mut()
        .find(|arg| arg.value == wrong_consumer_init)
        .expect("empty-slice call argument");
    assert_eq!(slice_arg.mode, CfgArgMode::Normal);
    slice_arg.mode = CfgArgMode::Borrow;
    let cfg = &mut wrong_consumer_state.functions[wrong_consumer_main].cfg;
    let (args_start, args_len) = cfg.push_call_args(consumer_args);
    let CfgInstData::Call {
        args_start: old_start,
        args_len: old_len,
        ..
    } = &mut cfg.get_inst_mut(consumer).data
    else {
        unreachable!("consumer stopped being a call")
    };
    *old_start = args_start;
    *old_len = args_len;
    let (wrong_consumer_cfg, wrong_consumer_inst, wrong_consumer_args, wrong_consumer_result) =
        find_intrinsic_in_function(&wrong_consumer_state, "main", "int_to_ptr");
    assert_eq!(wrong_consumer_cfg.value_use_count(wrong_consumer_inst), 1);
    assert_eq!(wrong_consumer_cfg.value_use_count(wrong_consumer_init), 1);
    let wrong_consumer_interp = Interp {
        state: &wrong_consumer_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    assert_eq!(
        wrong_consumer_interp.classify_unsupported_intrinsic(
            wrong_consumer_cfg,
            wrong_consumer_inst,
            "int_to_ptr",
            &wrong_consumer_args,
            wrong_consumer_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "the synthesized slice StructInit must have one normal call consumer"
    );

    let mut wrong_init_state = compile_to_cfg_with_preview_features(source, &preview_features)
        .expect("empty-slice StructInit type probe must compile");
    let (_, wrong_init_pointer, _, _) =
        find_intrinsic_in_function(&wrong_init_state, "main", "int_to_ptr");
    let wrong_init_main = wrong_init_state
        .functions
        .iter()
        .position(|function| function.cfg.fn_name() == "main")
        .expect("main CFG");
    let slice_init = {
        let cfg = &wrong_init_state.functions[wrong_init_main].cfg;
        cfg.blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| {
                let CfgInstData::StructInit {
                    fields_start,
                    fields_len,
                    ..
                } = cfg.get_inst(*value).data
                else {
                    return false;
                };
                cfg.get_extra(fields_start, fields_len).first() == Some(&wrong_init_pointer)
            })
            .expect("empty-slice StructInit")
    };
    wrong_init_state.functions[wrong_init_main]
        .cfg
        .get_inst_mut(slice_init)
        .ty = Type::UNIT;
    let (wrong_init_cfg, wrong_init_inst, wrong_init_args, wrong_init_result) =
        find_intrinsic_in_function(&wrong_init_state, "main", "int_to_ptr");
    let wrong_init_interp = Interp {
        state: &wrong_init_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    assert_eq!(
        wrong_init_interp.classify_unsupported_intrinsic(
            wrong_init_cfg,
            wrong_init_inst,
            "int_to_ptr",
            &wrong_init_args,
            wrong_init_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "the slice StructInit result type must match its struct metadata"
    );
}

#[test]
fn field_pointer_gaps_require_in_bounds_projection_metadata() {
    let source = "struct Pair { a: i32, b: i32 }
        fn main() -> i32 {
            let mut p = Pair { a: 1, b: 2 };
            checked { @intCast(@ptr_to_int(@field_ptr(p.a))) }
        }";
    let mut state = compile_to_cfg(source).expect("field-pointer metadata probe must compile");
    {
        let (cfg, inst, args, result) = find_intrinsic_in_function(&state, "main", "field_ptr");
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
            interp.classify_unsupported_intrinsic(cfg, inst, "field_ptr", &args, result),
            UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
                UnsupportedIntrinsicKind::FieldPointer,
            ))
        );
    }

    let main_index = state
        .functions
        .iter()
        .position(|function| function.cfg.fn_name() == "main")
        .expect("main CFG");
    let (_, _, args, _) = find_intrinsic_in_function(&state, "main", "field_ptr");
    let field_read = args[0];
    let (base, base_type, struct_id) = {
        let cfg = &state.functions[main_index].cfg;
        let CfgInstData::PlaceRead { place } = &cfg.get_inst(field_read).data else {
            panic!("field_ptr argument must be a PlaceRead")
        };
        let Some(Projection::Field { struct_id, .. }) =
            cfg.get_place_projections(place).last().copied()
        else {
            panic!("field_ptr argument must end in a Field projection")
        };
        (place.base, place.base_type, struct_id)
    };
    let out_of_bounds = state.type_pool.struct_def(struct_id).field_count() as u32;
    let invalid_place = state.functions[main_index].cfg.make_place(
        base,
        base_type,
        [Projection::Field {
            struct_id,
            field_index: out_of_bounds,
        }],
    );
    state.functions[main_index]
        .cfg
        .get_inst_mut(field_read)
        .data = CfgInstData::PlaceRead {
        place: invalid_place,
    };
    let (cfg, inst, args, result) = find_intrinsic_in_function(&state, "main", "field_ptr");
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
        interp.classify_unsupported_intrinsic(cfg, inst, "field_ptr", &args, result),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature)
    );
}

#[test]
fn option_returning_intrinsics_require_the_exact_payload_type() {
    let state = compile_to_cfg(
        r#"fn Option(comptime T: type) -> type { enum { Some(T), None } }
        fn parse32() -> i32 {
            let O = Option(i32);
            match @parse_i32("1") { O.Some(n) => n, O.None => 0 }
        }
        fn parse64() -> i32 {
            let O = Option(i64);
            match @parse_i64("2") { O.Some(n) => @intCast(n), O.None => 0 }
        }
        fn line() -> i32 {
            let O = Option(String);
            match @read_line() { O.Some(_s) => 1, O.None => 0 }
        }
        fn main() -> i32 { parse32() + parse64() + line() }"#,
    )
    .expect("Option intrinsic signature probe must compile");
    let interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
    };
    let (parse32_cfg, parse32_inst, parse32_args, parse32_result) =
        find_intrinsic_in_function(&state, "parse32", "parse_i32");
    let (parse64_cfg, parse64_inst, parse64_args, parse64_result) =
        find_intrinsic_in_function(&state, "parse64", "parse_i64");
    let (line_cfg, line_inst, line_args, line_result) =
        find_intrinsic_in_function(&state, "line", "read_line");

    assert_eq!(
        interp.classify_unsupported_intrinsic(
            parse32_cfg,
            parse32_inst,
            "parse_i32",
            &parse32_args,
            parse32_result,
        ),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::ParseI32,
        ))
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            parse64_cfg,
            parse64_inst,
            "parse_i64",
            &parse64_args,
            parse64_result,
        ),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::ParseI64,
        ))
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            line_cfg,
            line_inst,
            "read_line",
            &line_args,
            line_result,
        ),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::StandardInput)
    );

    assert_eq!(
        interp.classify_unsupported_intrinsic(
            parse32_cfg,
            parse32_inst,
            "parse_i32",
            &parse32_args,
            parse64_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "Option(i64) is not a valid parse_i32 result"
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            line_cfg,
            line_inst,
            "read_line",
            &line_args,
            parse32_result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "an arbitrary Option is not a valid read_line result"
    );
}
