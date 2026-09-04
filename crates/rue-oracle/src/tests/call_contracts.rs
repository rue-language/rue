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

fn expect_modeled_unit(result: Step<Value>) {
    match result {
        Ok(Value::Unit) => {}
        Ok(value) => panic!("expected a modeled unit, got {value:?}"),
        Err(Flow::Unsupported(_)) => panic!("expected a modeled unit, got Unsupported"),
        Err(Flow::Panic(_)) => panic!("expected a modeled unit, got a panic"),
    }
}

fn expect_observed(result: Step<()>) {
    match result {
        Ok(()) => {}
        Err(Flow::Unsupported(_)) => panic!("expected a modeled observation, got Unsupported"),
        Err(Flow::Panic(_)) => panic!("expected a modeled observation, got a panic"),
    }
}

fn contract_interp(state: &CompileState) -> Interp<'_> {
    Interp {
        state,
        stdout_trace: Vec::new(),
        stdout_bytes: 0,
        stdout_cap: MAX_STDOUT_BYTES,
        stderr_cap: MAX_STDERR_BYTES,
        budget: STEP_BUDGET,
        depth: 0,
        heap: Vec::new(),
        small_free_heads: [None; ORACLE_SMALL_CLASS_COUNT],
        heap_metadata_bytes: 0,
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
fn text_output_routes_reject_cross_abi_shapes() {
    let strbuf = r#"pub struct StrBuf {
            buf: ptr mut u8,
            len: u64,
            cap: u64,
            fn len(borrow self) -> u64 { self.len }
            fn as_ptr(borrow self) -> ptr mut u8 { self.buf }
        }
        drop fn StrBuf(self) { }"#;
    let state = query_cfg_state_with_trusted_std(
        "const strbuf = @import(\"std/strbuf.rue\"); const StrBuf = strbuf.StrBuf; fn main() -> i32 { let s: StrBuf = \"x\"; print(s); 0 }",
        &[(2, "/project/std/strbuf.rue", strbuf)],
    )
        .expect("text output probe must compile");
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
    let strbuf = state
        .type_pool()
        .lang_item_type(rue_air::LangItem::StrBuf)
        .expect("text probe must register StrBuf");
    assert_eq!(
        interp.classify_unsupported_runtime_call(
            RuntimeCallKind::StrPrintAggregate,
            &[Value::str_view("x")],
            &[strbuf],
            &[CfgArgMode::Normal],
            Type::UNIT,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
    );
    assert_eq!(
        interp.classify_unsupported_runtime_call(
            RuntimeCallKind::StrPrintProjected,
            &[Value::Ptr(None), Value::Int(0)],
            &[Type::U64, Type::U64],
            &[CfgArgMode::Normal, CfgArgMode::Normal],
            Type::UNIT,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::RuntimeCallSignature)
    );
}

#[test]
fn modeled_text_runtime_routes_share_the_ordered_trace() {
    let state = query_cfg_state("fn main() -> i32 { print(\"x\"); 0 }")
        .expect("text output probe must compile");
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

    let aggregate_ptr = interp.test_alloc_str_ptr("hé".as_bytes());
    let Value::Ptr(Some(aggregate_target)) = aggregate_ptr else {
        panic!("test text allocation must produce a pointer")
    };
    let aggregate = Value::Aggregate(vec![Value::Ptr(Some(aggregate_target)), Value::Int(3)]);
    expect_modeled_unit(interp.eval_runtime_output_call(
        RuntimeCallKind::StrPrintAggregate,
        &[aggregate],
        &[Type::U64],
    ));

    let projected_target = interp.test_alloc_str_ptr(b"!");
    expect_modeled_unit(interp.eval_runtime_output_call(
        RuntimeCallKind::StrPrintlnProjected,
        &[projected_target, Value::Int(1)],
        &[Type::U64, Type::U64],
    ));

    assert_eq!(interp.stdout_trace, "hé!\n".as_bytes());
}

#[test]
fn output_routes_bound_claimed_lengths_before_reading() {
    let state = query_cfg_state("fn main() -> i32 { print(\"x\"); 0 }")
        .expect("text output probe must compile");
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
    let aggregate_huge = Value::Aggregate(vec![Value::Ptr(None), Value::Int(i128::MAX)]);
    let aggregate_error = expect_flow_unsupported(interp.eval_runtime_output_call(
        RuntimeCallKind::StrPrintAggregate,
        &[aggregate_huge],
        &[Type::U64],
    ));
    assert_eq!(
        aggregate_error.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes)
    );

    let projected_error = expect_flow_unsupported(interp.eval_runtime_output_call(
        RuntimeCallKind::StrPrintProjected,
        &[Value::Ptr(None), Value::Int(i128::MAX)],
        &[Type::U64, Type::U64],
    ));
    assert_eq!(
        projected_error.kind(),
        UnsupportedKind::ResourceLimit(ResourceLimitKind::StdoutBytes)
    );

    let syscall_error = expect_flow_unsupported(interp.eval_stdout_syscall(&[
        Value::Int(1),
        Value::Int(1),
        Value::Int(0),
        Value::Int(u64::MAX as i128),
    ]));
    assert_eq!(
        syscall_error.kind(),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::SystemCall)
    );

    let pointer = interp.test_alloc_str_ptr(b"a");
    let oob = expect_flow_unsupported(interp.eval_runtime_output_call(
        RuntimeCallKind::StrPrintProjected,
        &[pointer, Value::Int(2)],
        &[Type::U64, Type::U64],
    ));
    assert_eq!(
        oob.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::PointerRead
        ))
    );
}

