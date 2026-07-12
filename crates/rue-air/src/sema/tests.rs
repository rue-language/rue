#[cfg(test)]
mod tests {
    use crate::inst::{AirArgMode, AirInstData, AirRef};
    use crate::sema::{Sema, SemaOutput};
    use crate::types::Type;
    use rue_error::{CompileErrors, ErrorKind, MultiErrorResult, PreviewFeature, PreviewFeatures};
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::{AstGen, RirParamMode};

    fn compile_to_air(source: &str) -> MultiErrorResult<SemaOutput> {
        compile_to_air_with_preview_features(source, PreviewFeatures::new())
    }

    fn compile_to_air_with_preview_features(
        source: &str,
        preview_features: PreviewFeatures,
    ) -> MultiErrorResult<SemaOutput> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from_error)?;
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse()?;

        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &mut interner, preview_features);
        sema.analyze_all()
    }

    #[test]
    fn test_analyze_simple_function() {
        let output = compile_to_air("fn main() -> i32 { 42 }").unwrap();
        let functions = &output.functions;

        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "main");

        let air = &functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        assert_eq!(air.len(), 2); // Const + Ret
    }

    fn named_method_lookup_work(irrelevant: usize) -> crate::BodyAnalysisWork {
        let mut source = String::from(
            "struct Target { fn answer() -> i32 { 42 } }\n\
             fn main() -> i32 { Target.answer() }\n",
        );
        for index in 0..irrelevant {
            source.push_str(&format!(
                "struct Irrelevant{index} {{ fn unused() -> i32 {{ {index} }} }}\n"
            ));
        }
        compile_to_air(&source).unwrap().body_analysis_work
    }

    #[test]
    fn reachable_named_method_lookup_is_invariant_to_irrelevant_declarations() {
        let one = named_method_lookup_work(1);
        let many = named_method_lookup_work(128);

        assert_eq!(one.named_method_record_lookups, 1);
        assert_eq!(many.named_method_record_lookups, 1);
        assert_eq!(one.free_function_record_lookups, 1);
        assert_eq!(many.free_function_record_lookups, 1);
        assert_eq!(one.reachable_declaration_rir_visits, 0);
        assert_eq!(many.reachable_declaration_rir_visits, 0);
    }

    fn named_destructor_lookup_work(irrelevant: usize) -> crate::BodyAnalysisWork {
        let mut source = String::from(
            "struct Target { value: i32 }\n\
             drop fn Target(self) {}\n\
             fn main() -> i32 { 0 }\n",
        );
        for index in 0..irrelevant {
            source.push_str(&format!("fn irrelevant{index}() -> i32 {{ {index} }}\n"));
        }
        compile_to_air(&source).unwrap().body_analysis_work
    }

    #[test]
    fn named_destructor_selection_is_invariant_to_irrelevant_declarations() {
        let one = named_destructor_lookup_work(1);
        let many = named_destructor_lookup_work(128);

        assert_eq!(one.named_destructor_declarations_visited, 1);
        assert_eq!(many.named_destructor_declarations_visited, 1);
        assert_eq!(one.named_destructor_selection_rir_visits, 0);
        assert_eq!(many.named_destructor_selection_rir_visits, 0);
    }

    #[test]
    fn panic_is_never_and_assert_is_unit_in_air() {
        // `@panic` diverges: its AIR result type is `!` (never), matching HM
        // and CFG (RUE-512). `@assert` returns on the success path, so it stays
        // unit-typed. Both must agree with the inference contract. The message
        // operand's own type never changes the intrinsic's result type
        // (StrBuf/String-alias messages, never-typed operands).
        for (name, body, expected) in [
            ("panic_no_message", "@panic()", Type::NEVER),
            ("panic_with_message", "@panic(\"boom\")", Type::NEVER),
            ("assertion", "@assert(true)", Type::UNIT),
            (
                "assertion_with_message",
                "@assert(true, \"ok\")",
                Type::UNIT,
            ),
            (
                "panic_with_strbuf",
                "let message: StrBuf = \"boom\"; @panic(message)",
                Type::NEVER,
            ),
            (
                "panic_with_string_alias",
                "let message: String = \"boom\"; @panic(message)",
                Type::NEVER,
            ),
            (
                "assertion_with_strbuf",
                "let message: StrBuf = \"ok\"; @assert(true, message)",
                Type::UNIT,
            ),
            (
                "never assertion condition",
                "@assert(diverge())",
                Type::UNIT,
            ),
            ("never panic message", "@panic(diverge())", Type::NEVER),
        ] {
            let source = format!(
                "fn diverge() -> ! {{ loop {{}} }} fn probe() {{ {body} }} fn main() -> i32 {{ probe(); 0 }}"
            );
            let output = compile_to_air(&source).unwrap();
            let function = output
                .functions
                .iter()
                .find(|function| function.name == "probe")
                .unwrap_or_else(|| panic!("missing analyzed function {name}"));
            let intrinsic_types: Vec<_> = function
                .air
                .iter()
                .filter_map(|(_, inst)| {
                    matches!(inst.data, AirInstData::Intrinsic { .. }).then_some(inst.ty)
                })
                .collect();
            assert_eq!(
                intrinsic_types,
                vec![expected],
                "{name} intrinsic result must agree with HM"
            );
            // A unit-valued trailing `@assert` still needs an implicit return; a
            // never-valued trailing `@panic` diverges, so no return is synthesized.
            let has_ret = function
                .air
                .iter()
                .any(|(_, inst)| matches!(inst.data, AirInstData::Ret(_)));
            if expected == Type::UNIT {
                assert!(has_ret, "{name}: a unit trailing intrinsic needs a return");
            } else {
                assert!(
                    !has_ret,
                    "{name}: a diverging trailing intrinsic must not synthesize a return"
                );
            }
        }
    }

    #[test]
    fn panic_and_assert_reject_invalid_operand_types_at_the_operand() {
        for (name, source, preview_string_trio, intrinsic, expected, found, offending) in [
            (
                "integer assertion condition",
                "fn main() -> i32 { @assert(1); 0 }",
                false,
                "assert",
                "bool condition",
                "i32",
                "1",
            ),
            (
                "aggregate assertion condition",
                "fn main() -> i32 { let s: StrBuf = \"truthy\"; @assert(s); 0 }",
                false,
                "assert",
                "bool condition",
                "StrBuf",
                "s",
            ),
            (
                "scalar panic message",
                "fn main() -> i32 { @panic(1); 0 }",
                false,
                "panic",
                "StrBuf message",
                "i32",
                "1",
            ),
            (
                "scalar assertion message",
                "fn main() -> i32 { @assert(false, 7); 0 }",
                false,
                "assert",
                "StrBuf message",
                "i32",
                "7",
            ),
            (
                "str view message",
                "fn main() -> i32 { let s: str = \"view\"; @panic(s); 0 }",
                true,
                "panic",
                "StrBuf message",
                "str",
                "s",
            ),
            (
                "fixed string message",
                "fn main() -> i32 { let s: Str(8) = \"fixed\"; @assert(false, s); 0 }",
                true,
                "assert",
                "StrBuf message",
                "Str(8)",
                "s",
            ),
            (
                "array message",
                "fn main() -> i32 { let a = [1, 2, 3]; @panic(a); 0 }",
                false,
                "panic",
                "StrBuf message",
                "[i32; 3]",
                "a",
            ),
            (
                "enum message",
                "enum Mode { A } fn main() -> i32 { @assert(false, Mode.A); 0 }",
                false,
                "assert",
                "StrBuf message",
                "Mode",
                "Mode.A",
            ),
            (
                "three-slot struct impostor",
                "struct Fake { a: u64, b: u64, c: u64 } fn main() -> i32 { @panic(Fake { a: 0, b: 0, c: 0 }); 0 }",
                false,
                "panic",
                "StrBuf message",
                "Fake",
                "Fake { a: 0, b: 0, c: 0 }",
            ),
        ] {
            let mut preview_features = PreviewFeatures::new();
            if preview_string_trio {
                preview_features.insert(PreviewFeature::StringTrio);
            }
            let errors =
                compile_to_air_with_preview_features(source, preview_features).unwrap_err();
            assert_eq!(errors.len(), 1, "{name} should fail once");
            let error = errors.iter().next().unwrap();
            match &error.kind {
                ErrorKind::IntrinsicTypeMismatch(data) => {
                    assert_eq!(data.name, intrinsic, "{name} intrinsic");
                    assert_eq!(data.expected, expected, "{name} expected type");
                    assert_eq!(data.found, found, "{name} found type");
                }
                other => panic!("{name} produced {other:?}, expected E0702"),
            }

            let span = error.span().expect("operand mismatch must be spanned");
            assert_eq!(
                &source[span.start as usize..span.end as usize],
                offending,
                "{name} must point at the offending operand",
            );
        }
    }

    #[test]
    fn panic_and_assert_preserve_primary_operand_errors() {
        for source in [
            "fn main() -> i32 { @panic(missing); 0 }",
            "fn main() -> i32 { @assert(missing); 0 }",
            "fn main() -> i32 { @assert(false, missing); 0 }",
        ] {
            let errors = compile_to_air(source).unwrap_err();
            assert_eq!(errors.len(), 1, "primary operand error must not cascade");
            let error = errors.iter().next().unwrap();
            assert!(
                matches!(&error.kind, ErrorKind::UndefinedVariable(name) if name == "missing"),
                "expected the operand's primary error, got {:?}",
                error.kind
            );
            let span = error.span().expect("undefined operand must be spanned");
            assert_eq!(&source[span.start as usize..span.end as usize], "missing");
        }
    }

    #[test]
    fn test_analyze_addition() {
        let output = compile_to_air("fn main() -> i32 { 1 + 2 }").unwrap();

        let air = &output.functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        // Const(1) + Const(2) + Add + Ret = 4 instructions
        assert_eq!(air.len(), 4);

        // Check that add instruction exists with correct type
        let add_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(add_inst.data, AirInstData::Add(_, _)));
        assert_eq!(add_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_all_binary_ops() {
        // Test that all binary operators compile correctly
        assert!(compile_to_air("fn main() -> i32 { 1 + 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 - 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 * 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 / 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 % 2 }").is_ok());
    }

    #[test]
    fn test_analyze_negation() {
        let output = compile_to_air("fn main() -> i32 { -42 }").unwrap();

        let air = &output.functions[0].air;
        // Const(42) + Neg + Ret = 3 instructions
        assert_eq!(air.len(), 3);

        let neg_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(neg_inst.data, AirInstData::Neg(_)));
        assert_eq!(neg_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_complex_expr() {
        let output = compile_to_air("fn main() -> i32 { (1 + 2) * 3 }").unwrap();

        let air = &output.functions[0].air;
        // Const(1) + Const(2) + Add + Const(3) + Mul + Ret = 6 instructions
        assert_eq!(air.len(), 6);

        // Check that result is multiplication
        let mul_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(mul_inst.data, AirInstData::Mul(_, _)));
    }

    #[test]
    fn test_analyze_let_binding() {
        let output = compile_to_air("fn main() -> i32 { let x = 42; x }").unwrap();

        assert_eq!(output.functions.len(), 1);
        assert_eq!(output.functions[0].num_locals, 1);

        let air = &output.functions[0].air;
        // Const(42) + StorageLive + Alloc + Block([StorageLive], Alloc) + Load + Block([alloc block], Load) + Ret = 7 instructions
        assert_eq!(air.len(), 7);

        // Check storage_live instruction
        let storage_live_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(
            storage_live_inst.data,
            AirInstData::StorageLive { slot: 0 }
        ));

        // Check alloc instruction
        let alloc_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(
            alloc_inst.data,
            AirInstData::Alloc { slot: 0, .. }
        ));

        // Check load instruction
        let load_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(load_inst.data, AirInstData::Load { slot: 0 }));

        // Check block instruction groups the alloc with the load
        let block_inst = air.get(AirRef::from_raw(5));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_analyze_let_mut_assignment() {
        let output = compile_to_air("fn main() -> i32 { let mut x = 10; x = 20; x }").unwrap();

        let air = &output.functions[0].air;
        // Const(10) + StorageLive + Alloc + Block([StorageLive], Alloc) + Const(20) + Store + Load + Block([alloc block, Store], Load) + Ret = 9 instructions
        assert_eq!(air.len(), 9);

        // Check store instruction
        let store_inst = air.get(AirRef::from_raw(5));
        assert!(matches!(
            store_inst.data,
            AirInstData::Store { slot: 0, .. }
        ));

        // Check block instruction groups statements
        let block_inst = air.get(AirRef::from_raw(7));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_undefined_variable() {
        let result = compile_to_air("fn main() -> i32 { x }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::UndefinedVariable(_)
        ));
    }

    #[test]
    fn test_assign_to_immutable() {
        let result = compile_to_air("fn main() -> i32 { let x = 10; x = 20; x }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::AssignToImmutable(_)
        ));
    }

    #[test]
    fn inout_str_view_cannot_be_reassigned_as_a_whole_value() {
        let source = r#"
            fn replace(inout target: str, replacement: str) {
                target = replacement;
            }
            fn main() -> i32 {
                let mut value: Str(8) = "hello";
                replace(inout value, "hi");
                0
            }
        "#;
        let mut preview = PreviewFeatures::new();
        preview.insert(PreviewFeature::StringTrio);
        let errors = compile_to_air_with_preview_features(source, preview).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::StrViewReassignment
        ));
    }

    #[test]
    fn method_and_assoc_view_calls_encode_physical_modes() {
        let source = r#"
            struct Item { value: i32 }

            struct Probe {
                bias: i32,

                fn method(
                    borrow self,
                    borrow read: str,
                    inout edit: str,
                    borrow item: Item,
                ) -> i32 {
                    self.bias + @intCast(read.len()) + @intCast(edit.len()) + item.value
                }

                fn assoc(borrow read: str, inout edit: str, borrow item: Item) -> i32 {
                    11 + @intCast(read.len()) + @intCast(edit.len()) + item.value
                }
            }

            fn main() -> i32 {
                let probe = Probe { bias: 11 };
                let read: Str(8) = "read";
                let mut edit: Str(8) = "editor";
                let item = Item { value: 0 };
                probe.method(borrow read, inout edit, borrow item)
                    + Probe.assoc(borrow read, inout edit, borrow item)
            }
        "#;
        let mut preview = PreviewFeatures::new();
        preview.insert(PreviewFeature::StringTrio);
        let output = compile_to_air_with_preview_features(source, preview).unwrap();

        let main = output
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        let call_modes: Vec<Vec<AirArgMode>> = main
            .air
            .iter()
            .filter_map(|(_, inst)| match inst.data {
                AirInstData::Call {
                    args_start,
                    args_len,
                    ..
                } => Some(
                    main.air
                        .get_call_args(args_start, args_len)
                        .map(|arg| arg.mode)
                        .collect(),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(
            call_modes,
            vec![
                vec![
                    AirArgMode::Borrow,
                    AirArgMode::Normal,
                    AirArgMode::Inout,
                    AirArgMode::Borrow,
                ],
                vec![AirArgMode::Normal, AirArgMode::Inout, AirArgMode::Borrow,],
            ]
        );

        let method = output
            .functions
            .iter()
            .find(|function| function.name == "Probe.method")
            .unwrap();
        assert_eq!(
            method.param_modes.by_ref(),
            &[true, false, false, true, true]
        );
        assert_eq!(
            method.param_modes.writable(),
            &[false, false, false, true, false]
        );

        let assoc = output
            .functions
            .iter()
            .find(|function| function.name == "Probe::assoc")
            .unwrap();
        assert_eq!(assoc.param_modes.by_ref(), &[false, false, true, true]);
        assert_eq!(assoc.param_modes.writable(), &[false, false, true, false]);
    }

    #[test]
    fn method_and_assoc_fixed_string_literals_are_contextual() {
        let source = r#"
            struct Probe {
                fn method(self, value: Str(8)) -> u64 { value.len() }
                fn assoc(value: Str(8)) -> u64 { value.len() }
            }

            fn main() -> i32 {
                let probe = Probe {};
                @intCast(
                    probe.method(if true { "hi" } else { "bye" })
                        + Probe.assoc(if false { "long" } else { "four" })
                )
            }
        "#;
        let mut preview = PreviewFeatures::new();
        preview.insert(PreviewFeature::StringTrio);
        compile_to_air_with_preview_features(source, preview).unwrap();
    }

    #[test]
    fn fixed_string_call_context_stops_at_nested_call_operands() {
        let mut preview = PreviewFeatures::new();
        preview.insert(PreviewFeature::StringTrio);
        let cases = [
            (
                "conditional result",
                r#"fn main() -> i32 { @intCast(take(if true { "hi" } else { "bye" })) }"#,
            ),
            (
                "block result",
                r#"fn main() -> i32 { @intCast(take({ let _marker = 0; "block" })) }"#,
            ),
            (
                "declared fixed-string return",
                r#"fn main() -> i32 { @intCast(take(make())) }"#,
            ),
            (
                "intrinsic statement before a fixed-string block result",
                r#"fn main() -> i32 { @intCast(take(with_assert())) }"#,
            ),
            (
                "never-returning user call",
                r#"fn main() -> i32 { @intCast(take(die("boom"))) }"#,
            ),
            (
                "never-returning generic user call",
                r#"fn main() -> i32 { @intCast(take(generic_die(StrBuf, "boom"))) }"#,
            ),
            (
                "never-returning intrinsic",
                r#"fn main() -> i32 { @intCast(take(@panic("boom"))) }"#,
            ),
        ];

        for (case, main) in cases {
            let source = format!(
                r#"
                    fn take(value: Str(8)) -> u64 {{ value.len() }}
                    fn make() -> Str(8) {{ "made" }}
                    fn with_assert() -> Str(8) {{ @assert(true, "message"); "checked" }}
                    fn die(message: StrBuf) -> ! {{ @panic(message) }}
                    fn generic_die(comptime T: type, message: T) -> ! {{ @panic("stop") }}
                    {main}
                "#
            );
            let output = compile_to_air_with_preview_features(&source, preview.clone())
                .unwrap_or_else(|errors| panic!("{case} must compile independently: {errors}"));

            if matches!(
                case,
                "never-returning user call" | "never-returning generic user call"
            ) {
                let main = output
                    .functions
                    .iter()
                    .find(|function| function.name == "main")
                    .unwrap();
                let mut literal_call_args = Vec::new();
                for (_, inst) in main.air.iter() {
                    let (args_start, args_len) = match &inst.data {
                        AirInstData::Call {
                            args_start,
                            args_len,
                            ..
                        }
                        | AirInstData::CallGeneric {
                            args_start,
                            args_len,
                            ..
                        } => (*args_start, *args_len),
                        _ => continue,
                    };
                    for arg in main.air.get_call_args(args_start, args_len) {
                        if matches!(main.air.get(arg.value).data, AirInstData::StringConst(_)) {
                            literal_call_args.push(arg);
                        }
                    }
                }

                assert_eq!(
                    literal_call_args.len(),
                    1,
                    "{case} must have one literal runtime call argument"
                );
                let arg = &literal_call_args[0];
                assert_eq!(arg.mode, AirArgMode::Normal);
                let arg_ty = main.air.get(arg.value).ty;
                assert_eq!(
                    arg_ty.safe_name_with_pool(Some(&output.type_pool)),
                    "StrBuf",
                    "{case} must materialize its nested message from the callee contract"
                );
                assert_eq!(
                    output.type_pool.abi_slot_count(arg_ty),
                    3,
                    "{case} must pass the three-slot StrBuf representation"
                );
            }
        }
    }

    #[test]
    fn fallible_intrinsic_keeps_structural_result_context() {
        let source = r#"
            fn Option(comptime T: type) -> type { enum { Some(T), None } }

            fn main() -> i32 {
                let Opt = Option(i64);
                let parsed: Opt = if true {
                    @parse_i64("42")
                } else {
                    @parse_i64("7")
                };
                match parsed {
                    Opt.Some(value) => @intCast(value),
                    Opt.None => 0,
                }
            }
        "#;
        compile_to_air(source).unwrap();
    }

    #[test]
    fn inout_param_assignment_is_constrained_and_store_typed_exactly() {
        // Parameter assignments participate in inference, so a literal takes
        // the declared integer width instead of defaulting to i32. Sema then
        // proves the same exact type again at the ParamStore chokepoint.
        let output = compile_to_air(
            "fn replace(inout value: i64) { value = 42; } \
             fn main() -> i32 { let mut value: i64 = 0; replace(inout value); 0 }",
        )
        .unwrap();
        let replace = output
            .functions
            .iter()
            .find(|function| function.name == "replace")
            .unwrap();
        let stored_value = replace
            .air
            .iter()
            .find_map(|(_, inst)| match inst.data {
                AirInstData::ParamStore { value, .. } => Some(value),
                _ => None,
            })
            .expect("replace must contain a ParamStore");
        assert_eq!(replace.air.get(stored_value).ty, Type::I64);

        // An arbitrary differently-typed RHS used to bypass inference and
        // reach ParamStore. It must now fail in the frontend.
        let errors = compile_to_air(
            "fn replace(inout value: i64) { value = true; } \
             fn main() -> i32 { let mut value: i64 = 0; replace(inout value); 0 }",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors.iter().next().unwrap().kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "i64" && found == "bool"
        ));

        // Constraining only mutable parameters preserves the primary target
        // diagnostic for normal and borrowed bindings; RHS type inference must
        // not mask these as E0206.
        let immutable = compile_to_air(
            "fn replace(value: i64) { value = true; } \
             fn main() -> i32 { replace(0); 0 }",
        )
        .unwrap_err();
        assert!(matches!(
            immutable.iter().next().unwrap().kind,
            ErrorKind::AssignToImmutable(_)
        ));

        let borrowed = compile_to_air(
            "fn replace(borrow value: i64) { value = true; } \
             fn main() -> i32 { let value: i64 = 0; replace(borrow value); 0 }",
        )
        .unwrap_err();
        assert!(matches!(
            borrowed.iter().next().unwrap().kind,
            ErrorKind::MutateBorrowedValue { .. }
        ));
    }

    #[test]
    fn inout_fixed_string_assignment_materializes_the_destination_type() {
        let mut preview = PreviewFeatures::new();
        preview.insert(PreviewFeature::StringTrio);
        let output = compile_to_air_with_preview_features(
            r#"
                fn replace(inout value: Str(8)) { value = "hi"; }
                fn main() -> i32 {
                    let mut value: Str(8) = "hello";
                    replace(inout value);
                    0
                }
            "#,
            preview.clone(),
        )
        .unwrap();
        let replace = output
            .functions
            .iter()
            .find(|function| function.name == "replace")
            .unwrap();
        let stored_value = replace
            .air
            .iter()
            .find_map(|(_, inst)| match inst.data {
                AirInstData::ParamStore { value, .. } => Some(value),
                _ => None,
            })
            .expect("replace must contain a ParamStore");
        assert_eq!(
            replace
                .air
                .get(stored_value)
                .ty
                .safe_name_with_pool(Some(&output.type_pool)),
            "Str(8)"
        );

        // Never coercion remains legal. The outer Str(8) expectation belongs
        // to the assignment result and must not leak into @panic's StrBuf
        // message operand.
        for rhs in ["@panic()", "@panic(\"boom\")"] {
            compile_to_air_with_preview_features(
                &format!(
                    "fn replace(inout value: Str(8)) {{ value = {rhs}; }} \
                     fn main() -> i32 {{ \
                         let mut value: Str(8) = \"hello\"; \
                         replace(inout value); \
                         0 \
                     }}"
                ),
                preview.clone(),
            )
            .unwrap_or_else(|errors| panic!("{rhs} must coerce from never: {errors}"));
        }

        compile_to_air_with_preview_features(
            r#"
                fn die(message: StrBuf) -> ! { @panic(message) }
                fn replace(inout value: Str(8)) { value = die("boom"); }
                fn main() -> i32 {
                    let mut value: Str(8) = "hello";
                    replace(inout value);
                    0
                }
            "#,
            preview.clone(),
        )
        .unwrap_or_else(|errors| {
            panic!("a never-returning call must keep its StrBuf argument context: {errors}")
        });

        // The same expected-type boundary applies to @assert: its message is
        // a StrBuf, and the assignment is rejected for the unit result rather
        // than misdiagnosing the message as Str(8).
        let assertion = compile_to_air_with_preview_features(
            r#"
                fn replace(inout value: Str(8)) { value = @assert(false, "boom"); }
                fn main() -> i32 {
                    let mut value: Str(8) = "hello";
                    replace(inout value);
                    0
                }
            "#,
            preview.clone(),
        )
        .unwrap_err();
        assert_eq!(assertion.len(), 1);
        assert!(matches!(
            &assertion.iter().next().unwrap().kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "Str(8)" && found == "()"
        ));

        let mismatch = compile_to_air_with_preview_features(
            r#"
                fn replace(inout value: Str(8), other: Str(16)) { value = other; }
                fn main() -> i32 {
                    let mut value: Str(8) = "small";
                    let other: Str(16) = "large";
                    replace(inout value, other);
                    0
                }
            "#,
            preview.clone(),
        )
        .unwrap_err();
        assert_eq!(mismatch.len(), 1);
        assert!(matches!(
            &mismatch.iter().next().unwrap().kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "Str(8)" && found == "Str(16)"
        ));

        // Different physical widths must be rejected at the sema chokepoint,
        // even though fixed-string inference deliberately defers string
        // coercion checks. This is the original 3-word-through-2-word hazard.
        let width_mismatch = compile_to_air_with_preview_features(
            r#"
                fn replace(inout value: Str(8), other: StrBuf) { value = other; }
                fn main() -> i32 {
                    let mut value: Str(8) = "small";
                    let other: StrBuf = "growable";
                    replace(inout value, other);
                    0
                }
            "#,
            preview.clone(),
        )
        .unwrap_err();
        assert_eq!(width_mismatch.len(), 1);
        assert!(matches!(
            &width_mismatch.iter().next().unwrap().kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "Str(8)" && found == "StrBuf"
        ));

        let too_long = compile_to_air_with_preview_features(
            r#"
                fn replace(inout value: Str(8)) { value = "123456789"; }
                fn main() -> i32 {
                    let mut value: Str(8) = "hello";
                    replace(inout value);
                    0
                }
            "#,
            preview,
        )
        .unwrap_err();
        assert_eq!(too_long.len(), 1);
        assert!(matches!(
            too_long.iter().next().unwrap().kind,
            ErrorKind::StrFixedCapacityExceeded {
                capacity: 8,
                byte_len: 9
            }
        ));
    }

    #[test]
    fn test_multiple_variables() {
        let output = compile_to_air("fn main() -> i32 { let x = 10; let y = 20; x + y }").unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_empty_block_evaluates_to_unit() {
        // Empty block should evaluate to () and not panic
        let output = compile_to_air("fn main() { let _x: () = {}; }").unwrap();

        let air = &output.functions[0].air;
        // Should have a UnitConst instruction for the empty block
        let has_unit_const = air
            .iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::UnitConst));
        assert!(has_unit_const, "Empty block should produce UnitConst");
    }

    // =========================================================================
    // Error recovery tests
    // =========================================================================
    // These tests verify that one type error does not cause cascading errors.
    // The issue rue-wqyw tracks the implementation of better error recovery.

    #[test]
    fn test_single_error_no_cascade_simple() {
        // A simple case where adding an integer and boolean should report exactly one error
        let result = compile_to_air("fn main() -> i32 { 1 + true }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(
            errors.len(),
            1,
            "Should have exactly 1 error, not cascading errors"
        );

        // Verify the error is about type mismatch (integer vs bool)
        let error = errors.iter().next().unwrap();
        assert!(
            matches!(&error.kind, ErrorKind::TypeMismatch { expected, found }
                if expected.contains("integer") && found.contains("bool")),
            "Error should mention integer and bool, got: {:?}",
            error.kind
        );
    }

    #[test]
    fn test_single_error_no_cascade_with_function_call() {
        // The error-typed variable is used in a function call - should not cascade
        let result = compile_to_air(
            "fn foo(a: i32, b: i32) -> i32 { a + b }
             fn main() -> i32 {
                 let x = 1 + true;
                 foo(x, 1)
             }",
        );
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(
            errors.len(),
            1,
            "Should have exactly 1 error for the original type mismatch"
        );
    }

    #[test]
    fn test_single_error_no_cascade_deep_chain() {
        // Deep chain of operations using error-typed value - should not cascade
        let result = compile_to_air(
            "fn main() -> i32 {
                 let x = 1 + true;
                 let y = x + 1;
                 let z = y * 2;
                 let w = z - 3;
                 let v = w / 4;
                 v
             }",
        );
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(
            errors.len(),
            1,
            "Should have exactly 1 error, not 5 cascading errors"
        );
    }

    #[test]
    fn test_bool_plus_int_error() {
        // Reversed order: bool + int should also give one error
        let result = compile_to_air("fn main() -> i32 { true + 1 }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_arithmetic_on_bool_type_error() {
        // Using bool in any arithmetic should be an error
        let result = compile_to_air("fn main() -> i32 { true * true }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
    }

    // =========================================================================
    // Scope management tests
    // =========================================================================
    // These tests verify that variable scoping works correctly, including
    // shadowing, nested scopes, and proper cleanup when exiting scopes.

    #[test]
    fn test_variable_shadowing_same_type() {
        // Variable shadowing with the same type should work
        let output = compile_to_air(
            "fn main() -> i32 {
                let x = 10;
                let x = 20;  // Shadow x with a new binding
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_variable_shadowing_different_type() {
        // Variable shadowing with a different type should work
        let output = compile_to_air(
            "fn main() -> bool {
                let x = 10;
                let x = true;  // Shadow x with a different type
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
        assert_eq!(output.functions[0].air.return_type(), Type::BOOL);
    }

    #[test]
    fn test_nested_scope_variable_not_visible_outside() {
        // Variable declared in inner scope should not be visible outside
        let result = compile_to_air(
            "fn main() -> i32 {
                {
                    let x = 10;
                }
                x  // Error: x is not in scope
            }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::UndefinedVariable(_)
        ));
    }

    #[test]
    fn test_shadowed_variable_restored_after_scope() {
        // After inner scope ends, the outer variable should be visible again
        let output = compile_to_air(
            "fn main() -> i32 {
                let x = 10;
                {
                    let x = 20;  // Shadow x in inner scope
                }
                x  // Should be 10 (outer x)
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_deeply_nested_scopes() {
        // Variables in deeply nested scopes should work correctly
        let output = compile_to_air(
            "fn main() -> i32 {
                let a = 1;
                {
                    let b = 2;
                    {
                        let c = 3;
                        {
                            let d = 4;
                            a + b + c + d
                        }
                    }
                }
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 4);
    }

    #[test]
    fn test_if_else_scope_isolation() {
        // Variables in if/else branches should not leak
        let result = compile_to_air(
            "fn main() -> i32 {
                if true {
                    let x = 10;
                    x
                } else {
                    y  // Error: y not defined in this branch
                }
            }",
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_loop_scope_isolation() {
        // Variables in loop body should not leak outside
        let result = compile_to_air(
            "fn main() -> i32 {
                let mut i = 0;
                loop {
                    let inner = 1;
                    i = i + inner;
                    if i > 5 {
                        break;
                    }
                }
                i
            }",
        );

        // This should compile successfully
        assert!(result.is_ok());

        // Using the loop-local variable after the loop is an error
        let result = compile_to_air(
            "fn main() -> i32 {
                let mut i = 0;
                loop {
                    let inner = 1;
                    i = i + inner;
                    if i > 5 {
                        break;
                    }
                }
                inner
            }",
        );
        assert!(result.is_err());
    }

    // =========================================================================
    // Declaration gathering tests
    // =========================================================================
    // These tests verify that declarations are properly gathered and validated.

    #[test]
    fn test_duplicate_struct_names() {
        // Two structs with the same name should error
        let result = compile_to_air(
            "struct Foo { x: i32 }
             struct Foo { y: bool }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateTypeDefinition { .. }
        ));
    }

    #[test]
    fn test_duplicate_enum_names() {
        // Two enums with the same name should error
        let result = compile_to_air(
            "enum Color { Red, Green }
             enum Color { Blue, Yellow }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateTypeDefinition { .. }
        ));
    }

    #[test]
    fn test_struct_and_enum_name_collision() {
        // A struct and enum with the same name should error
        let result = compile_to_air(
            "struct Foo { x: i32 }
             enum Foo { A, B }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateTypeDefinition { .. }
        ));
    }

    #[test]
    fn test_function_and_type_name_collision() {
        let result = compile_to_air(
            "struct Foo { x: i32 }
             fn Foo() -> i32 { 1 }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateFunctionDefinition { .. }
        ));
    }

    #[test]
    fn test_duplicate_struct_field() {
        // Duplicate field names in a struct should error
        let result = compile_to_air(
            "struct Foo { x: i32, x: bool }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateField { .. }
        ));
    }

    #[test]
    fn test_duplicate_enum_variant() {
        // Duplicate variant names in an enum should error
        let result = compile_to_air(
            "enum Color { Red, Blue, Red }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateVariant { .. }
        ));
    }

    #[test]
    fn test_struct_field_type_resolution() {
        // Struct with field of another struct type should resolve correctly
        let output = compile_to_air(
            "@copy struct Inner { x: i32 }
             @copy struct Outer { inner: Inner }
             fn main() -> i32 {
                let o = Outer { inner: Inner { x: 42 } };
                o.inner.x
             }",
        )
        .unwrap();

        assert_eq!(output.type_pool.stats().struct_count, 3); // Inner, Outer, and String (builtin)
    }

    #[test]
    fn test_copy_struct_with_copy_fields() {
        // @copy struct with only Copy fields should compile
        let output = compile_to_air(
            "@copy struct Point { x: i32, y: i32 }
             fn main() -> i32 {
                let p = Point { x: 1, y: 2 };
                let q = p;  // Copy, not move
                p.x + q.x
             }",
        )
        .unwrap();

        assert!(
            output
                .type_pool
                .all_struct_ids()
                .iter()
                .map(|id| output.type_pool.struct_def(*id))
                .any(|s| s.name == "Point" && s.is_copy)
        );
    }

    #[test]
    fn test_copy_struct_with_non_copy_field_rejected() {
        // @copy struct with non-copy field should error
        let result = compile_to_air(
            "struct NonCopy { x: i32 }
             @copy struct Wrapper { inner: NonCopy }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::CopyStructNonCopyField(_)
        ));
    }

    #[test]
    fn test_recursive_struct_via_array() {
        // Self-referential struct through array is not allowed (no arrays of non-copy structs yet)
        // But circular reference through other means should be detected
        let result = compile_to_air(
            "struct Node { value: i32 }
             fn main() -> i32 { 0 }",
        );

        // Simple non-recursive struct should work
        assert!(result.is_ok());
    }

    #[test]
    fn test_function_signature_resolution() {
        // Function parameters and return types should resolve correctly
        let output = compile_to_air(
            "fn add(a: i32, b: i32) -> i32 { a + b }
             fn main() -> i32 { add(1, 2) }",
        )
        .unwrap();

        assert_eq!(output.functions.len(), 2);
    }

    // =========================================================================
    // Builtin type tests
    // =========================================================================
    // These tests verify that builtin types (String, etc.) work correctly.

    #[test]
    fn test_string_type_injected() {
        // StrBuf type should exist after builtin injection (ADR-0043; the
        // growable-string type, formerly `String`).
        let output = compile_to_air(
            "fn main() -> i32 {
                let s = \"hello\";
                0
            }",
        )
        .unwrap();

        // StrBuf struct should exist in the pool
        assert!(
            output
                .type_pool
                .all_struct_ids()
                .iter()
                .map(|id| output.type_pool.struct_def(*id))
                .any(|s| s.name == "StrBuf")
        );
    }

    #[test]
    fn test_string_len_method() {
        // String.len() should return u64
        let output = compile_to_air(
            "fn main() -> u64 {
                let s = \"hello\";
                s.len()
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::U64);
    }

    #[test]
    fn test_string_is_empty_method() {
        // String.is_empty() should return bool
        let output = compile_to_air(
            "fn main() -> bool {
                let s = \"hello\";
                s.is_empty()
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::BOOL);
    }

    #[test]
    fn test_string_literal_type_inference() {
        // String literal should have type String
        let output = compile_to_air(
            "fn main() -> bool {
                let s = \"hello\";
                let t = \"world\";
                s.is_empty()
            }",
        )
        .unwrap();

        // Should have local storage for two string variables
        assert!(output.functions[0].num_locals >= 2);
    }

    // =========================================================================
    // Move tracking integration tests
    // =========================================================================
    // These tests verify move semantics work correctly through the full pipeline.

    #[test]
    fn test_use_after_move_error() {
        // Using a moved value should error
        let result = compile_to_air(
            "struct NonCopy { x: i32 }
             fn consume(n: NonCopy) -> i32 { n.x }
             fn main() -> i32 {
                 let n = NonCopy { x: 42 };
                 let x = consume(n);
                 n.x  // Error: n was moved
             }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::UseAfterMove { .. }
        ));
    }

    #[test]
    fn test_partial_move_sibling_still_valid() {
        // After moving one field, sibling fields should still be usable
        // Note: Inner is non-copy, Outer is also non-copy (can't be @copy with non-copy field)
        let output = compile_to_air(
            "struct Inner { x: i32 }
             struct Outer { a: Inner, b: i32 }
             fn consume(i: Inner) -> i32 { i.x }
             fn main() -> i32 {
                 let o = Outer { a: Inner { x: 1 }, b: 2 };
                 let x = consume(o.a);  // Move o.a
                 o.b  // OK: o.b is still valid (it's Copy)
             }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_copy_type_not_moved() {
        // Copy types should not be moved, allowing multiple uses
        let output = compile_to_air(
            "@copy struct Point { x: i32, y: i32 }
             fn use_point(p: Point) -> i32 { p.x }
             fn main() -> i32 {
                 let p = Point { x: 1, y: 2 };
                 let a = use_point(p);
                 let b = use_point(p);  // OK: Point is Copy
                 a + b
             }",
        )
        .unwrap();

        assert_eq!(output.functions.len(), 2);
    }

    // =========================================================================
    // Type inference tests
    // =========================================================================
    // These tests verify type inference works correctly.

    #[test]
    fn test_integer_literal_infers_i32_by_default() {
        // Unconstrained integer literal should default to i32
        let output = compile_to_air(
            "fn main() -> i32 {
                let x = 42;
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_integer_literal_infers_from_context() {
        // Integer literal should infer type from context
        let output = compile_to_air(
            "fn main() -> i64 {
                let x: i64 = 42;
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I64);
    }

    #[test]
    fn test_integer_literal_infers_from_return_type() {
        // Integer literal should infer type from function return type
        let output = compile_to_air("fn main() -> u8 { 42 }").unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::U8);
    }

    #[test]
    fn test_integer_literal_infers_from_binary_op() {
        // Integer literal should infer type from binary operation context
        let output = compile_to_air(
            "fn main() -> i64 {
                let x: i64 = 10;
                x + 5  // 5 should infer to i64
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I64);
    }

    // =========================================================================
    // Array type tests
    // =========================================================================

    #[test]
    fn test_array_type_inference() {
        // Array element type should be inferred
        let output = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                arr[0]
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_array_index_signed_type_is_accepted() {
        // Array index may be any integer type, signed or unsigned (spec
        // 7.1:7). A negative/out-of-range value traps at runtime, it is not a
        // compile-time type error (RUE-81).
        let output = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                let i: i32 = 1;
                arr[i]  // OK: signed index accepted
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_array_index_non_integer_is_rejected() {
        // A non-integer index (bool here) is still a type error.
        let result = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                let b: bool = true;
                arr[b]  // Error: bool is not an integer
            }",
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_array_index_literal_infers_integer() {
        // Integer literal used as array index should compile; it defaults to
        // i32 (the integer-literal default) rather than being forced unsigned.
        let output = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                arr[1]
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_array_length_mismatch() {
        // Array length in type annotation must match initializer
        let result = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2];  // Error: length mismatch
                arr[0]
            }",
        );

        assert!(result.is_err());
    }

    // ========================================================================
    // Type Intern Pool tests (ADR-0024 Phase 1)
    //
    // These tests verify that the TypeInternPool is correctly populated during
    // declaration collection and that its contents match the existing type
    // registries (struct_defs, enum_defs).
    // ========================================================================

    /// Helper to gather declarations and return the Sema state for testing.
    fn gather_declarations_for_testing(source: &str) -> Sema<'static> {
        // We need to leak the interner for the static lifetime
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();

        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        // Leak both to get 'static lifetime for testing
        let rir = Box::leak(Box::new(rir));
        let interner = Box::leak(Box::new(interner));

        let mut sema = Sema::new(rir, interner, PreviewFeatures::new());
        sema.inject_builtin_types();
        sema.register_type_names().unwrap();
        sema.resolve_declarations().unwrap();
        sema
    }

    #[test]
    fn named_method_registration_preserves_complete_parameter_contract() {
        let sema = gather_declarations_for_testing(
            "struct Probe {
                value: i32,
                fn modes(
                    borrow self,
                    borrow read: i32,
                    inout write: i32,
                    comptime scale: i32,
                ) -> i32 { read + write + scale }
            }
            fn main() -> i32 { 0 }",
        );

        let probe_name = sema.interner.get("Probe").unwrap();
        let method_name = sema.interner.get("modes").unwrap();
        let struct_id = *sema.structs.get(&probe_name).unwrap();
        let method = sema.methods.get(&(struct_id, method_name)).unwrap();

        assert_eq!(
            sema.param_arena.modes(method.params),
            &[
                RirParamMode::Borrow,
                RirParamMode::Inout,
                RirParamMode::Normal,
            ]
        );
        assert_eq!(
            sema.param_arena.comptime(method.params),
            &[false, false, true]
        );

        // The inference-side signature must carry the identical source-mode
        // vector rather than a type-only shadow of MethodInfo (RUE-634).
        let inference = sema.build_inference_context();
        let signature = inference
            .method_sigs
            .get(&(struct_id, method_name))
            .unwrap();
        assert_eq!(
            signature.param_modes,
            vec![
                RirParamMode::Borrow,
                RirParamMode::Inout,
                RirParamMode::Normal,
            ]
        );
    }

    #[test]
    fn gather_adapter_rebuilds_exact_named_method_records() {
        let sema = gather_declarations_for_testing(
            "struct Probe { fn answer() -> i32 { 42 } }
             fn main() -> i32 { Probe.answer() }",
        );
        let rebuilt = crate::sema::gather::rebuild_named_method_declarations(
            sema.rir,
            &sema.structs,
            &sema.structs_by_file_name,
        );

        assert_eq!(rebuilt, sema.named_method_declarations);
        let probe = sema.interner.get("Probe").unwrap();
        let answer = sema.interner.get("answer").unwrap();
        let struct_id = sema.structs[&probe];
        let declaration = rebuilt[&(struct_id, answer)];
        let rue_rir::InstData::FnDecl { name, body, .. } = sema.rir.get(declaration).data else {
            panic!("rebuilt named method record must point at FnDecl");
        };
        assert_eq!(name, answer);
        assert_eq!(body, sema.methods[&(struct_id, answer)].body);
    }

    #[test]
    fn anonymous_method_self_mode_participates_in_structural_identity() {
        let output = compile_to_air(
            "fn ByValue() -> type {
                struct { value: i32, fn f(self) -> i32 { self.value } }
            }
            fn ByBorrow() -> type {
                struct { value: i32, fn f(borrow self) -> i32 { self.value } }
            }
            fn main() -> i32 {
                let V = ByValue();
                let B = ByBorrow();
                let v = V { value: 20 };
                let b = B { value: 22 };
                v.f() + b.f()
            }",
        )
        .unwrap();

        let mut physical_modes: Vec<Vec<bool>> = output
            .functions
            .iter()
            .filter(|function| {
                function.name.starts_with("__anon_struct_") && function.name.ends_with(".f")
            })
            .map(|function| function.param_modes.by_ref().to_vec())
            .collect();
        physical_modes.sort();
        assert_eq!(physical_modes, vec![vec![false], vec![true]]);
    }

    #[test]
    fn anonymous_method_explicit_mode_participates_in_structural_identity() {
        let output = compile_to_air(
            "fn Normal() -> type {
                struct { fn f(borrow self, x: i64) -> i64 { x } }
            }
            fn Borrowed() -> type {
                struct { fn f(borrow self, borrow x: i64) -> i64 { x } }
            }
            fn main() -> i32 {
                let N = Normal();
                let B = Borrowed();
                let n = N {};
                let b = B {};
                let x: i64 = 20;
                let y: i64 = 22;
                @intCast(n.f(x) + b.f(borrow y))
            }",
        )
        .unwrap();

        let mut physical_modes: Vec<Vec<bool>> = output
            .functions
            .iter()
            .filter(|function| {
                function.name.starts_with("__anon_struct_") && function.name.ends_with(".f")
            })
            .map(|function| function.param_modes.by_ref().to_vec())
            .collect();
        physical_modes.sort();
        assert_eq!(physical_modes, vec![vec![true, false], vec![true, true]]);
    }

    #[test]
    fn anonymous_method_comptime_flag_participates_in_structural_identity() {
        let output = compile_to_air(
            "fn Runtime() -> type {
                struct { fn f(x: i32) -> i32 { x } }
            }
            fn Compiletime() -> type {
                struct { fn f(comptime x: i32) -> i32 { x } }
            }
            fn main() -> i32 {
                let R = Runtime();
                let C = Compiletime();
                let _r = R {};
                let _c = C {};
                0
            }",
        )
        .unwrap();

        let main = output
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        let struct_ids: std::collections::HashSet<_> = main
            .air
            .iter()
            .filter_map(|(_, inst)| match inst.data {
                AirInstData::StructInit { struct_id, .. } => Some(struct_id),
                _ => None,
            })
            .collect();
        assert_eq!(struct_ids.len(), 2);
    }

    #[test]
    fn test_type_pool_populated_with_builtin_string() {
        // The StrBuf type should be in the pool after builtin injection
        // (ADR-0043; formerly `String`).
        let sema = gather_declarations_for_testing("fn main() -> i32 { 0 }");

        let strbuf_name = sema.interner.get("StrBuf").unwrap();
        let pool_string = sema.type_pool.get_struct_by_name(strbuf_name);

        assert!(pool_string.is_some(), "StrBuf should be in the type pool");

        // Verify the struct lookup has it
        let registry_string = sema.structs.get(&strbuf_name);
        assert!(
            registry_string.is_some(),
            "StrBuf should be in struct registry"
        );

        // The deprecated `String` alias resolves to the *same* struct id in the
        // registry, so existing `s: String` annotations keep type-checking.
        let alias_name = sema.interner.get("String").unwrap();
        assert_eq!(
            sema.structs.get(&alias_name),
            registry_string,
            "`String` alias should resolve to the StrBuf struct"
        );

        // Check the pool definition
        let pool_def = sema.type_pool.get_struct_def(pool_string.unwrap()).unwrap();

        assert_eq!(pool_def.name, "StrBuf");
        assert!(pool_def.is_builtin, "StrBuf should be marked as builtin");
    }

    #[test]
    fn test_type_pool_populated_with_user_struct() {
        let sema = gather_declarations_for_testing(
            "struct Point { x: i32, y: i32 }
             fn main() -> i32 { 0 }",
        );

        let point_name = sema.interner.get("Point").unwrap();

        // Check the pool has the struct
        let pool_point = sema.type_pool.get_struct_by_name(point_name);
        assert!(pool_point.is_some(), "Point should be in the type pool");

        // Verify struct lookup has it
        let registry_point = sema.structs.get(&point_name);
        assert!(
            registry_point.is_some(),
            "Point should be in struct registry"
        );

        // Check the pool definition
        let pool_def = sema.type_pool.get_struct_def(pool_point.unwrap()).unwrap();

        assert_eq!(pool_def.name, "Point");
        assert_eq!(pool_def.fields.len(), 2);
        assert_eq!(pool_def.fields[0].name, "x");
        assert_eq!(pool_def.fields[1].name, "y");
        assert!(
            !pool_def.is_builtin,
            "Point should not be marked as builtin"
        );
    }

    #[test]
    fn test_type_pool_populated_with_enum() {
        let sema = gather_declarations_for_testing(
            "enum Color { Red, Green, Blue }
             fn main() -> i32 { 0 }",
        );

        let color_name = sema.interner.get("Color").unwrap();

        // Check the pool has the enum
        let pool_color = sema.type_pool.get_enum_by_name(color_name);
        assert!(pool_color.is_some(), "Color should be in the type pool");

        // Verify pool and registry agree - enum_id is now pool-based
        let registry_color = sema.enums.get(&color_name);
        assert!(registry_color.is_some(), "Color should be in enum registry");

        // Use type_pool.enum_def() to get the definition using pool-based EnumId
        let enum_id = *registry_color.unwrap();
        let pool_def = sema.type_pool.enum_def(enum_id);

        assert_eq!(pool_def.name, "Color");
        assert_eq!(pool_def.variants.len(), 3);
        assert_eq!(pool_def.variants[0], "Red");
        assert_eq!(pool_def.variants[1], "Green");
        assert_eq!(pool_def.variants[2], "Blue");
    }

    #[test]
    fn test_type_pool_copy_struct() {
        let sema = gather_declarations_for_testing(
            "@copy
             struct Data { value: i32 }
             fn main() -> i32 { 0 }",
        );

        let data_name = sema.interner.get("Data").unwrap();
        let pool_data = sema.type_pool.get_struct_by_name(data_name).unwrap();
        let pool_def = sema.type_pool.get_struct_def(pool_data).unwrap();

        assert!(pool_def.is_copy, "Data should be marked as @copy");
    }

    #[test]
    fn test_type_pool_stats() {
        let sema = gather_declarations_for_testing(
            "struct A {}
             struct B {}
             enum E { X }
             fn main() -> i32 { 0 }",
        );

        let stats = sema.type_pool.stats();

        // 3 structs: String (builtin) + A + B
        assert_eq!(stats.struct_count, 3);
        // 3 enums: Arch (builtin) + Os (builtin) + E
        assert_eq!(stats.enum_count, 3);
        // No arrays in Phase 1
        assert_eq!(stats.array_count, 0);
        // Total: 6 composite types
        assert_eq!(stats.total, 6);
    }

    #[test]
    fn test_type_pool_all_registries_match() {
        // Test with multiple types to verify complete consistency
        let sema = gather_declarations_for_testing(
            "struct Point { x: i32, y: i32 }
             struct Empty {}
             @copy struct Value { v: bool }
             enum Status { Ok, Error }
             enum Direction { Up, Down, Left, Right }
             fn main() -> i32 { 0 }",
        );

        // Verify all structs in registry are in pool
        for (name_spur, &struct_id) in &sema.structs {
            // Use type_pool.struct_def() which takes pool-based struct_id
            let pool_def = sema.type_pool.struct_def(struct_id);

            // Also verify the pool can look up by name
            let pool_type = sema.type_pool.get_struct_by_name(*name_spur);
            assert!(
                pool_type.is_some(),
                "Struct '{}' should be in pool by name",
                pool_def.name
            );
        }

        // Verify all enums in registry are in pool
        for (name_spur, &enum_id) in &sema.enums {
            // Use type_pool.enum_def() which takes pool-based enum_id
            let pool_def = sema.type_pool.enum_def(enum_id);

            // Also verify the pool can look up by name
            let pool_type = sema.type_pool.get_enum_by_name(*name_spur);
            assert!(
                pool_type.is_some(),
                "Enum '{}' should be in pool by name",
                pool_def.name
            );
        }

        // Verify stats are available
        let stats = sema.type_pool.stats();
        assert!(stats.struct_count > 0); // At least String builtin
        assert!(stats.enum_count > 0); // The enum we added
    }
}
