//! Adversarial contracts for calls, intrinsics, and pointer provenance.

use super::*;

fn expect_flow_unsupported<T>(result: Step<T>) -> Unsupported {
    match result {
        Err(Flow::Unsupported(unsupported)) => unsupported,
        Err(Flow::Panic(_)) => panic!("expected Unsupported, got a modeled panic"),
        Ok(_) => panic!("expected Unsupported, got a value"),
    }
}

fn expect_modeled_value(result: Step<Option<Value>>) -> Option<Value> {
    match result {
        Ok(value) => value,
        Err(Flow::Unsupported(_)) => panic!("expected a modeled value, got Unsupported"),
        Err(Flow::Panic(_)) => panic!("expected a modeled value, got a panic"),
    }
}

fn find_call_metadata(
    state: &CompileState,
    expected_name: &str,
) -> (Vec<Type>, Vec<CfgArgMode>, Type) {
    for function in &state.functions {
        for block in function.cfg.blocks() {
            for &value in &block.insts {
                let inst = function.cfg.get_inst(value);
                let CfgInstData::Call { name, .. } = &inst.data else {
                    continue;
                };
                if function.interner.resolve(name) == expected_name {
                    let args = function.cfg.get_call_args(&inst.data);
                    return (
                        args.iter()
                            .map(|arg| function.cfg.get_inst(arg.value).ty)
                            .collect(),
                        args.iter().map(|arg| arg.mode).collect(),
                        inst.ty,
                    );
                }
            }
        }
    }
    panic!("missing call to {expected_name}")
}

#[test]
fn stable_text_runtime_calls_require_exact_metadata() {
    use SemanticGapKind as Semantic;
    use UnsupportedRuntimeCallKind as RuntimeCall;

    let state = query_cfg_state("fn main() -> i32 { print(\"x\"); println(\"y\"); 0 }")
        .expect("stable text print probes must compile");
    let interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
    };
    for (runtime, kind) in [
        (RuntimeCallKind::StrPrintAggregate, RuntimeCall::Print),
        (RuntimeCallKind::StrPrintlnAggregate, RuntimeCall::Println),
    ] {
        let name = runtime.helper().symbol();
        let (types, modes, result) = find_call_metadata(&state, name);
        let values = [Value::str_view("x")];
        assert_eq!(
            interp.classify_unsupported_runtime_call(runtime, &values, &types, &modes, result),
            UnsupportedKind::SemanticGap(Semantic::RuntimeCall(kind))
        );
        for values in [&[][..], &[Value::str_view("x"), Value::str_view("y")][..]] {
            assert_eq!(
                interp.classify_unsupported_runtime_call(runtime, values, &types, &modes, result),
                UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity)
            );
        }
        for (types, modes) in [
            (&[][..], &[][..]),
            (
                &[types[0], types[0], types[0]][..],
                &[CfgArgMode::Normal; 3][..],
            ),
            (&types[..], &[][..]),
        ] {
            assert_eq!(
                interp.classify_unsupported_runtime_call(runtime, &values, types, modes, result),
                UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallArity)
            );
        }
        assert_eq!(
            interp.classify_unsupported_runtime_call(
                runtime,
                &values,
                &[Type::BOOL],
                &modes,
                result,
            ),
            UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
        );
        assert_eq!(
            interp.classify_unsupported_runtime_call(
                runtime,
                &values,
                &types,
                &[CfgArgMode::Borrow],
                result,
            ),
            UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
        );
        assert_eq!(
            interp.classify_unsupported_runtime_call(
                runtime,
                &[Value::Int(0)],
                &types,
                &modes,
                result,
            ),
            UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
        );
        assert_eq!(
            interp.classify_unsupported_runtime_call(runtime, &values, &types, &modes, Type::BOOL,),
            UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
        );
    }
    for runtime in [
        RuntimeCallKind::StrByteAt,
        RuntimeCallKind::DebugI64,
        RuntimeCallKind::Alloc,
    ] {
        assert_eq!(
            interp.classify_unsupported_runtime_call(runtime, &[], &[], &[], Type::UNIT),
            UnsupportedKind::ContractViolation(ContractViolationKind::MissingFunctionBody)
        );
    }
}

