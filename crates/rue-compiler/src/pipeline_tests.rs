use crate::*;
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_single_source_adapter_executes_each_frontend_phase_once() {
        let snapshot = SourceSnapshot::single("<test>", "fn main() -> i32 { 42 }").unwrap();
        let (_rir, semantic, session) =
            test_frontend_snapshot(&snapshot, &CompileOptions::default()).unwrap();
        assert_eq!(session.last_parse.syntax.parser_invocations, 1);
        assert_eq!(session.rir.executions, 1);
        assert_eq!(session.last_rir.modules_visited, 1);
        assert_eq!(semantic.work().binding.bind_invocations, 1);
        assert_eq!(semantic.work().cfg.cfg_builds_attempted, 1);
        assert_eq!(semantic.work().cfg.cfg_builds_succeeded, 1);
        assert_eq!(semantic.functions().len(), 1);
    }

    #[test]
    fn documented_snapshot_session_embedding_carries_complete_identity() {
        let root = FileId::new(7);
        let paths = std::collections::HashMap::from([(root, "src/main.rue".to_owned())]);
        let metadata = SourceMetadata::new(root, paths.clone(), paths).unwrap();
        let snapshot = SourceSnapshot::new(
            metadata,
            vec![(root, Arc::new("fn main() -> i32 { 0 }".to_owned()))],
        )
        .unwrap();
        let options = CompileOptions::default();
        let expected_link = LinkInputDescriptor::from_compile_options(&snapshot, &options);

        let mut session = CompilerSession::new();
        session.update(&snapshot).into_result().unwrap();
        let semantic = session.semantic(&options).unwrap();

        assert_eq!(semantic.functions().len(), 1);
        assert_eq!(semantic.input(), &expected_link.codegen);
        assert_eq!(
            semantic.input().semantic.sources.root(),
            snapshot.source_revision().root()
        );
    }

    fn assert_invalid_compiler_input(errors: CompileErrors, expected: &str) {
        assert_eq!(errors.len(), 1);
        let error = errors.iter().next().unwrap();
        assert!(
            matches!(&error.kind, ErrorKind::InvalidCompilerInput(_)),
            "expected E1400, got {error:?}"
        );
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn ast_presentation_reports_syntax_errors_in_caller_selected_order() {
        let root = FileId::new(4);
        let helper = FileId::new(9);
        let sources = [
            SourceView::new("helper.rue", "fn helper() -> i32 { # }", helper),
            SourceView::new("main.rue", "fn main() -> i32 { $ }", root),
        ];
        let metadata =
            SourceMetadata::from_sources(&sources, root, std::collections::HashMap::new()).unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata.clone()).unwrap();

        let errors = parse_source_snapshot_for_ast_presentation(&snapshot).unwrap_err();
        assert_eq!(
            errors
                .iter()
                .map(|error| error.span().unwrap().file_id)
                .collect::<Vec<_>>(),
            [helper, root]
        );
    }

    #[test]
    fn canonical_batch_rejects_metadata_drift_before_lexing() {
        let file_id = FileId::new(3);
        let metadata = SourceMetadata::new(
            file_id,
            std::collections::HashMap::from([(file_id, "expected.rue".to_string())]),
            std::collections::HashMap::from([(file_id, "stable.rue".to_string())]),
        )
        .unwrap();
        let sources = [SourceView::new("actual.rue", "$", file_id)];

        let errors =
            test_compile_sources_with_metadata(&sources, &metadata, &CompileOptions::default())
                .unwrap_err();
        assert_invalid_compiler_input(
            errors,
            "invalid compiler input: physical path for 3 is \"expected.rue\", but source file uses \"actual.rue\"",
        );
    }

    #[test]
    fn borrowed_and_snapshot_batch_routes_produce_identical_artifacts() {
        let root = FileId::new(7);
        let helper = FileId::new(2);
        let sources = [
            SourceView::new(
                "/checkout/main.rue",
                "const helper = @import(\"helper.rue\"); fn main() -> i32 { let unused = 0; helper.answer() }",
                root,
            ),
            SourceView::new(
                "/checkout/helper.rue",
                "pub fn answer() -> i32 { 42 }",
                helper,
            ),
        ];
        let metadata = SourceMetadata::from_sources(
            &sources,
            root,
            std::collections::HashMap::from([
                (root, "main.rue".to_string()),
                (helper, "helper.rue".to_string()),
            ]),
        )
        .unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata.clone()).unwrap();

        let borrowed =
            test_compile_sources_with_metadata(&sources, &metadata, &CompileOptions::default())
                .unwrap();
        let owned = compile_snapshot(&snapshot, &CompileOptions::default()).unwrap();

        assert_eq!(borrowed.elf, owned.elf);
        assert_eq!(borrowed.source_stats, owned.source_stats);
        assert!(!borrowed.warnings.is_empty());
        assert_eq!(
            borrowed
                .warnings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            owned
                .warnings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn canonical_batch_reports_one_pass_structural_work() {
        let root = FileId::new(7);
        let helper = FileId::new(2);
        let sources = [
            SourceView::new(
                "/checkout/main.rue",
                "const helper = @import(\"helper.rue\"); fn main() -> i32 { helper.answer() }",
                root,
            ),
            SourceView::new(
                "/checkout/helper.rue",
                "pub fn answer() -> i32 { 42 }",
                helper,
            ),
        ];
        let metadata = SourceMetadata::from_sources(
            &sources,
            root,
            std::collections::HashMap::from([
                (root, "main.rue".to_string()),
                (helper, "helper.rue".to_string()),
            ]),
        )
        .unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata).unwrap();

        let output = compile_snapshot(&snapshot, &CompileOptions::default()).unwrap();
        let stats = output.source_stats;
        let work = output.work;

        assert_eq!(stats.files, sources.len());
        assert_eq!(work.parsed.modules_considered, sources.len());
        assert_eq!(work.parsed.modules_reparsed, sources.len());
        assert_eq!(work.parsed.syntax.lexer_invocations, sources.len());
        assert_eq!(work.parsed.syntax.parser_invocations, sources.len());
        assert_eq!(work.merged.parser_invocations, 0);
        assert_eq!(work.merged.ast_payload_clones, 0);
        assert_eq!(work.merged.source_text_clones, 0);
        assert_eq!(work.merged.source_bytes_rehashed, 0);
        assert_eq!(work.lowered.parser_invocations, 0);
        assert_eq!(work.lowered.ast_payload_clones, 0);
        assert_eq!(work.lowered.source_text_clones, 0);
        assert_eq!(work.semantic.binding.bind_invocations, 1);
        assert_eq!(work.semantic.manifest.build_invocations, 1);
        assert_eq!(work.semantic.declaration_reuse.semantic_epochs_started, 1);
        assert_eq!(work.semantic.declaration_reuse.declaration_indexes_built, 1);
        assert_eq!(
            work.semantic.declaration_reuse.shell_predeclaration_epochs,
            1
        );
        assert_eq!(work.semantic.cfg.cfg_builds_attempted, 2);
        assert_eq!(work.semantic.cfg.cfg_builds_succeeded, 2);
        assert!(!work.semantic.stable_ids_requested);
        assert!(work.semantic.bound_definitions.is_none());
    }

    #[test]
    fn canonical_batch_frontend_errors_are_deterministic() {
        let cases = [
            ("fn z() -> i32 { # }", "fn main() -> i32 { $ }"),
            (
                "fn dup() {} fn dup() {} fn main() -> i32 { 0 }",
                "fn helper() {}",
            ),
            (
                "struct Dup {} struct Dup {} fn main() -> i32 { 0 }",
                "fn helper() {}",
            ),
            (
                "struct clash {} fn clash() {} fn main() -> i32 { 0 }",
                "fn helper() {}",
            ),
            ("fn main() -> i32 { 1 }", "fn main() -> i32 { 2 }"),
            ("fn main() -> i32 { missing_name }", "fn helper() {}"),
        ];
        for (left, right) in cases {
            for reversed in [false, true] {
                let mut sources = vec![
                    SourceView::new("z.rue", left, FileId::new(9)),
                    SourceView::new("a.rue", right, FileId::new(2)),
                ];
                if reversed {
                    sources.reverse();
                }
                let metadata = SourceMetadata::from_sources(
                    &sources,
                    sources[0].file_id,
                    std::collections::HashMap::new(),
                )
                .unwrap();
                let snapshot = SourceSnapshot::from_sources(&sources, metadata).unwrap();
                let options = CompileOptions::default();
                let canonical_errors = compile_snapshot(&snapshot, &options).unwrap_err();
                let repeated = compile_snapshot(&snapshot, &options).unwrap_err();
                let fingerprint = |errors: &CompileErrors| {
                    errors
                        .iter()
                        .map(|error| {
                            (
                                error.kind.code().to_string(),
                                error.span(),
                                error.to_string(),
                                format!("{:?}", error.diagnostic()),
                            )
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    fingerprint(&canonical_errors),
                    fingerprint(&repeated),
                    "diagnostics changed between identical queries: reversed={reversed}, left={left}"
                );
            }
        }
    }

    #[test]
    fn canonical_batch_output_is_stable_for_relocated_import_graph() {
        let snapshot = |root: FileId, helper: FileId, directory: &str, reversed: bool| {
            let main_path = format!("{directory}/main.rue");
            let helper_path = format!("{directory}/helper.rue");
            let mut sources = vec![
                SourceView::new(
                    &main_path,
                    "const helper = @import(\"helper.rue\"); fn main() -> i32 { let unused = 1; helper.answer() }",
                    root,
                ),
                SourceView::new(&helper_path, "pub fn answer() -> i32 { 42 }", helper),
            ];
            if reversed {
                sources.reverse();
            }
            let metadata = SourceMetadata::from_sources(
                &sources,
                root,
                std::collections::HashMap::from([
                    (root, "main.rue".to_string()),
                    (helper, "helper.rue".to_string()),
                ]),
            )
            .unwrap();
            SourceSnapshot::from_sources(&sources, metadata).unwrap()
        };
        let snapshots = [
            snapshot(FileId::new(7), FileId::new(2), "/first/checkout", false),
            snapshot(FileId::new(31), FileId::new(80), "/moved/checkout", true),
        ];

        for opt_level in [OptLevel::O0, OptLevel::O1] {
            let options = CompileOptions {
                opt_level,
                ..CompileOptions::default()
            };
            let mut expected = None;
            for snapshot in &snapshots {
                let canonical = compile_snapshot(snapshot, &options).unwrap();
                let warnings = canonical
                    .warnings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();

                if let Some((elf, expected_warnings)) = &expected {
                    assert_eq!(elf, &canonical.elf);
                    assert_eq!(expected_warnings, &warnings);
                } else {
                    expected = Some((canonical.elf, warnings));
                }
            }
        }

        for options in [
            CompileOptions {
                linker: LinkerMode::System("/definitely/missing/rue-linker".to_string()),
                ..CompileOptions::default()
            },
            CompileOptions {
                target: *Target::all()
                    .iter()
                    .find(|&&target| target != Target::host().unwrap())
                    .expect("at least one non-host target"),
                ..CompileOptions::default()
            },
        ] {
            compile_snapshot(&snapshots[0], &options).unwrap_err();
        }
    }

    #[test]
    fn production_sources_do_not_restore_the_retired_peer_orchestrator() {
        let retired_type = ["Compilation", "Unit"].concat();
        let retired_module = ["mod ", "unit", ";"].concat();
        for (path, source) in [
            ("rue-compiler/src/lib.rs", include_str!("lib.rs")),
            ("rue-compiler/src/session.rs", include_str!("session.rs")),
            (
                "rue-compiler/src/parsed_modules.rs",
                include_str!("parsed_modules.rs"),
            ),
            (
                "rue-compiler/src/canonical_merge.rs",
                include_str!("canonical_merge.rs"),
            ),
            (
                "rue-compiler/src/canonical_lower.rs",
                include_str!("canonical_lower.rs"),
            ),
            (
                "rue-compiler/src/canonical_semantic.rs",
                include_str!("canonical_semantic.rs"),
            ),
        ] {
            assert!(
                !source.contains(&retired_type),
                "{path} must query CompilerSession directly"
            );
            assert!(
                !source.contains(&retired_module),
                "{path} must not restore independent unit.rs phase sequencing"
            );
        }
    }

    #[test]
    fn test_embedded_runtime_is_valid() {
        validate_runtime().expect("embedded runtime should be valid");
    }

    #[test]
    fn test_runtime_validation_rejects_archive_without_objects() {
        let err = parse_runtime_archive(b"!<arch>\n")
            .expect_err("empty archive wrapper must not validate as a usable runtime");

        assert_eq!(err, "embedded rue-runtime archive contains no object files");
    }

    /// The embedded runtime is host-only (RUE-36 / ADR-0034): linking for
    /// the host target must succeed; any other target must be refused with
    /// an error that names both targets and points at `--emit asm`.
    #[test]
    fn test_runtime_only_available_for_host_target() {
        let host = Target::host().unwrap();
        assert!(runtime_for_target(host).is_ok());

        for &target in Target::all() {
            if target == host {
                continue;
            }
            let err =
                runtime_for_target(target).expect_err("cross-target link must be refused (RUE-36)");
            let msg = err.to_string();
            assert!(msg.contains(&target.to_string()), "names target: {msg}");
            assert!(msg.contains(&host.to_string()), "names host: {msg}");
            assert!(msg.contains("RUE-36"), "references RUE-36: {msg}");
            assert!(msg.contains("--emit asm"), "suggests --emit asm: {msg}");
        }
    }

    #[test]
    fn test_runtime_refused_on_unsupported_host() {
        let err = runtime_for_target_with_host(Target::X86_64Linux, None, "x86-64-macos")
            .expect_err("unsupported host must not link by pretending to be another target");
        let msg = err.to_string();

        assert!(msg.contains("x86-64-linux"), "names target: {msg}");
        assert!(msg.contains("x86-64-macos"), "names host: {msg}");
        assert!(msg.contains("RUE-36"), "references RUE-36: {msg}");
        assert!(msg.contains("--emit asm"), "suggests --emit asm: {msg}");
    }

    /// RUE-347: a break-less `loop {}` in value position has type `!` and
    /// must NOT wire a unit value into the enclosing join's typed block
    /// parameter. The CFG verifier type-checks every edge — including edges
    /// from unreachable blocks, where this bug parked — so compiling at all
    /// (verify() runs inside optimize()) proves the invariant.
    #[test]
    fn test_breakless_loop_value_position_join_well_typed() {
        test_cfg(
            "fn cond() -> bool { false }\n\
             fn diverge() -> ! { loop {} }\n\
             struct P { a: i64, b: i64 }\n\
             fn main() -> i32 {\n\
                 let x: i32 = if cond() { 42 } else { loop {} };\n\
                 let y: i32 = if cond() { 1 } else { diverge() };\n\
                 let s: P = match x {\n\
                     42 => P { a: 1, b: 2 },\n\
                     _ => loop {},\n\
                 };\n\
                 @dbg(s.a);\n\
                 x + y\n\
             }",
        )
        .expect("never-typed arms (break-less loop, `-> !` call) must diverge, not thread a value into a typed join (RUE-347)");
    }

    #[test]
    fn test_compile_simple() {
        let output = test_compile_source("fn main() -> i32 { 42 }").unwrap();
        // Should produce a valid executable (ELF on Linux, Mach-O on macOS)
        let magic = &output.elf[0..4];
        let is_elf = magic == &[0x7F, b'E', b'L', b'F'];
        let is_macho = magic == &0xFEEDFACF_u32.to_le_bytes();
        assert!(
            is_elf || is_macho,
            "should produce valid ELF or Mach-O binary"
        );
    }

    #[test]
    fn test_compile_no_main() {
        let result = test_compile_source("fn foo() -> i32 { 42 }");
        assert!(result.is_err());
    }

    #[test]
    fn test_unused_variable_warning() {
        let output = test_compile_source("fn main() -> i32 { let x = 42; 0 }").unwrap();
        assert_eq!(output.warnings.len(), 1);
        assert!(output.warnings[0].to_string().contains("unused variable"));
        assert!(output.warnings[0].to_string().contains("'x'"));
    }

    #[test]
    fn test_underscore_prefix_no_warning() {
        let output = test_compile_source("fn main() -> i32 { let _x = 42; 0 }").unwrap();
        assert_eq!(output.warnings.len(), 0);
    }

    #[test]
    fn test_used_variable_no_warning() {
        let output = test_compile_source("fn main() -> i32 { let x = 42; x }").unwrap();
        assert_eq!(output.warnings.len(), 0);
    }

    #[test]
    fn test_test_cfg_includes_warnings() {
        let state = test_cfg("fn main() -> i32 { let x = 42; 0 }").unwrap();
        assert_eq!(state.warnings.len(), 1);
        assert!(state.warnings[0].to_string().contains("unused variable"));
    }

    #[test]
    fn test_multiple_errors_collected() {
        // Test that errors from multiple functions are collected together
        // Use examples that both result in type mismatch errors
        // Note: Functions must be called from main() to be analyzed (lazy analysis)
        let source = r#"
            fn foo() -> i32 { true }
            fn bar() -> i32 { false }
            fn main() -> i32 { foo() + bar() }
        "#;
        let result = test_cfg(source);
        let errors = match result {
            Ok(_) => panic!("expected error, got success"),
            Err(e) => e,
        };

        // Should have at least 2 errors (one from foo, one from bar)
        assert!(
            errors.len() >= 2,
            "expected at least 2 errors, got {}",
            errors.len()
        );

        // All errors should be type mismatches (returning bool where i32 expected)
        for error in errors.iter() {
            assert!(
                error.to_string().contains("type mismatch"),
                "expected type mismatch error, got: {}",
                error
            );
        }
    }

    #[test]
    fn test_multiple_errors_display() {
        // Use examples that both result in type mismatch errors
        // Note: Functions must be called from main() to be analyzed (lazy analysis)
        let source = r#"
            fn foo() -> i32 { true }
            fn bar() -> i32 { false }
            fn main() -> i32 { foo() + bar() }
        "#;
        let errors = match test_cfg(source) {
            Ok(_) => panic!("expected error, got success"),
            Err(e) => e,
        };

        // Display should show both errors
        let display = errors.to_string();
        assert!(
            display.contains("type mismatch"),
            "display should contain error message"
        );
        if errors.len() > 1 {
            assert!(
                display.contains("more error"),
                "display should indicate more errors"
            );
        }
    }

    #[test]
    fn test_single_error_still_works() {
        // Single error should still be collected and returned properly
        let source = "fn main() -> i32 { true }";
        let errors = match test_cfg(source) {
            Ok(_) => panic!("expected error, got success"),
            Err(e) => e,
        };

        assert_eq!(errors.len(), 1);
        assert!(
            errors
                .first()
                .unwrap()
                .to_string()
                .contains("type mismatch")
        );
    }

    // Cross-File Semantic Analysis Tests
    // ========================================================================

    #[test]
    fn test_cross_file_function_call() {
        // Function in main.rue calls function in utils.rue through its module.
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"const utils = @import("utils.rue");
                fn main() -> i32 { utils.helper() }"#,
                FileId::new(1),
            ),
            SourceView::new("utils.rue", "pub fn helper() -> i32 { 42 }", FileId::new(2)),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file function call should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_function_call_with_args() {
        // Function in main.rue calls function in utils.rue with arguments
        // through its module.
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"const utils = @import("utils.rue");
                fn main() -> i32 { utils.add(10, 32) }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "utils.rue",
                "pub fn add(a: i32, b: i32) -> i32 { a + b }",
                FileId::new(2),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file function call with args should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_struct_usage() {
        // Struct defined in types.rue, used explicitly through its module.
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"const types = @import("types.rue");
                fn main() -> i32 {
                    let p = types.Point { x: 1, y: 2 };
                    p.x + p.y
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "types.rue",
                "pub struct Point { x: i32, y: i32 }",
                FileId::new(2),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file struct usage should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_struct_as_function_param() {
        // Struct defined in types.rue, function in utils.rue takes it as an
        // explicitly module-qualified param type.
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"const types = @import("types.rue");
                const utils = @import("utils.rue");
                fn main() -> i32 {
                    let p = types.Point { x: 10, y: 5 };
                    utils.get_sum(p)
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "types.rue",
                "pub struct Point { x: i32, y: i32 }",
                FileId::new(2),
            ),
            SourceView::new(
                "utils.rue",
                r#"const types = @import("types.rue");
                pub fn get_sum(p: types.Point) -> i32 { p.x + p.y }"#,
                FileId::new(3),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file struct as function param should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_enum_usage() {
        // Enum defined in types.rue, used explicitly through its module.
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"const types = @import("types.rue");
                fn main() -> i32 {
                    let c = types.Color.Red;
                    match c {
                        types.Color.Red => 1,
                        types.Color.Green => 2,
                        types.Color.Blue => 3,
                    }
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "types.rue",
                "pub enum Color { Red, Green, Blue }",
                FileId::new(2),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "cross-file enum usage should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_unqualified_sibling_type_and_const_lookup_is_rejected() {
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"const lib = @import("lib.rue");
                fn main() -> i32 {
                    let s = Shared { n: LIMIT };
                    match Mode::Fast { Mode::Fast => s.n, Mode::Slow => 0 }
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "lib.rue",
                r#"pub struct Shared { n: i32 }
                pub enum Mode { Fast, Slow }
                pub const LIMIT: i32 = 11;"#,
                FileId::new(2),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_err(),
            "loaded sibling modules should not inject public types or constants into the caller's unqualified namespace"
        );
    }

    #[test]
    fn test_cross_file_no_main_function() {
        // No main function in any file
        let sources = vec![
            SourceView::new("a.rue", "fn foo() -> i32 { 1 }", FileId::new(1)),
            SourceView::new("b.rue", "fn bar() -> i32 { 2 }", FileId::new(2)),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(result.is_err(), "should fail without main function");

        let errors = result.unwrap_err();
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("main") && err_msg.contains("function"),
            "error should mention missing main function: {}",
            err_msg
        );
    }

    #[test]
    fn test_cross_file_duplicate_main() {
        // main() defined in multiple files
        let sources = vec![
            SourceView::new("a.rue", "fn main() -> i32 { 1 }", FileId::new(1)),
            SourceView::new("b.rue", "fn main() -> i32 { 2 }", FileId::new(2)),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(result.is_err(), "should fail with duplicate main");

        let errors = result.unwrap_err();
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("main"),
            "error should mention duplicate main: {}",
            err_msg
        );
    }

    #[test]
    fn test_cross_file_undefined_function() {
        // main.rue calls function that doesn't exist
        let sources = vec![
            SourceView::new(
                "main.rue",
                "fn main() -> i32 { nonexistent() }",
                FileId::new(1),
            ),
            SourceView::new("utils.rue", "fn helper() -> i32 { 42 }", FileId::new(2)),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(result.is_err(), "should fail with undefined function");

        let errors = result.unwrap_err();
        let err_msg = errors.first().unwrap().to_string();
        assert!(
            err_msg.contains("nonexistent") || err_msg.contains("undefined"),
            "error should mention undefined function: {}",
            err_msg
        );
    }

    #[test]
    fn test_cross_file_three_files_chain() {
        // main.rue -> utils.rue -> math.rue chain of module-qualified calls
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"const utils = @import("utils.rue");
                fn main() -> i32 { utils.compute(6, 7) }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "utils.rue",
                r#"const math = @import("math.rue");
                pub fn compute(a: i32, b: i32) -> i32 { math.multiply(a, b) }"#,
                FileId::new(2),
            ),
            SourceView::new(
                "math.rue",
                "pub fn multiply(x: i32, y: i32) -> i32 { x * y }",
                FileId::new(3),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "chain of cross-file calls should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cross_file_mutual_calls() {
        // Two files calling each other through explicit module bindings
        // (mutual recursion remains possible).
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"const utils = @import("utils.rue");
                fn main() -> i32 { is_even(4) }
                pub fn is_even(n: i32) -> i32 {
                    if n == 0 { 1 } else { utils.is_odd(n - 1) }
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "utils.rue",
                r#"const mainmod = @import("main.rue");
                pub fn is_odd(n: i32) -> i32 {
                    if n == 0 { 0 } else { mainmod.is_even(n - 1) }
                }"#,
                FileId::new(2),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "mutual cross-file calls should compile: {:?}",
            result.err()
        );
    }

    // ========================================================================
    // Module Import Tests
    // ========================================================================

    #[test]
    fn test_module_member_access() {
        // Test that @import returns a module type and member access works
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"fn main() -> i32 {
                    let math = @import("math.rue");
                    math.add(1, 2)
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "math.rue",
                "fn add(a: i32, b: i32) -> i32 { a + b }",
                FileId::new(2),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "module member access should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_module_member_access_same_function_names_in_distinct_modules() {
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"fn main() -> i32 {
                    let left = @import("left.rue");
                    let right = @import("right.rue");
                    left.value() + right.value()
                }"#,
                FileId::new(1),
            ),
            SourceView::new("left.rue", "fn value() -> i32 { 10 }", FileId::new(2)),
            SourceView::new("right.rue", "fn value() -> i32 { 20 }", FileId::new(3)),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "qualified calls to same-named functions in distinct modules should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_module_local_unqualified_calls_with_duplicate_names() {
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"fn main() -> i32 {
                    let left = @import("left.rue");
                    let right = @import("right.rue");
                    left.entry() + right.entry()
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "left.rue",
                r#"pub fn entry() -> i32 { value() + first() + recur(1) }
                fn value() -> i32 { 10 }
                fn first() -> i32 { second() }
                fn second() -> i32 { 1 }
                fn recur(n: i32) -> i32 {
                    if n == 0 { 0 } else { recur(n - 1) }
                }"#,
                FileId::new(2),
            ),
            SourceView::new(
                "right.rue",
                r#"pub fn entry() -> i32 { value() + first() + recur(1) }
                fn value() -> i32 { 20 }
                fn first() -> i32 { second() }
                fn second() -> i32 { 2 }
                fn recur(n: i32) -> i32 {
                    if n == 0 { 0 } else { recur(n - 1) }
                }"#,
                FileId::new(3),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "unqualified calls should resolve within the caller's module even when sibling modules define the same names: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_module_duplicate_function_symbols_use_stable_paths_not_file_ids() {
        fn duplicate_value_function_names(
            physical_root: &str,
            main_id: FileId,
            left_id: FileId,
            right_id: FileId,
            reverse_siblings: bool,
        ) -> Vec<String> {
            let main_path = format!("{physical_root}/main.rue");
            let left_path = format!("{physical_root}/left.rue");
            let right_path = format!("{physical_root}/right.rue");
            let mut sources = vec![
                SourceView::new(
                    &main_path,
                    r#"fn main() -> i32 {
                        let left = @import("left.rue");
                        let right = @import("right.rue");
                        left.value() + right.value()
                    }"#,
                    main_id,
                ),
                SourceView::new(&left_path, "fn value() -> i32 { 10 }", left_id),
                SourceView::new(&right_path, "fn value() -> i32 { 20 }", right_id),
            ];
            if reverse_siblings {
                sources.swap(1, 2);
            }
            let source_metadata = SourceMetadata::from_sources(
                &sources,
                main_id,
                std::collections::HashMap::from([
                    (main_id, "main.rue".to_string()),
                    (left_id, "left.rue".to_string()),
                    (right_id, "right.rue".to_string()),
                ]),
            )
            .unwrap();
            let snapshot = SourceSnapshot::from_sources(&sources, source_metadata).unwrap();
            let (_, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())
                .expect("frontend should compile");
            let mut names: Vec<_> = semantic
                .functions()
                .iter()
                .map(|func| func.analyzed.name.clone())
                .filter(|name| name.ends_with("__value"))
                .collect();
            names.sort();
            names
        }

        let names = duplicate_value_function_names(
            "/tmp/rue-short-root",
            FileId::new(1),
            FileId::new(2),
            FileId::new(3),
            false,
        );
        assert_eq!(
            names,
            vec![
                "__rue_fn_left_2erue__value".to_string(),
                "__rue_fn_right_2erue__value".to_string(),
            ]
        );

        assert_eq!(
            names,
            duplicate_value_function_names(
                "/tmp/a-deliberately-much-longer-relocated-rue-root",
                FileId::new(100),
                FileId::new(42),
                FileId::new(7),
                true,
            ),
            "generated symbols should ignore physical roots, FileIds, and source-vector order"
        );
    }

    #[test]
    fn test_colliding_type_symbols_use_stable_paths_not_file_ids() {
        fn colliding_type_names(
            physical_root: &str,
            left_id: FileId,
            right_id: FileId,
            reverse_siblings: bool,
        ) -> std::collections::BTreeSet<String> {
            let main_path = format!("{physical_root}/main.rue");
            let left_path = format!("{physical_root}/left/shared.rue");
            let right_path = format!("{physical_root}/right/shared.rue");
            let mut sources = vec![
                SourceView::new(
                    &main_path,
                    r#"fn main() -> i32 {
                        let left = @import("left/shared.rue");
                        let right = @import("right/shared.rue");
                        left.entry() + right.entry()
                    }"#,
                    FileId::new(1),
                ),
                SourceView::new(
                    &left_path,
                    r#"pub struct Payload {
                        value: i32,
                        text: StrBuf,
                        fn score(borrow self) -> i32 { self.value }
                    }
                    drop fn Payload(self) { @dbg(self.value); }
                    pub enum Choice { Empty, Text(StrBuf) }
                    pub fn entry() -> i32 {
                        let payload = Payload { value: 10, text: "left" };
                        payload.score()
                    }"#,
                    left_id,
                ),
                SourceView::new(
                    &right_path,
                    r#"pub struct Payload {
                        value: i32,
                        text: StrBuf,
                        fn score(borrow self) -> i32 { self.value }
                    }
                    drop fn Payload(self) { @dbg(self.value); }
                    pub enum Choice { Empty, Text(StrBuf) }
                    pub fn entry() -> i32 {
                        let payload = Payload { value: 20, text: "right" };
                        payload.score()
                    }"#,
                    right_id,
                ),
            ];
            if reverse_siblings {
                sources.swap(1, 2);
            }

            let source_metadata = SourceMetadata::from_sources(
                &sources,
                FileId::new(1),
                std::collections::HashMap::from([
                    (FileId::new(1), "main.rue".to_string()),
                    (left_id, "left/shared.rue".to_string()),
                    (right_id, "right/shared.rue".to_string()),
                ]),
            )
            .unwrap();
            let snapshot = SourceSnapshot::from_sources(&sources, source_metadata).unwrap();
            let (_, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())
                .expect("frontend should compile");
            let pool = semantic.type_pool();
            let mut names = std::collections::BTreeSet::new();
            for id in pool.all_struct_ids() {
                if pool.struct_def(id).name == "Payload" {
                    names.insert(format!("struct:{}", pool.struct_symbol_name(id)));
                }
            }
            for id in pool.all_enum_ids() {
                if pool.enum_def(id).name == "Choice" {
                    names.insert(format!("enum:{}", pool.enum_symbol_name(id)));
                }
            }
            for function in semantic.functions() {
                let name = &function.analyzed.name;
                if name.contains("Payload$") || name.contains("Choice$") {
                    names.insert(format!("fn:{name}"));
                }
            }
            names
        }

        let expected: std::collections::BTreeSet<_> = [
            "struct:Payload$left_2fshared_2erue",
            "struct:Payload$right_2fshared_2erue",
            "enum:Choice$left_2fshared_2erue",
            "enum:Choice$right_2fshared_2erue",
            "fn:Payload$left_2fshared_2erue.score",
            "fn:Payload$right_2fshared_2erue.score",
            "fn:Payload$left_2fshared_2erue.__drop",
            "fn:Payload$right_2fshared_2erue.__drop",
            "fn:__rue_drop_Payload$left_2fshared_2erue",
            "fn:__rue_drop_Payload$right_2fshared_2erue",
            "fn:__rue_drop_Choice$left_2fshared_2erue",
            "fn:__rue_drop_Choice$right_2fshared_2erue",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(
            colliding_type_names("/tmp/rue-short-root", FileId::new(2), FileId::new(3), false,),
            expected
        );
        assert_eq!(
            colliding_type_names(
                "/tmp/a-deliberately-much-longer-relocated-rue-root",
                FileId::new(42),
                FileId::new(7),
                true,
            ),
            expected,
            "type-derived symbols should ignore physical roots, FileIds, and source-vector order"
        );
    }

    #[test]
    fn test_cross_kind_type_collision_qualifies_drop_glue() {
        let main_id = FileId::new(1);
        let struct_id = FileId::new(2);
        let enum_id = FileId::new(3);
        let sources = vec![
            SourceView::new("main.rue", "fn main() -> i32 { 0 }", main_id),
            SourceView::new(
                "left/clash.rue",
                "pub struct Clash { text: StrBuf }",
                struct_id,
            ),
            SourceView::new(
                "right/clash.rue",
                "pub enum Clash { Text(StrBuf) }",
                enum_id,
            ),
        ];

        let source_metadata = SourceMetadata::from_sources(
            &sources,
            main_id,
            std::collections::HashMap::from([
                (main_id, "main.rue".to_string()),
                (struct_id, "left/clash.rue".to_string()),
                (enum_id, "right/clash.rue".to_string()),
            ]),
        )
        .unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, source_metadata).unwrap();
        let (_, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())
            .expect("frontend should compile");
        let drop_glue_names: std::collections::BTreeSet<_> = semantic
            .functions()
            .iter()
            .map(|function| function.analyzed.name.as_str())
            .filter(|name| name.starts_with("__rue_drop_Clash"))
            .collect();
        assert_eq!(
            drop_glue_names,
            std::collections::BTreeSet::from([
                "__rue_drop_Clash$left_2fclash_2erue",
                "__rue_drop_Clash$right_2fclash_2erue",
            ])
        );
    }

    #[test]
    fn test_imported_private_helper_called_by_public_api_is_not_unused() {
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"const lib = @import("lib.rue");

                fn main() -> i32 { 0 }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "lib.rue",
                r#"pub fn api() -> i32 { helper() }

                fn helper() -> i32 { 1 }"#,
                FileId::new(2),
            ),
        ];
        let metadata = SourceMetadata::from_sources(
            &sources,
            FileId::new(1),
            std::collections::HashMap::new(),
        )
        .unwrap();
        let snapshot = SourceSnapshot::from_sources(&sources, metadata).unwrap();
        let (_, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())
            .expect("frontend should compile");
        let warnings = semantic
            .warnings()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !warnings.contains("helper"),
            "a private helper with a static call site should not be reported as unused:\n{warnings}"
        );
    }

    #[test]
    fn test_module_member_access_multiple_functions() {
        // Test accessing multiple functions from an imported module
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"fn main() -> i32 {
                    let math = @import("math.rue");
                    let sum = math.add(10, 20);
                    let diff = math.sub(sum, 5);
                    diff
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "math.rue",
                r#"fn add(a: i32, b: i32) -> i32 { a + b }
                fn sub(a: i32, b: i32) -> i32 { a - b }"#,
                FileId::new(2),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "module with multiple functions should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_module_type_constructor_anonymous_method_is_emitted() {
        // A module-qualified comptime type constructor may return an anonymous
        // struct with methods. The lazy frontend must treat a later receiver
        // method call as a dependency, or backend linking sees a call to
        // `__anon_struct_N.method` without an emitted method body.
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"fn main() -> i32 {
                    let lib = @import("lib.rue");
                    let Box = lib.Box(i32);
                    let boxed = Box { value: 42 };
                    boxed.get()
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "lib.rue",
                r#"pub fn Box(comptime T: type) -> type {
                    struct {
                        value: T,
                        fn get(borrow self) -> T { self.value }
                    }
                }"#,
                FileId::new(2),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "module-qualified anonymous method should be emitted: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_module_qualified_std_option_type_constructor_uses_resolved_member() {
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"fn main() -> i32 {
                    let std = @import("std.rue");
                    let OptI = std.option.Option(i64);
                    let n: i64 = 42;
                    let value = OptI.Some(n);
                    match value {
                        OptI.Some(n) => 1,
                        OptI.None => 0,
                    }
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "std.rue",
                r#"pub const option = @import("option.rue");"#,
                FileId::new(2),
            ),
            SourceView::new(
                "option.rue",
                r#"pub fn Option(comptime T: type) -> type {
                    enum {
                        Some(T),
                        None,
                    }
                }"#,
                FileId::new(3),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_ok(),
            "std.option.Option should resolve through the module member path: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_module_undefined_function_error() {
        // Test that accessing an undefined function in a module produces an error
        let sources = vec![
            SourceView::new(
                "main.rue",
                r#"fn main() -> i32 {
                    let math = @import("math.rue");
                    math.nonexistent(1, 2)
                }"#,
                FileId::new(1),
            ),
            SourceView::new(
                "math.rue",
                "fn add(a: i32, b: i32) -> i32 { a + b }",
                FileId::new(2),
            ),
        ];
        let result = test_compile_sources(&sources, &CompileOptions::default());
        assert!(
            result.is_err(),
            "undefined module function should fail to compile"
        );
        let err = result.err().unwrap().to_string();
        assert!(
            err.contains("undefined function") || err.contains("nonexistent"),
            "error should mention undefined function: {}",
            err
        );
    }
}