#[test]
fn text_dbg_preserves_invalid_bytes_in_the_raw_trace() {
    let state = query_cfg_state("fn main() -> i32 { print(\"x\"); 0 }")
        .expect("text output probe must compile");
    let (types, _, _) = find_call_metadata(&state, "__rue_str_print");
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
    let pointer = interp.test_alloc_str_ptr(&[0xff]);
    let Value::Ptr(Some(target)) = pointer else {
        panic!("test text allocation must produce a pointer")
    };
    let value = Value::Aggregate(vec![Value::Ptr(Some(target)), Value::Int(1)]);

    expect_observed(interp.write_dbg(&value, types[0]));
    assert_eq!(interp.stdout_trace, [0xff, b'\n']);
}

#[test]
fn random_intrinsic_requires_exact_arity_and_result_type() {
    let state = query_cfg_state("fn main() -> i32 { let n: u32 = @random_u32(); @intCast(n) }")
        .expect("random probe must compile");
    let (cfg, inst, args, result) = find_intrinsic_in_function(&state, "main", "random_u32");
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
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            cfg,
            inst,
            rue_air::IntrinsicOperation::RandomU32,
            &args,
            result,
        ),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::RandomU32)
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            cfg,
            inst,
            rue_air::IntrinsicOperation::RandomU32,
            &[inst],
            result,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicArity)
    );
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            cfg,
            inst,
            rue_air::IntrinsicOperation::RandomU32,
            &args,
            Type::U64,
        ),
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

fn find_empty_slice_pointer_in_function<'a>(
    state: &'a CompileState,
    function_name: &str,
) -> (&'a Cfg, CfgValue, CfgValue) {
    let function = state
        .functions
        .iter()
        .position(|function| function.is_source_named(function_name))
        .unwrap_or_else(|| panic!("missing CFG for {function_name}"));
    let cfg = &state.functions[function].cfg;
    let pool = &state.functions[function].type_pool;
    let mut found = None;
    for init in cfg
        .blocks()
        .iter()
        .flat_map(|block| block.insts.iter().copied())
    {
        let data = &cfg.get_inst(init).data;
        let CfgInstData::StructInit { struct_id, .. } = data else {
            continue;
        };
        let fields = cfg.get_struct_fields(data);
        if !rue_air::is_slice_struct_name(&pool.struct_def(*struct_id).name)
            || fields.len() != 2
            || !matches!(cfg.get_inst(fields[0]).data, CfgInstData::Const(0))
            || !cfg.get_inst(fields[0]).ty.is_ptr_const()
            || cfg.get_inst(fields[1]).ty != Type::U64
            || !matches!(cfg.get_inst(fields[1]).data, CfgInstData::Const(0))
        {
            continue;
        }
        assert!(
            found.replace((fields[0], init)).is_none(),
            "expected exactly one empty-slice pointer in {function_name}"
        );
    }
    let (pointer, init) =
        found.unwrap_or_else(|| panic!("missing empty-slice pointer in {function_name}"));
    (cfg, pointer, init)
}