#[test]
fn random_intrinsic_requires_exact_arity_and_result_type() {
    let state = query_cfg_state("fn main() -> i32 { let n: u32 = @random_u32(); @intCast(n) }")
        .expect("random probe must compile");
    let (cfg, inst, args, result) = find_intrinsic_in_function(&state, "main", "random_u32");
    let interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
    };
    assert_eq!(
        interp.classify_unsupported_intrinsic(cfg, inst, "random_u32", &args, result),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::RandomU32)
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(cfg, inst, "random_u32", &[inst], result),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity)
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(cfg, inst, "random_u32", &args, Type::U64),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature)
    );
}

fn find_intrinsic_in_function<'a>(
    state: &'a CompileState,
    function_name: &str,
    expected_name: &str,
) -> (&'a Cfg, CfgValue, Vec<CfgValue>, Type) {
    let function = state
        .functions
        .iter()
        .position(|function| function.is_source_named(function_name))
        .unwrap_or_else(|| panic!("missing CFG for {function_name}"));
    let cfg = &state.functions[function].cfg;
    let mut found = None;
    for value in cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
    {
        let inst = cfg.get_inst(value);
        let CfgInstData::Intrinsic { name, .. } = &inst.data else {
            continue;
        };
        if state.functions[function].interner.resolve(name) != expected_name {
            continue;
        }
        let item = (value, cfg.get_intrinsic_args(&inst.data).to_vec(), inst.ty);
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
fn shared_str_character_builtins_require_and_model_ptr_len_offset() {
    let state = query_cfg_state(
        "fn main() -> i32 { checked { let pointer: ptr mut u8 = @alloc(1, 1); @free(pointer, 1, 1); }; 0 }",
    )
    .expect("probe must compile");
    let mut interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
    };
    // The builtin contract only needs the logical pointer kind. Look up the
    // pointer registered by the rooted main body rather than mutating the
    // completed universe.
    let ptr = Type::new_ptr_mut(
        state
            .type_pool()
            .get_ptr_mut_by_type(Type::U8)
            .expect("Probe must register ptr mut u8"),
    );
    let types = [ptr, Type::U64, Type::U64];
    let modes = [CfgArgMode::Normal; 3];
    // The projected-char builtins take a raw text pointer + length; back it with
    // a real heap allocation ("hé" is 3 bytes).
    let text_ptr = interp.test_alloc_str_ptr("hé".as_bytes());
    let args = [text_ptr, Value::Int(3), Value::Int(1)];

    assert_eq!(
        expect_modeled_value(interp.string_builtin(
            RuntimeCallKind::StrCharScalar,
            &args,
            &types,
            &modes,
            Type::U32,
        )),
        Some(Value::Int('é' as i128))
    );
    assert_eq!(
        expect_modeled_value(interp.string_builtin(
            RuntimeCallKind::StrCharNext,
            &args,
            &types,
            &modes,
            Type::U64,
        )),
        Some(Value::Int(3))
    );

    let invalid_ptr = interp.test_alloc_str_ptr(&[b'a', 0xff, b'b']);
    let invalid = [invalid_ptr, Value::Int(3), Value::Int(1)];
    assert_eq!(
        expect_modeled_value(interp.string_builtin(
            RuntimeCallKind::StrCharScalarLossy,
            &invalid,
            &types,
            &modes,
            Type::U32,
        )),
        Some(Value::Int('\u{fffd}' as i128))
    );
    match interp.string_builtin(
        RuntimeCallKind::StrCharScalar,
        &invalid,
        &types,
        &modes,
        Type::U32,
    ) {
        Err(Flow::Panic(panic)) => assert_eq!(panic.kind, TrapKind::InvalidUtf8),
        _ => panic!("strict invalid UTF-8 must trap"),
    }

    let arity = expect_flow_unsupported(interp.string_builtin(
        RuntimeCallKind::StrCharScalar,
        &args[..2],
        &types[..2],
        &modes[..2],
        Type::U32,
    ));
    assert_eq!(
        arity.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArity)
    );
    let wrong_types = [Type::U64, Type::U64, Type::U64];
    let signature = expect_flow_unsupported(interp.string_builtin(
        RuntimeCallKind::StrCharScalar,
        &args,
        &wrong_types,
        &modes,
        Type::U32,
    ));
    assert_eq!(
        signature.kind(),
        UnsupportedKind::ContractViolation(ContractViolationKind::BuiltinArgumentType)
    );
}

