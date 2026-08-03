#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::ConstValue;
    use crate::inst::{AirArgMode, AirInstData, AirRef};
    use crate::sema::{Sema, SemaOutput};
    use crate::types::{StructId, Type};
    use lasso::ThreadedRodeo;
    use rue_error::{
        CompileErrors, CompileResult, ErrorKind, MultiErrorResult, PreviewFeature, PreviewFeatures,
    };
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::{AstGen, InstData, InternalIntrinsic, Rir, RirParamMode};
    use rue_span::{FileId, Span};

    struct TestModule {
        id: String,
        file_id: FileId,
        path: String,
    }

    struct TestSite {
        importer: String,
        offset: u32,
        specifier: String,
        target: String,
    }

    struct TestCanonicalImportView {
        modules: Vec<TestModule>,
        sites: Vec<TestSite>,
    }

    impl crate::CanonicalImportView for TestCanonicalImportView {
        fn visit_modules(
            &self,
            visitor: &mut dyn FnMut(&str, FileId, &str) -> CompileResult<()>,
        ) -> CompileResult<()> {
            for module in &self.modules {
                visitor(&module.id, module.file_id, &module.path)?;
            }
            Ok(())
        }

        fn visit_resolved_sites(
            &self,
            visitor: &mut dyn FnMut(&str, u32, &str, &str) -> CompileResult<()>,
        ) -> CompileResult<()> {
            for site in &self.sites {
                visitor(&site.importer, site.offset, &site.specifier, &site.target)?;
            }
            Ok(())
        }
    }

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

        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let sema = Sema::new_synthetic(&rir, &mut interner, preview_features);
        sema.analyze_all()
    }

    fn gather_two_file_declarations_for_testing(main: &str, dependency: &str) -> Sema<'static> {
        let dependency_file = FileId::new(1);
        let (rir, interner) =
            lower_files(&[(main, FileId::DEFAULT), (dependency, dependency_file)]);
        let import = rir
            .iter()
            .find_map(|(_, inst)| {
                let InstData::Intrinsic { name, args } = &inst.data else {
                    return None;
                };
                (interner.resolve(name) == "import").then_some((inst.span.start, args))
            })
            .expect("two-file test main must import its dependency");
        let argument = rir
            .intrinsic_args(import.1)
            .get(0)
            .expect("import must have one argument");
        let InstData::StringConst { content, .. } = rir.get(argument).data else {
            panic!("import argument must be a string")
        };
        let specifier = interner.resolve(&content).to_owned();
        let view = TestCanonicalImportView {
            modules: vec![
                TestModule {
                    id: "main.rue".to_owned(),
                    file_id: FileId::DEFAULT,
                    path: "/main.rue".to_owned(),
                },
                TestModule {
                    id: specifier.clone(),
                    file_id: dependency_file,
                    path: "/dep.rue".to_owned(),
                },
            ],
            sites: vec![TestSite {
                importer: "main.rue".to_owned(),
                offset: import.0,
                specifier: specifier.clone(),
                target: specifier,
            }],
        };
        let rir = Box::leak(Box::new(rir));
        let interner = Box::leak(Box::new(interner));
        let mut sema = Sema::new_synthetic(rir, interner, PreviewFeatures::new());
        sema.set_root_file_id(FileId::DEFAULT);
        sema.set_file_paths(HashMap::from([
            (FileId::DEFAULT, "/main.rue".to_owned()),
            (dependency_file, "/dep.rue".to_owned()),
        ]));
        sema.set_canonical_imports(&view).unwrap();
        sema.inject_builtin_types();
        sema.register_type_names().unwrap();
        sema.resolve_declarations().unwrap();
        sema
    }

    fn compile_to_air_with_authoritative_identity_order(
        source: &str,
    ) -> MultiErrorResult<SemaOutput> {
        // Synthetic tests hash names into token slots. These ordering-sensitive
        // cases install the production contract instead: definition tokens
        // follow the stable declaration-key order.
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from_error)?;
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse()?;
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let shells = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()?;
        let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
        let (_, owners) = authoritative_test_endpoints(&shell_records);
        let mut definition_shells = shell_records;
        definition_shells.sort_by(|left, right| left.identity.cmp(&right.identity));
        let definitions = definition_shells
            .iter()
            .enumerate()
            .map(|(slot, shell)| crate::SemanticDefinitionEndpoint {
                token: crate::SemanticDefinitionToken::new(92, slot as u32),
                file: shell.declaration_span.file_id.index(),
                name: shell.identity.name.clone(),
                kind: shell.identity.kind,
                owner: shell.identity.owner.clone(),
            })
            .collect::<Vec<_>>();
        shells
            .install_stable_identity_endpoints(&definitions, &[])
            .unwrap()
            .resolve_declarations()?
            .install_body_owner_tokens(&owners)
            .unwrap()
            .analyze_all_bodies_for_test()
    }

    fn lower_files(files: &[(&str, FileId)]) -> (Rir, ThreadedRodeo) {
        let mut interner = ThreadedRodeo::default();
        let mut items = Vec::new();
        for &(source, file_id) in files {
            let (tokens, next) = Lexer::with_interner_and_file_id(source, interner, file_id)
                .tokenize()
                .unwrap();
            let (ast, next) = Parser::new(tokens, next).parse().unwrap();
            items.extend(ast.items);
            interner = next;
        }
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&items);
        let rir = astgen.finish();
        (rir, interner)
    }

    fn authoritative_test_endpoints(
        shells: &[crate::SemanticDeclarationShell],
    ) -> (
        Vec<crate::SemanticDefinitionEndpoint>,
        Vec<crate::BodyOwnerEndpoint>,
    ) {
        let definitions = shells
            .iter()
            .enumerate()
            .map(|(slot, shell)| crate::SemanticDefinitionEndpoint {
                token: crate::SemanticDefinitionToken::new(91, slot as u32),
                file: shell.declaration_span.file_id.index(),
                name: shell.identity.name.clone(),
                kind: shell.identity.kind,
                owner: shell.identity.owner.clone(),
            })
            .collect();
        let owners = shells
            .iter()
            .filter_map(|shell| {
                let (kind, name, owner_name) = match shell.identity.kind {
                    crate::StableDefinitionKind::Function => (
                        crate::BodyOwnerKind::FreeFunction,
                        shell.identity.name.to_string(),
                        None,
                    ),
                    crate::StableDefinitionKind::Method => (
                        crate::BodyOwnerKind::Method,
                        shell.identity.name.to_string(),
                        shell.identity.owner.as_deref().map(str::to_owned),
                    ),
                    crate::StableDefinitionKind::AssociatedFunction => (
                        crate::BodyOwnerKind::AssociatedFunction,
                        shell.identity.name.to_string(),
                        shell.identity.owner.as_deref().map(str::to_owned),
                    ),
                    crate::StableDefinitionKind::Destructor => {
                        let owner = shell.identity.owner.as_deref()?.to_owned();
                        (crate::BodyOwnerKind::Destructor, owner.clone(), Some(owner))
                    }
                    _ => return None,
                };
                Some(crate::BodyOwnerEndpoint {
                    token: crate::BodyOwnerToken::new(91, shell.source_order),
                    kind,
                    file: shell.declaration_span.file_id.index(),
                    name,
                    owner_name,
                })
            })
            .collect();
        (definitions, owners)
    }

    fn authoritative_bound<'a>(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        definitions: &[crate::SemanticDefinitionEndpoint],
        owners: &[crate::BodyOwnerEndpoint],
    ) -> crate::sema::BoundSema<'a> {
        Sema::new_synthetic(rir, interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()
            .unwrap()
            .install_stable_identity_endpoints(definitions, &[])
            .unwrap()
            .resolve_declarations()
            .unwrap()
            .install_body_owner_tokens(owners)
            .unwrap()
    }

    #[test]
    #[should_panic(expected = "production body analysis requires installed body-owner tokens")]
    fn production_owner_identity_rejects_a_present_state_without_tokens() {
        let tokens = HashMap::new();
        super::super::resolve_body_owner_token(
            &tokens,
            FileId::DEFAULT,
            "main",
            None,
            crate::BodyOwnerKind::FreeFunction,
            false,
        );
    }

    #[test]
    fn synthetic_owner_identity_retains_fixture_zero_issuer() {
        let tokens = HashMap::new();
        assert_eq!(
            super::super::resolve_body_owner_token(
                &tokens,
                FileId::DEFAULT,
                "main",
                None,
                crate::BodyOwnerKind::FreeFunction,
                true,
            ),
            crate::BodyOwnerToken::new(0, FileId::DEFAULT.index())
        );
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

    #[test]
    fn reserved_looking_source_intrinsics_never_select_internal_operations() {
        for (name, args) in [
            ("__rue_iter_len", "0"),
            ("__rue_char_scalar", "0, 0"),
            ("__rue_char_next", "0, 0"),
            ("__rue_char_scalar_lossy", "0, 0"),
            ("__rue_char_next_lossy", "0, 0"),
        ] {
            let source = format!("fn main() {{ @{name}({args}) }}");
            let errors = compile_to_air(&source).expect_err("source spelling must stay unknown");
            assert!(
                errors.iter().any(|error| matches!(
                    &error.kind,
                    ErrorKind::UnknownIntrinsic(actual) if actual == name
                )),
                "{name}: {errors:?}"
            );
        }
    }

    #[test]
    fn malformed_internal_intrinsic_arity_is_reported_without_panicking() {
        let source = "fn main() { @dbg(1); }";
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, mut interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let mut rir = astgen.finish_editor();
        let intrinsic_ref = rir
            .iter()
            .find_map(|(inst_ref, inst)| match inst.data {
                InstData::Intrinsic { .. } => Some(inst_ref),
                _ => None,
            })
            .unwrap();
        rir.replace_internal_intrinsic(intrinsic_ref, InternalIntrinsic::IterLen, &[])
            .unwrap();

        let errors = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .analyze_all()
            .expect_err("malformed compiler RIR must be diagnosed");
        assert!(errors.iter().any(|error| matches!(
            &error.kind,
            ErrorKind::InternalError(message)
                if message.contains("`__rue_iter_len` expects 1 argument, found 0")
        )));
    }

    #[test]
    fn main_rejects_runtime_parameters() {
        let errors = compile_to_air("fn main(value: i32) -> i32 { value }")
            .expect_err("a runtime parameter would violate the entry ABI");
        assert!(errors.iter().any(|error| matches!(
            error.kind,
            ErrorKind::InvalidMainSignature {
                reason: "`main` must not declare parameters"
            }
        )));
        assert!(errors.iter().all(|error| error.has_span()));
    }

    #[test]
    fn main_rejects_comptime_parameters() {
        let errors = compile_to_air("fn main(comptime T: type) -> i32 { 0 }")
            .expect_err("a generic main cannot be specialized by the runtime");
        assert!(errors.iter().any(|error| matches!(
            error.kind,
            ErrorKind::InvalidMainSignature {
                reason: "`main` must not declare parameters"
            }
        )));
    }

    #[test]
    fn main_rejects_non_abi_return_type() {
        let errors = compile_to_air("fn main() -> bool { true }")
            .expect_err("the runtime cannot consume a bool return value");
        assert!(errors.iter().any(|error| matches!(
            error.kind,
            ErrorKind::InvalidMainSignature {
                reason: "`main` must return `i32` or `()`"
            }
        )));
    }

    #[test]
    fn main_rejects_aggregate_return_type() {
        let errors =
            compile_to_air("struct Exit { code: i32 } fn main() -> Exit { Exit { code: 0 } }")
                .expect_err("the runtime cannot consume an aggregate return value");
        assert!(errors.iter().any(|error| matches!(
            error.kind,
            ErrorKind::InvalidMainSignature {
                reason: "`main` must return `i32` or `()`"
            }
        )));
    }

    #[test]
    fn main_accepts_i32_and_unit_return_types() {
        assert!(compile_to_air("fn main() -> i32 { 0 }").is_ok());
        assert!(compile_to_air("fn main() {}").is_ok());
        assert!(compile_to_air("fn main() -> () {}").is_ok());
    }

    #[test]
    fn byref_arguments_reject_non_places_during_air_analysis() {
        let cases = [
            (
                "const VALUE: i32 = 1; fn take(inout x: i32) {} fn main() -> i32 { take(inout VALUE); 0 }",
                true,
            ),
            (
                "const VALUE: i32 = 1; fn take(borrow x: i32) {} fn main() -> i32 { take(borrow VALUE); 0 }",
                false,
            ),
            (
                "fn take(inout x: i32) {} fn main() -> i32 { take(inout 1); 0 }",
                true,
            ),
            (
                "fn take(borrow x: i32) {} fn main() -> i32 { take(borrow (1 + 2)); 0 }",
                false,
            ),
            (
                "fn value() -> i32 { 1 } fn take(borrow x: i32) {} fn main() -> i32 { take(borrow value()); 0 }",
                false,
            ),
        ];

        for (source, is_inout) in cases {
            let errors = compile_to_air(source).expect_err("non-place must fail in sema");
            assert!(
                errors.iter().any(|error| {
                    if is_inout {
                        matches!(error.kind, ErrorKind::InoutNonLvalue)
                    } else {
                        matches!(error.kind, ErrorKind::BorrowNonLvalue)
                    }
                }),
                "source: {source}\nerrors: {errors:#?}"
            );
        }
    }

    #[test]
    fn byref_arguments_accept_places_and_forwarded_projections() {
        compile_to_air(
            "struct Pair { value: i32 }
             fn edit(inout x: i32) { x = x + 1; }
             fn read(borrow x: i32) -> i32 { x }
             fn forward(inout edit_pair: Pair, borrow read_pair: Pair) -> i32 {
                 edit(inout edit_pair.value);
                 read(borrow read_pair.value)
             }
             fn main() -> i32 {
                 let mut pair = Pair { value: 1 };
                 let other = Pair { value: 2 };
                 let mut values = [1, 2];
                 edit(inout pair.value);
                 edit(inout values[0]);
                 read(borrow pair.value) + read(borrow values[1]) + forward(inout pair, borrow other)
             }",
        )
        .unwrap();
    }

    #[test]
    fn byref_method_receiver_rejects_call_result_during_air_analysis() {
        let errors = compile_to_air(
            "struct Pair {
                 value: i32,
                 fn read(borrow self) -> i32 { self.value }
             }
             fn make() -> Pair { Pair { value: 1 } }
             fn main() -> i32 { make().read() }",
        )
        .expect_err("a call result is not an addressable method receiver");
        assert!(
            errors
                .iter()
                .any(|error| matches!(error.kind, ErrorKind::BorrowNonLvalue)),
            "errors: {errors:#?}"
        );
    }

    #[test]
    fn body_work_counts_reachable_call_graph_exactly() {
        let output = compile_to_air(
            "fn leaf() -> i32 { 1 }\nfn middle() -> i32 { leaf() }\nfn main() -> i32 { middle() }",
        )
        .unwrap();
        let work = output.body_analysis_work;
        assert_eq!(work.bodies_attempted, 3);
        assert_eq!(work.bodies_succeeded, 3);
        assert_eq!(work.bodies_failed, 0);
        assert_eq!(
            work.air_instructions_produced,
            output
                .functions
                .iter()
                .map(|function| function.air.len())
                .sum()
        );
        assert_eq!(work.local_strings_produced, 0);
        assert_eq!(work.string_ids_remapped, 0);
    }

    #[test]
    fn ordinary_body_exports_are_owned_pre_specialization_and_counted_exactly() {
        let output = compile_to_air("fn leaf() -> i32 { 1 }\nfn main() -> i32 { leaf() }").unwrap();
        assert_eq!(output.body_analysis_work.ordinary_body_exports_attempted, 2);
        assert_eq!(output.body_analysis_work.ordinary_body_exports_succeeded, 2);
        assert_eq!(output.body_analysis_work.ordinary_body_exports_rejected, 0);
        assert_eq!(output.ordinary_body_exports.len(), 2);
        assert_eq!(
            output
                .body_analysis_work
                .ordinary_body_export_instructions_emitted,
            output
                .ordinary_body_exports
                .iter()
                .map(|export| export.body.instructions.len())
                .sum()
        );
        assert_eq!(
            output
                .body_analysis_work
                .ordinary_body_export_places_emitted,
            output
                .ordinary_body_exports
                .iter()
                .map(|export| export.body.places.len())
                .sum()
        );
        assert_eq!(
            output
                .body_analysis_work
                .ordinary_body_export_strings_emitted,
            0
        );

        let main =
            output
                .ordinary_body_exports
                .iter()
                .find(|export| {
                    output
                        .analyzed_body_owners
                        .iter()
                        .any(|owner| owner.token() == Some(export.owner))
                        && export.body.instructions.iter().any(|inst| {
                            matches!(&inst.data, crate::SemanticBodyInstData::Call { .. })
                        })
                })
                .expect("main durable body");
        assert!(main.body.strings.is_empty());
        assert!(
            main.body
                .instructions
                .iter()
                .all(|inst| { !matches!(inst.data, crate::SemanticBodyInstData::CallGeneric) })
        );
    }

    #[test]
    fn unresolved_generic_call_rejects_only_its_durable_candidate() {
        let output = compile_to_air(
            "fn id(comptime T: type, value: T) -> T { value }\nfn main() -> i32 { id(i32, 1) }",
        )
        .unwrap();
        assert_eq!(output.body_analysis_work.ordinary_body_exports_attempted, 1);
        assert_eq!(output.body_analysis_work.ordinary_body_exports_succeeded, 0);
        assert_eq!(output.body_analysis_work.ordinary_body_exports_rejected, 1);
        assert!(output.ordinary_body_exports.is_empty());
        assert!(
            output
                .functions
                .iter()
                .any(|function| function.name == "main")
        );
        assert_eq!(
            output
                .body_analysis_work
                .ordinary_body_export_instructions_emitted,
            0
        );
        assert_eq!(
            output
                .body_analysis_work
                .ordinary_body_export_places_emitted,
            0
        );
        assert_eq!(
            output
                .body_analysis_work
                .ordinary_body_export_strings_emitted,
            0
        );
    }

    #[test]
    fn durable_body_anchors_ignore_surrounding_source_relocation() {
        let original = compile_to_air("fn main() -> i32 { 42 }").unwrap();
        let relocated = compile_to_air("\n\nfn main() -> i32 { 42 }\n").unwrap();
        assert_eq!(original.ordinary_body_exports.len(), 1);
        assert_eq!(relocated.ordinary_body_exports.len(), 1);
        assert_eq!(
            original.ordinary_body_exports[0].body,
            relocated.ordinary_body_exports[0].body
        );
    }

    #[test]
    fn warning_body_exports_without_losing_ordinary_warning() {
        let output = compile_to_air("fn main() { let unused = 1; }").unwrap();
        assert_eq!(output.body_analysis_work.ordinary_body_exports_attempted, 1);
        assert_eq!(output.body_analysis_work.ordinary_body_exports_succeeded, 1);
        assert_eq!(output.body_analysis_work.ordinary_body_exports_rejected, 0);
        assert_eq!(output.ordinary_body_exports.len(), 1);
        assert_eq!(output.ordinary_body_exports[0].body.warnings.len(), 1);
        assert!(output.warnings.iter().any(|warning| {
            matches!(warning.kind, rue_error::WarningKind::UnusedVariable(ref name) if name == "unused")
        }));
        assert_eq!(
            output
                .body_analysis_work
                .ordinary_body_export_instructions_emitted,
            output.ordinary_body_exports[0].body.instructions.len()
        );
        assert_eq!(
            output
                .body_analysis_work
                .ordinary_body_export_places_emitted,
            output.ordinary_body_exports[0].body.places.len()
        );
        assert_eq!(
            output
                .body_analysis_work
                .ordinary_body_export_strings_emitted,
            0
        );
    }

    #[test]
    fn exported_supported_body_round_trips_through_a_fresh_air_epoch_exactly() {
        let source = "fn main() -> i32 { if 1 < 2 { (3 + 4) * 5 } else { 6 - 7 } }";
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let main = interner.get("main").unwrap();
        let body_span = rir
            .iter()
            .find_map(|(_, inst)| match inst.data {
                rue_rir::InstData::FnDecl { name, body, .. } if name == main => {
                    Some(rir.get(body).span)
                }
                _ => None,
            })
            .expect("main body span");
        let output = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .analyze_all()
            .unwrap();
        let export = output
            .ordinary_body_exports
            .iter()
            .find(|export| {
                output
                    .analyzed_body_owners
                    .iter()
                    .any(|owner| owner.token() == Some(export.owner))
            })
            .expect("warning-free main export");
        let epoch = crate::SemanticImportEpoch::<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >::new(vec![], vec![], vec![])
        .unwrap();
        let imported = epoch.import_body(&export.body, body_span).unwrap();
        let source = output
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();

        assert_eq!(source.air.return_type(), imported.air.return_type());
        assert_eq!(source.air.len(), imported.air.len());
        for ((source_ref, source_inst), (imported_ref, imported_inst)) in
            source.air.iter().zip(imported.air.iter())
        {
            assert_eq!(source_ref.as_u32(), imported_ref.as_u32());
            assert_eq!(
                format!("{:?}", source_inst.data),
                format!("{:?}", imported_inst.data)
            );
            assert_eq!(source_inst.ty, imported_inst.ty);
            assert_eq!(source_inst.span, imported_inst.span);
        }
        assert_eq!(
            format!("{:?}", source.air.places()),
            format!("{:?}", imported.air.places())
        );
        assert_eq!(
            format!("{:?}", source.air.projections()),
            format!("{:?}", imported.air.projections())
        );
        assert_eq!(source.air.param_drops(), imported.air.param_drops());
        assert_eq!(source.num_locals, imported.num_locals);
        assert_eq!(source.num_param_slots, imported.num_param_slots);
        assert_eq!(source.param_modes, imported.param_modes);
        assert_eq!(
            source.allow_unreachable_code,
            imported.allow_unreachable_code
        );
        assert_eq!(output.strings, imported.strings);
        assert!(imported.warnings.is_empty());
        for slot in 0..source.num_locals {
            assert_eq!(
                source.air.is_borrow_slot(slot),
                imported.air.is_borrow_slot(slot)
            );
        }
    }

    #[test]
    fn str_literal_body_imports_fresh() {
        let source = "fn make() -> str { \"hello\" } fn main() { make(); }";
        let output = compile_to_air(source).unwrap();
        assert_eq!(output.ordinary_body_exports.len(), 2);
        let make_type = output
            .functions
            .iter()
            .find(|function| function.name == "make")
            .unwrap()
            .air
            .return_type();
        let body = output
            .ordinary_body_exports
            .iter()
            .map(|export| &export.body)
            .find(|body| body.strings.as_ref() == [std::sync::Arc::from("hello")])
            .expect("reachable str-returning helper export");
        assert_eq!(body.strings.as_ref(), &[std::sync::Arc::from("hello")]);
        assert!(
            body.instructions
                .iter()
                .all(|inst| !matches!(inst.ty, crate::SemanticImportType::Nominal(_)))
        );

        let epoch = crate::SemanticImportEpoch::<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >::new(vec![], vec![], vec![])
        .unwrap();
        let imported = epoch
            .import_body(
                body,
                rue_span::Span::with_file(rue_span::FileId::DEFAULT, 0, source.len() as u32),
            )
            .unwrap();
        assert_eq!(imported.strings, vec!["hello"]);
        assert_eq!(
            imported
                .air
                .return_type()
                .safe_name_with_pool(Some(epoch.type_pool())),
            make_type.safe_name_with_frozen_pool(Some(&output.type_pool))
        );
    }

    #[test]
    fn non_string_tail_in_str_function_is_rejected_during_sema() {
        let errors = compile_to_air(
            r#"
                fn choose(c: bool) -> str {
                    // Parenthesized so the `if` is in operand position: in
                    // statement/tail position a block-like expression is a
                    // complete statement and a following `==` is a syntax error
                    // (RUE-918). The bool comparison tail still mismatches `str`.
                    (if c { "hello" } else { "world" }) == choose(true)
                }
                fn main() -> i32 {
                    let s: str = "hello";
                    if s == choose(true) { 0 } else { 1 }
                }
            "#,
        )
        .expect_err("a bool tail cannot satisfy a str return type");

        assert!(matches!(
            &errors.iter().next().unwrap().kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "bool" && found == "str"
                    || expected == "str" && found == "bool"
        ));
    }

    #[test]
    fn specialization_work_separates_unique_and_duplicate_requests() {
        let output = compile_to_air(
            "fn identity(comptime T: type, value: T) -> T { value }\nfn main() -> i32 { identity(i32, 1) + identity(i32, 2) }",
        )
        .unwrap();
        let work = output.body_analysis_work;
        assert_eq!(work.generic_calls_observed, 2);
        assert_eq!(work.specialization_requests_unique, 1);
        assert_eq!(work.specialization_requests_duplicate, 1);
        assert_eq!(work.specialization_rewrites, 2);
        assert_eq!(work.specialization_rounds, 1);
        assert_eq!(work.specialized_bodies_attempted, 1);
        assert_eq!(work.specialized_bodies_succeeded, 1);
        assert_eq!(work.specialized_bodies_failed, 0);
    }

    #[test]
    fn completed_specializations_export_stable_identity_and_exact_work() {
        let output = compile_to_air(
            "fn id(comptime T: type, value: T) -> T { value }\nfn main() -> i32 { id(i32, 1) + id(i32, 2) }",
        )
        .unwrap();
        let work = output.body_analysis_work;
        assert_eq!(work.specialized_body_exports_attempted, 1);
        assert_eq!(work.specialized_body_exports_succeeded, 1);
        assert_eq!(work.specialized_body_exports_rejected, 0);
        let [export] = output.specialized_body_exports.as_slice() else {
            panic!("one deduplicated specialization export expected");
        };
        assert_eq!(export.identity.base.issuer(), 0);
        assert_eq!(export.identity.type_arguments.len(), 1);
        assert!(export.identity.value_arguments.is_empty());
        assert_eq!(
            work.specialized_body_export_instructions_emitted,
            export.body.instructions.len()
        );
        assert_eq!(
            work.specialized_body_export_places_emitted,
            export.body.places.len()
        );
        assert_eq!(
            work.specialized_body_export_strings_emitted,
            export.body.strings.len()
        );
        assert!(export.body.instructions.iter().all(|instruction| !matches!(
            instruction.data,
            crate::SemanticBodyInstData::CallGeneric
        )));
    }

    #[test]
    fn output_type_pool_includes_composite_type_interned_by_specialization() {
        let output = compile_to_air(
            "fn zeros(comptime N: i32) -> [i32; N] { [0; N] }\n\
             fn main() -> i32 { let _values = zeros(3); 0 }",
        )
        .unwrap();

        // The generic declaration cannot intern `[i32; N]`: its length is
        // unknown until the reachable call specializes `N` to 3.
        assert_eq!(output.body_analysis_work.specialized_bodies_succeeded, 1);
        assert!(
            output.type_pool.stats().array_count >= 1,
            "the finalized pool must retain the array interned by specialization"
        );
        assert!(output.functions.iter().any(|function| {
            function.name != "main"
                && function
                    .air
                    .return_type()
                    .safe_name_with_frozen_pool(Some(&output.type_pool))
                    == "[i32; 3]"
        }));
    }

    #[test]
    fn output_type_pool_includes_late_anonymous_destructor_owner() {
        let output = compile_to_air(
            "fn Box(comptime T: type) -> type {\n\
                 struct { value: T, drop fn(self) {} }\n\
             }\n\
             fn main() { let B = Box(i32); let _value = B { value: 1 }; }",
        )
        .unwrap();

        let anonymous = output
            .type_pool
            .all_struct_ids()
            .into_iter()
            .find(|&id| {
                output
                    .type_pool
                    .struct_def(id)
                    .name
                    .starts_with("__anon_struct_")
            })
            .expect("body-time type discovery must survive in the finalized pool");
        let destructor_name = format!("{}.__drop", output.type_pool.struct_def(anonymous).name);
        assert!(
            output
                .functions
                .iter()
                .any(|function| function.name == destructor_name),
            "the anonymous destructor owner and its analyzed body must agree"
        );
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

    #[test]
    fn arithmetic_heavy_runtime_local_chain_uses_one_declaration_index() {
        // This mirrors the shape that exposed quadratic semantic work in the
        // arithmetic_heavy performance fixture: each runtime local depends on
        // the preceding local. None of these names is a declaration candidate,
        // so body analysis must not restart top-level constant discovery for
        // every failed comptime-type-alias probe.
        const LOCAL_COUNT: usize = 2_500;
        let mut source = String::from("fn chain(start: i32) -> i32 {\n");
        source.push_str("let v0 = start + 1;\n");
        for index in 1..LOCAL_COUNT {
            source.push_str(&format!("let v{index} = v{} + 1;\n", index - 1));
        }
        source.push_str(&format!("v{}\n}}\n", LOCAL_COUNT - 1));
        source.push_str("fn main() -> i32 { chain(0) }\n");

        let lexer = Lexer::new(&source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let bound = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        let binding_work = bound.binding_work();
        assert_eq!(binding_work.bind_invocations, 1);
        assert_eq!(binding_work.declaration_index_build_invocations, 1);
        assert_eq!(binding_work.indexed_const_candidates, 0);

        let output = bound.analyze_all_bodies_for_test().unwrap();
        assert_eq!(output.body_analysis_work.bodies_succeeded, 2);
        assert_eq!(output.body_analysis_work.bodies_failed, 0);
        assert_eq!(
            output.body_analysis_work.reachable_declaration_rir_visits,
            0
        );
    }

    #[test]
    fn bound_sema_contains_every_source_free_function_signature() {
        // The struct and `main` both use a type constructor declared later in
        // source order. Declaration binding may collect that signature early,
        // but the resulting BoundSema must contain the complete source
        // function namespace before any body is analyzed.
        let source = "struct Holder { value: Wrapper(i32) }\n\
                      fn helper(value: i32) -> i32 { value }\n\
                      fn main() -> i32 { helper(1) }\n\
                      fn Wrapper(comptime T: type) -> type { struct { value: T } }";
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let bound = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert_eq!(bound.binding_work().indexed_free_functions, 3);
        assert_eq!(bound.source_free_function_signature_count(), 3);
        assert!(bound.source_free_function_signatures_are_complete());

        // Body analysis exercises ordinary source-function lookup; the shared
        // boundary invariant rejects source-signature mutation on every body
        // path, including anonymous and specialized bodies.
        let output = bound.analyze_all_bodies_for_test().unwrap();
        assert_eq!(output.body_analysis_work.bodies_failed, 0);
    }

    #[test]
    fn body_generated_overlays_do_not_mutate_the_frozen_source_namespace() {
        let source = "fn Factory() -> type {
                struct {
                    value: i32,
                    fn get(self) -> i32 { self.value }
                }
            }
            fn main() -> i32 {
                let Item = Factory();
                let item = Item { value: 42 };
                item.get()
            }";
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let bound = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        let (output, before, after) = bound.analyze_all_bodies_with_namespace_probe();
        output.unwrap();

        assert_eq!(after.source_counts, before.source_counts);
        assert_eq!(after.source_fingerprint, before.source_fingerprint);
        assert!(after.generated_structs > before.generated_structs);
        assert!(after.anonymous_methods > before.anonymous_methods);
    }

    #[test]
    fn late_qualified_import_types_use_one_declaration_index() {
        // Put every import after the declaration that uses it. Binding must
        // resolve each dependency from the declaration index; qualified type
        // lookup must never rediscover an import by scanning the full RIR.
        const IMPORT_COUNT: usize = 64;
        let mut main = String::from("struct Holder {\n");
        for index in 0..IMPORT_COUNT {
            main.push_str(&format!("field{index}: dep{index}.Item,\n"));
        }
        main.push_str("}\nfn main() {}\n");
        for index in 0..IMPORT_COUNT {
            main.push_str(&format!(
                "const dep{index} = @import(\"dep{index}.rue\");\n"
            ));
        }

        let dependencies = (0..IMPORT_COUNT)
            .map(|_| "pub struct Item { value: i32 }")
            .collect::<Vec<_>>();
        let mut files = vec![(main.as_str(), FileId::DEFAULT)];
        files.extend(
            dependencies
                .iter()
                .enumerate()
                .map(|(index, source)| (*source, FileId::new((index + 1) as u32))),
        );
        let (rir, mut interner) = lower_files(&files);

        let mut paths = HashMap::from([(FileId::DEFAULT, "/main.rue".to_string())]);
        for index in 0..IMPORT_COUNT {
            paths.insert(FileId::new((index + 1) as u32), format!("/dep{index}.rue"));
        }
        let modules = (0..=IMPORT_COUNT)
            .map(|index| TestModule {
                id: if index == 0 {
                    "main.rue".to_owned()
                } else {
                    format!("dep{}.rue", index - 1)
                },
                file_id: FileId::new(index as u32),
                path: paths[&FileId::new(index as u32)].clone(),
            })
            .collect();
        let imports = rir
            .iter()
            .filter_map(|(_, inst)| {
                let rue_rir::InstData::Intrinsic { name, args } = &inst.data else {
                    return None;
                };
                let args = rir.intrinsic_args(args);
                if args.len() != 1 {
                    return None;
                }
                (interner.resolve(name) == "import").then(|| {
                    let argument = args.get(0).unwrap();
                    let rue_rir::InstData::StringConst {
                        content: specifier, ..
                    } = rir.get(argument).data
                    else {
                        unreachable!()
                    };
                    let specifier = interner.resolve(&specifier).to_owned();
                    TestSite {
                        importer: "main.rue".to_owned(),
                        offset: inst.span.start,
                        specifier: specifier.clone(),
                        target: specifier,
                    }
                })
            })
            .collect();
        let view = TestCanonicalImportView {
            modules,
            sites: imports,
        };
        let mut sema = Sema::new_synthetic(&rir, &mut interner, PreviewFeatures::new());
        sema.set_root_file_id(FileId::DEFAULT);
        sema.set_file_paths(paths);
        sema.set_canonical_imports(&view).unwrap();

        let bound = sema.bind_declarations().unwrap();
        let binding_work = bound.binding_work();
        assert_eq!(binding_work.declaration_index_build_invocations, 1);
        assert_eq!(binding_work.indexed_const_candidates, IMPORT_COUNT);

        let output = bound.analyze_all_bodies_for_test().unwrap();
        assert_eq!(output.body_analysis_work.bodies_failed, 0);
        assert_eq!(
            output.body_analysis_work.reachable_declaration_rir_visits,
            0
        );
    }

    #[test]
    fn runtime_parameter_prevents_false_local_type_alias_precomputation() {
        // The file-level type-valued constant has the same name as the runtime
        // parameter. The parameter wins lexically, so `local` is an ordinary
        // runtime value and cannot subsequently be used as a type alias.
        let errors = compile_to_air(
            "const value = i32;\n\
             fn use(value: i32) -> i32 {\n\
                 let local = value;\n\
                 let result: local = 1;\n\
                 result\n\
             }\n\
             fn main() -> i32 { use(0) }",
        )
        .unwrap_err();
        assert!(
            errors.iter().any(
                |error| matches!(&error.kind, ErrorKind::UnknownType(name) if name == "local")
            )
        );
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
        // (text messages, never-typed operands).
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
        for (name, source, _stable_strings, intrinsic, expected, found, offending) in [
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
                "struct Fake { value: i32 } fn main() -> i32 { let s = Fake { value: 1 }; @assert(s); 0 }",
                false,
                "assert",
                "bool condition",
                "Fake",
                "s",
            ),
            (
                "scalar panic message",
                "fn main() -> i32 { @panic(1); 0 }",
                false,
                "panic",
                "text message",
                "i32",
                "1",
            ),
            (
                "scalar assertion message",
                "fn main() -> i32 { @assert(false, 7); 0 }",
                false,
                "assert",
                "text message",
                "i32",
                "7",
            ),
            (
                "array message",
                "fn main() -> i32 { let a = [1, 2, 3]; @panic(a); 0 }",
                false,
                "panic",
                "text message",
                "[i32; 3]",
                "a",
            ),
            (
                "enum message",
                "enum Mode { A } fn main() -> i32 { @assert(false, Mode.A); 0 }",
                false,
                "assert",
                "text message",
                "Mode",
                "Mode.A",
            ),
            (
                "three-slot struct impostor",
                "struct Fake { a: u64, b: u64, c: u64 } fn main() -> i32 { @panic(Fake { a: 0, b: 0, c: 0 }); 0 }",
                false,
                "panic",
                "text message",
                "Fake",
                "Fake { a: 0, b: 0, c: 0 }",
            ),
        ] {
            let preview_features = PreviewFeatures::new();
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
        let preview = PreviewFeatures::new();
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
        let preview = PreviewFeatures::new();
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
                AirInstData::Call { ref args, .. } => {
                    Some(main.air.get_call_args(args).map(|arg| arg.mode).collect())
                }
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
            .find(|function| {
                function.name.starts_with("Probe$") && function.name.ends_with(".method")
            })
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
            .find(|function| {
                function.name.starts_with("Probe$") && function.name.ends_with("::assoc")
            })
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
        let preview = PreviewFeatures::new();
        compile_to_air_with_preview_features(source, preview).unwrap();
    }

    #[test]
    fn string_literal_default_is_stable_str() {
        let output = compile_to_air_with_preview_features(
            r#"
                fn main() -> i32 {
                    let value = "hello";
                    let first = value;
                    let second = value;
                    @intCast(first.len() + second.len())
                }
            "#,
            PreviewFeatures::new(),
        )
        .expect("the stable str default must be Copy and reusable");
        let literal = output
            .functions
            .iter()
            .flat_map(|function| function.air.iter())
            .find_map(|(_, inst)| matches!(inst.data, AirInstData::StringConst(_)).then_some(inst))
            .expect("main must materialize its literal");
        assert_eq!(
            literal
                .ty
                .safe_name_with_frozen_pool(Some(&output.type_pool)),
            "str"
        );
        assert_eq!(output.type_pool.abi_slot_count(literal.ty), 2);
    }

    #[test]
    fn string_default_survives_control_flow_and_aggregate_joins() {
        let preview = PreviewFeatures::new();
        let output = compile_to_air_with_preview_features(
            r#"
                struct Holder { value: str }

                fn main() -> i32 {
                    let branch = if true { "a" } else { "bb" };
                    let branch_first = branch;
                    let branch_second = branch;

                    let block = { let marker = 0; "ccc" };
                    let block_first = block;
                    let block_second = block;

                    let matched = match true {
                        true => "dddd",
                        false => "eeeee",
                    };
                    let match_first = matched;
                    let match_second = matched;

                    let holder = Holder {
                        value: if false { "ffffff" } else { "ggggggg" },
                    };
                    let field_first = holder.value;
                    let field_second = holder.value;

                    @intCast(
                        branch_first.len() + branch_second.len()
                            + block_first.len() + block_second.len()
                            + match_first.len() + match_second.len()
                            + field_first.len() + field_second.len()
                    )
                }
            "#,
            preview,
        )
        .expect("literal-derived joins must remain Copy first-class str values");

        let literal_types: Vec<Type> = output
            .functions
            .iter()
            .flat_map(|function| function.air.iter())
            .filter_map(|(_, inst)| {
                matches!(inst.data, AirInstData::StringConst(_)).then_some(inst.ty)
            })
            .collect();
        assert_eq!(literal_types.len(), 7);
        assert!(literal_types.iter().all(|&ty| {
            ty.safe_name_with_frozen_pool(Some(&output.type_pool)) == "str"
                && output.type_pool.abi_slot_count(ty) == 2
                && ty.is_copy_in_frozen_pool(&output.type_pool)
        }));
    }

    #[test]
    fn string_literal_join_cannot_default_through_an_integer_literal() {
        let preview = PreviewFeatures::new();
        let errors = compile_to_air_with_preview_features(
            r#"
                fn choose(cond: bool) {
                    let mixed = if cond { 42 } else { "not an integer" };
                }
                fn main() -> i32 { choose(true); 0 }
            "#,
            preview,
        )
        .expect_err("integer and string literal branches must not share a default");
        assert!(matches!(
            &errors.iter().next().unwrap().kind,
            ErrorKind::TypeMismatch { expected, found }
                if expected == "string type" && found == "{integer}"
        ));
    }

    #[test]
    fn fixed_string_call_context_stops_at_nested_call_operands() {
        let preview = PreviewFeatures::new();
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
                    {main}
                "#
            );
            compile_to_air_with_preview_features(&source, preview.clone())
                .unwrap_or_else(|errors| panic!("{case} must compile independently: {errors}"));
        }
    }

    #[test]
    fn expected_string_type_reaches_only_structural_result_positions() {
        let source = r#"
            fn choose(flag: bool) -> Str(8) {
                if "left" == "right" {
                    if flag { "yes" } else { "no" }
                } else {
                    { let marker = 0; "fallback" }
                }
            }
            fn loop_probe() -> Str(8) {
                while false { "loop body"; }
                "tail"
            }
            fn main() -> i32 {
                @intCast(choose(true).len() + loop_probe().len())
            }
        "#;
        let preview = PreviewFeatures::new();
        let output = compile_to_air_with_preview_features(source, preview).unwrap();

        let literal_types: Vec<String> = output
            .functions
            .iter()
            .flat_map(|function| function.air.iter())
            .filter(|(_, inst)| matches!(inst.data, AirInstData::StringConst(_)))
            .map(|(_, inst)| inst.ty.safe_name_with_frozen_pool(Some(&output.type_pool)))
            .collect();

        // Comparison operands and a discarded loop-body value have no buffer
        // context, so they use the first-class `str`
        // default. If/match-style branches and block tails remain transparent
        // and materialize as the declared Str(8).
        assert_eq!(literal_types.iter().filter(|ty| *ty == "str").count(), 3);
        assert_eq!(literal_types.iter().filter(|ty| *ty == "Str(8)").count(), 4);
    }

    #[test]
    fn declared_enum_payload_and_ptr_write_pointee_contextualize_fixed_strings() {
        let source = r#"
            enum Message { Text(Str(8)) }
            fn make() -> Message { Message.Text("hello") }
            fn main() -> i32 {
                let mut value: Str(8) = "old";
                checked {
                    let p: ptr mut Str(8) = @raw_mut(value);
                    @ptr_write(p, "new");
                };
                @intCast(value.len())
            }
        "#;
        let preview = PreviewFeatures::new();
        compile_to_air_with_preview_features(source, preview).unwrap();
    }

    #[test]
    fn fallible_intrinsic_rejects_local_option_context() {
        // RUE-1112: a fallible intrinsic's result IS the exact trusted std
        // `Option`; a local same-shape `Option` lookalike used as its
        // annotation is not accepted. This direct AIR harness intentionally has
        // no registry install, so the intrinsic fails closed with an internal
        // incompleteness diagnostic before context can adopt the lookalike. (The
        // trusted-std positive path is covered by compiler-crate acceptance tests.)
        let source = r#"
            fn Option(comptime T: type) -> type { enum { Some(T), None } }

            fn main() -> i32 {
                let Opt = Option(i64);
                let parsed: Opt = @parse_i64("42");
                match parsed {
                    Opt.Some(value) => @intCast(value),
                    Opt.None => 0,
                }
            }
        "#;
        let errors =
            compile_to_air(source).expect_err("a local-Option annotation is no longer accepted");
        assert!(
            errors.to_string().contains("parse_i64"),
            "expected fail-closed missing-registry diagnostics on @parse_i64: {errors}",
        );
    }

    #[test]
    fn fallible_intrinsic_resolver_has_no_registry_miss_shape_fallback() {
        let source = include_str!("analysis/intrinsics.rs");
        for forbidden in ["trusted_try_producer", "find_compatible_anon_enum"] {
            assert!(
                !source.contains(forbidden),
                "fallible intrinsic resolution regained a registry-miss fallback: {forbidden}"
            );
        }
        assert!(source.contains("reached body analysis without"));
        assert!(source.contains("ErrorKind::InternalError"));
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
        let preview = PreviewFeatures::new();
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
                .safe_name_with_frozen_pool(Some(&output.type_pool)),
            "Str(8)"
        );

        // Never coercion remains legal. The outer Str(8) expectation belongs
        // to the assignment result and must not leak into @panic's text
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

        // The same expected-type boundary applies to @assert: its message is
        // text, and the assignment is rejected for the unit result rather
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
            "fn shadow() -> bool {
                let x = 10;
                let x = true;  // Shadow x with a different type
                x
            }
            fn main() -> i32 { if shadow() { 0 } else { 1 } }",
        )
        .unwrap();

        let shadow = output
            .functions
            .iter()
            .find(|function| function.name == "shadow")
            .unwrap();
        assert_eq!(shadow.num_locals, 2);
        assert_eq!(shadow.air.return_type(), Type::BOOL);
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
                .map(|id| output.type_pool.struct_def(id))
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
    // These tests verify the stable core text type.

    #[test]
    fn test_strbuf_is_not_injected() {
        let output = compile_to_air(
            "fn main() -> i32 {
                let s = \"hello\";
                0
            }",
        )
        .unwrap();

        assert!(
            !output
                .type_pool
                .all_struct_ids()
                .map(|id| output.type_pool.struct_def(id))
                .any(|s| s.name == "StrBuf")
        );
    }

    #[test]
    fn test_string_len_method() {
        // String.len() should return u64
        let output = compile_to_air(
            "fn length() -> u64 {
                let s = \"hello\";
                s.len()
            }
            fn main() -> i32 { if length() == 5 { 0 } else { 1 } }",
        )
        .unwrap();

        assert_eq!(
            output
                .functions
                .iter()
                .find(|function| function.name == "length")
                .unwrap()
                .air
                .return_type(),
            Type::U64
        );
    }

    #[test]
    fn test_string_is_empty_method() {
        // String.is_empty() should return bool
        let output = compile_to_air(
            "fn empty() -> bool {
                let s = \"hello\";
                s.len() == 0
            }
            fn main() -> i32 { if empty() { 0 } else { 1 } }",
        )
        .unwrap();

        assert_eq!(
            output
                .functions
                .iter()
                .find(|function| function.name == "empty")
                .unwrap()
                .air
                .return_type(),
            Type::BOOL
        );
    }

    #[test]
    fn test_string_literal_type_inference() {
        // String literal should have type String
        let output = compile_to_air(
            "fn has_content() -> bool {
                let s = \"hello\";
                let t = \"world\";
                s.len() != 0
            }
            fn main() -> i32 { if has_content() { 0 } else { 1 } }",
        )
        .unwrap();

        // Should have local storage for two string variables
        assert!(
            output
                .functions
                .iter()
                .find(|function| function.name == "has_content")
                .unwrap()
                .num_locals
                >= 2
        );
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
            "fn value() -> i64 {
                let x: i64 = 42;
                x
            }
            fn main() -> i32 { if value() == 42 { 0 } else { 1 } }",
        )
        .unwrap();

        assert_eq!(
            output
                .functions
                .iter()
                .find(|function| function.name == "value")
                .unwrap()
                .air
                .return_type(),
            Type::I64
        );
    }

    #[test]
    fn test_integer_literal_infers_from_return_type() {
        // Integer literal should infer type from function return type
        let output = compile_to_air(
            "fn value() -> u8 { 42 } fn main() -> i32 { if value() == 42 { 0 } else { 1 } }",
        )
        .unwrap();

        assert_eq!(
            output
                .functions
                .iter()
                .find(|function| function.name == "value")
                .unwrap()
                .air
                .return_type(),
            Type::U8
        );
    }

    #[test]
    fn test_integer_literal_infers_from_binary_op() {
        // Integer literal should infer type from binary operation context
        let output = compile_to_air(
            "fn value() -> i64 {
                let x: i64 = 10;
                x + 5  // 5 should infer to i64
            }
            fn main() -> i32 { if value() == 15 { 0 } else { 1 } }",
        )
        .unwrap();

        assert_eq!(
            output
                .functions
                .iter()
                .find(|function| function.name == "value")
                .unwrap()
                .air
                .return_type(),
            Type::I64
        );
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
    // Canonical type pool tests (ADR-0024)
    //
    // These tests verify that the TypeInternPool is correctly populated during
    // declaration collection and that its typed indexes remain internally
    // consistent.
    // ========================================================================

    /// Helper to gather declarations and return the Sema state for testing.
    fn gather_declarations_for_testing(source: &str) -> Sema<'static> {
        // We need to leak the interner for the static lifetime
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();

        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        // Leak both to get 'static lifetime for testing
        let rir = Box::leak(Box::new(rir));
        let interner = Box::leak(Box::new(interner));

        let mut sema = Sema::new_synthetic(rir, interner, PreviewFeatures::new());
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
        let struct_id = *sema
            .structs_by_file_name
            .get(&(FileId::DEFAULT, probe_name))
            .unwrap();
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
        let facts = crate::sema::HostInferenceFacts::new(&inference, &sema);
        let signature =
            crate::inference::LazyInferenceFacts::method_sig(&facts, (struct_id, method_name))
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
    fn qualified_file_zero_lookup_excludes_generated_type_overlays() {
        // Anonymous structs use names in this form. A genuine file-0 source
        // declaration with the same interned name must remain authoritative
        // for qualified lookup even when the body overlay has that name too.
        let mut sema = gather_declarations_for_testing(
            "struct __anon_struct_0 { source: i32 }
             struct Overlay { generated: i32 }
             fn main() -> i32 { 0 }",
        );
        let name = sema.interner.get("__anon_struct_0").unwrap();
        let source_id = *sema
            .structs_by_file_name
            .get(&(FileId::new(0), name))
            .unwrap();
        let overlay_name = sema.interner.get("Overlay").unwrap();
        let generated_id = *sema
            .structs_by_file_name
            .get(&(FileId::new(0), overlay_name))
            .unwrap();
        assert_ne!(source_id, generated_id);
        sema.generated_structs.insert(name, generated_id);

        let inference = sema.build_inference_context();
        let facts = crate::sema::HostInferenceFacts::new(&inference, &sema);
        assert_eq!(
            crate::inference::LazyInferenceFacts::struct_type_by_file(
                &facts,
                (FileId::new(0), name)
            ),
            Some(Type::new_struct(source_id))
        );
    }

    fn callable_collision_index_fixture() -> (Sema<'static>, StructId, lasso::Spur) {
        let sema = gather_declarations_for_testing(
            "struct __anon_struct_5 { fn f(borrow self) -> i32 { 10 } }
             fn main() -> i32 { 0 }",
        );
        let owner_name = sema.interner.get("__anon_struct_5").unwrap();
        let owner = *sema
            .structs_by_file_name
            .get(&(FileId::DEFAULT, owner_name))
            .unwrap();
        let method = sema.interner.get("f").unwrap();
        (sema, owner, method)
    }

    #[test]
    fn callable_symbol_collision_preserves_named_before_anonymous_precedence() {
        let (mut sema, named_owner, method) = callable_collision_index_fixture();
        let named = (named_owner, method);
        let anonymous = (StructId(u32::MAX - 1), method);
        let symbol = "__anon_struct_5.f".to_string();
        Sema::<crate::sema::MutableDeclarations>::insert_callable_method_candidate(
            Arc::make_mut(&mut sema.named_callable_methods_by_symbol),
            symbol.clone(),
            named,
        );
        Sema::<crate::sema::MutableDeclarations>::insert_callable_method_candidate(
            &mut sema.anonymous_callable_methods_by_symbol,
            symbol.clone(),
            anonymous,
        );

        assert_eq!(
            sema.callable_method_key_by_symbol(&symbol),
            Some((false, named))
        );
    }

    #[test]
    fn callable_symbol_same_tier_collision_fails_closed() {
        let (mut sema, _, method) = callable_collision_index_fixture();
        let symbol = "__anon_struct_5.f".to_string();
        for owner in [StructId(u32::MAX - 1), StructId(u32::MAX)] {
            Sema::<crate::sema::MutableDeclarations>::insert_callable_method_candidate(
                &mut sema.anonymous_callable_methods_by_symbol,
                symbol.clone(),
                (owner, method),
            );
        }

        assert_eq!(sema.callable_method_key_by_symbol(&symbol), None);
    }

    #[test]
    fn final_anonymous_method_nested_destructor_remains_rooted() {
        let output = compile_to_air_with_authoritative_identity_order(
            r#"
fn good_drop() -> i32 { 0 }
fn Good() -> type {
    struct { x: i32, drop fn(self) { good_drop(); } }
}
fn A() -> type {
    struct {
        x: i32,
        fn get(self) -> i32 {
            let T = Good();
            let value: T = T { x: 0 };
            10
        }
    }
}
fn B() -> type {
    struct { x: i32, fn get(self) -> i32 { 1 } }
}
fn run(comptime n: i32) -> i32 {
    let T = A();
    let value: T = T { x: n };
    value.get()
}
fn main() -> i32 {
    let T = B();
    let value: T = T { x: 0 };
    value.get() + run(0)
}
"#,
        )
        .unwrap();

        assert!(
            output
                .functions
                .iter()
                .any(|function| function.name.ends_with(".__drop"))
        );
        assert!(
            output
                .functions
                .iter()
                .any(|function| function.name == "good_drop")
        );
        assert!(output.aggregate_type_identities_by_type.keys().any(|ty| {
            ty.as_struct().is_some_and(|id| {
                let def = output.type_pool.struct_def(id);
                def.name.starts_with("__anon_struct_") && def.destructor.is_some()
            })
        }));
    }

    #[test]
    fn independently_reachable_abandoned_nested_type_keeps_its_destructor() {
        let output = compile_to_air_with_authoritative_identity_order(
            r#"
fn bad_drop() -> i32 { 0 }
fn A() -> type {
    struct { x: i32, fn get(self) -> i32 { 10 } }
}
fn Bad() -> type {
    struct { x: i32, drop fn(self) { bad_drop(); } }
}
fn B() -> type {
    struct {
        x: i32,
        fn get(self) -> i32 {
            let T = Bad();
            let value: T = T { x: 0 };
            1
        }
    }
}
fn run(comptime n: i32) -> i32 {
    let T = A();
    let value: T = T { x: n };
    value.get()
}
fn main() -> i32 {
    let BadT = Bad();
    let bad: BadT = BadT { x: 0 };
    let T = B();
    let value: T = T { x: 0 };
    value.get() + run(0)
}
"#,
        )
        .unwrap();

        assert!(
            output
                .functions
                .iter()
                .any(|function| function.name.ends_with(".__drop"))
        );
        assert!(
            output
                .functions
                .iter()
                .any(|function| function.name == "bad_drop")
        );
        assert!(output.aggregate_type_identities_by_type.keys().any(|ty| {
            ty.as_struct().is_some_and(|id| {
                let def = output.type_pool.struct_def(id);
                def.name.starts_with("__anon_struct_") && def.destructor.is_some()
            })
        }));
    }

    #[test]
    fn materialized_anonymous_body_identity_round_trips_and_missing_key_discards_atomically() {
        let source = r#"
fn Box() -> type {
    struct {
        value: i32,
        fn get(self) -> i32 { self.value }
    }
}
fn consume(value: Box()) -> i32 { value.get() }
fn main() -> i32 {
    let value: Box() = Box() { value: 42 };
    consume(value)
}
"#;
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();

        let shells = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new())
            .predeclare_declaration_shells_for_test()
            .unwrap();
        let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
        let (definitions, owners) = authoritative_test_endpoints(&shell_records);
        let cold = shells
            .install_stable_identity_endpoints(&definitions, &[])
            .unwrap()
            .resolve_declarations()
            .unwrap()
            .install_body_owner_tokens(&owners)
            .unwrap()
            .analyze_all_bodies_for_test()
            .unwrap();
        let wanted = ["consume", "main"];
        let owner_tokens = owners
            .iter()
            .filter(|endpoint| wanted.contains(&endpoint.name.as_str()))
            .map(|endpoint| endpoint.token)
            .collect::<std::collections::HashSet<_>>();
        let candidates = cold
            .ordinary_body_exports
            .iter()
            .filter(|export| owner_tokens.contains(&export.owner))
            .map(|export| crate::SemanticBodyCandidate {
                owner: export.owner,
                body_span: rue_span::Span::with_file(
                    rue_span::FileId::DEFAULT,
                    0,
                    source.len() as u32,
                ),
                body: export.body.clone(),
            })
            .collect::<Vec<_>>();
        assert_eq!(candidates.len(), 2);

        let reused = authoritative_bound(&rir, &interner, &definitions, &owners)
            .install_ordinary_body_candidates(
                candidates,
                |token: &crate::SemanticDefinitionToken| {
                    Ok::<_, crate::SemanticStableResolutionFailure>(*token)
                },
                |token: &crate::SemanticModuleToken| {
                    Ok::<_, crate::SemanticStableResolutionFailure>(*token)
                },
            )
            .analyze_all_bodies_for_test()
            .unwrap();
        assert_eq!(reused.body_analysis_work.ordinary_body_import_attempts, 2);
        assert_eq!(reused.body_analysis_work.ordinary_body_import_successes, 2);
        assert_eq!(reused.body_analysis_work.ordinary_body_import_failures, 0);
        assert_eq!(reused.body_analysis_work.ordinary_body_analyses_skipped, 2);

        let main_token = owners
            .iter()
            .find(|endpoint| endpoint.name == "main")
            .unwrap()
            .token;
        let mut missing = cold
            .ordinary_body_exports
            .iter()
            .find(|export| export.owner == main_token)
            .unwrap()
            .body
            .clone();
        let mut instructions = missing.instructions.to_vec();
        let identity = instructions
            .iter_mut()
            .find_map(|instruction| match &mut instruction.ty {
                crate::SemanticImportType::AnonymousNominal(identity) => Some(identity),
                _ => None,
            })
            .expect("main body contains an anonymous nominal type");
        identity.kind = match identity.kind {
            crate::AnonymousNominalKind::Struct => crate::AnonymousNominalKind::Enum,
            crate::AnonymousNominalKind::Enum => crate::AnonymousNominalKind::Struct,
        };
        missing.instructions = instructions.into();
        let rejected = authoritative_bound(&rir, &interner, &definitions, &owners)
            .install_ordinary_body_candidates(
                vec![crate::SemanticBodyCandidate {
                    owner: main_token,
                    body_span: rue_span::Span::with_file(
                        rue_span::FileId::DEFAULT,
                        0,
                        source.len() as u32,
                    ),
                    body: missing,
                }],
                |token: &crate::SemanticDefinitionToken| {
                    Ok::<_, crate::SemanticStableResolutionFailure>(*token)
                },
                |token: &crate::SemanticModuleToken| {
                    Ok::<_, crate::SemanticStableResolutionFailure>(*token)
                },
            )
            .analyze_all_bodies_for_test()
            .unwrap();
        assert_eq!(rejected.body_analysis_work.ordinary_body_import_attempts, 1);
        assert_eq!(
            rejected.body_analysis_work.ordinary_body_import_successes,
            0
        );
        assert_eq!(rejected.body_analysis_work.ordinary_body_import_failures, 1);
        assert_eq!(
            rejected
                .body_analysis_work
                .last_ordinary_body_import_failure,
            Some(crate::SemanticBodyImportFailureKind::StructuralValidation)
        );
        assert_eq!(
            rejected
                .body_analysis_work
                .ordinary_body_import_atomic_discards,
            1
        );
        assert!(
            rejected
                .functions
                .iter()
                .any(|function| function.name == "main")
        );
    }

    #[test]
    fn direct_anonymous_type_alias_and_const_receive_authoritative_producers() {
        let local = compile_to_air(
            r#"
fn main() -> i32 {
    let T = struct { value: i32 };
    let value: T = T { value: 42 };
    value.value
}
"#,
        )
        .unwrap();
        assert_eq!(local.anonymous_nominal_identities_by_type.len(), 1);

        let constant = compile_to_air(
            r#"
const T: type = struct { value: i32 };
fn main() -> i32 {
    let value: T = T { value: 42 };
    value.value
}
"#,
        )
        .unwrap();
        assert_eq!(constant.anonymous_nominal_identities_by_type.len(), 1);
    }

    #[test]
    fn mixed_comptime_arguments_preserve_each_declaration_order_stream() {
        let output = compile_to_air(
            r#"
fn Mixed(
    comptime Z: type,
    comptime n: i32,
    comptime A: type,
    comptime flag: bool,
) -> type {
    struct { first: Z, second: A, values: [i32; n] }
}
fn main() -> i32 {
    let T = Mixed(i64, 2, i32, true);
    let value: T = T { first: 1, second: 2, values: [19, 21] };
    value.values[0] + value.values[1] + value.second
}
"#,
        )
        .unwrap();
        let identity = output
            .anonymous_nominal_identities_by_type
            .values()
            .next()
            .unwrap();
        assert_eq!(
            identity.arguments.types.as_ref(),
            &[crate::TypeInstanceKey::I64, crate::TypeInstanceKey::I32]
        );
        assert_eq!(
            identity.arguments.values.as_ref(),
            &[
                crate::CanonicalArgumentValue::Integer(2),
                crate::CanonicalArgumentValue::Bool(true),
            ]
        );
    }

    #[test]
    fn test_type_pool_has_no_implicit_strbuf() {
        let sema = gather_declarations_for_testing("fn main() -> i32 { 0 }");
        assert!(sema.interner.get("StrBuf").is_none());
        assert!(
            sema.type_pool
                .all_struct_ids()
                .iter()
                .all(|id| sema.type_pool.struct_def(*id).name != "StrBuf")
        );
    }

    #[test]
    fn test_type_pool_populated_with_user_struct() {
        let sema = gather_declarations_for_testing(
            "struct Point { x: i32, y: i32 }
             fn main() -> i32 { 0 }",
        );

        let point_name = sema.interner.get("Point").unwrap();

        // Check the pool has the struct
        let pool_point = sema
            .type_pool
            .get_struct_by_file_name(FileId::DEFAULT, point_name);
        assert!(pool_point.is_some(), "Point should be in the type pool");

        // Verify struct lookup has it
        let registry_point = sema
            .structs_by_file_name
            .get(&(FileId::DEFAULT, point_name));
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
        let pool_color = sema
            .type_pool
            .get_enum_by_file_name(FileId::DEFAULT, color_name);
        assert!(pool_color.is_some(), "Color should be in the type pool");

        // Verify pool and registry agree - enum_id is now pool-based
        let registry_color = sema.enums_by_file_name.get(&(FileId::DEFAULT, color_name));
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
        let pool_data = sema
            .type_pool
            .get_struct_by_file_name(FileId::DEFAULT, data_name)
            .unwrap();
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

        // Only source structs are registered; StrBuf comes from an explicit
        // std import and is absent in this source-only unit fixture.
        assert_eq!(stats.struct_count, 2);
        // 4 enums: Arch (builtin) + Os (builtin) + DataModel (builtin) + E
        assert_eq!(stats.enum_count, 4);
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
        for ((file_id, name_spur), &struct_id) in &sema.structs_by_file_name {
            // Use type_pool.struct_def() which takes pool-based struct_id
            let pool_def = sema.type_pool.struct_def(struct_id);

            // Also verify the pool can look up by name
            let pool_type = sema.type_pool.get_struct_by_file_name(*file_id, *name_spur);
            assert!(
                pool_type.is_some(),
                "Struct '{}' should be in pool by name",
                pool_def.name
            );
        }

        // Verify all enums in registry are in pool
        for ((file_id, name_spur), &enum_id) in &sema.enums_by_file_name {
            // Use type_pool.enum_def() which takes pool-based enum_id
            let pool_def = sema.type_pool.enum_def(enum_id);

            // Also verify the pool can look up by name
            let pool_type = sema.type_pool.get_enum_by_file_name(*file_id, *name_spur);
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

    #[test]
    fn shared_type_syntax_observes_each_nominal_dependency_once_at_every_depth() {
        for (syntax, expected_total_work) in [
            ("Leaf", 1),
            ("[Leaf; 2]", 1),
            ("Make()", 1),
            ("Id(Leaf)", 1),
            ("Id(Id(Leaf))", 1),
            ("ArrayOf(Leaf)", 1),
            ("[Leaf; Width(Leaf)]", 1),
        ] {
            let source = format!(
                "struct Leaf {{ value: i32 }}
                 fn Id(comptime T: type) -> type {{ T }}
                 fn ArrayOf(comptime T: type) -> type {{ [T; 2] }}
                 fn Width(comptime T: type) -> i32 {{ 2 }}
                 fn Make() -> type {{ Leaf }}
                 fn main() -> i32 {{
                     let size: u64 = @intCast(@size_of({syntax}));
                     @intCast(size)
                 }}"
            );
            let output = compile_to_air(&source)
                .unwrap_or_else(|errors| panic!("type syntax '{syntax}' must resolve: {errors:?}"));
            let dependencies = output
                .declaration_type_dependencies
                .iter()
                .filter(|event| event.source_name == "main")
                .collect::<Vec<_>>();

            assert_eq!(
                dependencies.len(),
                1,
                "'{syntax}' must emit one exact nominal edge"
            );
            assert_eq!(dependencies[0].target_name, "Leaf");
            assert_eq!(
                dependencies[0].target_kind,
                crate::DeclarationTypeDependencyTargetKind::Struct
            );
            assert_eq!(
                output.body_analysis_work.declaration_type_dependency_events, expected_total_work,
                "'{syntax}' must increment work exactly once per declaration body"
            );
        }
    }

    #[test]
    fn selected_alias_observes_the_alias_and_its_nominal_result_once_each() {
        let output = compile_to_air(
            "struct Leaf { value: i32 }
             const Alias = Leaf;
             fn main() -> i32 {
                 let size: u64 = @intCast(@size_of(Alias));
                 @intCast(size)
             }",
        )
        .unwrap();
        let dependencies = output
            .declaration_type_dependencies
            .iter()
            .filter(|event| event.source_name == "main")
            .map(|event| (event.target_name.as_str(), event.target_kind))
            .collect::<Vec<_>>();

        assert_eq!(
            dependencies,
            [
                (
                    "Alias",
                    crate::DeclarationTypeDependencyTargetKind::ValueConst
                ),
                ("Leaf", crate::DeclarationTypeDependencyTargetKind::Struct),
            ]
        );
        assert_eq!(
            output.body_analysis_work.declaration_type_dependency_events,
            3
        );
    }

    #[test]
    fn substituted_signature_wrappers_emit_one_stable_edge_per_body() {
        for (position, source) in [
            (
                "parameter",
                "@copy struct Leaf { value: i32 }
                 fn consume(comptime T: type, values: [T; 2]) -> i32 { 0 }
                 fn main() -> i32 {
                     consume(Leaf, [Leaf { value: 1 }, Leaf { value: 2 }])
                 }",
            ),
            (
                "return",
                "@copy struct Leaf { value: i32 }
                 fn pair(comptime T: type, value: T) -> [T; 2] { [value, value] }
                 fn main() -> i32 {
                     let values = pair(Leaf, Leaf { value: 1 });
                     values[0].value
                 }",
            ),
        ] {
            let output = compile_to_air(source).unwrap_or_else(|errors| {
                panic!("substituted {position} type must resolve: {errors:?}")
            });
            let dependencies = output
                .declaration_type_dependencies
                .iter()
                .filter(|event| event.source_name == "main")
                .collect::<Vec<_>>();

            assert_eq!(
                dependencies.len(),
                1,
                "substituted {position} type must emit one exact nominal edge"
            );
            assert_eq!(dependencies[0].target_name, "Leaf");
            assert_eq!(
                dependencies[0].target_kind,
                crate::DeclarationTypeDependencyTargetKind::Struct
            );
            assert_eq!(
                output.body_analysis_work.declaration_type_dependency_events, 2,
                "substituted {position} type must increment work once in the caller and once in the specialized body"
            );
        }
    }

    #[test]
    fn slice_preview_failure_precedes_unknown_element_resolution() {
        let errors = compile_to_air(
            "fn take(value: [Unknown]) -> i32 { 0 }
             fn main() -> i32 { 0 }",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(
                &errors.iter().next().unwrap().kind,
                ErrorKind::PreviewFeatureRequired {
                    feature: PreviewFeature::Slices,
                    ..
                }
            ),
            "unexpected diagnostics: {errors:?}"
        );
    }

    #[test]
    fn deferred_array_lengths_reject_qualified_value_heads_but_allow_qualified_type_arguments() {
        let span = Span::with_file(FileId::DEFAULT, 0, 0);
        let mut sema = gather_two_file_declarations_for_testing(
            "const dep = @import(\"dep.rue\"); fn main() -> i32 { 0 }",
            "pub fn Width(comptime T: type) -> i32 { 2 }",
        );
        let type_param = sema.interner.get_or_intern("T");
        let error = sema
            .validate_deferred_type_position_for_testing("[T; dep.Width(T)]", &[type_param], span)
            .unwrap_err();
        assert!(matches!(
            &error.kind,
            ErrorKind::UnknownType(name) if name == "dep.Width(...)"
        ));

        let mut sema = gather_two_file_declarations_for_testing(
            "const dep = @import(\"dep.rue\");
             fn Width(comptime T: type) -> i32 { 2 }
             fn main() -> i32 { 0 }",
            "pub struct Item { value: i32 }",
        );
        let type_param = sema.interner.get_or_intern("T");
        assert_eq!(
            sema.validate_deferred_type_position_for_testing(
                "[T; Width(dep.Item)]",
                &[type_param],
                span,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn array_length_nested_type_argument_preserves_unknown_type_error() {
        let errors = compile_to_air(
            "fn Width(comptime T: type) -> i32 { 2 }
             fn main() -> i32 {
                 let values: [i32; Width(Unknown)] = [1, 2];
                 values[0]
             }",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors.iter().next().unwrap().kind,
            ErrorKind::UnknownType(name) if name == "Unknown"
        ));
    }

    #[test]
    fn array_length_nested_type_argument_preserves_provider_diagnostic() {
        let mut sema = gather_declarations_for_testing(
            "fn Width(comptime T: type) -> i32 { 2 }
             fn main() -> i32 { 0 }",
        );
        let error = sema
            .resolve_type_syntax_for_testing(
                "[i32; Width([i32])]",
                Span::with_file(FileId::DEFAULT, 0, 0),
            )
            .unwrap_err();
        assert!(matches!(
            &error.kind,
            ErrorKind::PreviewFeatureRequired {
                feature: PreviewFeature::Slices,
                ..
            }
        ));
    }

    #[test]
    fn array_length_reducer_failure_preserves_provider_diagnostic() {
        let mut sema = gather_declarations_for_testing(
            "fn Width(comptime T: type) -> i32 { Width(T) }
             fn main() -> i32 { 0 }",
        );
        let error = sema
            .resolve_type_syntax_for_testing(
                "[i32; Width(i32)]",
                Span::with_file(FileId::DEFAULT, 0, 0),
            )
            .unwrap_err();
        assert!(
            matches!(
                &error.kind,
            ErrorKind::ComptimeEvaluationFailed { reason }
                if reason.contains("maximum nesting depth")
            ),
            "unexpected diagnostic: {error:?}"
        );
    }

    #[test]
    fn deferred_value_calls_preserve_compile_time_function_diagnostic_wording() {
        for (source, expected_reason) in [
            (
                "fn Width(comptime n: i32) -> i32 { n }
                 fn use(comptime T: type, value: [T; Width()]) -> i32 { 0 }
                 fn main() -> i32 { 0 }",
                "compile-time function 'Width' expects 1 comptime argument(s), but 0 were provided",
            ),
            (
                "fn RuntimeWidth(n: i32) -> i32 { n }
                 fn use(comptime T: type, value: [T; RuntimeWidth(2)]) -> i32 { 0 }
                 fn main() -> i32 { 0 }",
                "call 'RuntimeWidth' is not a compile-time value; all of its parameters must be comptime",
            ),
        ] {
            let errors = compile_to_air(source).unwrap_err();
            assert_eq!(errors.len(), 1, "unexpected diagnostics: {errors:?}");
            assert!(matches!(
                &errors.iter().next().unwrap().kind,
                ErrorKind::ComptimeEvaluationFailed { reason } if reason == expected_reason
            ));
        }
    }

    #[test]
    fn unknown_type_constructor_diagnostic_preserves_placeholder_call_spelling() {
        let errors = compile_to_air("fn main() -> i32 { @size_of(Foo(i32)) }").unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors.iter().next().unwrap().kind,
            ErrorKind::UnknownType(syntax) if syntax == "Foo(...)"
        ));
    }

    #[test]
    fn speculative_global_type_resolution_excludes_file_zero_aliases_and_qualified_paths() {
        let mut sema = gather_declarations_for_testing(
            "struct Leaf { value: i32 }
             const Alias = Leaf;
             const CAP: i32 = 4;
             fn Make() -> type { Leaf }
             fn main() -> i32 { 0 }",
        );
        let leaf = sema.interner.get("Leaf").unwrap();
        let alias = sema.interner.get("Alias").unwrap();
        let make = sema.interner.get_or_intern("Make()");
        let qualified = sema.interner.get_or_intern("api.Leaf");
        let array_const = sema.interner.get_or_intern("[i32; CAP]");
        let fixed_str_const = sema.interner.get_or_intern("Str(CAP)");
        let plain_str = sema.interner.get_or_intern("str");
        let array_literal = sema.interner.get_or_intern("[i32; 4]");
        let fixed_str_literal = sema.interner.get_or_intern("Str(4)");

        assert!(sema.resolve_type_for_comptime(make).is_some());
        assert_eq!(sema.resolve_type_for_comptime(leaf), None);
        assert_eq!(sema.resolve_type_for_comptime(alias), None);
        assert_eq!(sema.resolve_type_for_comptime(qualified), None);
        assert_eq!(sema.resolve_type_for_comptime(array_const), None);
        assert_eq!(sema.resolve_type_for_comptime(fixed_str_const), None);
        assert_eq!(sema.resolve_type_for_comptime(plain_str), None);
        assert!(sema.resolve_type_for_comptime(array_literal).is_some());
        assert!(sema.resolve_type_for_comptime(fixed_str_literal).is_some());

        let cap = sema.interner.get("CAP").unwrap();
        let value_substitutions = HashMap::from([(cap, ConstValue::Integer(4))]);
        assert!(
            sema.resolve_type_for_comptime_with_subst_and_values(
                array_const,
                &HashMap::new(),
                &value_substitutions,
            )
            .is_some()
        );
        assert!(
            sema.resolve_type_for_comptime_with_subst_and_values(
                fixed_str_const,
                &HashMap::new(),
                &value_substitutions,
            )
            .is_some()
        );
    }

    // ========================================================================
    // Place-returning borrow accessors (ADR-0062, RUE-662)
    // ========================================================================

    fn compile_with_accessors(source: &str) -> MultiErrorResult<SemaOutput> {
        let mut features = PreviewFeatures::new();
        features.insert(PreviewFeature::BorrowAccessors);
        compile_to_air_with_preview_features(source, features)
    }

    const GRID_ACCESSOR: &str = "
struct Grid {
    cells: [i64; 4],

    fn at(borrow self, i: u64) -> borrow i64 {
        if i >= 4 {
            @panic(\"index out of bounds\");
        }
        yield self.cells[i];
    }
}
";

    #[test]
    fn accessor_call_inlines_with_no_call_shape() {
        // ADR-0062 §3: a call `g.at(2)` compiles by inlining — guards plus
        // the yielded place — so the caller's AIR must contain NO call to the
        // accessor; the read is an ordinary projected `PlaceRead`.
        let source = format!(
            "{GRID_ACCESSOR}
fn main() -> i32 {{
    let g = Grid {{ cells: [10, 20, 30, 40] }};
    if g.at(2) == 30 {{ 0 }} else {{ 1 }}
}}"
        );
        let output = compile_with_accessors(&source).expect("accessor call compiles");
        let main = output
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("main is analyzed");
        let calls_accessor = main.air.iter().any(|(_, inst)| {
            matches!(&inst.data, AirInstData::Call { name, .. }
            if output.strings.is_empty() || {
                // Call names are interned; compare through the printer-safe path.
                let _ = name;
                false
            })
        });
        assert!(
            !calls_accessor,
            "an accessor call must not lower to an AIR call"
        );
        // The inlined result place is read: main contains a PlaceRead with an
        // index projection, which an ordinary method call would never emit.
        let has_place_read = main
            .air
            .iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::PlaceRead { .. }));
        assert!(has_place_read, "the yielded place is read in the caller");
        // And no Call instruction at all exists in main (the only callee in
        // this program is the accessor).
        let has_any_call = main
            .air
            .iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::Call { .. }));
        assert!(
            !has_any_call,
            "mandatory inlining leaves no call in the caller"
        );
    }

    #[test]
    fn accessor_declaration_requires_preview_gate() {
        let source = format!("{GRID_ACCESSOR}\nfn main() -> i32 {{ 0 }}");
        let errors = compile_to_air(&source).expect_err("the gate is off");
        assert!(errors.iter().any(|error| matches!(
            &error.kind,
            ErrorKind::PreviewFeatureRequired { feature, .. }
                if *feature == PreviewFeature::BorrowAccessors
        )));
    }

    #[test]
    fn accessor_result_cannot_be_returned() {
        let source = format!(
            "{GRID_ACCESSOR}
fn read(borrow g: Grid) -> i64 {{
    return g.at(0);
}}
fn main() -> i32 {{
    let g = Grid {{ cells: [1, 2, 3, 4] }};
    read(borrow g);
    0
}}"
        );
        let errors = compile_with_accessors(&source).expect_err("return escape");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorResultReturned { .. }))
        );
    }

    #[test]
    fn accessor_result_cannot_be_tail_returned() {
        let source = format!(
            "{GRID_ACCESSOR}
fn read(borrow g: Grid) -> i64 {{
    g.at(0)
}}
fn main() -> i32 {{
    let g = Grid {{ cells: [1, 2, 3, 4] }};
    read(borrow g);
    0
}}"
        );
        let errors = compile_with_accessors(&source).expect_err("tail-return escape");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorResultReturned { .. }))
        );
    }

    #[test]
    fn accessor_result_cannot_be_let_bound() {
        let source = format!(
            "{GRID_ACCESSOR}
fn main() -> i32 {{
    let g = Grid {{ cells: [1, 2, 3, 4] }};
    let b = g.at(0);
    0
}}"
        );
        let errors = compile_with_accessors(&source).expect_err("let escape");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorResultBound { .. }))
        );
    }

    #[test]
    fn accessor_result_cannot_be_stored() {
        let source = format!(
            "{GRID_ACCESSOR}
fn main() -> i32 {{
    let g = Grid {{ cells: [1, 2, 3, 4] }};
    let mut x = 0;
    x = g.at(0);
    0
}}"
        );
        let errors = compile_with_accessors(&source).expect_err("store escape");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorResultStored { .. }))
        );
    }

    #[test]
    fn accessor_result_cannot_be_captured_in_aggregate() {
        let source = format!(
            "{GRID_ACCESSOR}
fn main() -> i32 {{
    let g = Grid {{ cells: [1, 2, 3, 4] }};
    let a = [g.at(0)];
    0
}}"
        );
        let errors = compile_with_accessors(&source).expect_err("capture escape");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorResultCaptured { .. }))
        );
    }

    #[test]
    fn accessor_loan_conflicts_with_inout_in_same_expression() {
        // The (Accessor-Call) loan spans the enclosing full expression:
        // `use(v.at(0), bump(inout v))` overlaps a shared accessor loan with
        // an exclusive `inout` loan on the same root (ADR-0062 §2).
        let source = format!(
            "{GRID_ACCESSOR}
fn using(a: i64, b: i64) -> i64 {{ a + b }}
fn bump(inout g: Grid) -> i64 {{ g.cells[0] = 9; 0 }}
fn main() -> i32 {{
    let mut g = Grid {{ cells: [1, 2, 3, 4] }};
    using(g.at(0), bump(inout g));
    0
}}"
        );
        let errors = compile_with_accessors(&source).expect_err("exclusivity conflict");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorLoanConflict { .. }))
        );
    }

    #[test]
    fn accessor_body_requires_trailing_yield() {
        let source = "
struct P {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        self.x
    }
}
fn main() -> i32 {
    let p = P { x: 1 };
    if p.xr() == 1 { 0 } else { 1 }
}";
        let errors = compile_with_accessors(source).expect_err("missing yield");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorBodyMissingYield))
        );
    }

    #[test]
    fn accessor_yield_must_root_at_receiver() {
        let source = "
struct P {
    x: i64,

    fn xr(borrow self, other: i64) -> borrow i64 {
        yield other;
    }
}
fn main() -> i32 {
    let p = P { x: 1 };
    if p.xr(2) == 1 { 0 } else { 1 }
}";
        let errors = compile_with_accessors(source).expect_err("non-receiver yield");
        assert!(errors.iter().any(|error| matches!(
            &error.kind,
            ErrorKind::AccessorYieldNotReceiverRooted { .. }
        )));
    }

    #[test]
    fn accessor_cannot_yield_a_call_to_itself() {
        // 6.6:14: the call is the inlined body, so a self-call in yield
        // position has no finite expansion (E0261) rather than a compiler
        // stack overflow (RUE-1211).
        let source = "
struct P {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        yield self.xr();
    }
}
fn main() -> i32 {
    let p = P { x: 1 };
    if p.xr() == 1 { 0 } else { 1 }
}";
        let errors = compile_with_accessors(source).expect_err("self-recursive accessor");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorRecursion { .. }))
        );
    }

    #[test]
    fn accessor_cannot_call_itself_from_a_guard() {
        // The cycle is rejected wherever the re-entrant call appears, not
        // only in yield position (RUE-1211).
        let source = "
struct P {
    x: i64,

    fn xr(borrow self) -> borrow i64 {
        let _ = self.xr();
        yield self.x;
    }
}
fn main() -> i32 {
    let p = P { x: 1 };
    if p.xr() == 1 { 0 } else { 1 }
}";
        let errors = compile_with_accessors(source).expect_err("self-recursive accessor guard");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorRecursion { .. }))
        );
    }

    #[test]
    fn mutually_recursive_accessors_are_rejected() {
        // A cycle through several accessors is the same non-terminating
        // expansion as a direct self-call, and is rejected at the point the
        // expansion re-enters an accessor already on the stack (6.6:14).
        let source = "
struct P {
    x: i64,

    fn a(borrow self) -> borrow i64 {
        yield self.b();
    }

    fn b(borrow self) -> borrow i64 {
        yield self.a();
    }
}
fn main() -> i32 {
    let p = P { x: 1 };
    if p.a() == 1 { 0 } else { 1 }
}";
        let errors = compile_with_accessors(source).expect_err("mutually recursive accessors");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorRecursion { .. }))
        );
    }

    #[test]
    fn yield_outside_accessor_is_rejected() {
        let errors =
            compile_with_accessors("fn main() -> i32 { yield 1; }").expect_err("stray yield");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::YieldOutsideAccessor))
        );
    }

    #[test]
    fn accessor_requires_borrow_self_receiver() {
        let source = "
struct P {
    x: i64,

    fn xr(inout self) -> borrow i64 {
        yield self.x;
    }
}
fn main() -> i32 { 0 }";
        let errors = compile_with_accessors(source).expect_err("inout self accessor");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorRequiresBorrowSelf { .. }))
        );
    }

    #[test]
    fn free_function_cannot_be_an_accessor() {
        let source = "
fn first(borrow v: i64) -> borrow i64 {
    yield v;
}
fn main() -> i32 { 0 }";
        let errors = compile_with_accessors(source).expect_err("free-fn accessor");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorRequiresBorrowSelf { .. }))
        );
    }

    #[test]
    fn accessor_params_must_be_by_value() {
        let source = "
struct P {
    x: i64,

    fn xr(borrow self, borrow k: i64) -> borrow i64 {
        yield self.x;
    }
}
fn main() -> i32 { 0 }";
        let errors = compile_with_accessors(source).expect_err("borrow accessor param");
        assert!(
            errors
                .iter()
                .any(|error| matches!(&error.kind, ErrorKind::AccessorParamModeUnsupported { .. }))
        );
    }

    #[test]
    fn accessor_guards_execute_before_the_read() {
        // The inlined guards must be part of the caller's AIR: the bounds
        // panic from the accessor body appears in main.
        let source = format!(
            "{GRID_ACCESSOR}
fn main() -> i32 {{
    let g = Grid {{ cells: [10, 20, 30, 40] }};
    if g.at(3) == 40 {{ 0 }} else {{ 1 }}
}}"
        );
        let output = compile_with_accessors(&source).expect("accessor call compiles");
        let main = output
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("main is analyzed");
        let has_panic = main.air.iter().any(|(_, inst)| {
            matches!(
                &inst.data,
                AirInstData::Intrinsic {
                    runtime: Some(crate::RuntimeCallKind::Panic),
                    ..
                }
            )
        });
        assert!(has_panic, "the accessor guard's panic inlines into main");
    }
}