#[test]
fn shared_str_character_builtins_require_and_model_ptr_len_offset() {
    let state = query_cfg_state(
        "fn main() -> i32 { checked { let pointer: ptr mut u8 = @alloc(1, 1); @free(pointer, 1, 1); }; 0 }",
    )
    .expect("probe must compile");
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
    for function_name in ["panic_no_message", "panic_with_message"] {
        state.select_source_function(function_name);
        let (cfg, _intrinsic, args, result_ty) =
            find_intrinsic_in_function(&state, function_name, "panic");
        let operation = if function_name == "panic_no_message" {
            rue_air::IntrinsicOperation::PanicNoMessage
        } else {
            rue_air::IntrinsicOperation::Panic
        };
        // `@panic` diverges, so the compiler types it `!` (never) (RUE-512).
        assert_eq!(result_ty, Type::NEVER, "{function_name} compiler metadata");
        // The abort preflight (the oracle's panic contract since RUE-589)
        // accepts exactly the never-typed shape...
        assert!(
            matches!(
                interp.preflight_abort_intrinsic(cfg, operation, &args, result_ty,),
                Ok(Some(AbortIntrinsic::Panic))
            ),
            "{function_name} never signature must pass preflight"
        );
        // ...and rejects stale unit-typed metadata as a contract violation.
        match interp.preflight_abort_intrinsic(cfg, operation, &args, Type::UNIT) {
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
fn oracle_intrinsic_execution_is_operation_selected() {
    let source = include_str!("../lib.rs");
    let intrinsic_branch = source
        .split("CfgInstData::Intrinsic {")
        .nth(1)
        .expect("oracle intrinsic execution branch");
    assert!(intrinsic_branch.contains("operation.expected_spelling()"));
    assert!(intrinsic_branch.contains("self.classify_unsupported_intrinsic(cfg, v, *operation"));
    assert!(
        intrinsic_branch.contains(".eval_pointer_intrinsic(cfg, frame, *operation, &args, ty)")
    );
    assert!(!intrinsic_branch.contains("self.interner().resolve(name)"));
    assert!(!intrinsic_branch.contains("&iname"));
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
            params: Vec::new(),
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
            promoted: HashMap::new(),
            param_places: HashMap::new(),
            place_return: false,
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
    // A comptime-decidable `@assert_eq` is what still lowers to the conditional
    // `assert` intrinsic: source `@assert` reports on the ADR-0083 §5.1 channel
    // instead (RUE-1953), so it is a branch around two runtime calls and no
    // longer an abort intrinsic to preflight. The intrinsic that remains takes
    // its condition and nothing else.
    let source = r#"fn main() -> i32 {
        let entropy: u32 = @random_u32();
        @assert_eq(1, 1);
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
                vec![assert_args[0], random],
                Type::UNIT,
                ContractViolationKind::IntrinsicArity,
            ),
            3 => (
                assertion,
                vec![random],
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
            params: Vec::new(),
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
            promoted: HashMap::new(),
            param_places: HashMap::new(),
            place_return: false,
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
    // As above: the surviving `assert` intrinsic is the comptime-decidable
    // comparison's, whose only operand is the condition. `@panic` is the one
    // abort intrinsic that still carries text.
    let source = r#"fn main() -> i32 {
        @assert_eq(1, 1);
        @panic("stop");
        0
    }"#;

    {
        let state = query_cfg_state(source).expect("abort value-shape probe must compile");
        let (cfg, intrinsic, args, _) = find_intrinsic_in_function(&state, "main", "panic");
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
            params: Vec::new(),
            locals: vec![None; cfg.num_locals() as usize],
            cache: HashMap::new(),
            promoted: HashMap::new(),
            param_places: HashMap::new(),
            place_return: false,
        };
        frame.cache.insert(args[0].as_u32(), Value::Int(7));

        let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, intrinsic));
        assert_eq!(
            unsupported.kind(),
            UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
            "@panic must reject a non-text runtime value"
        );
    }

    let state = query_cfg_state(source).expect("assert condition-shape probe must compile");
    let (cfg, assertion, args, _) = find_intrinsic_in_function(&state, "main", "assert");
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
        params: Vec::new(),
        locals: vec![None; cfg.num_locals() as usize],
        cache: HashMap::new(),
        promoted: HashMap::new(),
        param_places: HashMap::new(),
        place_return: false,
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
fn empty_slice_null_is_a_const_gap_and_user_int_to_ptr_stays_distinct() {
    let source = r#"const ZERO: u64 = 0;
        fn slice_len(borrow s: [i32]) -> u64 { s.len() }
        fn user_pointer(borrow s: [i32]) -> u64 {
            checked {
                let p: ptr mut i32 = @int_to_ptr(ZERO);
                @ptr_to_int(p) + s.len()
            }
        }
        fn route(borrow s: [i32]) -> u64 {
            slice_len(borrow s) + user_pointer(borrow s)
        }
        fn main() -> i32 {
            let empty: [i32; 0] = [];
            @intCast(route(borrow empty))
        }"#;

    let state = query_cfg_state(source).expect("empty-slice provenance probe must compile");
    let (cfg, pointer, _init) = find_empty_slice_pointer_in_function(&state, "main");
    state.select_source_function("main");
    let mut interp = contract_interp(&state);
    assert!(interp.is_empty_slice_pointer(cfg, pointer));
    let mut frame = Frame {
        params: Vec::new(),
        locals: vec![None; cfg.num_locals() as usize],
        cache: HashMap::new(),
        promoted: HashMap::new(),
        param_places: HashMap::new(),
        place_return: false,
    };
    let unsupported = expect_flow_unsupported(interp.eval(cfg, &mut frame, pointer));
    assert_eq!(
        unsupported.kind(),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::EmptySlicePointer,
        ))
    );
    assert!(
        cfg.blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .filter_map(|value| match cfg.get_inst(value).data {
                CfgInstData::Intrinsic { operation, .. } => Some(operation),
                _ => None,
            })
            .all(|operation| operation != rue_air::IntrinsicOperation::IntToPtr),
        "the compiler-owned null must not masquerade as source @int_to_ptr"
    );

    let (user_cfg, user_inst, user_args, user_result) =
        find_intrinsic_in_function(&state, "user_pointer", "int_to_ptr");
    state.select_source_function("user_pointer");
    assert!(matches!(
        user_cfg.get_inst(user_args[0]).data,
        CfgInstData::Const(0)
    ));
    assert_eq!(
        contract_interp(&state).classify_unsupported_intrinsic(
            user_cfg,
            user_inst,
            rue_air::IntrinsicOperation::IntToPtr,
            &user_args,
            user_result,
        ),
        UnsupportedKind::SemanticGap(SemanticGapKind::Intrinsic(
            UnsupportedIntrinsicKind::IntToPointer,
        )),
        "a user-authored zero keeps the source IntToPtr identity"
    );

    let preview = PreviewFeatures::new();

    let mut extra_pointer_use = query_cfg_state_with_preview_features(source, &preview)
        .expect("extra pointer-use probe must compile");
    let main = extra_pointer_use
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
        .unwrap();
    let (pointer, pointer_ty, outer_cast) = {
        let cfg = &extra_pointer_use.functions[main].cfg;
        let (_, pointer, _) = find_empty_slice_pointer_in_function(&extra_pointer_use, "main");
        let outer_cast = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| matches!(cfg.get_inst(*value).data, CfgInstData::IntCast { .. }))
            .expect("outer result cast");
        (pointer, cfg.get_inst(pointer).ty, outer_cast)
    };
    let pool = extra_pointer_use.functions[main].type_pool.clone();
    extra_pointer_use.functions[main]
        .cfg
        .try_edit(&pool, |editor| {
            editor.replace_int_cast(outer_cast, pointer, pointer_ty)
        })
        .unwrap();
    let cfg = &extra_pointer_use.functions[main].cfg;
    assert_eq!(cfg.value_use_count(pointer), 2);
    assert!(!contract_interp(&extra_pointer_use).is_empty_slice_pointer(cfg, pointer));

    let mut extra_init_use = query_cfg_state_with_preview_features(source, &preview)
        .expect("extra slice-use probe must compile");
    let main = extra_init_use
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
        .unwrap();
    let (pointer, init, init_ty, outer_cast) = {
        let cfg = &extra_init_use.functions[main].cfg;
        let (_, pointer, init) = find_empty_slice_pointer_in_function(&extra_init_use, "main");
        let outer_cast = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find(|value| matches!(cfg.get_inst(*value).data, CfgInstData::IntCast { .. }))
            .expect("outer result cast");
        (pointer, init, cfg.get_inst(init).ty, outer_cast)
    };
    let pool = extra_init_use.functions[main].type_pool.clone();
    extra_init_use.functions[main]
        .cfg
        .try_edit(&pool, |editor| {
            editor.replace_int_cast(outer_cast, init, init_ty)
        })
        .unwrap();
    let cfg = &extra_init_use.functions[main].cfg;
    assert_eq!(cfg.value_use_count(init), 2);
    assert!(!contract_interp(&extra_init_use).is_empty_slice_pointer(cfg, pointer));

    let mut wrong_consumer = query_cfg_state_with_preview_features(source, &preview)
        .expect("consumer-mode probe must compile");
    let main = wrong_consumer
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
        .unwrap();
    let (pointer, init, consumer, mut args) = {
        let cfg = &wrong_consumer.functions[main].cfg;
        let (_, pointer, init) = find_empty_slice_pointer_in_function(&wrong_consumer, "main");
        let (consumer, args) = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .find_map(|value| {
                let data = &cfg.get_inst(value).data;
                matches!(data, CfgInstData::Call { .. })
                    .then(|| (value, cfg.get_call_args(data).to_vec()))
                    .filter(|(_, args)| args.iter().any(|arg| arg.value == init))
            })
            .expect("slice call consumer");
        (pointer, init, consumer, args)
    };
    args.iter_mut()
        .find(|arg| arg.value == init)
        .expect("slice argument")
        .mode = CfgArgMode::Borrow;
    let pool = wrong_consumer.functions[main].type_pool.clone();
    wrong_consumer.functions[main]
        .cfg
        .try_edit(&pool, |editor| editor.replace_call_args(consumer, args))
        .unwrap();
    let cfg = &wrong_consumer.functions[main].cfg;
    assert!(!contract_interp(&wrong_consumer).is_empty_slice_pointer(cfg, pointer));

    let mut wrong_init_type = query_cfg_state_with_preview_features(source, &preview)
        .expect("slice type probe must compile");
    let main = wrong_init_type
        .functions
        .iter()
        .position(|function| function.is_source_named("main"))
        .unwrap();
    let (_, pointer, init) = find_empty_slice_pointer_in_function(&wrong_init_type, "main");
    let pool = wrong_init_type.functions[main].type_pool.clone();
    wrong_init_type.functions[main]
        .cfg
        .try_edit(&pool, |editor| editor.replace_inst_type(init, Type::UNIT))
        .unwrap();
    let cfg = &wrong_init_type.functions[main].cfg;
    assert!(!contract_interp(&wrong_init_type).is_empty_slice_pointer(cfg, pointer));
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
        assert_eq!(
            interp.classify_unsupported_intrinsic(
                cfg,
                inst,
                rue_air::IntrinsicOperation::FieldPtr,
                &args,
                result,
            ),
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

const TRUSTED_STRBUF_MODULE_SOURCE: &str = r#"pub struct StrBuf {
    buf: ptr mut u8,
    len: u64,
    cap: u64,
    fn len(borrow self) -> u64 { self.len }
    fn as_ptr(borrow self) -> ptr mut u8 { self.buf }
}
drop fn StrBuf(self) { }"#;