#[test]
fn panic_never_signature_is_an_oracle_contract_for_both_arities() {
    let state = query_cfg_state(
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
        heap: Vec::new(),
    };
    for function_name in ["panic_no_message", "panic_with_message"] {
        state.select_source_function(function_name);
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
        let mut state = query_cfg_state(source).expect("call-layout probe must compile");
        let main_index = state
            .functions
            .iter()
            .position(|function| function.is_source_named("main"))
            .expect("main CFG");
        // A call site names its callee by internal symbol, so the probe reads
        // that symbol off the callee's own CFG rather than assuming a spelling.
        let callee_symbol = state
            .functions
            .iter()
            .find(|function| function.is_source_named(callee_name))
            .map(|function| function.cfg.fn_name().to_owned())
            .unwrap_or_else(|| panic!("missing CFG for {callee_name}"));
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
                        if state.interner().resolve(name) == "random_u32" =>
                    {
                        random = Some(value);
                    }
                    CfgInstData::Call { name, .. }
                        if state.interner().resolve(name) == callee_symbol =>
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

        let type_pool = state.type_pool().clone();
        let cfg = &mut state.functions[main_index].cfg;
        let mut args = cfg.get_call_args(&cfg.get_inst(call).data).to_vec();
        assert_eq!(args.len(), 1, "{callee_name} probe arity");
        args[0].value = random;
        args[0].mode = replacement_mode;
        cfg.try_edit(&type_pool, |editor| editor.replace_call_args(call, args))
            .unwrap();

        let cfg = &state.functions[main_index].cfg;
        let mut interp = Interp {
            state: &state,
            stdout: String::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
            heap: Vec::new(),
        };
        let mut frame = Frame {
            params: Vec::new(),
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
            promoted: HashMap::new(),
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
        let mut state = query_cfg_state(source).expect("abort-preflight probe must compile");
        let main_index = state
            .functions
            .iter()
            .position(|function| function.is_source_named("main"))
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

        let type_pool = state.type_pool().clone();
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
        cfg.try_edit(&type_pool, |editor| {
            editor.replace_intrinsic_args(outer, replacement_args)?;
            editor.replace_inst_type(outer, replacement_ty)?;
            Ok::<_, rue_cfg::CfgEditError>(())
        })
        .unwrap();

        let cfg = &state.functions[main_index].cfg;
        let mut interp = Interp {
            state: &state,
            stdout: String::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
            heap: Vec::new(),
        };
        let mut frame = Frame {
            params: Vec::new(),
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
            promoted: HashMap::new(),
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
        let state = query_cfg_state(source).expect("abort value-shape probe must compile");
        let (cfg, intrinsic, args, _) = find_intrinsic_in_function(&state, "main", name);
        let mut interp = Interp {
            state: &state,
            stdout: String::new(),
            stdout_bytes: 0,
            stdout_cap: MAX_STDOUT_BYTES,
            stderr_cap: MAX_STDERR_BYTES,
            budget: STEP_BUDGET,
            depth: 0,
            heap: Vec::new(),
        };
        let mut frame = Frame {
            params: Vec::new(),
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
            promoted: HashMap::new(),
        };
        let corrupted = if name == "panic" { args[0] } else { args[1] };
        frame.cache.insert(corrupted.as_u32(), Value::Int(7));

        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, intrinsic));
        assert_eq!(
            unsupported.kind(),
            UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
            "@{name} must reject a non-text runtime value"
        );
    }

    let state = query_cfg_state(source).expect("assert condition-shape probe must compile");
    let (cfg, assertion, args, _) = find_intrinsic_in_function(&state, "main", "assert");
    let mut interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
    };
    let mut frame = Frame {
        params: Vec::new(),
        locals: vec![None; cfg.num_locals() as usize],
        cache: HashMap::new(),
        promoted: HashMap::new(),
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
    let preview_features = PreviewFeatures::new();
    let source = r#"const ZERO: u64 = 0;
        fn slice_len(borrow s: [i32]) -> u64 { s.len() }
        fn user_pointer(borrow s: [i32]) -> u64 {
            checked {
                let p: ptr mut i32 = @int_to_ptr(ZERO);
                @ptr_to_int(p) + s.len()
            }
        }
        fn offset_probe() -> u64 {
            let a = [1, 2];
            checked { @ptr_to_int(@ptr_offset(@raw(a[0]), 1)) }
        }
        fn main() -> i32 {
            let empty: [i32; 0] = [];
            let values = [1, 2];
            @intCast(slice_len(borrow empty) + user_pointer(borrow values) + offset_probe())
        }"#;
    let state = query_cfg_state(source).expect("pointer provenance probe must compile");
    let interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
    };

    let (slice_cfg, slice_inst, slice_args, slice_result) =
        find_intrinsic_in_function(&state, "main", "int_to_ptr");
    state.select_source_function("main");
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
    state.select_source_function("user_pointer");
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
    state.select_source_function("offset_probe");
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
    let mut drift_state = query_cfg_state_with_preview_features(source, &preview_features)
        .expect("pointer provenance drift probe must compile");
    let (_, _, _, drift_slice_result) =
        find_intrinsic_in_function(&drift_state, "main", "int_to_ptr");
    let main_index = drift_state.select_source_function("main");
    let (slice_pointer_is_mut, slice_pointee) = match drift_slice_result.kind() {
        TypeKind::PtrConst(id) => (
            false,
            drift_state.functions[main_index]
                .type_pool
                .ptr_const_def(id),
        ),
        TypeKind::PtrMut(id) => (
            true,
            drift_state.functions[main_index].type_pool.ptr_mut_def(id),
        ),
        _ => panic!("empty-slice pointer synthesis must return a pointer"),
    };
    let (_, drift_user_inst, _, _) =
        find_intrinsic_in_function(&drift_state, "user_pointer", "int_to_ptr");
    let drift_user_index = drift_state
        .functions
        .iter()
        .position(|function| function.is_source_named("user_pointer"))
        .expect("user_pointer CFG");
    drift_state.select_source_function("user_pointer");
    let type_pool = drift_state.type_pool().clone();
    let drift_slice_result = if slice_pointer_is_mut {
        Type::new_ptr_mut(
            type_pool
                .all_ptr_mut_ids()
                .find(|id| type_pool.ptr_mut_def(*id) == slice_pointee)
                .expect("user-pointer domain contains the synthesized slice pointer type"),
        )
    } else {
        Type::new_ptr_const(
            type_pool
                .all_ptr_const_ids()
                .find(|id| type_pool.ptr_const_def(*id) == slice_pointee)
                .expect("user-pointer domain contains the synthesized slice pointer type"),
        )
    };
    drift_state.functions[drift_user_index]
        .cfg
        .try_edit(&type_pool, |editor| {
            editor.replace_inst_type(drift_user_inst, drift_slice_result)
        })
        .unwrap();
    let (drift_cfg, drift_inst, drift_args, drift_result) =
        find_intrinsic_in_function(&drift_state, "user_pointer", "int_to_ptr");
    drift_state.select_source_function("user_pointer");
    let drift_interp = Interp {
        state: &drift_state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
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

    let mut extra_use_state = query_cfg_state_with_preview_features(source, &preview_features)
        .expect("empty-slice exclusive-use probe must compile");
    let (_, extra_slice_inst, _, extra_slice_result) =
        find_intrinsic_in_function(&extra_use_state, "main", "int_to_ptr");
    let extra_main = extra_use_state
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
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
    let type_pool = extra_use_state.type_pool().clone();
    extra_use_state.functions[extra_main]
        .cfg
        .try_edit(&type_pool, |editor| {
            editor.replace_int_cast(outer_cast, extra_slice_inst, extra_slice_result)
        })
        .unwrap();
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
        heap: Vec::new(),
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

    let mut extra_init_use_state = query_cfg_state_with_preview_features(source, &preview_features)
        .expect("empty-slice StructInit exclusive-use probe must compile");
    let (_, extra_init_pointer, _, _) =
        find_intrinsic_in_function(&extra_init_use_state, "main", "int_to_ptr");
    let extra_init_main = extra_init_use_state
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
        .expect("main CFG");
    let (slice_init, slice_ty, outer_cast) = {
        let cfg = &extra_init_use_state.functions[extra_init_main].cfg;
        let slice_init = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| {
                let data = &cfg.get_inst(*value).data;
                let CfgInstData::StructInit { .. } = data else {
                    return false;
                };
                cfg.get_struct_fields(data).first() == Some(&extra_init_pointer)
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
    let type_pool = extra_init_use_state.type_pool().clone();
    extra_init_use_state.functions[extra_init_main]
        .cfg
        .try_edit(&type_pool, |editor| {
            editor.replace_int_cast(outer_cast, slice_init, slice_ty)
        })
        .unwrap();
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
        heap: Vec::new(),
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

    let mut wrong_consumer_state = query_cfg_state_with_preview_features(source, &preview_features)
        .expect("empty-slice consumer-mode probe must compile");
    let (_, wrong_consumer_pointer, _, _) =
        find_intrinsic_in_function(&wrong_consumer_state, "main", "int_to_ptr");
    let wrong_consumer_main = wrong_consumer_state
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
        .expect("main CFG");
    let (wrong_consumer_init, consumer, mut consumer_args) = {
        let cfg = &wrong_consumer_state.functions[wrong_consumer_main].cfg;
        let init = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| {
                let data = &cfg.get_inst(*value).data;
                let CfgInstData::StructInit { .. } = data else {
                    return false;
                };
                cfg.get_struct_fields(data).first() == Some(&wrong_consumer_pointer)
            })
            .expect("empty-slice StructInit");
        let (consumer, args) = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| {
                let data = &cfg.get_inst(value).data;
                let CfgInstData::Call { .. } = data else {
                    return None;
                };
                let args = cfg.get_call_args(data);
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
    let type_pool = wrong_consumer_state.type_pool().clone();
    let cfg = &mut wrong_consumer_state.functions[wrong_consumer_main].cfg;
    cfg.try_edit(&type_pool, |editor| {
        editor.replace_call_args(consumer, consumer_args)
    })
    .unwrap();
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
        heap: Vec::new(),
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

    let mut wrong_init_state = query_cfg_state_with_preview_features(source, &preview_features)
        .expect("empty-slice StructInit type probe must compile");
    let (_, wrong_init_pointer, _, _) =
        find_intrinsic_in_function(&wrong_init_state, "main", "int_to_ptr");
    let wrong_init_main = wrong_init_state
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
        .expect("main CFG");
    let slice_init = {
        let cfg = &wrong_init_state.functions[wrong_init_main].cfg;
        cfg.blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| {
                let data = &cfg.get_inst(*value).data;
                let CfgInstData::StructInit { .. } = data else {
                    return false;
                };
                cfg.get_struct_fields(data).first() == Some(&wrong_init_pointer)
            })
            .expect("empty-slice StructInit")
    };
    let type_pool = wrong_init_state.type_pool().clone();
    wrong_init_state.functions[wrong_init_main]
        .cfg
        .try_edit(&type_pool, |editor| {
            editor.replace_inst_type(slice_init, Type::UNIT)
        })
        .unwrap();
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
        heap: Vec::new(),
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
fn validated_cfg_rejects_out_of_bounds_field_pointer_projection_metadata() {
    let source = "struct Pair { a: i32, b: i32 }
        fn main() -> i32 {
            let mut p = Pair { a: 1, b: 2 };
            checked { @intCast(@ptr_to_int(@field_ptr(p.a))) }
        }";
    let mut state = query_cfg_state(source).expect("field-pointer metadata probe must compile");
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
            heap: Vec::new(),
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
        .position(|function| function.is_source_named("main"))
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
    let out_of_bounds = state.type_pool().struct_def(struct_id).field_count() as u32;
    let type_pool = state.type_pool().clone();
    let error = state.functions[main_index]
        .cfg
        .try_edit(&type_pool, |editor| {
            editor.replace_place_read(
                field_read,
                base,
                base_type,
                [Projection::Field {
                    struct_id,
                    field_index: out_of_bounds,
                }],
            )
        })
        .expect_err("ValidatedCfg must reject an out-of-bounds field projection");
    assert!(matches!(
        error,
        rue_cfg::CfgEditTransactionError::Verification(_)
    ));
}

/// Compile `root` with the given trusted standard-library modules present,
/// driving real import discovery so each std module is acquired at its physical
/// path under the discovery std root and receives trusted-standard-library
/// provenance. Each entry is `(file_index, canonical_physical_path, source)`;
/// `root` reaches a module by importing that physical path (e.g.
/// `@import("std/option.rue")`).
///
/// Trusted provenance is what a fallible intrinsic's result and `?` require:
/// they bind the exact std `Option`/`Result`, so a fixture exercising them must
/// name the real std producer rather than a same-shape local lookalike. The
/// lightweight fixture-import graph used inside the compiler crate is
/// `cfg(test)`-only and unavailable here, so the oracle drives the supported
/// discovery loop end to end.
fn query_cfg_state_with_trusted_std(
    root: &str,
    std_modules: &[(u32, &str, &str)],
) -> Result<CompileState, CompileErrors> {
    use rue_compiler::unstable::{
        AcceptedImportSource, DiscoverySourceAssembler, ImportDemandMode, ImportObservation,
        begin_import_input_request, close_import_input_request, import_demand_frontier_for_roots,
        import_observation_ledger, publish_import_observation_batch, stage_import_input_request,
    };
    use rue_compiler::{
        CompilerSession, FileMetadataFingerprint, ImportDiscoveryContext, PhysicalFileIdentity,
    };
    use std::sync::Arc;

    let context =
        ImportDiscoveryContext::new(1, "/project", Some("/project/std"), "oracle-trusted-std")
            .expect("valid discovery context");
    let root = Arc::new(root.to_owned());
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        "/project/main.rue",
        "/project/main.rue",
        PhysicalFileIdentity::new(1, 1),
        FileMetadataFingerprint::new(root.len() as u64, 0, 0),
        root,
    )
    .expect("root source assembler");
    let std_sources = std_modules
        .iter()
        .map(|(index, path, source)| (*index as u64, *path, Arc::new(source.to_string())))
        .collect::<Vec<_>>();

    let mut session = CompilerSession::new();
    let initial = assembler.snapshot().expect("trusted std root snapshot");
    let mut revision = begin_import_input_request(
        &mut session,
        &initial,
        context.clone(),
        assembler.accepted_read_manifest(),
    )
    .expect("begin trusted std request");
    loop {
        let ledger = import_observation_ledger(&session, revision).expect("current std ledger");
        let plan = stage_import_input_request(&mut session, revision)
            .expect("valid trusted std discovery plan");
        let frontier = import_demand_frontier_for_roots(
            &mut session,
            revision,
            &plan,
            ImportDemandMode::Rooted,
            &plan.demand_roots(),
        )
        .expect("trusted std frontier");
        if frontier.requests().is_empty() {
            close_import_input_request(&mut session, revision)
                .expect("close valid import discovery revision");
            break;
        }
        let observations = frontier
            .requests()
            .iter()
            .cloned()
            .map(|request| {
                let requested = request.requested_path();
                let (index, canonical, source) = std_sources
                    .iter()
                    .find(|(_, path, _)| *path == requested)
                    .unwrap_or_else(|| panic!("unexpected import request {requested}"));
                let accepted = AcceptedImportSource::new(
                    requested,
                    *canonical,
                    PhysicalFileIdentity::new(1, *index),
                    FileMetadataFingerprint::new(source.len() as u64, 0, 0),
                    source.clone(),
                )
                .expect("accepted trusted standard-library source");
                ImportObservation::accepted(request, accepted)
                    .expect("observation matches discovery request")
            })
            .collect::<Vec<_>>();
        let mut assembly_ledger = ledger;
        for observation in observations.iter().cloned() {
            assembly_ledger
                .record(observation)
                .expect("unique representative observation");
        }
        assembler
            .add_plan_reads(&plan, &assembly_ledger)
            .expect("assemble accepted std reads");
        let successor = assembler.snapshot().expect("successor std snapshot");
        revision = publish_import_observation_batch(
            &mut session,
            &frontier,
            &successor,
            assembler.accepted_read_manifest(),
            observations,
        )
        .expect("publish std observation batch");
    }
    query_cfg_state_from_session(session, &CompileOptions::default())
}

/// The trusted standard-library `Option` producer source, provided verbatim at
/// the `\0rue-std/option.rue` identity by `query_cfg_state_with_trusted_std`.
const TRUSTED_OPTION_MODULE_SOURCE: &str =
    "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }";

#[test]
fn option_returning_intrinsics_require_the_exact_payload_type() {
    // A fallible intrinsic's result is the exact trusted std `Option(payload)`
    // (RUE-1112), so the match arms must name the real std `Option`, imported
    // from the trusted module — a same-shape local `fn Option` lookalike is a
    // different producer and is rejected as the intrinsic annotation.
    let state = query_cfg_state_with_trusted_std(
        r#"const option = @import("std/option.rue");
        const Option = option.Option;
        fn parse32() -> i32 {
            let O = Option(i32);
            match @parse_i32("1") { O.Some(n) => n, O.None => 0 }
        }
        fn parse64() -> i32 {
            let O = Option(i64);
            match @parse_i64("2") { O.Some(n) => @intCast(n), O.None => 0 }
        }
        fn main() -> i32 { parse32() + parse64() }"#,
        &[(2, "/project/std/option.rue", TRUSTED_OPTION_MODULE_SOURCE)],
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
        heap: Vec::new(),
    };
    let (parse32_cfg, parse32_inst, parse32_args, parse32_result) =
        find_intrinsic_in_function(&state, "parse32", "parse_i32");
    let (parse64_cfg, parse64_inst, parse64_args, parse64_result) =
        find_intrinsic_in_function(&state, "parse64", "parse_i64");

    state.select_source_function("parse32");
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
    state.select_source_function("parse64");
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
    state.select_source_function("parse32");
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            parse32_cfg,
            parse32_inst,
            "parse_i32",
            &parse32_args,
            Type::U64,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "Option(i64) is not a valid parse_i32 result"
    );
}

#[test]
fn read_line_requires_trusted_source_strbuf_payload_metadata() {
    let strbuf = r#"pub struct StrBuf {
            buf: ptr mut u8,
            len: u64,
            cap: u64,
            fn len(borrow self) -> u64 { self.len }
            fn as_ptr(borrow self) -> ptr mut u8 { self.buf }
        }
        drop fn StrBuf(self) { }"#;
    let state = query_cfg_state_with_trusted_std(
        r#"const strbuf = @import("std/strbuf.rue");
        const option = @import("std/option.rue");
        const StrBuf = strbuf.StrBuf;
        const Option = option.Option;
        fn line() -> i32 {
            let O = Option(StrBuf);
            match @read_line() { O.Some(_line) => 1, O.None => 0 }
        }
        fn parse() -> i32 {
            let O = Option(i32);
            match @parse_i32("1") { O.Some(value) => value, O.None => 0 }
        }
        fn main() -> i32 { line() + parse() }"#,
        &[
            (2, "/project/std/strbuf.rue", strbuf),
            (3, "/project/std/option.rue", TRUSTED_OPTION_MODULE_SOURCE),
        ],
    )
    .expect("trusted Option(StrBuf) @read_line probe must compile");
    let interp = Interp {
        state: &state,
        stdout: String::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
    };
    let (cfg, inst, args, result) = find_intrinsic_in_function(&state, "line", "read_line");
    state.select_source_function("line");
    assert_eq!(
        interp.classify_unsupported_intrinsic(cfg, inst, "read_line", &args, result),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::StandardInput)
    );

    let _ = find_intrinsic_in_function(&state, "parse", "parse_i32");
    state.select_source_function("line");
    assert_eq!(
        interp.classify_unsupported_intrinsic(cfg, inst, "read_line", &args, Type::I32),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "an arbitrary Option payload must not satisfy @read_line"
    );
}