#[test]
fn every_source_intrinsic_operation_survives_semantic_export_import_and_cfg_build() {
    let source = r#"const option = @import("std/option.rue");
        const strbuf = @import("std/strbuf.rue");
        const Option = option.Option;
        const StrBuf = strbuf.StrBuf;

        struct Pair { value: i32 }

        fn panic_no_message() { @panic() }
        fn panic_with_message() { @panic("boom") }

        fn fallible() -> i32 {
            let OStrBuf = Option(StrBuf);
            let OI32 = Option(i32);
            let OI64 = Option(i64);
            let OU32 = Option(u32);
            let OU64 = Option(u64);
            let _line: OStrBuf = @read_line();
            let _i32: OI32 = @parse_i32("1");
            let _i64: OI64 = @parse_i64("2");
            let _u32: OU32 = @parse_u32("3");
            let _u64: OU64 = @parse_u64("4");
            0
        }

        fn bounds(borrow values: [i64], index: u64) -> i32 {
            @intCast(values[index])
        }

        fn pointer_and_runtime() -> i32 {
            let mut value: i32 = 1;
            let mut pair = Pair { value: 2 };
            let signed: i32 = -1;
            let unsigned: u32 = 1;
            let wide_signed: i64 = -2;
            let wide_unsigned: u64 = 2;
            @assert_eq(1, 1);
            @dbg(wide_signed);
            @dbg(wide_unsigned);
            @dbg(true);
            @dbg("text");
            let _: u32 = @bitCast(signed);
            let _: i32 = @bitCast(unsigned);
            let _: u32 = @random_u32();
            let _: u64 = @random_u64();
            let _: u64 = @arg_count();
            let _: ptr mut u8 = checked { @arg_ptr(0) };
            let _: u64 = @arg_len(0);
            let _: u64 = @env_count();
            let _: ptr mut u8 = checked { @env_ptr(0) };
            let _: u64 = @env_len(0);
            checked {
                let shared: ptr const i32 = @raw(value);
                let mutable: ptr mut i32 = @raw_mut(value);
                let field: ptr mut i32 = @field_ptr(pair.value);
                let address: u64 = @ptr_to_int(shared);
                let _: ptr mut i32 = @int_to_ptr(address);
                let _: i32 = @ptr_read(shared);
                let _: i32 = @ptr_read_unaligned(shared);
                @ptr_write(mutable, 3);
                @ptr_write_unaligned(field, 4);
                let _: ptr const i32 = @ptr_offset(shared, 1);

                let raw: ptr mut u8 = @alloc(8, 1);
                let zeroed: ptr mut u8 = @alloc_zeroed(8, 1);
                @byte_copy(raw, zeroed, 1);
                @byte_move(raw, zeroed, 1);
                @byte_set(raw, 0, 1);
                let grown: ptr mut u8 = @realloc(raw, 8, 1, 16);
                let _: bool = @resize(grown, 16, 1, 8);
                @free(grown, 16, 1);
                @free(zeroed, 8, 1);
                let _: i64 = @syscall(0);
            };
            0
        }

        fn main() -> i32 {
            let seed = @random_u32();
            let values: [i64; 2] = [1, 2];
            if seed == 1 { panic_no_message(); }
            if seed == 2 { panic_with_message(); }
            fallible() + bounds(borrow values, @intCast(seed)) + pointer_and_runtime()
        }"#;
    let state = query_cfg_state_with_trusted_std(
        source,
        &[
            (2, "/project/std/strbuf.rue", TRUSTED_STRBUF_MODULE_SOURCE),
            (3, "/project/std/option.rue", TRUSTED_OPTION_MODULE_SOURCE),
        ],
    )
    .expect("the full intrinsic source table must compile through durable CFG import");
    let mut operations = std::collections::HashSet::new();
    for function in &state.functions {
        for value in function
            .cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
        {
            if let CfgInstData::Intrinsic { operation, .. } = function.cfg.get_inst(value).data {
                operations.insert(operation);
            }
        }
    }
    assert_eq!(rue_air::IntrinsicOperation::ALL.len(), 51);
    for operation in rue_air::IntrinsicOperation::ALL {
        if matches!(
            operation,
            rue_air::IntrinsicOperation::DebugFloat
                | rue_air::IntrinsicOperation::IntToFloat
                | rue_air::IntrinsicOperation::FloatToInt
                | rue_air::IntrinsicOperation::FloatCast
                | rue_air::IntrinsicOperation::TotalCmp
                | rue_air::IntrinsicOperation::FloatSqrt
                | rue_air::IntrinsicOperation::FloatFloor
                | rue_air::IntrinsicOperation::FloatCeil
                | rue_air::IntrinsicOperation::FloatTrunc
                | rue_air::IntrinsicOperation::FloatRound
        ) {
            // The float intrinsics are exercised by the dedicated float
            // contract tests below rather than this integer fixture.
            continue;
        }

        assert!(
            operations.contains(&operation),
            "source-to-CFG pipeline lost {operation:?}"
        );
    }
}

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
    let (parse32_cfg, parse32_inst, parse32_args, parse32_result) =
        find_intrinsic_in_function(&state, "parse32", "parse_i32");
    let (parse64_cfg, parse64_inst, parse64_args, parse64_result) =
        find_intrinsic_in_function(&state, "parse64", "parse_i64");

    state.select_source_function("parse32");
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            parse32_cfg,
            parse32_inst,
            rue_air::IntrinsicOperation::ParseI32,
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
            rue_air::IntrinsicOperation::ParseI64,
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
            rue_air::IntrinsicOperation::ParseI32,
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
    let (cfg, inst, args, result) = find_intrinsic_in_function(&state, "line", "read_line");
    state.select_source_function("line");
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            cfg,
            inst,
            rue_air::IntrinsicOperation::ReadLine,
            &args,
            result,
        ),
        UnsupportedKind::ExternalDependency(ExternalDependencyKind::StandardInput)
    );

    let _ = find_intrinsic_in_function(&state, "parse", "parse_i32");
    state.select_source_function("line");
    assert_eq!(
        interp.classify_unsupported_intrinsic(
            cfg,
            inst,
            rue_air::IntrinsicOperation::ReadLine,
            &args,
            Type::I32,
        ),
        UnsupportedKind::ContractViolation(ContractViolationKind::IntrinsicSignature),
        "an arbitrary Option payload must not satisfy @read_line"
    );
}
